// Offline compatibility oracle. Never linked into the Rust client.
package main

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math/big"
	"math/rand"
	"net"
	"os"
	"sort"
	"strings"
	"time"

	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/log"
	_ "github.com/dominant-strategies/go-quai/p2p/node/peerManager/peerdb"
	topics "github.com/dominant-strategies/go-quai/p2p/node/pubsubManager"
	pb "github.com/dominant-strategies/go-quai/p2p/pb"
	"github.com/dominant-strategies/go-quai/p2p/protocol"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/dominant-strategies/go-quai/trie"
	"github.com/libp2p/go-libp2p/core/network"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/reflect/protoregistry"
	"google.golang.org/protobuf/types/dynamicpb"
)

var referenceRevision string

func must(e error) {
	if e != nil {
		panic(e)
	}
}
func hx(b []byte) string    { return "0x" + hex.EncodeToString(b) }
func unhex(s string) []byte { b, e := hex.DecodeString(strings.TrimPrefix(s, "0x")); must(e); return b }
func marsh(m proto.Message) []byte {
	b, e := (proto.MarshalOptions{Deterministic: true}).Marshal(m)
	must(e)
	return b
}
func fill(m protoreflect.Message, seed, depth int) {
	if seed == 0 {
		return
	}
	fields := m.Descriptor().Fields()
	for i := 0; i < fields.Len(); i++ {
		f := fields.Get(i)
		if o := f.ContainingOneof(); o != nil && !o.IsSynthetic() {
			if o.Fields().Get(seed%o.Fields().Len()).Number() != f.Number() {
				continue
			}
		}
		scalar := func(d protoreflect.FieldDescriptor, s int) protoreflect.Value {
			switch d.Kind() {
			case protoreflect.Uint32Kind:
				return protoreflect.ValueOfUint32(uint32(s) * 0x10203041)
			case protoreflect.Uint64Kind:
				return protoreflect.ValueOfUint64(uint64(s) * 0x1020304050607081)
			case protoreflect.BoolKind:
				return protoreflect.ValueOfBool(s%2 == 1)
			case protoreflect.StringKind:
				return protoreflect.ValueOfString(fmt.Sprintf("peer-%d", s))
			case protoreflect.BytesKind:
				b := make([]byte, []int{0, 1, 31, 32, 65, 7, 128, 2}[s%8])
				for j := range b {
					b[j] = byte(s + j)
				}
				return protoreflect.ValueOfBytes(b)
			default:
				panic(d.Kind())
			}
		}
		value := func(d protoreflect.FieldDescriptor, s int) protoreflect.Value {
			if d.Kind() == protoreflect.MessageKind {
				child := dynamicpb.NewMessage(d.Message())
				if depth > 0 {
					fill(child, s, depth-1)
				}
				return protoreflect.ValueOfMessage(child)
			}
			return scalar(d, s)
		}
		if f.IsMap() {
			v := m.Mutable(f).Map()
			for j := 0; j < seed%4; j++ {
				v.Set(protoreflect.ValueOfUint32(uint32(j*127+1)).MapKey(), value(f.MapValue(), seed+j))
			}
		} else if f.IsList() {
			v := m.Mutable(f).List()
			for j := 0; j < seed%4; j++ {
				v.Append(value(f, seed+j))
			}
		} else {
			m.Set(f, value(f, seed))
		}
	}
}

type rawList [][]byte

func (r rawList) Len() int                           { return len(r) }
func (r rawList) EncodeIndex(i int, b *bytes.Buffer) { b.Write(r[i]) }
func root(r types.DerivableList) common.Hash         { return types.DeriveSha(r, trie.NewStackTrie(nil)) }
func locationNumbers(location common.Location) []int {
	out := []int{}
	for _, n := range location {
		out = append(out, int(n))
	}
	return out
}
func contextName(ctx int) string { return []string{"prime", "region", "zone"}[ctx] }
func commitmentValid(wo *types.WorkObject, ctx int) bool {
	h := wo.Header()
	if wo.HeaderHash() != h.Hash() {
		return false
	}
	if ctx == common.ZONE_CTX {
		return len(wo.Manifest()) == 0 && len(wo.InterlinkHashes()) == 0 && root(wo.Transactions()) == h.TxHash() && root(wo.OutboundEtxs()) == h.OutboundEtxHash() && types.CalcUncleHash(wo.Uncles()) == h.UncleHash()
	}
	if len(wo.Transactions()) != 0 || len(wo.OutboundEtxs()) != 0 || len(wo.Uncles()) != 0 {
		return false
	}
	r := root(wo.Manifest())
	if r == types.EmptyRootHash || r != h.ManifestHash(ctx+1) {
		return false
	}
	return ctx != common.PRIME_CTX || root(wo.InterlinkHashes()) == h.InterlinkRootHash()
}

