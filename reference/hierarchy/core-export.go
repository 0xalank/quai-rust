// Offline-only access to actual prime/region policy and arithmetic methods.
package core

import (
	"encoding/binary"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/consensus/kawpow"
	"github.com/dominant-strategies/go-quai/consensus/progpow"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/dominant-strategies/go-quai/trie"
	"math/big"
)

func hrHC(ctx int) *HeaderChain { return nativeHC(ctx) }
func hrThreshold(previous, score uint16) uint16 {
	if previous == 0 {
		if score > params.TREE_EXPANSION_THRESHOLD {
			return 1
		}
		return 0
	}
	if previous < params.TREE_EXPANSION_TRIGGER_WINDOW && score < params.TREE_EXPANSION_THRESHOLD || previous == params.TREE_EXPANSION_TRIGGER_WINDOW+params.TREE_EXPANSION_WAIT_COUNT {
		return 0
	}
	return previous + 1
}
func hrBody(w *types.WorkObject, ctx int) {
	w.Body().SetManifest(types.BlockManifest{common.HexToHash("0xa1"), common.HexToHash("0xa2")})
	w.Header().SetManifestHash(types.DeriveSha(w.Manifest(), trie.NewStackTrie(nil)), ctx+1)
	w.Body().SetInterlinkHashes(common.Hashes{common.HexToHash("0xb1")})
	w.Header().SetInterlinkRootHash(types.DeriveSha(w.InterlinkHashes(), trie.NewStackTrie(nil)))
}
func hrChild(hc *HeaderChain, p *types.WorkObject) *types.WorkObject {
	ctx := hc.NodeCtx()
	w := hnParent(p.PrimeTerminusNumber().Uint64(), p.NumberU64(2)+1, nh(2))
	w.SetParentHash(p.Hash(), ctx)
	n := new(big.Int).Add(p.Number(ctx), nh(1))
	if hc.IsGenesisHash(p.Hash()) {
		n = nh(1)
	}
	w.SetNumber(n, ctx)
	w.WorkObjectHeader().SetTime(p.Time() + 5)
	w.WorkObjectHeader().SetLocation(p.Location())
	_, order, e := hc.CalcOrder(p)
	if e != nil {
		panic(e)
	}
	w.Header().SetParentEntropy(hc.TotalLogEntropy(p), ctx)
	if ctx == 1 && order == 1 {
		w.Header().SetParentDeltaEntropy(hc.DeltaLogEntropy(p), ctx)
		w.Header().SetParentUncledDeltaEntropy(hc.UncledDeltaLogEntropy(p), ctx)
	}
	if ctx == 0 {
		var score, count uint16
		if p.NumberU64(0) != 0 {
			score, e = hc.ComputeEfficiencyScore(p)
			if e != nil {
				panic(e)
			}
			count = hrThreshold(p.ThresholdCount(), score)
		}
		w.Header().SetEfficiencyScore(score)
		w.Header().SetThresholdCount(count)
		bits := p.EtxEligibleSlices()
		if !hc.IsGenesisHash(p.Hash()) {
			bits = hc.UpdateEtxEligibleSlices(p, p.Location())
		}
		w.Header().SetEtxEligibleSlices(bits)
		w.Header().SetMinerDifficulty(hc.ComputeMinerDifficulty(p))
	}
	hrBody(w, ctx)
	hnSync(w)
	hnBind(w)
	return hnRound(w)
}
func hrParent(prime uint64, ctx, mode, index int) (*HeaderChain, *types.WorkObject, []string, common.Hash) {
	hc := hrHC(ctx)
	root := hnParent(prime, 0, nh(2))
	root.SetNumber(nh(0), ctx)
	root.Header().SetEfficiencyScore(65535)
	root.Header().SetThresholdCount(65535)
	hnSync(root)
	hnBind(root)
	root = hnRound(root)
	hc.AddGenesisHash(root.Hash())
	records := []string{hp(root)}
	if mode == 0 {
		return hc, root, records, root.Hash()
	}
	p := hnParent(prime, []uint64{259199, 259200, 259201}[index%3], nh(2))
	p.SetNumber(nh([]uint64{1, 262000, 262001}[index%3]), ctx)
	p.SetParentHash(root.Hash(), ctx)
	p.SetParentHash(common.HexToHash("0x1234"), 0)
	p.Header().SetMinerDifficulty(nh(uint64(index+1) * 10000))
	p.Header().SetEfficiencyScore([]uint16{0, 1, 6553, 32768, 65534, 65535}[index%6])
	p.Header().SetThresholdCount([]uint16{0, 1, 143, 144, 1167, 1168, 65535}[index%7])
	p.Header().SetParentEntropy(new(big.Int).Lsh(nh(100), 64), ctx)
	p.Header().SetParentDeltaEntropy(new(big.Int).Lsh(nh(1000), 64), 1)
	p.Header().SetParentDeltaEntropy(new(big.Int).Lsh(nh(1000), 64), 2)
	p.Header().SetParentUncledDeltaEntropy(nh(uint64(index%3)), 1)
	p.Header().SetParentUncledDeltaEntropy(nh(uint64(index%7)), 2)
	p.Header().SetUncledEntropy(nh(11))
	p.Header().SetEtxEligibleSlices(common.HexToHash("0x88001122ffffffff"))
	var pow common.Hash
	if mode == 1 {
		pow[31] = 1
	} else if mode == 2 {
		pow = common.BigToHash(new(big.Int).Div(new(big.Int).Lsh(nh(1), 255), nh(3)))
	} else {
		pow = common.BigToHash(new(big.Int).Lsh(nh(1), 255))
	}
	p.WorkObjectHeader().SetMixHash(pow)
	if p.AuxPow() != nil {
		p.AuxPow().Header().SetNonce64(uint64(index)<<48 | uint64(mode)<<44 | uint64(ctx)<<40 | prime)
	}
	hnSync(p)
	hnBind(p)
	p = hnRound(p)
	records = append(records, hp(p))
	return hc, p, records, root.Hash()
}
func hrMutate(w *types.WorkObject, ctx, id int) {
	h := w.WorkObjectHeader()
	b := w.Header()
	switch id {
	case 1:
		b.SetExtra(make([]byte, 33))
	case 2:
		h.SetTime(1769113000)
	case 3:
		h.SetTime(1)
	case 4:
		b.SetParentEntropy(new(big.Int).Add(w.ParentEntropy(ctx), nh(1)), ctx)
	case 5:
		b.SetParentDeltaEntropy(new(big.Int).Add(w.ParentDeltaEntropy(ctx), nh(1)), ctx)
	case 6:
		b.SetParentUncledDeltaEntropy(new(big.Int).Add(w.ParentUncledDeltaEntropy(ctx), nh(1)), ctx)
	case 7:
		h.SetLocation(common.Location{1, 1})
	case 8:
		b.SetEfficiencyScore(w.EfficiencyScore() + 1)
	case 9:
		b.SetThresholdCount(w.ThresholdCount() + 1)
	case 10:
		b.SetEtxEligibleSlices(common.HexToHash("0x01"))
	case 11:
		b.SetPrimeStateRoot(common.HexToHash("0x01"))
	case 12:
		b.SetRegionStateRoot(common.HexToHash("0x01"))
	case 13:
		b.SetMinerDifficulty(new(big.Int).Add(w.MinerDifficulty(), nh(1)))
	case 14:
		w.SetNumber(new(big.Int).Add(w.Number(ctx), nh(1)), ctx)
	case 15:
		h.SetDifficulty(nh(0))
	case 16:
		h.SetData(nil)
	case 17:
		h.SetLock(255)
	case 18:
		a := make([]byte, 20)
		a[0] = 0x77
		a[1] = 0x80
		h.SetPrimaryCoinbase(common.BytesToAddress(a, common.Location{0, 0}))
	case 19:
		b.SetBaseFee(nh(999))
	case 20:
		b.SetGasLimit(1 << 63)
		b.SetGasUsed(^uint64(0))
		b.SetStateUsed(^uint64(0))
	case 21:
		b.SetExpansionNumber(255)
	case 22:
		b.SetExtra(make([]byte, 32))
	case 23:
		h.SetLocation(common.Location{})
	case 24:
		h.SetTime(1769112015)
	case 25:
		h.SetTime(1769112016)
	case 26:
		h.SetTime(1769113000)
	case 27:
		b.SetPrimeTerminusHash(common.HexToHash("0x1234"))
	case 28:
		b.SetParentDeltaEntropy(nh(99), 2)
	case 29:
		b.SetParentEntropy(nh(99), 2)
	case 30:
		b.SetParentHash(common.HexToHash("0x9876"), 1-ctx)
	case 31:
		if h.KawpowDifficulty() != nil {
			h.SetKawpowDifficulty(nh(1))
			h.SetShaShareTarget(nh(1))
			h.SetShaDiffAndCount(types.NewPowShareDiffAndCount(nh(1), nh(1), nh(1)))
		}
	}
	hnSync(w)
	hnBind(w)
	if id == 32 {
		h.SetHeaderHash(common.HexToHash("0x01"))
	}
	if id == 33 && h.AuxPow() != nil {
		sig := append([]byte{}, h.AuxPow().Signature()...)
		sig[0] ^= 1
		h.AuxPow().SetSignature(sig)
	}
}
func hrCase(hc *HeaderChain, p, w *types.WorkObject, records []string, genesis common.Hash, id string, uncle bool) map[string]interface{} {
	err := hc.verifyHeader(w, p, uncle, 1769112000)
	_, order, e := hc.CalcOrder(p)
	if e != nil {
		panic(e)
	}
	loc := []int{}
	for _, b := range hc.NodeLocation() {
		loc = append(loc, int(b))
	}
	return map[string]interface{}{"id": id, "context": hc.NodeCtx(), "location": loc, "records": records, "genesis": genesis.Hex(), "block": hp(w), "now": 1769112000, "uncle": uncle, "valid": err == nil, "error": fmt.Sprint(err), "parent_order": order, "parent_total": hc.TotalLogEntropy(p).String(), "parent_delta": hc.DeltaLogEntropy(p).String(), "parent_uncled_delta": hc.UncledDeltaLogEntropy(p).String()}
}
func NativeHierarchyVectors() map[string]interface{} {
	out := map[string]interface{}{}
	hc := hrHC(0)
	defer hc.headerDb.Close()
	dummy := common.HexToHash("0x1111")
	hc.AddGenesisHash(dummy)
	scores := []interface{}{}
	for i := 0; i < 384; i++ {
		p := hnParent(0, 1, nh(2))
		prev := []uint16{0, 1, 7, 6553, 32768, 65534, 65535}[i%7]
		d0 := new(big.Int).Lsh(nh(uint64(i%19+1)), uint(i%200))
		d1 := nh(uint64(i % 7))
		u0 := new(big.Int).Lsh(nh(uint64(i%23)), uint(i%250))
		u1 := nh(uint64(i % 11))
		if i%5 == 0 {
			u0 = nh(0)
			u1 = nh(0)
		}
		if i%9 == 0 {
			d0 = nh(0)
			d1 = nh(0)
			u0 = nh(0)
			u1 = nh(0)
		}
		gen := i%4 == 0
		p.SetParentHash(common.HexToHash("0x2222"), 0)
		if gen {
			p.SetParentHash(dummy, 0)
		}
		p.Header().SetEfficiencyScore(prev)
		p.Header().SetParentDeltaEntropy(d0, 1)
		p.Header().SetParentDeltaEntropy(d1, 2)
		p.Header().SetParentUncledDeltaEntropy(u0, 1)
		p.Header().SetParentUncledDeltaEntropy(u1, 2)
		score, e := hc.ComputeEfficiencyScore(p)
		if e != nil {
			panic(e)
		}
		scores = append(scores, map[string]interface{}{"previous": prev, "delta": []string{d0.String(), d1.String()}, "uncled": []string{u0.String(), u1.String()}, "parent_of_parent_genesis": gen, "expected": score})
	}
	out["scores"] = scores
	miners := []interface{}{}
	for i := 0; i < 128; i++ {
		p := hnParent(0, 1, nh(2))
		number := []uint64{0, 262000, 262001, ^uint64(0)}[i%4]
		d := new(big.Int).Lsh(nh(uint64(i+1)), uint(i%240))
		previous := new(big.Int).Lsh(nh(uint64(i+17)), uint(i%240))
		if i == 127 {
			d.Sub(new(big.Int).Lsh(nh(1), 256), nh(1))
			previous.Set(d)
		}
		p.SetNumber(nh(number), 0)
		p.WorkObjectHeader().SetDifficulty(d)
		p.Header().SetMinerDifficulty(previous)
		miners = append(miners, map[string]interface{}{"number": number, "difficulty": d.String(), "previous": previous.String(), "expected": hc.ComputeMinerDifficulty(p).String()})
	}
	out["miners"] = miners
	bitmaps := []interface{}{}
	for pos := 0; pos < 256; pos++ {
		loc := common.Location{byte(pos / 16), byte(pos % 16)}
		for _, number := range []uint64{0, 259200, 259201, ^uint64(0)} {
			for pattern := 0; pattern < 3; pattern++ {
				p := hnParent(0, number, nh(2))
				var bits common.Hash
				for j := range bits {
					if pattern == 1 {
						bits[j] = 255
					} else if pattern == 2 {
						bits[j] = byte(j*17 + pos)
					}
				}
				p.Header().SetEtxEligibleSlices(bits)
				result := hc.UpdateEtxEligibleSlices(p, loc)
				bitmaps = append(bitmaps, map[string]interface{}{"location": []int{int(loc[0]), int(loc[1])}, "number": number, "initial": bits.Hex(), "expected": result.Hex()})
			}
		}
	}
	out["bitmaps"] = bitmaps
	headers := []interface{}{}
	for _, ctx := range []int{0, 1} {
		for hi, prime := range []uint64{0, 1171500, 1301100, 1755000, 2237000} {
			for mode := 0; mode < 4; mode++ {
				ch, p, records, gen := hrParent(prime, ctx, mode, hi*4+mode)
				base := hrChild(ch, p)
				for variant := 0; variant < 34; variant++ {
					w := hnRound(base)
					hrMutate(w, ctx, variant)
					w = hnRound(w)
					v := hrCase(ch, p, w, records, gen, fmt.Sprintf("%d/%d/%d/%d", ctx, prime, mode, variant), variant == 26)
					v["variant"] = variant
					v["mode"] = mode
					headers = append(headers, v)
				}
				ch.headerDb.Close()
			}
		}
	}
	out["headers"] = headers
	thresholds := []interface{}{}
	for _, prev := range []uint16{0, 1, 143, 144, 1167, 1168, 65534, 65535} {
		for _, score := range []uint16{0, 1, 6553, 65535} {
			for signal := 0; signal < 2; signal++ {
				ch, p, _, gen := hrParent(0, 0, 1, int(prev)%256)
				p.Header().SetThresholdCount(prev)
				p.Header().SetEfficiencyScore(score)
				p.Header().SetParentUncledDeltaEntropy(nh(uint64(signal)), 1)
				p.Header().SetParentUncledDeltaEntropy(nh(0), 2)
				hnSync(p)
				hnBind(p)
				p = hnRound(p)
				records := []string{hp(p)}
				base := hrChild(ch, p)
				for mutation := 0; mutation < 2; mutation++ {
					w := hnRound(base)
					if mutation == 1 {
						w.Header().SetThresholdCount(w.ThresholdCount() + 1)
						hnSync(w)
					}
					v := hrCase(ch, p, w, records, gen, fmt.Sprintf("threshold/%d/%d/%d/%d", prev, score, signal, mutation), false)
					v["mutation"] = mutation
					thresholds = append(thresholds, v)
				}
				ch.headerDb.Close()
			}
		}
	}
	out["thresholds"] = thresholds
	out["native"] = hrNative()
	return out
}
func hrMine(hc *HeaderChain, w *types.WorkObject, want int) {
	target := new(big.Int).Div(new(big.Int).Set(common.Big2e256), w.Difficulty())
	for nonce := uint64(0); nonce < 1000; nonce++ {
		h := w.WorkObjectHeader()
		var mix, pow common.Hash
		if h.AuxPow() == nil {
			var b [8]byte
			binary.BigEndian.PutUint64(b[:], nonce)
			h.SetNonce(types.BlockNonce(b))
			mix, pow = hc.GetEngineForHeader(h).ComputePowLight(h)
			h.SetMixHash(mix)
		} else {
			h.AuxPow().Header().SetNonce64(nonce)
			mix, pow = hc.GetEngineForHeader(h).ComputePowLight(h)
			h.AuxPow().Header().SetMixHash(mix)
		}
		if new(big.Int).SetBytes(pow[:]).Cmp(target) > 0 {
			continue
		}
		if want >= 0 {
			_, order, e := hc.CalcOrder(w)
			if e != nil {
				panic(e)
			}
			if order != want {
				continue
			}
		}
		return
	}
	panic("hierarchy native nonce bound")
}
func hrNative() []interface{} {
	out := []interface{}{}
	engines := []consensus.Engine{progpow.New(params.PowConfig{}, nil, false, log.Global), kawpow.New(params.PowConfig{}, nil, false, log.Global)}
	for _, ctx := range []int{0, 1} {
		for _, prime := range []uint64{0, 1171500, 1755000} {
			hc, root, _, gen := hrParent(prime, ctx, 0, 0)
			hc.engine = engines
			p := hrChild(hc, root)
			p.Header().SetParentDeltaEntropy(new(big.Int).Lsh(nh(1000), 64), 1)
			p.Header().SetParentDeltaEntropy(new(big.Int).Lsh(nh(1000), 64), 2)
			p.Header().SetParentUncledDeltaEntropy(nh(0), 1)
			p.Header().SetParentUncledDeltaEntropy(nh(0), 2)
			hnSync(p)
			hnBind(p)
			hrMine(hc, p, ctx)
			base := hrChild(hc, p)
			hrMine(hc, base, ctx)
			for variant := 0; variant < 9; variant++ {
				parent := hnRound(p)
				w := hnRound(base)
				switch variant {
				case 1:
					if w.AuxPow() == nil {
						mix := w.MixHash()
						mix[0] ^= 1
						w.WorkObjectHeader().SetMixHash(mix)
					} else {
						mix := w.AuxPow().Header().MixHash()
						mix[0] ^= 1
						w.AuxPow().Header().SetMixHash(mix)
					}
				case 2:
					w.Header().SetParentEntropy(nh(1), ctx)
					hnSync(w)
				case 3:
					w.Body().SetManifest(types.BlockManifest{})
				case 4:
					if parent.AuxPow() == nil {
						mix := parent.MixHash()
						mix[0] ^= 1
						parent.WorkObjectHeader().SetMixHash(mix)
					} else {
						mix := parent.AuxPow().Header().MixHash()
						mix[0] ^= 1
						parent.AuxPow().Header().SetMixHash(mix)
					}
					w.SetParentHash(parent.Hash(), ctx)
				case 5:
					if w.AuxPow() != nil {
						sig := append([]byte{}, w.AuxPow().Signature()...)
						sig[0] ^= 1
						w.AuxPow().SetSignature(sig)
					} else {
						w.SetNumber(new(big.Int).Add(w.Number(ctx), nh(1)), ctx)
						hnSync(w)
					}
				case 6:
					if ctx == 0 {
						w.Header().SetPrimeStateRoot(common.HexToHash("0x01"))
					} else {
						w.Header().SetRegionStateRoot(common.HexToHash("0x01"))
					}
					hnSync(w)
				case 7:
					w.Body().SetInterlinkHashes(common.Hashes{})
				case 8:
					w.Body().SetUncles([]*types.WorkObjectHeader{parent.WorkObjectHeader()})

				}
				err := hc.verifyHeader(w, parent, false, 1769112040)
				inspect := err
				if inspect == nil {
					_, inspect = hc.VerifySeal(w.WorkObjectHeader())
				}
				if inspect == nil {
					inspect = NewBlockValidator(hc.config, hc, hc.engine).ValidateBody(w)
				}
				loc := []int{}
				for _, b := range hc.NodeLocation() {
					loc = append(loc, int(b))
				}
				out = append(out, map[string]interface{}{"id": fmt.Sprintf("native/%d/%d/%d", ctx, prime, variant), "context": ctx, "location": loc, "records": []string{hp(root), hp(parent)}, "genesis": gen.Hex(), "block": hp(w), "now": 1769112040, "valid": err == nil, "error": fmt.Sprint(err), "inspect_valid": inspect == nil, "inspect_error": fmt.Sprint(inspect)})
			}
			hc.headerDb.Close()
		}
	}
	for _, ctx := range []int{0, 1} {
		for _, prime := range []uint64{0, 1171500, 1755000} {
			for _, trusted := range []bool{false, true} {
				hc := hrHC(ctx)
				hc.engine = engines
				p := hnParent(prime, 0, nh(2))
				p.SetNumber(nh(0), ctx)
				p.Header().SetParentEntropy(nh(17), 0)
				p.Header().SetParentDeltaEntropy(nh(9), 1)
				p.Header().SetParentDeltaEntropy(nh(11), 2)
				hnSync(p)
				hnBind(p)
				p = hnRound(p)
				gen := common.Hash{}
				if trusted {
					hc.AddGenesisHash(p.Hash())
					gen = p.Hash()
				}
				w := hrChild(hc, p)
				hrMine(hc, w, ctx)
				err := hc.verifyHeader(w, p, false, 1769112040)
				inspect := err
				if inspect == nil {
					_, inspect = hc.VerifySeal(w.WorkObjectHeader())
				}
				if inspect == nil {
					inspect = NewBlockValidator(hc.config, hc, hc.engine).ValidateBody(w)
				}
				loc := []int{}
				for _, b := range hc.NodeLocation() {
					loc = append(loc, int(b))
				}
				out = append(out, map[string]interface{}{"id": fmt.Sprintf("zero-context/%d/%d/%v", ctx, prime, trusted), "context": ctx, "location": loc, "records": []string{hp(p)}, "genesis": gen.Hex(), "block": hp(w), "now": 1769112040, "valid": err == nil, "error": fmt.Sprint(err), "inspect_valid": inspect == nil, "inspect_error": fmt.Sprint(inspect)})
				hc.headerDb.Close()
			}
		}
	}

	return out
}
