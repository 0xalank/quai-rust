// Offline ProcessQiTx and MuHash oracle; no node, public peers or wallet state.
package main

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"github.com/btcsuite/btcd/btcec/v2"
	"github.com/btcsuite/btcd/btcec/v2/schnorr"
	"github.com/btcsuite/btcd/btcec/v2/schnorr/musig2"
	"github.com/btcsuite/btcd/chaincfg/chainhash"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/consensus/misc"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/crypto/multiset"
	"github.com/dominant-strategies/go-quai/log"
	"google.golang.org/protobuf/proto"
	"math"
	"math/big"
	"os"
	"strconv"
	"strings"
)

func aggregatePrivate(keys []*btcec.PrivateKey) *btcec.PrivateKey {
	if len(keys) == 1 {
		return keys[0]
	}
	pubs := make([]*btcec.PublicKey, len(keys))
	var joined []byte
	for i, k := range keys {
		pubs[i] = k.PubKey()
		joined = append(joined, pubs[i].SerializeCompressed()...)
	}
	fingerprint := chainhash.TaggedHash(musig2.KeyAggTagList, joined)
	var second []byte
	for _, p := range pubs {
		if !bytes.Equal(p.SerializeCompressed(), pubs[0].SerializeCompressed()) {
			second = p.SerializeCompressed()
			break
		}
	}
	n, _ := new(big.Int).SetString("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141", 16)
	sum := new(big.Int)
	for i, k := range keys {
		coefficient := big.NewInt(1)
		if second == nil || !bytes.Equal(second, pubs[i].SerializeCompressed()) {
			digest := chainhash.TaggedHash(musig2.KeyAggTagCoeff, fingerprint[:], pubs[i].SerializeCompressed())
			coefficient.SetBytes(digest[:])
			coefficient.Mod(coefficient, n)
		}
		term := new(big.Int).Mul(coefficient, new(big.Int).SetBytes(k.Serialize()))
		sum.Add(sum, term)
		sum.Mod(sum, n)
	}
	key, _ := btcec.PrivKeyFromBytes(sum.Bytes())
	agg, _, _, e := musig2.AggregateKeys(pubs, false)
	must(e)
	if !bytes.Equal(key.PubKey().SerializeCompressed(), agg.FinalKey.SerializeCompressed()) {
		panic("aggregate test key mismatch")
	}
	return key
}

var referenceRevision string

func must(e error) {
	if e != nil {
		panic(e)
	}
}
func u(n uint64) *big.Int { return new(big.Int).SetUint64(n) }
func hx(b []byte) string  { return "0x" + hex.EncodeToString(b) }

type chain struct{ prime *types.WorkObject }

func (c chain) Engine(*types.WorkObjectHeader) consensus.Engine          { panic("unused oracle method") }
func (c chain) GetHeaderOrCandidateByHash(common.Hash) *types.WorkObject { return c.prime }
func (c chain) GetHeaderByHash(common.Hash) *types.WorkObject            { return c.prime }
func (c chain) GetBlockByHash(common.Hash) *types.WorkObject             { return c.prime }
func (c chain) NodeCtx() int                                             { return common.ZONE_CTX }
func (c chain) IsGenesisHash(common.Hash) bool                           { return false }
func (c chain) CheckIfEtxIsEligible(bits common.Hash, l common.Location) bool {
	n := l.BytePrefix()
	return bits[n/8]&(1<<(n%8)) != 0
}
func (c chain) CheckInCalcOrderCache(common.Hash) (*big.Int, int, bool) { return nil, 0, false }
func (c chain) AddToCalcOrderCache(common.Hash, int, *big.Int)          {}
func (c chain) CalcBaseFee(*types.WorkObject) *big.Int                  { return u(1) }
func (c chain) CalcOrder(*types.WorkObject) (*big.Int, int, error) {
	return nil, 0, fmt.Errorf("unused oracle method")
}

var keyCache = map[string]*btcec.PrivateKey{}