type tcpStream struct {
	network.Stream
	transport net.Conn
}

func (s *tcpStream) Read(b []byte) (int, error)         { return s.transport.Read(b) }
func (s *tcpStream) Write(b []byte) (int, error)        { return s.transport.Write(b) }
func (s *tcpStream) SetWriteDeadline(t time.Time) error { return s.transport.SetWriteDeadline(t) }
func wireServer() {
	listener, e := net.Listen("tcp4", "127.0.0.1:0")
	must(e)
	defer listener.Close()
	must(json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"listen": listener.Addr().String()}))
	conn, e := listener.Accept()
	must(e)
	defer conn.Close()
	must(conn.SetDeadline(time.Now().Add(10 * time.Second)))
	stream := &tcpStream{transport: conn}
	for i := 0; i < 3; i++ {
		raw, e := common.ReadMessageFromStream(stream, protocol.ProtocolVersion, nil)
		must(e)
		envelope := new(pb.QuaiMessage)
		must(proto.Unmarshal(raw, envelope))
		r := envelope.GetRequest()
		if r == nil || r.GetHash() == nil {
			panic("missing request/hash")
		}
		reply := &pb.QuaiMessage{Payload: &pb.QuaiMessage_Response{Response: &pb.QuaiResponseMessage{Id: r.Id, Location: r.Location, Response: &pb.QuaiResponseMessage_BlockHash{BlockHash: r.GetHash()}}}}
		must(common.WriteMessageToStream(stream, marsh(reply), protocol.ProtocolVersion, nil))
	}
	_, e = common.ReadMessageFromStream(stream, protocol.ProtocolVersion, nil)
	if e == nil || !strings.Contains(e.Error(), "exceeds max stream message size") {
		panic("oversize frame not rejected")
	}
	must(json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"passed": true, "exchanges": 3, "oversize_rejected": true, "libp2p_transport": false}))
}
func executionVectors(all, etxs types.Transactions) map[string]interface{} {
	rng := rand.New(rand.NewSource(2026110902))
	random := func(n int) []byte { b := make([]byte, n); rng.Read(b); return b }
	result := map[string]interface{}{"scope": "Intrinsic transaction gas, bounded input-helper checks and live StateDB ETX trie operations. No full EVM/transaction/block validity."}
	gasCases := []interface{}{}
	for i := 0; i < 96; i++ {
		data := random([]int{0, 1, 2, 31, 32, 127, 128, 1024, 4096, 65536}[i%10])
		for j := range data {
			if (i+j)%3 == 0 {
				data[j] = 0
			}
		}
		access := types.AccessList{}
		keys := []int{}
		for j := 0; j < i%6; j++ {
			entry := types.AccessTuple{Address: common.ZeroAddress(common.Location{0, 0}), StorageKeys: []common.Hash{}}
			for k := 0; k < (i+j)%7; k++ {
				entry.StorageKeys = append(entry.StorageKeys, common.Hash{})
			}
			access = append(access, entry)
			keys = append(keys, len(entry.StorageKeys))
		}
		creation := i%2 == 1
		gas, e := core.IntrinsicGas(data, access, creation)
		must(e)
		gasCases = append(gasCases, map[string]interface{}{"id": fmt.Sprintf("intrinsic-%d", i), "data": hx(data), "keys_per_address": keys, "creation": creation, "gas": gas})
	}
	result["intrinsic_gas"] = gasCases
	qis := types.Transactions{}
	var qiKey, quaiKey []byte
	for _, tx := range all {
		if tx.Type() != types.QiTxType {
			continue
		}
		qis = append(qis, tx)
		for _, input := range tx.TxIn() {
			a := crypto.PubkeyBytesToAddress(input.PubKey, common.Location{0, 0})
			if a.IsInQiLedgerScope() {
				qiKey = input.PubKey
			} else {
				quaiKey = input.PubKey
			}
		}
	}
	if len(qiKey) == 0 || len(quaiKey) == 0 {
		panic("missing fixture key classes")
	}
	qiCases := []interface{}{}
	for i := 0; i < 192; i++ {
		mode := i % 16
		header := types.EmptyWorkObject(common.ZONE_CTX)
		header.WorkObjectHeader().SetLocation(common.Location{0, 0})
		header.SetNumber(big.NewInt(42), common.ZONE_CTX)
		p, e := qis[i%len(qis)].ProtoEncode()
		must(e)
		p.ChainId = []byte{9}
		p.Data = []byte{}
		if len(p.TxIns.TxIns) > 4 {
			panic("fixture bound")
		}
		for _, input := range p.TxIns.TxIns {
			input.PubKey = qiKey
		}
		for _, output := range p.TxOuts.TxOuts {
			output.Denomination = proto.Uint32(uint32(i % 15))
			output.Lock = []byte{}
		}
		switch mode {
		case 5:
			if len(p.TxOuts.TxOuts) == 0 {
				p.TxOuts.TxOuts = append(p.TxOuts.TxOuts, &types.ProtoTxOut{Denomination: proto.Uint32(0), Address: make([]byte, 20)})
			}
			p.TxOuts.TxOuts[0].Lock = []byte{1}
		case 6:
			if len(p.TxOuts.TxOuts) == 0 {
				p.TxOuts.TxOuts = append(p.TxOuts.TxOuts, &types.ProtoTxOut{Denomination: proto.Uint32(0), Address: make([]byte, 20)})
			}
			p.TxOuts.TxOuts[0].Denomination = proto.Uint32(15)
		case 7:
			p.ChainId = []byte{10}
		case 8:
			p.Data = []byte{1}
		case 9:
			p.Data = make([]byte, 20)
			if i%32 < 16 {
				p.Data[1] = 128
			}
		case 10:
			p.Data = make([]byte, 22)
			if i%32 < 16 {
				p.Data[3] = 128
			}
		case 12:
			for _, input := range p.TxIns.TxIns {
				input.PubKey = quaiKey
			}
		case 13:
			p.TxIns.TxIns = append(p.TxIns.TxIns, proto.Clone(p.TxIns.TxIns[0]).(*types.ProtoTxIn))
		case 15:
			p.Data = make([]byte, 20)
		}
		tx := new(types.Transaction)
		must(tx.ProtoDecode(p, common.Location{0, 0}))
		db := rawdb.NewMemoryDatabase(log.Global)
		entries := []interface{}{}
		seen := map[types.OutPoint]bool{}
		unique := true
		for j, input := range tx.TxIn() {
			point := input.PreviousOutPoint
			if seen[point] {
				unique = false
				continue
			}
			seen[point] = true
			if mode == 1 && j == 0 {
				continue
			}
			address := crypto.PubkeyBytesToAddress(input.PubKey, common.Location{0, 0}).Bytes()
			lock := big.NewInt(0)
			denomination := uint8((i + j) % 15)
			if mode == 2 && j == 0 {
				lock.SetUint64(43)
			}
			if mode == 11 {
				lock.SetUint64(42)
			}
			if mode == 14 {
				lock = new(big.Int).Lsh(big.NewInt(1), 255)
			}
			if mode == 3 && j == 0 {
				address = append([]byte{}, address...)
				address[19] ^= 1
			}
			if mode == 4 && j == 0 {
				denomination = 15
			}
			entry := &types.UtxoEntry{Denomination: denomination, Address: address, Lock: lock}
			must(rawdb.CreateUTXO(db, point.TxHash, point.Index, entry))
			entries = append(entries, map[string]interface{}{"hash": point.TxHash.Hex(), "index": point.Index, "denomination": denomination, "address": hx(address), "lock": hx(lock.Bytes())})
		}
		total, err := core.ValidateQiTxInputs(tx, nil, db, header, types.NewSigner(big.NewInt(9), common.Location{0, 0}), common.Location{0, 0}, *big.NewInt(9))
		must(db.Close())
		var expected interface{}
		var message interface{}
		if err == nil {
			expected = total.String()
		} else {
			message = err.Error()
		}
		canonical, e := tx.ProtoEncode()
		must(e)
		factors := []float64{0, 1, 14.9999, 15, 15.0001, 16, 30.1, 1000000}
		factor := factors[i%len(factors)]
		qiCases = append(qiCases, map[string]interface{}{"id": fmt.Sprintf("qi-inputs-%d", i), "transaction": hx(marsh(canonical)), "utxos": entries, "chain_id": "0x09", "height": 42, "location": []int{0, 0}, "input_total": expected, "reference_error": message, "unique_inputs": unique, "scaling_factor": factor, "intrinsic_gas": types.CalculateIntrinsicQiTxGas(tx, factor), "signature_math_not_part_of_helper": true})
	}
	result["qi_inputs"] = qiCases
	traces := []interface{}{}
	for scenario, count := range []int{0, 1, 2, 3, 15, 127, 128, 129, 255, 256, 257} {
		disk := rawdb.NewMemoryDatabase(log.Global)
		cache := state.NewDatabase(disk)
		s, e := state.New(types.EmptyRootHash, types.EmptyRootHash, big.NewInt(0), cache, cache, nil, common.Location{0, 0}, log.Global)
		must(e)
		steps := []interface{}{}
		length := 0
		record := func(kind string, extra map[string]interface{}) {
			old, e := s.GetOldestIndex()
			must(e)
			newest, e := s.GetNewestIndex()
			must(e)
			extra["op"] = kind
			extra["root"] = s.ETXRoot().Hex()
			extra["oldest"] = old.Uint64()
			extra["newest"] = newest.Uint64()
			extra["length"] = length
			steps = append(steps, extra)
		}
		record("initial", map[string]interface{}{})
		if scenario%2 == 1 {
			must(s.UpdateKQuai(big.NewInt(12345)))
			key := common.HexToHash("0xfffffffffffffffffffffffffffffffffffffffffffffffffffffffa")
			record("auxiliary", map[string]interface{}{"key": key.Hex(), "value": "0x3039"})
			must(s.FreezeKQuai())
			key = common.HexToHash("0xfffffffffffffffffffffffffffffffffffffffffffffffffffffffb")
			record("auxiliary", map[string]interface{}{"key": key.Hex(), "value": "0x00"})
		}
		push := func(n int) {
			incoming := types.Transactions{}
			values := []string{}
			for j := 0; j < n; j++ {
				tx := etxs[(scenario+j)%len(etxs)]
				incoming = append(incoming, tx)
				p, e := tx.ProtoEncode()
				must(e)
				values = append(values, hx(marsh(p)))
			}
			must(s.PushETXs(incoming))
			length += n
			record("push", map[string]interface{}{"transactions": values})
		}
		pop := func() {
			tx, e := s.PopETX()
			must(e)
			var value interface{}
			if tx != nil {
				p, e := tx.ProtoEncode()
				must(e)
				value = hx(marsh(p))
				length--
			}
			record("pop", map[string]interface{}{"transaction": value})
		}
		push(count)
		for _, index := range []uint64{0, 1, uint64(count), uint64(count + 1)} {
			tx, e := s.ReadETX(new(big.Int).SetUint64(index))
			must(e)
			var value interface{}
			if tx != nil {
				p, e := tx.ProtoEncode()
				must(e)
				value = hx(marsh(p))
			}
			record("read", map[string]interface{}{"index": index, "transaction": value})
		}
		checkpoint := s.Copy()
		checkpointLength := length
		record("checkpoint", map[string]interface{}{})
		for j := 0; j < 3; j++ {
			pop()
		}
		push(3)
		s = checkpoint.Copy()
		length = checkpointLength
		record("revert", map[string]interface{}{})
		for j := 0; j < count+1; j++ {
			pop()
		}
		push(2)
		pop()
		pop()
		pop()
		traces = append(traces, map[string]interface{}{"id": fmt.Sprintf("live-etx-state-%d", count), "steps": steps})
		must(disk.Close())
	}
	result["etx_traces"] = traces
	return result
}

