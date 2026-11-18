// Offline projections through actual verifyHeader and economic helpers.
package core

import (
	"encoding/binary"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/consensus/kawpow"
	"github.com/dominant-strategies/go-quai/consensus/misc"
	"github.com/dominant-strategies/go-quai/consensus/progpow"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"math/big"
)

func hnRound(w *types.WorkObject) *types.WorkObject {
	p, e := w.ProtoEncode(types.BlockObject)
	if e != nil {
		panic(e)
	}
	n := new(types.WorkObject)
	if e = n.ProtoDecode(p, common.Location{0, 0}, types.BlockObject); e != nil {
		panic(e)
	}
	return n
}
func hnSync(w *types.WorkObject) { w.WorkObjectHeader().SetHeaderHash(w.Header().Hash()) }
func hnBind(w *types.WorkObject) {
	if w.PrimeTerminusNumber().Uint64() < params.KawPowForkBlock {
		return
	}
	h := w.WorkObjectHeader()
	if h.KawpowDifficulty() == nil {
		h.SetKawpowDifficulty(nh(0))
		h.SetShaShareTarget(nh(0))
		h.SetScryptShareTarget(nh(0))
		h.SetShaDiffAndCount(types.NewPowShareDiffAndCount(nh(0), nh(0), nh(0)))
		h.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(nh(0), nh(0), nh(0)))
	}
	t := types.DefaultKawpowAuxTemplate()
	tx := types.NewAuxPowCoinbaseTx(types.Kawpow, t.Height(), t.CoinbaseOut(), w.SealHash(), t.SignatureTime())
	mr := types.CalculateMerkleRoot(types.Kawpow, tx, t.MerkleBranch())
	donor := types.NewBlockHeader(types.Kawpow, int32(t.Version()), t.PrevHash(), mr, t.SignatureTime()+1, t.Bits(), 0, t.Height())
	w.WorkObjectHeader().SetAuxPow(types.NewAuxPow(types.Kawpow, donor, t.AuxPow2(), t.Sigs(), t.MerkleBranch(), tx))
}
func hnHC() *HeaderChain {
	hc := histHC()
	hc.powConfig.GasCeil = 50000000
	hc.fetchPrimeBlock = hc.GetBlockByHash
	return hc
}
func hnParent(prime, number uint64, d *big.Int) *types.WorkObject {
	w := hb(number, common.Hash{}, prime)
	h := w.WorkObjectHeader()
	h.SetDifficulty(d)
	h.SetTime(1769112000)
	if h.AuxPow() != nil {
		h.AuxPow().SetTransaction([]byte{1})
		h.AuxPow().Header().SetNonce64(prime<<32 ^ number)
	}
	h.SetMixHash(common.BigToHash(new(big.Int).Div(new(big.Int).Set(common.Big2e256), d)))
	h.SetShaDiffAndCount(types.NewPowShareDiffAndCount(nh(7000000000000000), nh(3<<32), nh(0)))
	h.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(nh(700000000000), nh(3<<32), nh(0)))
	h.SetShaShareTarget(nh(4 << 32))
	h.SetScryptShareTarget(nh(4 << 32))
	h.SetKawpowDifficulty(nh(21500000000000))
	w.Header().SetExchangeRate(new(big.Int).Set(params.ExchangeRate))
	hnSync(w)
	hnBind(w)
	return hnRound(w)
}
func hnChild(hc *HeaderChain, p *types.WorkObject) *types.WorkObject {
	prime := p.PrimeTerminusNumber().Uint64()
	w := hb(p.NumberU64(2)+1, p.Hash(), prime)
	h := w.WorkObjectHeader()
	h.SetDifficulty(hc.CalcDifficulty(p.WorkObjectHeader(), p.ExpansionNumber()))
	h.SetTime(p.Time() + 5)
	_, order, e := hc.CalcOrder(p)
	if e != nil {
		panic(e)
	}
	w.Header().SetParentEntropy(hc.TotalLogEntropy(p), 2)
	if order == 2 {
		w.Header().SetParentDeltaEntropy(hc.DeltaLogEntropy(p), 2)
		w.Header().SetParentUncledDeltaEntropy(hc.UncledDeltaLogEntropy(p), 2)
	}
	exp, e := hc.ComputeExpansionNumber(p)
	if e != nil {
		panic(e)
	}
	w.Header().SetExpansionNumber(exp)
	w.Header().SetGasLimit(CalcGasLimit(p, hc.powConfig.GasCeil))
	w.Header().SetStateLimit(misc.CalcStateLimit(p, params.StateCeil))
	w.Header().SetBaseFee(hc.CalcBaseFee(p))
	if order == 0 || hc.IsGenesisHash(p.Hash()) {
		w.Header().SetPrimeTerminusHash(p.Hash())
		h.SetPrimeTerminusNumber(p.Number(0))
	} else {
		w.Header().SetPrimeTerminusHash(p.PrimeTerminusHash())
		h.SetPrimeTerminusNumber(p.PrimeTerminusNumber())
	}
	prime = h.PrimeTerminusNumber().Uint64()
	if prime >= params.KawPowForkBlock {
		sd, sc, su := hc.CalculatePowDiffAndCount(p, h, types.SHA_BTC)
		td, tc, tu := hc.CalculatePowDiffAndCount(p, h, types.Scrypt)
		h.SetShaDiffAndCount(types.NewPowShareDiffAndCount(sd, sc, su))
		h.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(td, tc, tu))
		h.SetShaShareTarget(hc.CalculateShareTarget(p, w))
		h.SetScryptShareTarget(hc.CalculateShareTarget(p, w))
		h.SetKawpowDifficulty(hc.CalculateKawpowDifficulty(p, w))
	}
	hnSync(w)
	hnBind(w)
	return hnRound(w)
}
func hnMutate(w *types.WorkObject, id int) {
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
		h.SetDifficulty(new(big.Int).Add(h.Difficulty(), nh(1)))
	case 5:
		h.SetLocation(common.Location{1, 1})
	case 6:
		b.SetParentEntropy(new(big.Int).Add(w.ParentEntropy(2), nh(1)), 2)
	case 7:
		b.SetParentDeltaEntropy(new(big.Int).Add(w.ParentDeltaEntropy(2), nh(1)), 2)
	case 8:
		b.SetParentUncledDeltaEntropy(new(big.Int).Add(w.ParentUncledDeltaEntropy(2), nh(1)), 2)
	case 9:
		b.SetExpansionNumber(w.ExpansionNumber() + 1)
	case 10:
		a := make([]byte, 20)
		a[0] = 0x11
		h.SetPrimaryCoinbase(common.BytesToAddress(a, common.Location{0, 0}))
	case 11:
		h.SetLock(1)
	case 12:
		h.SetData(nil)
	case 13:
		h.SetData([]byte{4})
	case 14:
		a := make([]byte, 21)
		a[1] = 0x11
		h.SetData(a)
	case 15:
		a := make([]byte, 41)
		a[21] = 0x11
		h.SetData(a)
	case 16:
		b.SetGasLimit(uint64(1) << 63)
	case 17:
		b.SetGasUsed(w.GasLimit() + 1)
	case 18:
		b.SetGasLimit(w.GasLimit() + 1)
	case 19:
		b.SetStateUsed(w.StateLimit() + 1)
	case 20:
		b.SetStateLimit(w.StateLimit() + 1)
	case 21:
		b.SetBaseFee(new(big.Int).Add(w.BaseFee(), nh(1)))
	case 22:
		b.SetPrimeTerminusHash(common.HexToHash("0xdead"))
	case 23:
		h.SetPrimeTerminusNumber(new(big.Int).Add(h.PrimeTerminusNumber(), nh(1)))
	case 24:
		h.SetNumber(new(big.Int).Add(h.Number(), nh(1)))
	case 25:
		h.SetData(make([]byte, 42))
	case 26:
		a := make([]byte, 20)
		a[1] = 0x80
		h.SetPrimaryCoinbase(common.BytesToAddress(a, common.Location{0, 0}))
	case 27:
		h.SetTime(1769112015)
	case 28:
		h.SetTime(1769112016)
	case 29:
		b.SetExtra(make([]byte, 32))
	case 41:
		h.SetLocation(common.Location{})
	case 42:
		h.SetLocation(common.Location{})
		h.SetData(make([]byte, 21))
	case 43:
		d := make([]byte, 41)
		d[22] = 0x80
		h.SetData(d)
	case 44:
		h.SetTime(1769112000)
	case 45:
		h.SetTime(1769113000)
	}
	if w.PrimeTerminusNumber().Uint64() >= params.KawPowForkBlock {
		switch id {
		case 30:
			t := h.ShaDiffAndCount()
			h.SetShaDiffAndCount(types.NewPowShareDiffAndCount(new(big.Int).Add(t.Difficulty(), nh(1)), t.Count(), t.Uncled()))
		case 31:
			t := h.ShaDiffAndCount()
			h.SetShaDiffAndCount(types.NewPowShareDiffAndCount(t.Difficulty(), new(big.Int).Add(t.Count(), nh(1)), t.Uncled()))
		case 32:
			t := h.ShaDiffAndCount()
			h.SetShaDiffAndCount(types.NewPowShareDiffAndCount(t.Difficulty(), t.Count(), new(big.Int).Add(t.Uncled(), nh(1))))
		case 33:
			t := h.ScryptDiffAndCount()
			h.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(new(big.Int).Add(t.Difficulty(), nh(1)), t.Count(), t.Uncled()))
		case 34:
			t := h.ScryptDiffAndCount()
			h.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(t.Difficulty(), new(big.Int).Add(t.Count(), nh(1)), t.Uncled()))
		case 35:
			t := h.ScryptDiffAndCount()
			h.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(t.Difficulty(), t.Count(), new(big.Int).Add(t.Uncled(), nh(1))))
		case 36:
			h.SetShaShareTarget(new(big.Int).Add(h.ShaShareTarget(), nh(1)))
		case 37:
			h.SetScryptShareTarget(new(big.Int).Add(h.ScryptShareTarget(), nh(1)))
		case 38:
			h.SetKawpowDifficulty(new(big.Int).Add(h.KawpowDifficulty(), nh(1)))
		}
	}
	hnSync(w)
	hnBind(w)
	if id == 39 {
		h.SetHeaderHash(common.HexToHash("0x01"))
	}
	if id == 40 && h.AuxPow() != nil {
		sig := append([]byte{}, h.AuxPow().Signature()...)
		sig[0] ^= 1
		h.AuxPow().SetSignature(sig)
	}
}
func NativeHeaderVectors() map[string]interface{} {
	out := map[string]interface{}{}
	hc := hnHC()
	defer hc.headerDb.Close()
	quotes := []interface{}{}
	heights := []uint64{0, 1171499, 1171500, 1754999, 1755000, 2236999, 2237000, 2237001}
	period := uint64(365 * 17280 * 269 / 100)
	numbers := []uint64{0, 1, 1219999, 1220000, 1220001, period - 1, period, period + 1, 2 * period, 2*period + 1, ^uint64(0)}
	for i := 0; i < 352; i++ {
		prime := heights[i%len(heights)]
		number := numbers[(i/len(heights))%len(numbers)]
		d := new(big.Int).Lsh(nh(uint64(5+i)*1000000000000), uint(i%120))
		w := hnParent(prime, number, d)
		h := w.WorkObjectHeader()
		if prime >= 1171500 {
			h.SetShaDiffAndCount(types.NewPowShareDiffAndCount(new(big.Int).Lsh(nh(7000000000000000), uint(i%60)), nh(uint64(i%11)<<32), nh(0)))
			h.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(nh(700000000000), nh(uint64(i%7)<<32), nh(0)))
		}
		er := new(big.Int).Lsh(new(big.Int).Set(params.ExchangeRate), uint(i%45))
		w.Header().SetAvgTxFees(new(big.Int).Mul(nh(uint64(1000+i)), nh(1000000000000000)))
		equiv := d
		if prime >= 1171500 {
			equiv = misc.ForkAwareKawPowEquivalentDifficulty(h, d)
		}
		quai := misc.CalculateQuaiReward(h, d, er)
		qi := misc.CalculateQiReward(h, d)
		conversion := misc.CalculateQuaiConversionReward(w, er, d)
		fee := new(big.Int).Div(misc.QiToQuai(w, er, d, nh(5)), nh(21000))
		hp, e := h.ProtoEncode()
		if e != nil {
			panic(e)
		}
		_ = hp
		quotes = append(quotes, map[string]interface{}{"header": hwh(h), "exchange": er.String(), "average_fees": w.AvgTxFees().String(), "one_over_kqi": params.OneOverKqi(number).String(), "equivalent": equiv.String(), "quai": quai.String(), "qi": qi.String(), "conversion_quai": conversion.String(), "base_fee": fee.String()})
	}
	out["quotes"] = quotes
	headers := []interface{}{}
	for i, prime := range []uint64{0, 261999, 262000, 1171499, 1171500, 1301099, 1301100, 1330000, 1530500, 1754999, 1755000, 1872600, 2236999, 2237000} {
		for mode := 0; mode < 5; mode++ {
			root := hnParent(prime, 0, nh(5000000000000))
			root.WorkObjectHeader().SetLocation(common.Location{})
			root.SetNumber(nh(prime), 0)
			root.Header().SetExpansionNumber(0)
			hnSync(root)
			hnBind(root)
			root = hnRound(root)
			hstore(hc, root)
			hc.AddGenesisHash(root.Hash())
			p := root
			extraRecords := []string{}
			if mode > 0 {
				p = hnParent(prime, uint64([]int{1, 259200, 1036800}[i%3]), nh(5000000000000))
				p.WorkObjectHeader().SetParentHash(root.Hash())
				p.SetParentHash(root.Hash(), 0)
				p.Header().SetPrimeTerminusHash(root.Hash())
				p.Header().SetGasLimit(uint64(i%2) * 12000000)
				p.Header().SetStateLimit(uint64(i%2) * 12000000)
				if mode == 2 || mode == 3 {
					shift := uint(20)
					if mode == 3 {
						shift = 3
					}
					p.WorkObjectHeader().SetMixHash(common.BigToHash(new(big.Int).Rsh(new(big.Int).Div(new(big.Int).Set(common.Big2e256), p.Difficulty()), shift)))
					if mode == 3 {
						p.WorkObjectHeader().SetMixHash(common.BigToHash(new(big.Int).Div(new(big.Int).Div(new(big.Int).Set(common.Big2e256), p.Difficulty()), nh(3))))
					}
					p.Header().SetParentDeltaEntropy(new(big.Int).Lsh(nh(1000), 64), 2)
					p.Header().SetParentDeltaEntropy(new(big.Int).Lsh(nh(1000), 64), 1)
				}
				if p.AuxPow() != nil {
					p.AuxPow().Header().SetNonce64(prime<<24 ^ uint64(mode)<<56 ^ p.NumberU64(2))
				}

				if mode == 4 {
					p.WorkObjectHeader().SetNumber(nh(uint64(259201 + i)))
					grand := hnParent(prime, p.NumberU64(2)-1, nh(5000000000000))
					grand.WorkObjectHeader().SetParentHash(root.Hash())
					grand.Header().SetPrimeTerminusHash(root.Hash())
					grand.WorkObjectHeader().SetTime(p.Time() - uint64(i%7))
					hnSync(grand)
					hnBind(grand)
					grand = hnRound(grand)
					hstore(hc, grand)
					extraRecords = append(extraRecords, hp(grand))
					p.WorkObjectHeader().SetParentHash(grand.Hash())
					p.Header().SetAvgTxFees(nh(123456789000))
				}
				hnSync(p)
				hnBind(p)
				p = hnRound(p)
				hstore(hc, p)
			}
			child := hnChild(hc, p)
			for variant := 0; variant < 46; variant++ {
				w := types.CopyWorkObject(child)
				hnMutate(w, variant)
				w = hnRound(w)
				err := hc.verifyHeader(w, p, variant == 45, 1769112000)
				records := append([]string{hp(root)}, extraRecords...)
				if mode > 0 {
					records = append(records, hp(p))
				}
				headers = append(headers, map[string]interface{}{"id": fmt.Sprintf("%d/%d/%d", prime, mode, variant), "prime": prime, "mode": mode, "variant": variant, "records": records, "genesis": root.Hash().Hex(), "block": hp(w), "now": 1769112000, "uncle": variant == 45, "valid": err == nil, "error": fmt.Sprint(err), "parent_order": func() int {
					_, o, e := hc.CalcOrder(p)
					if e != nil {
						panic(e)
					}
					return o
				}(), "parent_total": hc.TotalLogEntropy(p).String(), "parent_delta": hc.DeltaLogEntropy(p).String(), "parent_uncled_delta": hc.UncledDeltaLogEntropy(p).String()})
			}
		}
	}
	out["headers"] = headers
	out["native"] = hnNative()
	out["expansions"] = hnExpansions()
	return out
}

