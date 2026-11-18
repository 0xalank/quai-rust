// Offline core-method oracle. Controlled work hashes are policy projections only.
package core

import (
	"encoding/hex"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/consensus/kawpow"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	lru "github.com/hashicorp/golang-lru/v2"
	"google.golang.org/protobuf/proto"
	"math/big"
)

func nh(v uint64) *big.Int { return new(big.Int).SetUint64(v) }
func hh(v []byte) string   { return "0x" + hex.EncodeToString(v) }
func hp(w *types.WorkObject) string {
	p, e := w.ProtoEncode(types.BlockObject)
	if e != nil {
		panic(e)
	}
	b, e := proto.Marshal(p)
	if e != nil {
		panic(e)
	}
	return hh(b)
}
func hwh(h *types.WorkObjectHeader) string {
	p, e := h.ProtoEncode()
	if e != nil {
		panic(e)
	}
	b, e := proto.Marshal(p)
	if e != nil {
		panic(e)
	}
	return hh(b)
}
func hb(n uint64, p common.Hash, prime uint64) *types.WorkObject {
	w := types.EmptyWorkObject(common.ZONE_CTX)
	w.SetNumber(nh(n), 2)
	w.SetNumber(nh(prime), 0)
	w.WorkObjectHeader().SetPrimeTerminusNumber(nh(prime))
	w.WorkObjectHeader().SetParentHash(p)
	w.WorkObjectHeader().SetDifficulty(nh(1000000000000))
	w.WorkObjectHeader().SetTime(n*5 + 1000)
	w.WorkObjectHeader().SetLocation(common.Location{0, 0})
	w.WorkObjectHeader().SetPrimaryCoinbase(common.BytesToAddress(make([]byte, 20), common.Location{0, 0}))
	w.WorkObjectHeader().SetData([]byte{0})
	if prime >= 1301100 {
		a := ha(types.Kawpow)
		a.SetHeader(types.NewBlockHeader(types.Kawpow, 1, [32]byte(p), [32]byte{}, uint32(n+1000), 0x1d00ffff, uint32(n), uint32(n)))
		w.WorkObjectHeader().SetAuxPow(a)
	}
	hsync(w)
	return w
}
func hsync(w *types.WorkObject) {
	w.Header().SetUncleHash(types.CalcUncleHash(w.Uncles()))
	w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
}
// Bind supplied synthetic ancestry before its hash is referenced or stored.
// This does not mine or validate that ancestry; native inclusion candidates below
// are still mined separately and must never be rebound after mining.
func histBindContext(w *types.WorkObject) {
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
	h.SetAuxPow(types.NewAuxPow(types.Kawpow, donor, t.AuxPow2(), t.Sigs(), t.MerkleBranch(), tx))
}
func ha(id types.PowID) *types.AuxPow {
	a := new(types.AuxPow)
	a.SetPowID(id)
	a.SetHeader(types.NewBlockHeader(id, 1, [32]byte{}, [32]byte{}, 1000, 0x1d00ffff, 0, 1))
	if id == types.Scrypt {
		a.SetAuxPow2(make([]byte, 32))
	}
	return a
}
func hstore(hc *HeaderChain, w *types.WorkObject) {
	rawdb.WriteTermini(hc.headerDb, w.Hash(), types.EmptyTermini())
	rawdb.WriteHeaderNumber(hc.headerDb, w.Hash(), w.NumberU64(2))
	rawdb.WriteWorkObject(hc.headerDb, w.Hash(), w, types.BlockObject, 2)
	rawdb.WriteWorkObject(hc.headerDb, w.Hash(), w, types.WorkShareObject, 2)
	hc.headerCache.Add(w.Hash(), *w)
	hc.bc.blockCache.Add(w.Hash(), *w)
}
func histHC() *HeaderChain {
	hc := nativeHC(2)
	bc := NewTestBodyDb(hc.headerDb)
	bc.chainConfig = hc.config
	hc.bc = bc
	hc.powConfig.WorkShareThreshold = 7
	hc.numberCache, _ = lru.New[common.Hash, uint64](128)
	return hc
}
func NativeHistoryVectors() map[string]interface{} {
	hc := histHC()
	defer hc.headerDb.Close()
	out := map[string]interface{}{}
	// Controller inputs are bounded positive magnitudes; both negative-floor and clamp paths.
	controls := []interface{}{}
	heights := []uint64{1171500, 1171501, 1329999, 1330000, 1330001, 1339999, 1340000, 1530499, 1530500, 1872599, 1872600, 2000000}
	for i := 0; i < 384; i++ {
		prime := heights[i%len(heights)]
		d := new(big.Int).Lsh(nh(uint64(137+i)), uint(i%170))
		pd := new(big.Int).Lsh(nh(uint64(11+i)), uint(i%160))
		q := uint64(1) << 32
		target := nh(3*q + uint64(i)*17003)
		pk := new(big.Int).Mul(d, nh(uint64(i%4+1)))
		par := hb(100, common.Hash{}, prime)
		par.WorkObjectHeader().SetDifficulty(d)
		par.WorkObjectHeader().SetKawpowDifficulty(pk)
		par.WorkObjectHeader().SetShaShareTarget(target)
		par.WorkObjectHeader().SetScryptShareTarget(target)
		tr := types.NewPowShareDiffAndCount(pd, nh(uint64(i%10)*q+37), nh(uint64(i%3)*q+17))
		par.WorkObjectHeader().SetShaDiffAndCount(tr)
		par.WorkObjectHeader().SetScryptDiffAndCount(tr)
		uncles := []*types.WorkObjectHeader{}
		sc, uc := i%33, i%7
		if uc > sc {
			uc = sc
		}
		for j := 0; j < sc; j++ {
			h := hb(uint64(j), common.Hash{}, prime).WorkObjectHeader()
			h.SetAuxPow(ha(types.SHA_BTC))
			if j < uc {
				a := make([]byte, 20)
				a[0] = 0x11
				h.SetPrimaryCoinbase(common.BytesToAddress(a, common.Location{0, 0}))
			}
			uncles = append(uncles, h)
		}
		for j := 0; j < sc; j++ {
			h := types.CopyWorkObjectHeader(uncles[j])
			h.SetAuxPow(ha(types.Scrypt))
			uncles = append(uncles, h)
		}
		par.Body().SetUncles(uncles)
		child := hb(101, par.Hash(), prime)
		sha, avg, unc := hc.CalculatePowDiffAndCount(par, child.WorkObjectHeader(), types.SHA_BTC)
		scrypt, savg, sunc := hc.CalculatePowDiffAndCount(par, child.WorkObjectHeader(), types.Scrypt)
		controls = append(controls, map[string]interface{}{"id": i, "prime": prime, "block_diff": d.String(), "parent_kaw": pk.String(), "parent_diff": pd.String(), "target": target.String(), "count": tr.Count().String(), "uncled": tr.Uncled().String(), "samples": sc, "uncled_samples": uc, "expected_target": hc.CalculateShareTarget(par, child).String(), "sha": []string{sha.String(), avg.String(), unc.String()}, "scrypt": []string{scrypt.String(), savg.String(), sunc.String()}})
	}
	ema := []interface{}{}
	for i := 0; i < 144; i++ {
		prime := uint64(1171501)
		if i%8 == 0 {
			prime = 1171500
		}
		par := hb(100, common.Hash{}, prime)
		previous := new(big.Int).Lsh(nh(uint64(17+i)), uint(i%200))
		par.WorkObjectHeader().SetKawpowDifficulty(previous)
		if i%6 != 0 {
			t := types.DefaultKawpowAuxTemplate()
			st := uint32(1000)
			if i%9 == 0 {
				st = 0xfffffff8
			}
			t.SetSignatureTime(st)
			tx := types.NewAuxPowCoinbaseTx(types.Kawpow, t.Height(), t.CoinbaseOut(), common.Hash{}, st)
			if i%7 == 0 {
				tx = []byte{1, 2, 3}
			}
			timestamp := st + uint32([]int{0, 17, 18, 19, 25}[i%5])
			bits := []uint32{0x1d00ffff, 0x1b0404cb, 0x207fffff, 0x03010000, 0x2100ffff}[i%5]
			aux := types.NewAuxPow(types.Kawpow, types.NewBlockHeader(types.Kawpow, 1, [32]byte{}, [32]byte{}, timestamp, bits, 0, 1), nil, nil, nil, tx)
			par.WorkObjectHeader().SetAuxPow(aux)
		}
		child := hb(101, par.Hash(), prime)
		v := map[string]interface{}{"prime": prime, "previous": previous.String(), "expected": hc.CalculateKawpowDifficulty(par, child).String()}
		if par.AuxPow() != nil {
			p := par.AuxPow().ProtoEncode()
			b, e := proto.Marshal(p)
			if e != nil {
				panic(e)
			}
			v["aux"] = hh(b)
		}
		ema = append(ema, v)
	}
	out["kaw_ema"] = ema
	out["controllers"] = controls
	kd := []interface{}{}
	for i := 0; i < 256; i++ {
		d := new(big.Int).Lsh(nh(uint64(1+i)), uint(i%200))
		q := uint64(1) << 32
		sc, tc := nh(uint64(i%10)*q), nh(uint64(i%6)*q)
		kc := new(big.Int).Mul(d, nh(10000))
		den := uint64([]int{0, 1, 7499, 7500, 7501, 8000, 8999, 9000, 9001, 10000}[i%10])
		k := nh(0)
		if den > 0 {
			k.Div(kc, nh(den))
		}
		w := hb(1, common.Hash{}, 1171500)
		w.WorkObjectHeader().SetDifficulty(d)
		w.WorkObjectHeader().SetKawpowDifficulty(k)
		w.WorkObjectHeader().SetShaDiffAndCount(types.NewPowShareDiffAndCount(nh(1), sc, nh(0)))
		w.WorkObjectHeader().SetScryptDiffAndCount(types.NewPowShareDiffAndCount(nh(1), tc, nh(0)))
		w.WorkObjectHeader().SetShaShareTarget(nh(4 * q))
		w.WorkObjectHeader().SetScryptShareTarget(nh(4 * q))
		kd = append(kd, map[string]interface{}{"difficulty": d.String(), "sha_count": sc.String(), "scrypt_count": tc.String(), "sha_target": fmt.Sprint(4 * q), "scrypt_target": fmt.Sprint(4 * q), "kaw": k.String(), "expected": CalculateKawpowShareDiff(w.WorkObjectHeader()).String()})
	}
	out["kaw_share"] = kd
	classes := []interface{}{}
	for _, prime := range []uint64{1171499, 1171500, 1301099, 1301100, 1301101, 2000000} {
		for _, id := range []int{-1, 1, 2, 3, 4} {
			for _, di := range []uint64{1024, 65536, 1000000000, 700000000000} {
				for off := -1; off <= 1; off++ {
					w := hb(1, common.Hash{}, prime)
					h := w.WorkObjectHeader()
					h.SetDifficulty(nh(di))
					h.SetKawpowDifficulty(nh(di * 2))
					h.SetShaShareTarget(nh(3 << 32))
					h.SetScryptShareTarget(nh(3 << 32))
					tr := types.NewPowShareDiffAndCount(nh(di), nh(3<<32), nh(0))
					h.SetShaDiffAndCount(tr)
					h.SetScryptDiffAndCount(tr)
					pow := new(big.Int).Div(new(big.Int).Set(common.Big2e256), nh(di))
					pow.Add(pow, big.NewInt(int64(off)))
					if pow.Sign() < 0 || pow.BitLen() > 256 {
						continue
					}
					ph := common.BigToHash(pow)
					h.SetMixHash(ph)
					if id >= 0 {
						h.SetAuxPow(ha(types.PowID(id)))
						if id >= 2 && prime >= 1171500 {
							ph = h.AuxPow().Header().PowHash()
						}
					}
					classes = append(classes, map[string]interface{}{"header": hwh(h), "hash": ph.Hex(), "expected": int(hc.UncleWorkShareClassification(h))})
				}
			}
		}
	}
	out["classifications"] = classes
	// Real stored objects through WorkShareDistance and VerifyUncles; controlled engine hashes.
	distances := []interface{}{}
	inclusions := []interface{}{}
	for _, prime := range []uint64{261999, 262000, 1171499, 1171500, 1329999, 1330000, 1530500} {
		chain := []*types.WorkObject{}
		ph := common.Hash{}
		for j := 0; j < 8; j++ {
			w := hb(uint64(10+j), ph, prime)
			hstore(hc, w)
			chain = append(chain, w)
			ph = w.Hash()
		}
		block := hb(18, ph, prime)
		records := []string{}
		for _, w := range chain {
			records = append(records, hp(w))
		}
		for branch := 0; branch < 7; branch++ {
			ws := hb(uint64(11+branch), chain[branch].Hash(), prime).WorkObjectHeader()
			d, e := hc.WorkShareDistance(block, ws)
			v := map[string]interface{}{"prime": prime, "records": records, "block": hp(block), "share": hwh(ws), "valid": e == nil}
			if e == nil {
				v["distance"] = d.String()
			}
			distances = append(distances, v)
		}
		if prime >= 1171500 {
			continue
		} // full source policy after fork additionally requires bound signed auxiliary shares
		for variant := 0; variant < 22; variant++ {
			b := types.CopyWorkObject(block)
			ws := hb(18, ph, prime).WorkObjectHeader()
			ws.SetTime(ws.Time() + 1)
			ws.SetMixHash(common.HexToHash("0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"))
			list := []*types.WorkObjectHeader{ws}
			switch variant {
			case 1:
				list = nil
			case 2:
				list = append(list, ws)
			case 3:
				list = []*types.WorkObjectHeader{chain[6].WorkObjectHeader()}
			case 4:
				ws.SetParentHash(common.HexToHash("0x1234"))
			case 5:
				ws.SetMixHash(common.Hash{})
			case 6:
				ws.SetData(nil)
			case 7:
				ws.SetData([]byte{4})
			case 8:
				ws.SetLock(1)
			case 9:
				ws.SetNumber(nh(19))
			case 10:
				ws.SetDifficulty(nh(13))
			case 11:
				ws.SetParentHash(chain[5].Hash())
				ws.SetNumber(nh(16))
			case 12:
				ws.SetData(append([]byte{0}, make([]byte, 20)...))
			case 13:
				d := make([]byte, 21)
				d[1] = 0x11
				ws.SetData(d)
			case 14:
				d := make([]byte, 21)
				d[2] = 0x80
				ws.SetData(d)
			case 15:
				d := make([]byte, 41)
				d[21] = 0x11
				ws.SetData(d)
			case 16:
				list = []*types.WorkObjectHeader{}
				for j := 0; j < 17; j++ {
					u := types.CopyWorkObjectHeader(ws)
					u.SetTime(uint64(2000 + j))
					list = append(list, u)
				}
			case 17:
				ws.SetParentHash(chain[4].Hash())
				ws.SetNumber(nh(15))
			case 18:
				ws.SetData(make([]byte, 42))
			case 19:
				a := types.CopyWorkObject(chain[7])
				a.Body().SetUncles([]*types.WorkObjectHeader{ws})
				hsync(a)
				hstore(hc, a)
				b.WorkObjectHeader().SetParentHash(a.Hash())
			case 20:
				a := make([]byte, 20)
				a[1] = 0x80
				ws.SetPrimaryCoinbase(common.BytesToAddress(a, common.Location{0, 0}))
			case 21:
				ws.SetParentHash(chain[6].Hash())
				ws.SetNumber(nh(17))
				ws.SetMixHash(common.Hash{})
			}
			b.Body().SetUncles(list)
			hsync(b)
			e := hc.VerifyUncles(b)
			v := map[string]interface{}{"prime": prime, "variant": variant, "records": records, "block": hp(b), "valid": e == nil}
			if e != nil {
				v["error"] = e.Error()
			}
			if variant == 19 {
				anc := hc.GetBlockByHash(b.ParentHash(2))
				v["records"] = append(append([]string{}, records...), hp(anc))
			}
			inclusions = append(inclusions, v)
		}
	}

	// Actual signed KAWPOW work through VerifyUncles. Ancestry is supplied test context.
	real := histHC()
	defer real.headerDb.Close()
	engine := kawpow.New(params.PowConfig{}, nil, false, log.Global)
	real.engine = []consensus.Engine{nativeProjectionEngine{}, engine}
	integrated := []interface{}{}
	for _, fork := range []uint64{1171500, 1330000, 1530500, 1872600} {
		root := hb(0, common.Hash{}, fork)
		root.WorkObjectHeader().SetDifficulty(nh(10))
		root.WorkObjectHeader().SetKawpowDifficulty(nh(21500000000000))
		root.WorkObjectHeader().SetShaShareTarget(nh(4 << 32))
		root.WorkObjectHeader().SetScryptShareTarget(nh(4 << 32))
		root.WorkObjectHeader().SetShaDiffAndCount(types.NewPowShareDiffAndCount(nh(7000000000000000), nh(3<<32), nh(0)))
		root.WorkObjectHeader().SetScryptDiffAndCount(types.NewPowShareDiffAndCount(nh(700000000000), nh(3<<32), nh(0)))
		root.WorkObjectHeader().SetLocation(common.Location{})
		root.WorkObjectHeader().SetTime(1769111400)
		hsync(root)
		histBindContext(root)
		hstore(real, root)
		real.AddGenesisHash(root.Hash())
		chain := []*types.WorkObject{root}
		depth := 3
		if fork >= 1330000 {
			depth = 4
		}
		for j := 1; j < depth; j++ {
			w := hb(uint64(j), chain[j-1].Hash(), fork)
			w.WorkObjectHeader().SetDifficulty(nh(10))
			hsync(w)
			histBindContext(w)
			hstore(real, w)
			chain = append(chain, w)
		}
		records := []string{}
		for _, w := range chain {
			records = append(records, hp(w))
		}
		for variant := 0; variant < 12; variant++ {
			w := hb(1, root.Hash(), fork)
			h := w.WorkObjectHeader()
			h.SetDifficulty(nh(10))
			h.SetTime(1769112000)
			h.SetKawpowDifficulty(nh(21500000000000))
			h.SetShaShareTarget(nh(3 << 32))
			h.SetScryptShareTarget(nh(3 << 32))
			sd, sc, su := real.CalculatePowDiffAndCount(root, h, types.SHA_BTC)
			td, tc, tu := real.CalculatePowDiffAndCount(root, h, types.Scrypt)
			h.SetShaDiffAndCount(types.NewPowShareDiffAndCount(sd, sc, su))
			h.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(td, tc, tu))
			switch variant {
			case 2:
				h.SetShaDiffAndCount(types.NewPowShareDiffAndCount(sd, new(big.Int).Add(sc, nh(1)), su))
			case 3:
				h.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(new(big.Int).Add(td, nh(1)), tc, tu))
			case 4:
				h.SetNumber(nh(2))
			case 5:
				h.SetPrimeTerminusNumber(nh(fork + 1))
			case 6:
				h.SetLock(1)
			case 7:
				h.SetData(nil)
			case 11:
				h.SetTime(1)
			}
			t := types.DefaultKawpowAuxTemplate()
			tx := types.NewAuxPowCoinbaseTx(types.Kawpow, t.Height(), t.CoinbaseOut(), w.SealHash(), t.SignatureTime())
			mr := types.CalculateMerkleRoot(types.Kawpow, tx, t.MerkleBranch())
			if variant == 10 {
				mr[0] ^= 1
			}
			donor := types.NewBlockHeader(types.Kawpow, int32(t.Version()), t.PrevHash(), mr, t.SignatureTime()+1, t.Bits(), 0, t.Height())
			a := types.NewAuxPow(types.Kawpow, donor, t.AuxPow2(), t.Sigs(), t.MerkleBranch(), tx)
			h.SetAuxPow(a)
			for nonce := uint64(0); ; nonce++ {
				donor.SetNonce64(nonce)
				mix, pow := engine.ComputePowLight(h)
				donor.SetMixHash(mix)
				if new(big.Int).SetBytes(pow[:]).Cmp(new(big.Int).Div(new(big.Int).Set(common.Big2e256), nh(3))) <= 0 {
					break
				}
				if nonce > 1000 {
					panic("KAWPOW fixture nonce bound")
				}
			}
			if variant == 8 {
				sig := append([]byte{}, a.Signature()...)
				sig[0] ^= 1
				a.SetSignature(sig)
			}
			if variant == 9 {
				mix := donor.MixHash()
				mix[0] ^= 1
				donor.SetMixHash(mix)
			}
			b := hb(uint64(depth), chain[depth-1].Hash(), fork)
			list := []*types.WorkObjectHeader{h}
			if variant == 1 {
				list = append(list, h)
			}
			b.Body().SetUncles(list)
			hsync(b)
			e := real.VerifyUncles(b)
			v := map[string]interface{}{"prime": fork, "variant": variant, "records": records, "genesis": root.Hash().Hex(), "block": hp(b), "valid": e == nil}
			if e != nil {
				v["error"] = e.Error()
			}
			integrated = append(integrated, v)
		}

	}
	if integrated[0].(map[string]interface{})["valid"] != true {
		panic(integrated[0])
	}
	out["native_inclusions"] = integrated
	out["distances"] = distances
	out["inclusions"] = inclusions
	return out
}