func stateTrieVectors() []interface{} {
	result := []interface{}{}
	rng := rand.New(rand.NewSource(90901))
	for _, count := range []int{0, 1, 2, 3, 4, 5, 16, 32, 128, 257} {
		for _, secured := range []bool{false, true} {
			disk := rawdb.NewMemoryDatabase(log.Global)
			database := trie.NewDatabase(disk)
			var tr interface {
				TryUpdate([]byte, []byte) error
				Hash() common.Hash
			}
			if secured {
				value, e := trie.NewSecure(types.EmptyRootHash, database)
				must(e)
				tr = value
			} else {
				value, e := trie.New(types.EmptyRootHash, database)
				must(e)
				tr = value
			}
			entries := []interface{}{}
			for i := 0; i < count; i++ {
				keys := [][]byte{{}, {0}, {0, 1}, {0, 1, 2}, {1}}
				var key []byte
				if i < len(keys) {
					key = keys[i]
				} else {
					key = []byte{2, byte(i >> 8), byte(i)}
				}
				value := make([]byte, 1+i%67)
				rng.Read(value)
				must(tr.TryUpdate(key, value))
				entries = append(entries, map[string]interface{}{"key": hx(key), "value": hx(value)})
			}
			result = append(result, map[string]interface{}{"id": fmt.Sprintf("state-trie-%d-%t", count, secured), "secure": secured, "entries": entries, "root": tr.Hash().Hex()})
			must(disk.Close())
		}
	}
	return result
}

