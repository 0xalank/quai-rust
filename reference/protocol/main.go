// Offline-only compatibility vector generator. Never part of the Rust build.
package main

import (
	"encoding/hex"
	"encoding/json"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/params"
	"google.golang.org/protobuf/proto"
	"math/big"
	"math/rand"
	"os"
)

var referenceRevision string

func hx(b []byte) string { return "0x" + hex.EncodeToString(b) }
func main() {
	rng := rand.New(rand.NewSource(1769))
	random := func(n int) []byte { b := make([]byte, n); rng.Read(b); return b }
	vectors := []interface{}{}
	heights := []uint64{0, params.KawPowForkBlock - 1, params.KawPowForkBlock, params.KawPowForkBlock + params.KawPowTransitionPeriod - 1, params.KawPowForkBlock + params.KawPowTransitionPeriod}
	for i := 0; i < 240; i++ {
		wo := types.EmptyWorkObject(common.ZONE_CTX)
		wh := wo.WorkObjectHeader()
		wh.SetHeaderHash(common.BytesToHash(random(32)))
		wh.SetParentHash(common.BytesToHash(random(32)))
		wh.SetTxHash(common.BytesToHash(random(32)))
		wh.SetMixHash(common.BytesToHash(random(32)))
		wh.SetNumber(new(big.Int).SetBytes(random(i % 33)))
		difficulty := new(big.Int).SetBytes(random(i%32 + 1))
		if i%6 == 0 {
			difficulty.SetInt64(int64(i%3 + 1))
		}
		if difficulty.Sign() == 0 {
			difficulty.SetInt64(1)
		}
		wh.SetDifficulty(difficulty)
		prime := heights[i%len(heights)]
		wh.SetPrimeTerminusNumber(new(big.Int).SetUint64(prime))
		nonce := rng.Uint64()
		if i%11 == 0 {
			nonce = 0
		}
		if i%13 == 0 {
			nonce = ^uint64(0)
		}
		wh.SetNonce(types.EncodeNonce(nonce))
		wh.SetTime(rng.Uint64())
		wh.SetLock(uint8(i % 4))
		location := common.Location{byte(i % 16), byte((i / 16) % 16)}
		if i%17 == 0 {
			location = common.Location{}
		} else if i%19 == 0 {
			location = common.Location{byte(i % 16)}
		}
		wh.SetLocation(location)
		wh.SetPrimaryCoinbase(common.BytesToAddress(random(20), common.Location{0, 0}))
		wh.SetData(random(i % 64))
		for _, share := range []*types.PowShareDiffAndCount{wh.ScryptDiffAndCount(), wh.ShaDiffAndCount()} {
			share.SetDifficulty(big.NewInt(int64(i)))
			share.SetCount(big.NewInt(int64(i % 3)))
			share.SetUncled(big.NewInt(int64(i % 2)))
		}
		wh.SetShaShareTarget(new(big.Int).SetBytes(random(i % 32)))
		wh.SetScryptShareTarget(new(big.Int).SetBytes(random((i + 1) % 32)))
		wh.SetKawpowDifficulty(big.NewInt(int64(i + 1)))
		p, err := wh.ProtoEncode()
		if err != nil {
			panic(err)
		}
		if i%7 == 0 {
			p.Number = append([]byte{0}, p.Number...)
		}
		if i%9 == 0 {
			p.Lock = nil
		}
		raw, err := proto.Marshal(p)
		if err != nil {
			panic(err)
		}
		if i%5 == 0 {
			raw = append(raw, 0xa0, 0x06, 0x01)
		} // Unknown field 100 is discarded by native type reconstruction.
		parsed := new(types.ProtoWorkObjectHeader)
		if err = proto.Unmarshal(raw, parsed); err != nil {
			panic(err)
		}
		decoded := new(types.WorkObjectHeader)
		if err = decoded.ProtoDecode(parsed, common.Location{0, 0}); err != nil {
			panic(err)
		}
		canonical, _ := decoded.ProtoEncode()
		canonicalBytes, _ := proto.Marshal(canonical)
		sealBytes, _ := proto.Marshal(decoded.SealEncode())
		// SealEncode includes coinbase; SealHash intentionally omits it after activation.
		if decoded.KawpowActivationHappened() {
			canonicalSeal := decoded.SealEncode()
			canonicalSeal.PrimaryCoinbase = nil
			sealBytes, _ = proto.Marshal(canonicalSeal)
		}
		legacy := decoded.WoProgpowHash()
		valid := new(big.Int).SetBytes(legacy.Bytes()).Cmp(new(big.Int).Div(common.Big2e256, decoded.Difficulty())) <= 0
		// WorkObjectHeader.Hash: the ProgPoW hash before KawPow and for transition
		// headers without AuxPow, otherwise blake3 of the protobuf AuxPow, which
		// is empty without one (a ProgPoW header at the last transition height).
		vectors = append(vectors, map[string]interface{}{"id": fmt.Sprintf("header-%03d", i), "input": hx(raw), "canonical": hx(canonicalBytes), "seal_bytes": hx(sealBytes), "seal_hash": decoded.SealHash().Hex(), "legacy_hash": legacy.Hex(), "block_hash": decoded.Hash().Hex(), "blake3_target_valid": valid})
	}
	bad := []interface{}{}
	base := vectors[0].(map[string]interface{})
	raw, _ := hex.DecodeString(base["input"].(string)[2:])
	original := new(types.ProtoWorkObjectHeader)
	proto.Unmarshal(raw, original)
	for _, name := range []string{"header_hash", "parent_hash", "number", "difficulty", "tx_hash", "nonce", "location", "mix_hash", "time", "prime_terminus_number", "primary_coinbase"} {
		p := proto.Clone(original).(*types.ProtoWorkObjectHeader)
		switch name {
		case "header_hash":
			p.HeaderHash = nil
		case "parent_hash":
			p.ParentHash = nil
		case "number":
			p.Number = nil
		case "difficulty":
			p.Difficulty = nil
		case "tx_hash":
			p.TxHash = nil
		case "nonce":
			p.Nonce = nil
		case "location":
			p.Location = nil
		case "mix_hash":
			p.MixHash = nil
		case "time":
			p.Time = nil
		case "prime_terminus_number":
			p.PrimeTerminusNumber = nil
		case "primary_coinbase":
			p.PrimaryCoinbase = nil
		}
		encoded, _ := proto.Marshal(p)
		err := new(types.WorkObjectHeader).ProtoDecode(p, common.Location{0, 0})
		if err == nil {
			panic("expected Go rejection")
		}
		bad = append(bad, map[string]interface{}{"id": "missing-" + name, "input": hx(encoded), "go_rejected": true})
	}
	targets := []interface{}{}
	for i := 0; i < 260; i++ {
		h := random(32)
		d := random(i%32 + 1)
		if i%3 == 0 {
			d = []byte{byte(i%255 + 1)}
		}
		difficulty := new(big.Int).SetBytes(d)
		if difficulty.Sign() == 0 {
			difficulty.SetInt64(1)
			d = []byte{1}
		}
		if i%13 == 0 {
			h = make([]byte, 32)
		}
		valid := new(big.Int).SetBytes(h).Cmp(new(big.Int).Div(common.Big2e256, difficulty)) <= 0
		targets = append(targets, map[string]interface{}{"hash": hx(h), "difficulty": hx(d), "valid": valid})
	}
	report := map[string]interface{}{"schema": 1, "reference_revision": referenceRevision, "schedule": map[string]interface{}{"kawpow_fork": params.KawPowForkBlock, "transition_period": params.KawPowTransitionPeriod}, "headers": vectors, "rejected": bad, "targets": targets, "aux_shapes": auxShapes(), "scope": "Bounded work-header codec/commitment and Blake3 target comparisons. Not ProgPoW/KAWPOW mining verification or complete block validation."}
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	if err := encoder.Encode(report); err != nil {
		panic(err)
	}
}