func key(loc common.Location) *btcec.PrivateKey {
	if k := keyCache[loc.Name()]; k != nil {
		return k
	}
	for i := uint64(1); ; i++ {
		k, _ := btcec.PrivKeyFromBytes(u(i).FillBytes(make([]byte, 32)))
		a := crypto.PubkeyBytesToAddress(k.PubKey().SerializeUncompressed(), loc)
		if a.IsInQiLedgerScope() && a.Location().Equal(loc) {
			keyCache[loc.Name()] = k
			return k
		}
	}
}
func addr(loc common.Location, n byte, qi bool) []byte {
	a := make([]byte, 20)
	a[0] = loc.BytePrefix()
	if qi {
		a[1] = 128
	}
	a[18] = 0x34
	a[19] = n
	return a
}
func run(kind string, loc common.Location, p uint64) map[string]interface{} {
	k := key(loc)
	owner := crypto.PubkeyBytesToAddress(k.PubKey().SerializeUncompressed(), loc)
	point := types.OutPoint{TxHash: common.Hash{0: loc.BytePrefix(), 31: 99}, Index: 7}
	entry := &types.UtxoEntry{Denomination: 14, Address: owner.Bytes(), Lock: u(0)}
	out := types.TxOuts{{Denomination: 13, Address: addr(loc, 1, true), Lock: u(0)}}
	data := []byte{}
	sigBad := false
	first := false
	pool := uint64(30000000)
	r, pbudget := uint64(1000000), uint64(1000000)
	factor := float64(0)
	base := uint64(1)
	eligible := common.Hash{}
	for i := range eligible {
		eligible[i] = 255
	}
	remoteR := common.Location{loc[0], (loc[1] + 1) % 16}
	remoteP := common.Location{(loc[0] + 1) % 16, loc[1]}
	switch kind {
	case "local":
	case "remote-region":
		out[0].Address = addr(remoteR, 1, true)
	case "remote-prime":
		out[0].Address = addr(remoteP, 1, true)
	case "mixed":
		out = append(out, types.TxOut{Denomination: 12, Address: addr(remoteR, 2, true), Lock: u(0)}, types.TxOut{Denomination: 11, Address: addr(remoteP, 3, true), Lock: u(0)})
	case "conversion", "conversion-two", "conversion-different":
		data = append([]byte{0, 0}, addr(loc, 5, true)...)
		out[0].Address = addr(loc, 1, false)
		if kind != "conversion" {
			a := out[0].Address
			if kind == "conversion-different" {
				a = addr(loc, 2, false)
			}
			out = append(out, types.TxOut{Denomination: 12, Address: a, Lock: u(0)})
		}
	case "wrapping", "wrapping-two":
		data = addr(loc, 9, false)
		out[0].Address = addr(loc, 1, false)
		if kind == "wrapping-two" {
			out = append(out, types.TxOut{Denomination: 12, Address: addr(loc, 2, false), Lock: u(0)})
		}
	case "reuse":
		out[0].Address = owner.Bytes()
	case "duplicate":
		out = append(out, out[0])
	case "overspend":
		out = types.TxOuts{{Denomination: 14, Address: addr(loc, 1, true), Lock: u(0)}, {Denomination: 14, Address: addr(loc, 2, true), Lock: u(0)}}
	case "locked":
		entry.Lock = u(4000001)
	case "output-lock":
		out[0].Lock = u(1)
	case "bad-signature":
		sigBad = true
	case "region-budget":
		out[0].Address = addr(remoteR, 1, true)
		r = 20999
	case "prime-budget":
		out[0].Address = addr(remoteP, 1, true)
		pbudget = 20999
	case "ineligible":
		out[0].Address = addr(remoteP, 1, true)
		eligible = common.Hash{}
	case "block-gas":
		pool = 1
	case "empty-parent-scaling":
		factor = math.Inf(-1)
	case "scaled":
		factor = 23.125
	case "fee-low":
		base = 1000000000
	case "combine", "combine-first":
		entry.Denomination = 12
		out[0].Denomination = 13
		first = kind == "combine-first"
	case "quai-output":
		out[0].Address = addr(loc, 1, false)
	case "empty-output":
		out = nil
	case "bounds-many":
		// 4,097 inputs and 4,097 outputs: one more than the former native
		// decode cap for each. Gas needs the 50,000,000 mainnet block limit.
		out = types.TxOuts{}
		for j := 0; j < 4097; j++ {
			a := addr(loc, 0, true)
			a[2] = byte(j >> 8)
			a[3] = byte(j)
			out = append(out, types.TxOut{Denomination: 0, Address: a, Lock: u(0)})
		}
		pool = 50000000
	}
	txi := &types.QiTx{ChainID: u(1337), TxIn: types.TxIns{{PreviousOutPoint: point, PubKey: k.PubKey().SerializeUncompressed()}}, TxOut: out, Data: data, Signature: new(schnorr.Signature)}
	keys := []*btcec.PrivateKey{k}
	if kind == "combine" || kind == "combine-first" {
		for j := 0; j < 10; j++ {
			q := point
			q.Index += uint16(j + 1)
			txi.TxIn = append(txi.TxIn, types.TxIn{PreviousOutPoint: q, PubKey: k.PubKey().SerializeUncompressed()})
			keys = append(keys, k)
		}
	}
	if kind == "duplicate-input" {
		txi.TxIn = append(txi.TxIn, txi.TxIn[0])
		keys = append(keys, k)
	}
	if kind == "bounds-many" {
		for j := 1; j < 4097; j++ {
			q := point
			q.Index += uint16(j)
			txi.TxIn = append(txi.TxIn, types.TxIn{PreviousOutPoint: q, PubKey: k.PubKey().SerializeUncompressed()})
			keys = append(keys, k)
		}
	}
	signingKey := aggregatePrivate(keys)
	signer := types.NewSigner(u(1337), loc)
	tx := types.NewTx(txi)
	digest, e := signer.Hash(tx)
	must(e)
	if !sigBad {
		txi.Signature, e = schnorr.Sign(signingKey, digest[:], schnorr.CustomNonce([32]byte{}))
		must(e)
	}
	tx = types.NewTx(txi)
	pb, e := tx.ProtoEncode()
	must(e)
	wire, e := proto.Marshal(pb)
	must(e)
	header := types.EmptyWorkObject(common.ZONE_CTX)
	header.SetNumber(u(4000000), common.ZONE_CTX)
	header.WorkObjectHeader().SetLocation(loc)
	header.WorkObjectHeader().SetPrimeTerminusNumber(u(p))
	header.WorkObjectHeader().SetDifficulty(u(1000000000000))
	gasLimit := uint64(30000000)
	if kind == "bounds-many" {
		gasLimit = 50000000
	}
	header.Header().SetGasLimit(gasLimit)
	header.Header().SetBaseFee(u(base))
	header.Header().SetAvgTxFees(u(197))
	header.Header().SetTotalFees(u(317))
	prime := types.EmptyWorkObject(common.PRIME_CTX)
	prime.Header().SetExchangeRate(u(1000000000))
	prime.Header().SetEtxEligibleSlices(eligible)
	db := rawdb.NewMemoryDatabase(log.Global)
	defer db.Close()
	for _, input := range txi.TxIn {
		must(rawdb.CreateUTXO(db, input.PreviousOutPoint.TxHash, input.PreviousOutPoint.Index, entry))
	}
	batch := db.NewBatch()
	batch.SetPending(true)
	gp := new(types.GasPool).AddGas(pool)
	used := uint64(1000)
	if kind == "cumulative-gas" {
		used = 29999999
	}
	added, removed := u(0), u(0)
	delta := new(core.UtxosCreatedDeleted)
	before := multiset.New()
	entries := []interface{}{}
	seen := map[types.OutPoint]bool{}
	for _, input := range txi.TxIn {
		q := input.PreviousOutPoint
		if seen[q] {
			continue
		}
		seen[q] = true
		h := types.UTXOHash(q.TxHash, q.Index, entry)
		before.Add(h.Bytes())
		entries = append(entries, map[string]interface{}{"hash": q.TxHash.Hex(), "index": q.Index, "denomination": entry.Denomination, "address": hx(entry.Address), "lock": hx(entry.Lock.Bytes()), "utxo_hash": h.Hex()})
	}
	row := map[string]interface{}{"id": fmt.Sprintf("%s-%d-%d-%d", kind, loc[0], loc[1], p), "transaction": hx(wire), "tx_hash": tx.Hash().Hex(), "location": []int{int(loc[0]), int(loc[1])}, "prime_number": p, "height": 4000000, "scaling_factor": func() interface{} {
		if math.IsInf(factor, -1) {
			return "-inf"
		}
		return factor
	}(), "first_qi": first, "base_fee": hx(u(base).FillBytes(make([]byte, 32))), "quai_reward": hx(misc.CalculateQuaiConversionReward(header, prime.ExchangeRate(), header.Difficulty()).FillBytes(make([]byte, 32))), "qi_reward": hx(misc.CalculateQiReward(header.WorkObjectHeader(), header.Difficulty()).FillBytes(make([]byte, 32))), "eligible_slices": eligible.Hex(), "gas_pool_initial": pool, "region_initial": r, "prime_initial": pbudget, "used_initial": used, "utxos": entries, "before_root": before.Hash().Hex(), "before_accumulator": hx(before.Serialize())}
	if gasLimit != 30000000 {
		row["block_gas_limit"] = gasLimit
	}
	fee, etxs, receipt, err, _ := core.ProcessQiTx(tx, chain{prime}, true, first, header, batch, db, gp, &used, signer, loc, *u(1337), factor, &r, &pbudget, delta, added, removed, false)
	row["accepted"] = err == nil
	var message interface{}
	if err != nil {
		message = err.Error()
		if kind == "region-budget" || kind == "prime-budget" {
			// Pinned ProcessQiTx formats a *uint64 with %d in these two errors.
			// Preserve the complete raw diagnostic in stderr; only its process-local
			// pointer value is excluded from frozen data. Limits and rejection stay.
			prefix, pointer, found := strings.Cut(err.Error(), "gas limit: ")
			if !found {
				panic("budget diagnostic changed")
			}
			_, e := strconv.ParseUint(pointer, 10, 64)
			must(e)
			fmt.Fprintln(os.Stderr, "raw Go budget diagnostic: "+err.Error())
			message = prefix + "gas limit: <Go pointer>"
		}
	}
	row["reference_error"] = message
	if err == nil {
		after := before.Clone()
		for _, h := range delta.UtxosDeletedHashes {
			after.Remove(h.Bytes())
		}
		for _, h := range delta.UtxosCreatedHashes {
			after.Add(h.Bytes())
		}
		row["after_root"] = after.Hash().Hex()
		row["after_accumulator"] = hx(after.Serialize())
		row["gas_pool"] = gp.Gas()
		row["used_gas"] = used
		row["gas_used"] = receipt.GasUsed
		row["receipt_cumulative"] = receipt.CumulativeGasUsed
		row["region_remaining"] = r
		row["prime_remaining"] = pbudget
		row["fee_qit"] = fee.Uint64()
		row["fee_quai"] = hx(misc.QiToQuai(header, prime.ExchangeRate(), header.Difficulty(), fee).FillBytes(make([]byte, 32)))
		row["added_qit"] = added.Uint64()
		row["removed_qit"] = removed.Uint64()
		outs := []string{}
		for _, etx := range etxs {
			p, e := types.NewTx(etx).ProtoEncode()
			must(e)
			b, e := proto.Marshal(p)
			must(e)
			outs = append(outs, hx(b))
		}
		row["outbound_etxs"] = outs
		must(batch.Write())
		created := []interface{}{}
		for index := range out {
			entry := rawdb.GetUTXO(db, tx.Hash(), uint16(index))
			if entry != nil {
				created = append(created, map[string]interface{}{"hash": tx.Hash().Hex(), "index": index, "denomination": entry.Denomination, "address": hx(entry.Address), "lock": hx(entry.Lock.Bytes())})
			}
		}
		row["created"] = created
	} else {
		row["after_root"] = before.Hash().Hex()
		row["after_accumulator"] = hx(before.Serialize())
	}
	return row
}
func main() {
	rows := []interface{}{}
	for _, loc := range []common.Location{{0, 0}, {1, 2}, {15, 15}} {
		for _, p := range []uint64{1171499, 1171500, 1191500, 1569999, 1570000, 1755000, 1775000, 2444573, 2444574} {
			for _, kind := range []string{"local", "remote-region", "remote-prime", "mixed", "conversion", "conversion-two", "conversion-different", "wrapping", "wrapping-two", "reuse", "duplicate", "overspend", "locked", "output-lock", "bad-signature", "region-budget", "prime-budget", "ineligible", "block-gas", "scaled", "fee-low", "combine", "combine-first", "quai-output", "empty-output", "duplicate-input", "cumulative-gas", "empty-parent-scaling"} {
				rows = append(rows, run(kind, loc, p))
			}
		}
	}
	rows = append(rows, run("bounds-many", common.Location{0, 0}, 2444574))
	must(json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"reference_revision": referenceRevision, "vectors": rows, "scope": "Actual ProcessQiTx with signature verification, real rawdb batch UTXO deltas and MuHash commitments. Rejected transactions roll back the block view; no full block admission or public network. The only normalized diagnostic bytes are process-local Go pointer values in the two ETX budget error messages; raw diagnostics remain in oracle stderr."}))
}