func main() {
	if len(os.Args) == 2 && os.Args[1] == "--wire-server" {
		wireServer()
		return
	}

	if len(os.Args) != 2 {
		panic("TRANSACTION_CORPUS")
	}
	var source struct {
		Transactions []struct {
			Canonical string `json:"canonical"`
		}
	}
	raw, e := os.ReadFile(os.Args[1])
	must(e)
	must(json.Unmarshal(raw, &source))
	all := types.Transactions{}
	etxs := types.Transactions{}
	for _, v := range source.Transactions {
		p := new(types.ProtoTransaction)
		must(proto.Unmarshal(unhex(v.Canonical), p))
		tx := new(types.Transaction)
		must(tx.ProtoDecode(p, common.Location{0, 0}))
		all = append(all, tx)
		if tx.Type() == types.ExternalTxType {
			etxs = append(etxs, tx)
		}
	}
	rng := rand.New(rand.NewSource(20261109))
	random := func(n int) []byte { b := make([]byte, n); rng.Read(b); return b }
	hash := func() common.Hash { return common.BytesToHash(random(32)) }
	out := map[string]interface{}{"schema": 1, "reference_revision": referenceRevision, "kind": "native-foundation", "scope": "Full raw Protobuf schemas; bounded canonical complete work objects; body/list/receipt commitments. No EVM, state transition, PoW validation or network authority.", "schedule": map[string]uint64{"kawpow_fork": params.KawPowForkBlock, "transition_period": params.KawPowTransitionPeriod}}
	schemaVectors := []interface{}{}
	protoregistry.GlobalFiles.RangeFiles(func(file protoreflect.FileDescriptor) bool {
		path := file.Path()
		wanted := path == "common/proto_common.proto" || path == "core/types/proto_block.proto" || path == "core/rawdb/db.proto" || path == "p2p/pb/quai_messages.proto" || path == "p2p/node/peerManager/peerdb/peer_info.proto"
		if !wanted {
			return true
		}
		messages := file.Messages()
		for i := 0; i < messages.Len(); i++ {
			desc := messages.Get(i)
			for seed := 0; seed < 8; seed++ {
				message := dynamicpb.NewMessage(desc)
				fill(message, seed, 3)
				raw := marsh(message)
				// Unknown field 500 must be discarded recursively by the bounded Rust codec.
				if seed%2 == 1 {
					raw = append(raw, 0xa0, 0x1f, 0x01)
				}
				parsed := dynamicpb.NewMessage(desc)
				must((proto.UnmarshalOptions{DiscardUnknown: true}).Unmarshal(raw, parsed))
				schemaVectors = append(schemaVectors, map[string]interface{}{"id": fmt.Sprintf("%s-%d", desc.FullName(), seed), "message": "." + string(desc.FullName()), "input": hx(raw), "canonical": hx(marsh(parsed))})
			}
		}
		return true
	})
	sort.Slice(schemaVectors, func(i, j int) bool {
		return schemaVectors[i].(map[string]interface{})["id"].(string) < schemaVectors[j].(map[string]interface{})["id"].(string)
	})
	out["schema_vectors"] = schemaVectors
	trieVectors := []interface{}{}
	for _, n := range []int{0, 1, 2, 3, 15, 16, 17, 126, 127, 128, 129, 255, 256, 257, 1024, 4096} {
		for mode := 0; mode < 3; mode++ {
			values := rawList{}
			texts := []string{}
			for j := 0; j < n; j++ {
				v := random(1 + (j*17+mode)%97)
				if mode == 0 {
					v = []byte{byte(j % 128)}
				}
				values = append(values, v)
				texts = append(texts, hx(v))
			}
			trieVectors = append(trieVectors, map[string]interface{}{"id": fmt.Sprintf("ordered-%d-%d", n, mode), "values": texts, "root": root(values).Hex()})
		}
	}
	out["trie_vectors"] = trieVectors
	works := []interface{}{}
	rejected := []interface{}{}
	headers := []interface{}{}
	invalidCommitments := []interface{}{}
	var fixtureWos []*types.WorkObject
	heights := []uint64{0, params.KawPowForkBlock - 1, params.KawPowForkBlock, params.KawPowForkBlock + params.KawPowTransitionPeriod - 1, params.KawPowForkBlock + params.KawPowTransitionPeriod}
	for i := 0; i < 90; i++ {
		ctx := i % 3
		location := common.Location{}
		if ctx >= 1 {
			location = append(location, byte(i%16))
		}
		if ctx == 2 {
			location = append(location, byte((i/16)%16))
		}
		wo := types.EmptyWorkObject(ctx)
		wh := wo.WorkObjectHeader()
		h := wo.Header()
		wh.SetLocation(location)
		wh.SetPrimaryCoinbase(common.ZeroAddress(common.Location{0, 0}))
		wh.SetNumber(big.NewInt(int64(i)))
		wh.SetPrimeTerminusNumber(new(big.Int).SetUint64(heights[i%5]))
		wh.SetTime(rng.Uint64())
		wh.SetNonce(types.EncodeNonce(rng.Uint64()))
		wh.SetData(random(i % 17))
		wh.SetParentHash(hash())
		wh.SetMixHash(hash())
		h.SetEVMRoot(hash())
		h.SetUTXORoot(hash())
		h.SetEtxSetRoot(hash())
		h.SetReceiptHash(hash())
		h.SetExtra(random(i % 32))
		h.SetGasUsed(rng.Uint64())
		h.SetBaseFee(new(big.Int).SetBytes(random(i % 32)))
		h.SetEfficiencyScore(uint16(i))
		h.SetThresholdCount(uint16(i * 7))
		h.SetExpansionNumber(uint8(i % 4))
		for k := 0; k < 3; k++ {
			h.SetParentEntropy(new(big.Int).SetBytes(random((i+k)%32)), k)
			h.SetParentDeltaEntropy(big.NewInt(int64(i+k)), k)
			h.SetParentUncledDeltaEntropy(big.NewInt(int64(k)), k)
			h.SetManifestHash(hash(), k)
		}
		body := wo.Body()
		txs := types.Transactions{}
		outs := types.Transactions{}
		uncles := []*types.WorkObjectHeader{}
		manifest := types.BlockManifest{}
		interlinks := common.Hashes{}
		if ctx == common.ZONE_CTX {
			for j := 0; j < i%19; j++ {
				txs = append(txs, all[(i*7+j*13)%len(all)])
			}
			for j := 0; j < i%4; j++ {
				outs = append(outs, etxs[(i+j)%len(etxs)])
			}
			for j := 0; j < (i/3)%3; j++ {
				uncle := types.CopyWorkObjectHeader(wh)
				uncle.SetParentHash(hash())
				uncle.SetTime(rng.Uint64())
				uncles = append(uncles, uncle)
			}
		} else {
			for j := 0; j < 1+i%9; j++ {
				manifest = append(manifest, hash())
			}
			h.SetManifestHash(root(manifest), ctx+1)
			if ctx == common.PRIME_CTX {
				for j := 0; j < i%11; j++ {
					interlinks = append(interlinks, hash())
				}
				h.SetInterlinkRootHash(root(interlinks))
			}
		}
		body.SetTransactions(txs)
		body.SetOutboundEtxs(outs)
		body.SetUncles(uncles)
		body.SetManifest(manifest)
		body.SetInterlinkHashes(interlinks)
		h.SetTxHash(root(txs))
		h.SetOutboundEtxHash(root(outs))
		h.SetUncleHash(types.CalcUncleHash(uncles))
		wh.SetHeaderHash(h.Hash())
		fixtureWos = append(fixtureWos, wo)
		p, e := wo.ProtoEncode(types.BlockObject)
		must(e)
		if i%7 == 0 {
			p.WoBody.Header.EfficiencyScore = proto.Uint64(1<<32 + uint64(i))
			p.WoBody.Header.ThresholdCount = proto.Uint64(1<<48 + uint64(i*7))
			p.WoBody.Header.ExpansionNumber = proto.Uint64(1<<32 + uint64(i%4))
		}
		if i%11 == 0 {
			p.WoBody.Header.GasLimit = nil
			p.WoBody.Header.GasUsed = nil
			p.WoBody.Header.StateUsed = nil
			p.WoBody.Header.StateLimit = nil
		}
		if i%13 == 0 {
			p.WoBody.Header.Difficulty = []byte{99}
			p.WoBody.Header.Nonce = proto.Uint64(13)
			p.WoBody.Header.Location = &common.ProtoLocation{Value: []byte{9, 9}}
		}
		input := marsh(p)
		if i%5 == 0 {
			input = append(input, 0xa0, 0x1f, 1)
		}
		parsed := new(types.ProtoWorkObject)
		must(proto.Unmarshal(input, parsed))
		decoded := new(types.WorkObject)
		must(decoded.ProtoDecode(parsed, location, types.BlockObject))
		canonical, e := decoded.ProtoEncode(types.BlockObject)
		must(e)
		// Optional normalization may change the header binding; recompute the expected outcome.
		works = append(works, map[string]interface{}{"id": fmt.Sprintf("work-%d", i), "context": contextName(ctx), "input": hx(input), "canonical": hx(marsh(canonical)), "body_header_hash": decoded.Header().Hash().Hex(), "transaction_root": root(decoded.Transactions()).Hex(), "outbound_root": root(decoded.OutboundEtxs()).Hex(), "uncle_root": types.CalcUncleHash(decoded.Uncles()).Hex(), "manifest_root": root(decoded.Manifest()).Hex(), "interlink_root": root(decoded.InterlinkHashes()).Hex(), "commitments_valid": commitmentValid(decoded, ctx)})
		hp, e := h.ProtoEncode()
		must(e)
		hraw := marsh(hp)
		decodedH := new(types.Header)
		must(decodedH.ProtoDecode(hp, location))
		hc, e := decodedH.ProtoEncode()
		must(e)
		headers = append(headers, map[string]interface{}{"id": fmt.Sprintf("body-header-%d", i), "input": hx(hraw), "canonical": hx(marsh(hc)), "hash": decodedH.Hash().Hex()})
		if i < 12 {
			for _, field := range []string{"ParentHash", "ManifestHash", "ParentEntropy", "ParentDeltaEntropy", "ParentUncledDeltaEntropy", "Number", "TxHash", "BaseFee", "PrimeStateRoot", "RegionStateRoot", "EfficiencyScore", "ExpansionNumber"} {
				clone := proto.Clone(hp).(*types.ProtoHeader)
				d := clone.ProtoReflect().Descriptor().Fields()
				for j := 0; j < d.Len(); j++ {
					f := d.Get(j)
					name := strings.ReplaceAll(f.TextName(), "_", "")
					if strings.EqualFold(name, field) {
						clone.ProtoReflect().Clear(f)
					}
				}
				err := new(types.Header).ProtoDecode(clone, location)
				if err == nil {
					panic("expected body-header rejection " + field)
				}
				rejected = append(rejected, map[string]interface{}{"id": fmt.Sprintf("missing-%d-%s", i, field), "kind": "header", "input": hx(marsh(clone)), "reference_error": err.Error()})
			}
		}
		// Tamper an inner commitment but rebind the outer header to isolate root checks.
		bad := types.CopyWorkObject(wo)
		if ctx == 2 {
			bad.Header().SetTxHash(hash())
		} else {
			bad.Header().SetManifestHash(hash(), ctx+1)
		}
		bad.WorkObjectHeader().SetHeaderHash(bad.Header().Hash())
		badp, e := bad.ProtoEncode(types.BlockObject)
		must(e)
		if commitmentValid(bad, ctx) {
			panic("tamper not rejected")
		}
		invalidCommitments = append(invalidCommitments, map[string]interface{}{"id": fmt.Sprintf("tampered-%d", i), "context": contextName(ctx), "input": hx(marsh(badp)), "commitments_valid": false})
	}
	positives := map[string]int{"prime": 0, "region": 0, "zone": 0}
	for _, v := range works {
		w := v.(map[string]interface{})
		if w["commitments_valid"].(bool) {
			positives[w["context"].(string)]++
		}
	}
	for context, count := range positives {
		if count == 0 {
			panic("no positive commitment cases for " + context)
		}
	}
	out["positive_commitments_by_context"] = positives
	out["work_objects"] = works
	out["body_headers"] = headers
	out["rejected"] = rejected
	out["invalid_commitments"] = invalidCommitments
	receipts := []interface{}{}
	rs := types.Receipts{}
	for i := 0; i < 132; i++ {
		r := &types.Receipt{Type: uint8(i % 3), Status: uint64(i % 4), CumulativeGasUsed: rng.Uint64(), Logs: []*types.Log{}, OutboundEtxs: types.Transactions{}}
		if i%7 == 0 {
			r.PostState = random(32)
		}
		logs := []interface{}{}
		for j := 0; j < i%5; j++ {
			log := &types.Log{Address: common.BytesToAddress(random(20), common.Location{0, 0}), Topics: []common.Hash{}, Data: random((i + j) % 70)}
			ts := []string{}
			for k := 0; k < (i+j)%5; k++ {
				t := hash()
				log.Topics = append(log.Topics, t)
				ts = append(ts, t.Hex())
			}
			r.Logs = append(r.Logs, log)
			logs = append(logs, map[string]interface{}{"address": hx(log.Address.Bytes()), "topics": ts, "data": hx(log.Data)})
		}
		encodedOut := []string{}
		for j := 0; j < i%4; j++ {
			t := etxs[(i+j)%len(etxs)]
			r.OutboundEtxs = append(r.OutboundEtxs, t)
			p, e := t.ProtoEncode()
			must(e)
			encodedOut = append(encodedOut, hx(marsh(p)))
		}
		r.Bloom = types.CreateBloom(types.Receipts{r})
		var encoded bytes.Buffer
		types.Receipts{r}.EncodeIndex(0, &encoded)
		rs = append(rs, r)
		receipts = append(receipts, map[string]interface{}{"id": fmt.Sprintf("receipt-%d", i), "type": r.Type, "status": r.Status, "post_state": hx(r.PostState), "cumulative_gas_used": r.CumulativeGasUsed, "logs": logs, "outbound": encodedOut, "bloom": hx(r.Bloom.Bytes()), "typed_rlp": hx(encoded.Bytes())})
	}
	out["receipts"] = receipts
	receiptRoots := []interface{}{}
	for _, n := range []int{0, 1, 2, 16, 127, 128, 129, 132} {
		receiptRoots = append(receiptRoots, map[string]interface{}{"count": n, "root": root(rs[:n]).Hex()})
	}
	out["receipt_roots"] = receiptRoots
	topicVectors := []interface{}{}
	for _, location := range []common.Location{{}, {0}, {15}, {0, 0}, {3, 12}, {15, 15}} {
		for i, value := range []interface{}{&types.WorkObjectHeaderView{}, &types.WorkObjectBlockView{}, &types.WorkObjectShareView{}, &types.AuxTemplate{}} {
			genesis := hash()
			t, e := topics.NewTopic(genesis, location, value)
			must(e)
			topicVectors = append(topicVectors, map[string]interface{}{"genesis": genesis.Hex(), "location": locationNumbers(location), "kind": []string{"headers", "blocks", "worksharev2", "auxtemplate"}[i], "topic": t.String()})
		}
	}
	out["topics"] = topicVectors
	out["execution"] = executionVectors(all, etxs)
	out["state_tries"] = stateTrieVectors()
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	must(encoder.Encode(out))
}
