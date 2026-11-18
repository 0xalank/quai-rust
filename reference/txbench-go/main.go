// Offline-only benchmark of the pinned go-quai transaction pipeline.
package main

import (
	"bufio"
	"bytes"
	"encoding/hex"
	"encoding/json"
	"io"
	"math/big"
	"os"
	"runtime"
	"sync"
	"time"

	"github.com/btcsuite/btcd/btcec/v2"
	"github.com/btcsuite/btcd/btcec/v2/schnorr/musig2"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/log"
	"google.golang.org/protobuf/proto"
)

var referenceRevision string
var buildCGO string

type vector struct {
	ID             string  `json:"id"`
	Type           int     `json:"type"`
	Input          string  `json:"input"`
	Canonical      string  `json:"canonical"`
	SigningBytes   string  `json:"signing_bytes"`
	SigningHash    string  `json:"signing_hash"`
	TypedRLP       string  `json:"typed_rlp"`
	Origin         []byte  `json:"origin"`
	IDExplicit     *string `json:"id_explicit"`
	IDAutomatic    *string `json:"id_automatic"`
	Sender         *string `json:"sender"`
	AggregateKey   *string `json:"aggregate_key"`
	SignatureValid *bool   `json:"signature_valid"`
}
type corpus struct {
	Schema       int      `json:"schema"`
	Reference    string   `json:"reference_revision"`
	Transactions []vector `json:"transactions"`
	Rejected     []vector `json:"rejected"`
}
type fixture struct {
	input     []byte
	decoded   *types.Transaction
	signer    types.Signer
	sender    []byte
	digest    common.Hash
	aggregate []byte
	valid     bool
}
type command struct {
	Workload   string `json:"workload"`
	Workers    int    `json:"workers"`
	Iterations int    `json:"iterations"`
	Measure    string `json:"measure"`
	Quit       bool   `json:"quit"`
}