// auxShapes: work headers whose AuxPow has or lacks a donor header and a
// coinbase transaction, on both sides of KawPow and of its transition.
// WorkObjectHeader.ProtoDecode keeps an AuxPow only with both
// (types/wo.go:1652) and drops any other without checking it.
func auxShapes() []interface{} {
	t := types.DefaultKawpowAuxTemplate()
	donor := func(id types.PowID) []byte {
		return types.NewBlockHeader(id, 1, [32]byte{}, [32]byte{}, 1000, 0x1d00ffff, 0, 1).Bytes()
	}
	coinbase := func(id types.PowID) []byte {
		return types.NewAuxPowCoinbaseTx(id, 1, t.CoinbaseOut(), common.Hash{}, 1000)
	}
	id := func(n uint32) *uint32 { return &n }
	shapes := []struct {
		name string
		aux  *types.ProtoAuxPow
	}{
		{"complete-kawpow", &types.ProtoAuxPow{ChainId: id(1), Header: donor(types.Kawpow), Transaction: coinbase(types.Kawpow)}},
		{"complete-sha", &types.ProtoAuxPow{ChainId: id(2), Header: donor(types.SHA_BTC), Transaction: coinbase(types.SHA_BTC)}},
		{"complete-scrypt", &types.ProtoAuxPow{ChainId: id(4), Header: donor(types.Scrypt), Auxpow2: make([]byte, 32), Transaction: coinbase(types.Scrypt)}},
		{"empty-transaction", &types.ProtoAuxPow{ChainId: id(1), Header: donor(types.Kawpow), Transaction: []byte{}}},
		{"header-only", &types.ProtoAuxPow{ChainId: id(1), Header: donor(types.Kawpow)}},
		{"header-only-sha", &types.ProtoAuxPow{ChainId: id(2), Header: donor(types.SHA_BTC), Signature: []byte{1, 2}}},
		{"transaction-only", &types.ProtoAuxPow{ChainId: id(1), Transaction: coinbase(types.Kawpow)}},
		{"neither", &types.ProtoAuxPow{ChainId: id(1)}},
		{"empty-message", &types.ProtoAuxPow{}},
		{"unknown-id-partial", &types.ProtoAuxPow{ChainId: id(9), Header: donor(types.Kawpow)}},
		{"scrypt-partial-no-auxpow2", &types.ProtoAuxPow{ChainId: id(4), Header: donor(types.Scrypt)}},
		{"empty-header", &types.ProtoAuxPow{ChainId: id(1), Header: []byte{}, Transaction: coinbase(types.Kawpow)}},
		{"short-header", &types.ProtoAuxPow{ChainId: id(1), Header: donor(types.Kawpow)[:100], Transaction: coinbase(types.Kawpow)}},
		{"unknown-id-complete", &types.ProtoAuxPow{ChainId: id(9), Header: donor(types.Kawpow), Transaction: coinbase(types.Kawpow)}},
		{"scrypt-complete-no-auxpow2", &types.ProtoAuxPow{ChainId: id(4), Header: donor(types.Scrypt), Transaction: coinbase(types.Scrypt)}},
	}
	out := []interface{}{}
	for _, prime := range []uint64{params.KawPowForkBlock - 1, params.KawPowForkBlock, params.KawPowForkBlock + params.KawPowTransitionPeriod - 1, params.KawPowForkBlock + params.KawPowTransitionPeriod, 2000000} {
		for _, shape := range shapes {
			wo := types.EmptyWorkObject(common.ZONE_CTX)
			wh := wo.WorkObjectHeader()
			wh.SetHeaderHash(common.HexToHash("0x11"))
			wh.SetParentHash(common.HexToHash("0x22"))
			wh.SetTxHash(common.HexToHash("0x33"))
			wh.SetMixHash(common.HexToHash("0x44"))
			wh.SetNumber(big.NewInt(1000))
			wh.SetDifficulty(big.NewInt(1000000))
			wh.SetPrimeTerminusNumber(new(big.Int).SetUint64(prime))
			wh.SetNonce(types.EncodeNonce(7))
			wh.SetTime(1700000000)
			wh.SetLocation(common.Location{0, 0})
			wh.SetPrimaryCoinbase(common.BytesToAddress(common.FromHex("0x00aa000000000000000000000000000000000001"), common.Location{0, 0}))
			wh.SetData([]byte{1})
			for _, share := range []*types.PowShareDiffAndCount{wh.ScryptDiffAndCount(), wh.ShaDiffAndCount()} {
				share.SetDifficulty(big.NewInt(5))
				share.SetCount(big.NewInt(1))
				share.SetUncled(big.NewInt(0))
			}
			wh.SetShaShareTarget(big.NewInt(6))
			wh.SetScryptShareTarget(big.NewInt(7))
			wh.SetKawpowDifficulty(big.NewInt(8))
			p, err := wh.ProtoEncode()
			if err != nil {
				panic(err)
			}
			p.AuxPow = shape.aux
			raw, err := proto.Marshal(p)
			if err != nil {
				panic(err)
			}
			parsed := new(types.ProtoWorkObjectHeader)
			if err = proto.Unmarshal(raw, parsed); err != nil {
				panic(err)
			}
			v := map[string]interface{}{"id": fmt.Sprintf("aux-%s-%d", shape.name, prime), "input": hx(raw)}
			decoded := new(types.WorkObjectHeader)
			if err = decoded.ProtoDecode(parsed, common.Location{0, 0}); err != nil {
				v["go_rejected"] = true
				out = append(out, v)
				continue
			}
			canonical, err := decoded.ProtoEncode()
			if err != nil {
				panic(err)
			}
			canonicalBytes, err := proto.Marshal(canonical)
			if err != nil {
				panic(err)
			}
			v["go_rejected"] = false
			v["canonical"] = hx(canonicalBytes)
			v["block_hash"] = decoded.Hash().Hex()
			v["aux"] = decoded.AuxPow() != nil
			out = append(out, v)
		}
	}
	return out
}