func hnMine(hc *HeaderChain, w *types.WorkObject, zone bool) {
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
		if new(big.Int).SetBytes(pow[:]).Cmp(target) <= 0 {
			if zone {
				_, order, e := hc.CalcOrder(w)
				if e != nil {
					panic(e)
				}
				if order != 2 {
					continue
				}
			}
			return
		}
	}
	panic("native proof nonce bound")
}
func hnNative() []interface{} {
	hc := hnHC()
	defer hc.headerDb.Close()
	hc.engine = []consensus.Engine{progpow.New(params.PowConfig{}, nil, false, log.Global), kawpow.New(params.PowConfig{}, nil, false, log.Global)}
	out := []interface{}{}
	for _, prime := range []uint64{0, 1171500, 1330000, 1755000, 2237000} {
		root := hnParent(prime, 0, nh(2))
		root.WorkObjectHeader().SetLocation(common.Location{})
		root.SetNumber(nh(prime), 0)
		hnSync(root)
		hnBind(root)
		root = hnRound(root)
		hstore(hc, root)
		hc.AddGenesisHash(root.Hash())
		parent := root
		records := []string{hp(root)}
		if prime == 0 {
			parent = hnChild(hc, root)
			hnMine(hc, parent, true)
			hstore(hc, parent)
			records = append(records, hp(parent))
		}
		base := hnChild(hc, parent)
		hnMine(hc, base, false)
		for variant := 0; variant < 6; variant++ {
			p := hnRound(parent)
			if p.Hash() != parent.Hash() {
				panic("parent canonical clone changed ID")
			}
			w := hnRound(base)
			list := append([]string{}, records...)
			switch variant {
			case 1:
				h := w.WorkObjectHeader()
				if h.AuxPow() == nil {
					mix := h.MixHash()
					mix[0] ^= 1
					h.SetMixHash(mix)
				} else {
					mix := h.AuxPow().Header().MixHash()
					mix[0] ^= 1
					h.AuxPow().Header().SetMixHash(mix)
				}
			case 2:
				w.Header().SetBaseFee(new(big.Int).Add(w.BaseFee(), nh(1)))
				hnSync(w)
			case 3:
				w.Header().SetParentEntropy(nh(1), 2)
				hnSync(w)
			case 4:
				if prime == 0 {
					mix := p.MixHash()
					mix[0] ^= 1
					p.WorkObjectHeader().SetMixHash(mix)
					w.WorkObjectHeader().SetParentHash(p.Hash())
					list = []string{hp(root), hp(p)}
				} else {
					sig := append([]byte{}, w.AuxPow().Signature()...)
					sig[0] ^= 1
					w.AuxPow().SetSignature(sig)
				}
			case 5:
				w.WorkObjectHeader().SetData([]byte{4})
			}
			hstore(hc, p)
			err := hc.verifyHeader(w, p, false, 1769112020)
			inspect := err
			if inspect == nil {
				_, inspect = hc.VerifySeal(w.WorkObjectHeader())
			}
			if inspect == nil {
				inspect = hc.VerifyUncles(w)
			}
			if inspect == nil {
				inspect = NewBlockValidator(hc.config, hc, hc.engine).ValidateBody(w)
			}
			out = append(out, map[string]interface{}{"id": fmt.Sprintf("native/%d/%d", prime, variant), "prime": prime, "variant": variant, "records": list, "genesis": root.Hash().Hex(), "block": hp(w), "now": 1769112020, "valid": err == nil, "error": fmt.Sprint(err), "inspect_valid": inspect == nil, "inspect_error": fmt.Sprint(inspect)})
		}
	}
	return out
}
func hnExpansions() []interface{} {
	out := []interface{}{}
	for _, exp := range []uint8{0, 1, 10, 254, 255} {
		for _, threshold := range []uint16{0, 1167, 1168, 1169} {
			for _, trusted := range []bool{false, true} {
				for _, location := range []common.Location{{0, 0}, {1, 1}} {
					hc := hnHC()
					hc.config.Location = location
					hc.powConfig.NodeLocation = location
					grand := hnParent(0, 0, nh(2))
					grand.Header().SetExpansionNumber(7)
					hnSync(grand)
					hnBind(grand)
					hstore(hc, grand)
					parent := hnParent(0, 1, nh(2))
					parent.SetParentHash(grand.Hash(), 0)
					parent.Header().SetExpansionNumber(exp)
					parent.Header().SetThresholdCount(threshold)
					parent.Header().SetParentDeltaEntropy(new(big.Int).Lsh(nh(1000000000000), 64), 1)
					parent.Header().SetParentDeltaEntropy(new(big.Int).Lsh(nh(1000000000000), 64), 2)
					var one common.Hash
					one[31] = 1
					parent.WorkObjectHeader().SetMixHash(one)
					hnSync(parent)
					hnBind(parent)
					hstore(hc, parent)
					if trusted {
						hc.AddGenesisHash(parent.Hash())
					}
					_, order, err := hc.CalcOrder(parent)
					if err != nil {
						panic(err)
					}
					result, err := hc.ComputeExpansionNumber(parent)
					if err != nil {
						panic(err)
					}
					out = append(out, map[string]interface{}{"records": []string{hp(grand), hp(parent)}, "parent": hp(parent), "genesis": func() string {
						if trusted {
							return parent.Hash().Hex()
						}
						return common.Hash{}.Hex()
					}(), "location": []int{int(location[0]), int(location[1])}, "order": order, "expected": result})
					hc.headerDb.Close()
				}
			}
		}
	}
	return out
}
