// Explicit offline arithmetic harness. The payout/fee branch is extracted
// verbatim from the official StateProcessor.Process, never shipped at runtime.
package core

import (
	"encoding/hex"
	"errors"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus/misc"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/crypto/multiset"
	"github.com/dominant-strategies/go-quai/params"
	lru "github.com/hashicorp/golang-lru/v2"
	"google.golang.org/protobuf/proto"
	"math/big"
)

func pyU(n uint64) *big.Int { return new(big.Int).SetUint64(n) }
func pyHex(b []byte) string { return "0x" + hex.EncodeToString(b) }
func pyMust(e error) {
	if e != nil {
		panic(e)
	}
}
func pyWire(w *types.WorkObject) string {
	p, e := w.ProtoEncode(types.BlockObject)
	pyMust(e)
	b, e := proto.Marshal(p)
	pyMust(e)
	return pyHex(b)
}
func pyAddr(loc common.Location, qi, remote bool) common.Address {
	var b [20]byte
	b[0] = loc.BytePrefix()
	b[19] = 55
	if qi {
		b[1] = 128
	}
	if remote {
		b[0] ^= 16
	}
	return common.BytesToAddress(b[:], loc)
}
func pyAux(w *types.WorkObject, id int, delta uint32) {
	h := w.WorkObjectHeader()
	if id == 0 {
		h.SetAuxPow(nil)
		return
	}
	t := types.DefaultKawpowAuxTemplate()
	seal := h.SealHash()
	var aux2 []byte
	if id == 4 {
		aux2 = make([]byte, 32)
		aux2[0] = 1
		seal = types.CreateAuxMerkleRoot(common.BytesToHash(aux2), seal)
	}
	signature := uint32(1769112000)
	tx := types.NewAuxPowCoinbaseTx(types.PowID(id), 100, t.CoinbaseOut(), seal, signature)
	mr := types.CalculateMerkleRoot(types.PowID(id), tx, nil)
	donor := types.NewBlockHeader(types.PowID(id), 1, [32]byte{}, mr, signature+delta, 0x1d00ffff, 0, 100)
	h.SetAuxPow(types.NewAuxPow(types.PowID(id), donor, aux2, make([]byte, 64), nil, tx))
}
func pyBlock(n, prime uint64, parent common.Hash, loc common.Location, qi bool) *types.WorkObject {
	w := types.EmptyWorkObject(2)
	h := w.WorkObjectHeader()
	h.SetLocation(loc)
	w.SetNumber(pyU(n), 2)
	w.SetNumber(pyU(prime), 0)
	h.SetPrimeTerminusNumber(pyU(prime))
	h.SetParentHash(parent)
	h.SetDifficulty(pyU(1000000000000))
	h.SetTime(1769112000 + n*5)
	h.SetData([]byte{2})
	h.SetPrimaryCoinbase(pyAddr(loc, qi, false))
	h.SetMixHash(common.BigToHash(new(big.Int).Lsh(pyU(1), 210)))
	h.SetShaDiffAndCount(types.NewPowShareDiffAndCount(pyU(7000000000000000), pyU(3<<32), pyU(0)))
	h.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(pyU(700000000000), pyU(3<<32), pyU(0)))
	h.SetShaShareTarget(pyU(4 << 32))
	h.SetScryptShareTarget(pyU(4 << 32))
	h.SetKawpowDifficulty(pyU(21500000000000))
	w.Header().SetAvgTxFees(pyU(101))
	w.Header().SetTotalFees(pyU(203))
	w.Header().SetExchangeRate(new(big.Int).Set(params.ExchangeRate))
	pySync(w)
	if prime >= params.KawPowForkBlock {
		pyAux(w, 1, 1)
	}
	return w
}
func pySync(w *types.WorkObject) {
	w.Header().SetUncleHash(types.CalcUncleHash(w.Uncles()))
	w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
}
func pyKeep(hc *HeaderChain, w *types.WorkObject) {
	rawdb.WriteTermini(hc.headerDb, w.Hash(), types.EmptyTermini())
	rawdb.WriteHeaderNumber(hc.headerDb, w.Hash(), w.NumberU64(2))
	hc.headerCache.Add(w.Hash(), *w)
	hc.bc.WriteBlock(w, 2)
}
func pyOne(prime uint64, loc common.Location, variant int) map[string]interface{} {
	hc := nativeHC(2)
	defer hc.headerDb.Close()
	hc.config.Location = loc
	hc.powConfig.NodeLocation = loc
	hc.numberCache, _ = lru.New[common.Hash, uint64](32)
	hc.bc = NewTestBodyDb(hc.headerDb)
	hc.bc.chainConfig = hc.config
	p := &StateProcessor{hc: hc}
	nodeCtx := common.ZONE_CTX
	number := uint64(4000000)
	depth := params.WorkSharesInclusionDepth
	if prime >= params.InclusionDepthChangeBlock {
		depth = params.NewWorkSharesInclusionDepth
	}
	targetNumber := number - uint64(depth)
	var parent *types.WorkObject
	records := []string{}
	var parentHash common.Hash
	alg := variant % 5
	if prime >= params.KawPowForkBlock+params.KawPowTransitionPeriod && alg == 0 {
		alg = 1
	}
	delta := []uint32{0, 3, 10, 18, 30, 31, 100, 0xffffffff}[variant%8]
	for index := 0; index < depth; index++ {
		targetPrime := prime
		if variant == 17 && prime == params.ConversionLockChangeForkBlock {
			targetPrime = prime - 1
		}
		w := pyBlock(targetNumber+uint64(index), targetPrime, parentHash, loc, variant%2 == 1)
		if index == 0 {
			w.Header().SetAvgTxFees(pyU(uint64(variant*7 + 1)))
			w.Header().SetTotalFees(pyU(uint64(variant*100 + 3)))
		}
		if index == depth-1 {
			share := pyBlock(targetNumber, targetPrime, common.Hash{31: 7}, loc, variant%3 == 1)
			share.WorkObjectHeader().SetPrimaryCoinbase(pyAddr(loc, variant%3 == 1, variant == 12))
			share.WorkObjectHeader().SetNonce(types.EncodeNonce(uint64(variant + 99)))
			if prime < params.KawPowForkBlock {
				if variant%2 == 1 {
					share.WorkObjectHeader().SetMixHash(common.BigToHash(new(big.Int).Lsh(pyU(1), 219)))
				}
			} else {
				pyAux(share, alg, delta)
			}
			pySync(share)
			if prime >= params.KawPowForkBlock && alg != 0 {
				pyAux(share, alg, delta)
			}
			w.Body().SetUncles([]*types.WorkObjectHeader{share.WorkObjectHeader()})
			if variant == 13 {
				w.Body().SetUncles([]*types.WorkObjectHeader{share.WorkObjectHeader(), share.WorkObjectHeader()})
			}
		}
		pySync(w)
		if targetPrime >= params.KawPowForkBlock {
			pyAux(w, 1, 1)
		}
		pyKeep(hc, w)
		records = append(records, pyWire(w))
		parent = w
		parentHash = w.Hash()
	}
	block := pyBlock(number, prime, parentHash, loc, false)
	if variant == 8 {
		block.WorkObjectHeader().SetShaDiffAndCount(types.NewPowShareDiffAndCount(pyU(7000000000000000), pyU(1<<32), pyU((1<<32)-1)))
		block.WorkObjectHeader().SetScryptDiffAndCount(types.NewPowShareDiffAndCount(pyU(700000000000), pyU(1<<32), pyU((1<<32)-1)))
	}
	if variant == 9 {
		block.WorkObjectHeader().SetShaDiffAndCount(types.NewPowShareDiffAndCount(pyU(7000000000000000), pyU(1<<32), pyU(1<<32)))
	}
	quaiFees := pyU(uint64(variant*13 + 11))
	qiFees := pyU(uint64(variant*17 + 19))
	primeTerminus := block
	half := new(big.Int).Add(new(big.Int).Div(new(big.Int).Set(quaiFees), pyU(2)), misc.QiToQuai(block, block.ExchangeRate(), block.Difficulty(), new(big.Int).Div(new(big.Int).Set(qiFees), pyU(2))))
	free := misc.CalculateQuaiReward(block.WorkObjectHeader(), block.Difficulty(), block.ExchangeRate())
	block.Header().SetAvgTxFees(hc.ComputeAverageTxFees(block, parent, half, free))
	block.Header().SetTotalFees(new(big.Int).Add(quaiFees, misc.QiToQuai(block, block.ExchangeRate(), block.Difficulty(), qiFees)))
	if variant == 14 {
		block.Header().SetAvgTxFees(new(big.Int).Add(block.AvgTxFees(), pyU(1)))
	}
	if variant == 15 {
		block.Header().SetTotalFees(new(big.Int).Add(block.TotalFees(), pyU(1)))
	}
	pySync(block)
	if prime >= params.KawPowForkBlock {
		pyAux(block, 1, 1)
	}
	emittedEtxs := []*types.Transaction{}
	prefix := variant % 4
	for index := 0; index < prefix; index++ {
		to := pyAddr(loc, false, false)
		emittedEtxs = append(emittedEtxs, types.NewTx(&types.ExternalTx{Gas: 21000, To: &to, Value: pyU(1), Sender: to, OriginatingTxHash: common.Hash{31: byte(index + 1)}}))
	}
	run := func() (types.Receipts, []*types.Transaction, []*types.Log, *state.StateDB, uint64, uint64, uint64, *multiset.MultiSet, []common.Unlock, error) {
		/* OFFICIAL_FEE_PAYOUT_BRANCH */
		return nil, nil, nil, nil, 0, 0, 0, nil, nil, nil
	}
	_, _, _, _, _, _, _, _, _, err := run()
	out := []string{}
	for _, tx := range emittedEtxs[prefix:] {
		p, e := tx.ProtoEncode()
		pyMust(e)
		b, e := proto.Marshal(p)
		pyMust(e)
		out = append(out, pyHex(b))
	}
	return map[string]interface{}{"id": fmt.Sprintf("payout-%d-%x-%d", prime, []byte(loc), variant), "prime": prime, "location": []uint8{loc[0], loc[1]}, "variant": variant, "records": records, "block": pyWire(block), "first_index": prefix, "quai_fees": pyHex(quaiFees.Bytes()), "qi_fees": pyHex(qiFees.Bytes()), "accepted": err == nil, "error": fmt.Sprint(err), "outbound": out}
}
func NativePayoutVectors() []interface{} {
	out := []interface{}{}
	for _, prime := range []uint64{1171499, 1171500, 1329999, 1330000, 1755000, 2236999, 2237000, 2444573, 2444574} {
		for _, loc := range []common.Location{{0, 0}, {2, 3}, {15, 15}} {
			for variant := 0; variant < 18; variant++ {
				out = append(out, pyOne(prime, loc, variant))
			}
		}
	}
	return out
}
