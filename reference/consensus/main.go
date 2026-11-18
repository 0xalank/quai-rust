// Explicit offline consensus oracle; no public peers, mining or wallet access.
package main

import (
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/consensus/kawpow"
	"github.com/dominant-strategies/go-quai/consensus/misc"
	"github.com/dominant-strategies/go-quai/consensus/progpow"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"math/big"
	"math/rand"
	"os"
)

var referenceRevision string

func main() {
	log.Global.SetOutput(os.Stderr)
	b, _ := hex.DecodeString("de5c89c8f378529b5afb71baca3807ce6128c6dc55a9a50245083f917a4dc0d5")
	groups := []interface{}{}
	for _, height := range []uint64{0, 388800} {
		groups = append(groups, progpow.NativeConsensusVector(height, b, 0x863f502af7e5e397))
	}
	for _, height := range []uint64{0, 7500, 2976194} {
		groups = append(groups, kawpow.NativeConsensusVector(height, b, 0x863f502af7e5e397))
	}

	rng := rand.New(rand.NewSource(2026110919))
	hexword := func(n *big.Int) string { return "0x" + hex.EncodeToString(n.FillBytes(make([]byte, 32))) }
	difficulty := []interface{}{}
	entropy := []interface{}{}
	ids := []interface{}{}
	limits := []interface{}{}
	thresholds := []interface{}{}
	for i := 0; i < 100; i++ {
		data := make([]byte, 30)
		rng.Read(data)
		d := new(big.Int).SetBytes(data)
		if i < 16 {
			d = new(big.Int).Lsh(big.NewInt(1), uint(i*13+1))
		}
		min := big.NewInt(int64(i%7 + 1))
		for _, dt := range []int64{-100, -1, 0, 1, 4, 5, 6, 99, 100, 101, 10000} {
			for _, duration := range []uint64{1, 5, 100} {
				p, g := uint64(20000+dt), uint64(20000)
				answer := core.NativeDifficultyVector(d, min, p, g, duration)
				difficulty = append(difficulty, map[string]interface{}{"parent": hexword(d), "minimum": hexword(min), "parent_time": p, "grandparent_time": g, "duration": duration, "expected": hexword(answer)})
			}
		}
	}
	for expansion := 0; expansion < 32; expansion++ {
		for j := 0; j < 16; j++ {
			for context := 0; context < 3; context++ {
				bits := uint(2 + j*5)
				diff := new(big.Int).Lsh(big.NewInt(1), bits)
				hashN := new(big.Int).Lsh(big.NewInt(1), uint(256-int(bits)-j*5))
				if j%3 == 2 {
					hashN.Add(hashN, big.NewInt(17))
					diff.Rsh(diff, 1)
				}
				hash := common.BytesToHash(hashN.Bytes())
				var pe, pd, pu [3]*big.Int
				pes, pds, pus := []string{}, []string{}, []string{}
				for k := 0; k < 3; k++ {
					pe[k] = new(big.Int).Lsh(big.NewInt(int64(1000+j*3+k)), 64)
					pd[k] = new(big.Int).Lsh(big.NewInt(int64(j*j+k)), 64)
					pu[k] = new(big.Int).Lsh(big.NewInt(int64(j+k)), 64)
					pes = append(pes, pe[k].String())
					pds = append(pds, pd[k].String())
					pus = append(pus, pu[k].String())
				}
				u := new(big.Int).Lsh(big.NewInt(int64(j)), 64)
				share := []int{-1, 0, 1, 2, 3, 8, 16, 32}[j%8]
				value := core.NativeEntropyVector(hash, diff, uint8(expansion), pe, pd, pu, u, share, context)
				entropy = append(entropy, map[string]interface{}{"hash": hash.Hex(), "difficulty": hexword(diff), "expansion": expansion, "parent_entropy": pes, "parent_delta": pds, "parent_uncled_delta": pus, "uncled_entropy": u.String(), "shares": share, "context": context, "expected": value})
			}
		}
	}
	for _, height := range []uint64{0, params.KawPowForkBlock - 1, params.KawPowForkBlock, params.KawPowForkBlock + 1, params.KawPowForkBlock + params.KawPowTransitionPeriod - 1, params.KawPowForkBlock + params.KawPowTransitionPeriod, params.KawPowForkBlock + params.KawPowTransitionPeriod + 1} {
		for id := -1; id <= 8; id++ {
			for _, share := range []bool{false, true} {
				ids = append(ids, map[string]interface{}{"prime": height, "aux_id": id, "workshare": share, "valid": core.NativePowID(height, id, share)})
			}
		}
	}
	for _, height := range []uint64{0, params.TimeToStartTx - 1, params.TimeToStartTx, params.TimeToStartTx + 1, 2*params.BlocksPerMonth - 1, 2 * params.BlocksPerMonth, 2*params.BlocksPerMonth + 1, ^uint64(0)} {
		for _, parent := range []uint64{0, 1, 12000000, ^uint64(0)} {
			for _, ceil := range []uint64{0, 1, 12000000, 50000000, ^uint64(0)} {
				wo := types.EmptyWorkObject(common.ZONE_CTX)
				wo.SetNumber(new(big.Int).SetUint64(height), common.ZONE_CTX)
				wo.Header().SetGasLimit(parent)
				wo.Header().SetStateLimit(parent)
				limits = append(limits, map[string]interface{}{"number": height, "parent": parent, "ceiling": ceil, "gas": core.CalcGasLimit(wo, ceil), "state": misc.CalcStateLimit(wo, ceil)})
			}
		}
	}
	for i := 0; i < 128; i++ {
		d := new(big.Int).Lsh(big.NewInt(1), uint(i*2))
		d.Add(d, big.NewInt(1))
		h := types.EmptyWorkObject(common.ZONE_CTX).WorkObjectHeader()
		h.SetDifficulty(d)
		for _, shift := range []int{1, 3, 7, 16, 127, 255} {
			t, e := consensus.CalcWorkShareThreshold(h, shift)
			if e != nil {
				panic(e)
			}
			thresholds = append(thresholds, map[string]interface{}{"difficulty": hexword(d), "shift": shift, "target": "0x" + hex.EncodeToString(t.Bytes())})
		}
	}
	donors := []interface{}{}
	for i := 0; i < 48; i++ {
		raw := make([]byte, 80)
		rng.Read(raw)
		if i == 0 {
			clear(raw)
		}
		if i == 1 {
			for j := range raw {
				raw[j] = 255
			}
		}
		values := map[string]interface{}{}
		for _, id := range []types.PowID{types.SHA_BTC, types.SHA_BCH, types.Scrypt} {
			var prev, merkle [32]byte
			copy(prev[:], raw[4:36])
			copy(merkle[:], raw[36:68])
			h := types.NewBlockHeader(id, int32(binary.LittleEndian.Uint32(raw[:4])), prev, merkle, binary.LittleEndian.Uint32(raw[68:72]), binary.LittleEndian.Uint32(raw[72:76]), binary.LittleEndian.Uint32(raw[76:]), 0)
			values[id.String()] = h.PowHash().Hex()
		}
		donors = append(donors, map[string]interface{}{"header": "0x" + hex.EncodeToString(raw), "expected": values})
	}
	value := map[string]interface{}{"schema": 1, "kind": "native-consensus", "reference_revision": referenceRevision, "pow_groups": groups, "donors": donors, "difficulty": difficulty, "entropy": entropy, "pow_ids": ids, "limits": limits, "thresholds": thresholds, "scope": "Native ProgPoW/KAWPOW light kernels and rule components. No aux template/signature/Merkle validity, full header/block acceptance, economic state or canonical chain authority."}
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	if err := encoder.Encode(value); err != nil {
		panic(err)
	}
}