func must(e error) {
	if e != nil {
		panic(e)
	}
}
func unhex(s string) []byte {
	if len(s) < 2 || s[:2] != "0x" {
		panic("missing hex prefix")
	}
	b, e := hex.DecodeString(s[2:])
	must(e)
	return b
}
func hx(b []byte) string { return "0x" + hex.EncodeToString(b) }
func decode(input []byte) *types.Transaction {
	p := new(types.ProtoTransaction)
	must(proto.Unmarshal(input, p))
	tx := new(types.Transaction)
	must(tx.ProtoDecode(p, common.Location{0, 0}))
	return tx
}
func aggregate(tx *types.Transaction) *btcec.PublicKey {
	ins := tx.TxIn()
	keys := make([]*btcec.PublicKey, len(ins))
	for i, in := range ins {
		key, e := btcec.ParsePubKey(in.PubKey)
		must(e)
		keys[i] = key
	}
	if len(keys) == 1 {
		return keys[0]
	}
	key, _, _, e := musig2.AggregateKeys(keys, false)
	must(e)
	return key.FinalKey
}
func qiVerify(tx *types.Transaction, signer types.Signer) bool {
	key := aggregate(tx)
	digest, e := signer.Hash(tx)
	must(e)
	return tx.GetSchnorrSignature().Verify(digest[:], key)
}
func prepare(path string) ([]fixture, map[string][]int, int) {
	raw, e := os.ReadFile(path)
	must(e)
	var c corpus
	must(json.Unmarshal(raw, &c))
	if c.Schema != 1 || c.Reference != referenceRevision {
		panic("wrong corpus/reference")
	}
	fixtures := []fixture{}
	groups := map[string][]int{}
	checks := 0
	for _, v := range c.Transactions {
		input := unhex(v.Input)
		tx := decode(input)
		chain := new(big.Int)
		if tx.Type() != types.ExternalTxType {
			chain = tx.ChainId()
		}
		signer := types.NewSigner(chain, common.Location{0, 0})
		p, e := tx.ProtoEncode()
		must(e)
		canonical, e := proto.Marshal(p)
		must(e)
		if hx(canonical) != v.Canonical {
			panic("canonical mismatch " + v.ID)
		}
		checks++
		p, e = tx.ProtoEncodeTxSigningData()
		must(e)
		signing, e := proto.Marshal(p)
		must(e)
		if hx(signing) != v.SigningBytes {
			panic("signing bytes mismatch " + v.ID)
		}
		checks++
		digest, e := signer.Hash(tx)
		must(e)
		if digest.Hex() != v.SigningHash {
			panic("digest mismatch " + v.ID)
		}
		checks++
		rlp, e := tx.MarshalBinary()
		must(e)
		if hx(rlp) != v.TypedRLP {
			panic("RLP mismatch " + v.ID)
		}
		checks++
		if v.IDExplicit != nil {
			fresh := decode(input)
			h, e := fresh.HashWithError(v.Origin...)
			must(e)
			if h.Hex() != *v.IDExplicit {
				panic("explicit ID mismatch " + v.ID)
			}
			checks++
		}
		h, e := decode(input).HashWithError()
		if v.IDAutomatic == nil {
			if e == nil {
				panic("unexpected automatic ID " + v.ID)
			}
		} else {
			must(e)
			if h.Hex() != *v.IDAutomatic {
				panic("automatic ID mismatch " + v.ID)
			}
		}
		checks++
		sender, e := types.Sender(signer, tx)
		var senderBytes []byte
		if v.Sender == nil {
			if e == nil {
				panic("unexpected sender " + v.ID)
			}
		} else {
			must(e)
			senderBytes = sender.Bytes()
			if hx(senderBytes) != *v.Sender {
				panic("sender mismatch " + v.ID)
			}
		}
		checks++
		f := fixture{input: input, decoded: tx, signer: signer, sender: senderBytes, digest: digest}
		names := []string{}
		if tx.Type() == types.QuaiTxType && v.Sender != nil {
			names = []string{"quai_full", "quai_decode", "quai_signing_hash"}
		}
		if tx.Type() == types.QiTxType {
			f.aggregate = aggregate(tx).SerializeCompressed()
			if v.AggregateKey == nil || hx(f.aggregate) != *v.AggregateKey {
				panic("aggregate mismatch " + v.ID)
			}
			checks++
			f.valid = qiVerify(tx, signer)
			if v.SignatureValid == nil || f.valid != *v.SignatureValid {
				panic("signature result mismatch " + v.ID)
			}
			checks++
			if f.valid {
				names = []string{"qi_decode", "qi_signing_hash"}
				if len(tx.TxIn()) == 1 {
					names = append(names, "qi_single_full")
				} else {
					names = append(names, "qi_multi_full", "qi_aggregate")
				}
			} else {
				names = []string{"qi_invalid_full"}
			}
		}
		for _, name := range names {
			groups[name] = append(groups[name], len(fixtures))
		}
		fixtures = append(fixtures, f)
	}
	for _, v := range c.Rejected {
		p := new(types.ProtoTransaction)
		e := proto.Unmarshal(unhex(v.Input), p)
		if e == nil {
			e = new(types.Transaction).ProtoDecode(p, common.Location{0, 0})
		}
		if e == nil {
			panic("rejected input accepted " + v.ID)
		}
		checks++
	}
	return fixtures, groups, checks
}
func operation(name string, f *fixture) uint64 {
	switch name {
	case "quai_full":
		tx := decode(f.input)
		sender, e := types.Sender(f.signer, tx)
		must(e)
		actual := sender.Bytes()
		if !bytes.Equal(actual, f.sender) {
			panic("sender mismatch")
		}
		return uint64(actual[0])
	case "qi_single_full", "qi_multi_full", "qi_invalid_full":
		actual := qiVerify(decode(f.input), f.signer)
		if actual != f.valid {
			panic("signature result mismatch")
		}
		if actual {
			return 1
		}
		return 0
	case "quai_decode", "qi_decode":
		tx := decode(f.input)
		if tx.Type() != f.decoded.Type() {
			panic("decoded type mismatch")
		}
		runtime.KeepAlive(tx)
		return uint64(tx.Type())
	case "quai_signing_hash", "qi_signing_hash":
		actual, e := f.signer.Hash(f.decoded)
		must(e)
		if actual != f.digest {
			panic("signing digest mismatch")
		}
		return uint64(actual[0])
	case "qi_aggregate":
		actual := aggregate(f.decoded).SerializeCompressed()
		if !bytes.Equal(actual, f.aggregate) {
			panic("aggregate key mismatch")
		}
		return uint64(actual[0])
	}
	panic("unknown workload")
}
func measure(c command, fixtures []fixture, groups map[string][]int) map[string]interface{} {
	indices, ok := groups[c.Workload]
	if !ok || (c.Workers != 1 && c.Workers != 4) || c.Iterations < 32 || c.Iterations > 2000000 {
		panic("invalid workload/limits")
	}
	runtime.GOMAXPROCS(c.Workers)
	for i := 0; i < 32; i++ {
		operation(c.Workload, &fixtures[indices[i%len(indices)]])
	}
	if c.Measure == "alloc" {
		if c.Workers != 1 {
			panic("allocations are measured sequentially")
		}
		runtime.GC()
		var before, after runtime.MemStats
		runtime.ReadMemStats(&before)
		var checksum uint64
		for i := 0; i < c.Iterations; i++ {
			checksum += operation(c.Workload, &fixtures[indices[i%len(indices)]])
		}
		runtime.ReadMemStats(&after)
		return map[string]interface{}{"passed": true, "workload": c.Workload, "workers": c.Workers, "iterations": c.Iterations, "measure": "alloc", "allocation_calls_per_transaction": float64(after.Mallocs-before.Mallocs) / float64(c.Iterations), "gross_allocated_bytes_per_transaction": float64(after.TotalAlloc-before.TotalAlloc) / float64(c.Iterations), "checksum": checksum, "validated_transactions": c.Iterations}
	}
	var ready sync.WaitGroup
	ready.Add(c.Workers)
	start := make(chan struct{})
	results := make(chan uint64, c.Workers)
	for worker := 0; worker < c.Workers; worker++ {
		go func(worker int) {
			ready.Done()
			<-start
			var checksum uint64
			for i := worker; i < c.Iterations; i += c.Workers {
				checksum += operation(c.Workload, &fixtures[indices[i%len(indices)]])
			}
			results <- checksum
		}(worker)
	}
	ready.Wait()
	now := time.Now()
	close(start)
	var checksum uint64
	for worker := 0; worker < c.Workers; worker++ {
		checksum += <-results
	}
	elapsed := time.Since(now).Seconds()
	return map[string]interface{}{"passed": true, "workload": c.Workload, "workers": c.Workers, "iterations": c.Iterations, "measure": "time", "elapsed_seconds": elapsed, "ns_per_transaction": elapsed * 1e9 / float64(c.Iterations), "transactions_per_second": float64(c.Iterations) / elapsed, "checksum": checksum, "validated_transactions": c.Iterations}
}
func main() {
	log.Global.SetOutput(io.Discard)
	if len(os.Args) != 2 {
		panic("tx-pipeline-go CORPUS")
	}
	fixtures, groups, checks := prepare(os.Args[1])
	counts := map[string]int{}
	for name, values := range groups {
		counts[name] = len(values)
	}
	enc := json.NewEncoder(os.Stdout)
	must(enc.Encode(map[string]interface{}{"ready": true, "language": "go", "allocation_instrumented": false, "groups": counts, "preflight": map[string]interface{}{"passed": true, "checks": checks}, "crypto_backend": "pinned go-quai; CGO=" + buildCGO + " ECDSA, btcec v2.3.2 Qi", "go_version": runtime.Version()}))
	scanner := bufio.NewScanner(os.Stdin)
	for scanner.Scan() {
		var c command
		must(json.Unmarshal(scanner.Bytes(), &c))
		if c.Quit {
			break
		}
		must(enc.Encode(measure(c, fixtures, groups)))
	}
	must(scanner.Err())
}
