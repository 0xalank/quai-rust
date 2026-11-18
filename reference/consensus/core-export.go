// Offline-only projections through actual core methods. Not a node or proof oracle.
package core

import (
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	lru "github.com/hashicorp/golang-lru/v2"
	"math/big"
)

// The arithmetic oracle supplies a hash. Real kernel proofs use separate vectors.
// This engine is never a Rust runtime mode and does not qualify a valid block.
type nativeProjectionEngine struct{}

func (nativeProjectionEngine) Seal(*types.WorkObject, chan<- *types.WorkObject, <-chan struct{}) error {
	return nil
}
func (nativeProjectionEngine) SetThreads(int) {}
func (nativeProjectionEngine) ComputePowHash(h *types.WorkObjectHeader) (common.Hash, error) {
	return h.MixHash(), nil
}
func (nativeProjectionEngine) ComputePowLight(h *types.WorkObjectHeader) (common.Hash, common.Hash) {
	return h.MixHash(), h.MixHash()
}
func nativeHC(context int) *HeaderChain {
	location := common.Location{}
	if context == 1 {
		location = common.Location{0}
	}
	if context == 2 {
		location = common.Location{0, 0}
	}
	config := *params.TestChainConfig
	config.Location = location
	db := rawdb.NewMemoryDatabase(log.Global)
	cache, _ := lru.New[common.Hash, types.WorkObject](8)
	orders, _ := lru.New[common.Hash, calcOrderResponse](8)
	return &HeaderChain{config: &config, powConfig: params.PowConfig{NodeLocation: location, DurationLimit: big.NewInt(5), MinDifficulty: big.NewInt(1)}, bc: &BodyDb{chainConfig: &config, db: db}, headerDb: db, headerCache: cache, calcOrderCache: orders, engine: []consensus.Engine{nativeProjectionEngine{}, nativeProjectionEngine{}}, logger: log.Global}
}
func NativeDifficultyVector(diff, min *big.Int, parentTime, grandTime, duration uint64) *big.Int {
	hc := nativeHC(2)
	defer hc.headerDb.Close()
	hc.powConfig.DurationLimit = new(big.Int).SetUint64(duration)
	hc.powConfig.MinDifficulty = new(big.Int).Set(min)
	grand := types.EmptyWorkObject(common.ZONE_CTX)
	grand.WorkObjectHeader().SetTime(grandTime)
	grand.WorkObjectHeader().SetDifficulty(big.NewInt(7))
	grand.SetNumber(big.NewInt(3), common.ZONE_CTX)
	rawdb.WriteTermini(hc.headerDb, grand.Hash(), types.EmptyTermini())
	hc.headerCache.Add(grand.Hash(), *grand)
	parent := types.EmptyWorkObject(common.ZONE_CTX)
	parent.WorkObjectHeader().SetDifficulty(new(big.Int).Set(diff))
	parent.WorkObjectHeader().SetTime(parentTime)
	parent.WorkObjectHeader().SetParentHash(grand.Hash())
	parent.SetNumber(big.NewInt(4), common.ZONE_CTX)
	return hc.CalcDifficulty(parent.WorkObjectHeader(), 0)
}
func NativeEntropyVector(hash common.Hash, diff *big.Int, exp uint8, pe, pd, pu [3]*big.Int, uncled *big.Int, shares int, context int) map[string]interface{} {
	hc := nativeHC(context)
	defer hc.headerDb.Close()
	wo := types.EmptyWorkObject(common.ZONE_CTX)
	wo.WorkObjectHeader().SetDifficulty(new(big.Int).Set(diff))
	wo.WorkObjectHeader().SetMixHash(hash)
	wo.Header().SetExpansionNumber(exp)
	for i := 0; i < 3; i++ {
		wo.SetNumber(big.NewInt(1), i)
		wo.Header().SetParentEntropy(new(big.Int).Set(pe[i]), i)
		wo.Header().SetParentDeltaEntropy(new(big.Int).Set(pd[i]), i)
		wo.Header().SetParentUncledDeltaEntropy(new(big.Int).Set(pu[i]), i)
	}
	wo.Header().SetUncledEntropy(new(big.Int).Set(uncled))
	if shares >= 0 {
		wo.WorkObjectHeader().SetPrimeTerminusNumber(new(big.Int).SetUint64(params.KawPowForkBlock))
		uncles := []*types.WorkObjectHeader{}
		for i := 0; i < shares; i++ {
			uncles = append(uncles, types.EmptyWorkObject(common.ZONE_CTX).WorkObjectHeader())
		}
		wo.Body().SetUncles(uncles)
	}
	wo.WorkObjectHeader().SetHeaderHash(wo.Header().Hash())
	if shares >= 0 {
		t := types.DefaultKawpowAuxTemplate()
		tx := types.NewAuxPowCoinbaseTx(types.Kawpow, t.Height(), t.CoinbaseOut(), wo.SealHash(), t.SignatureTime())
		mr := types.CalculateMerkleRoot(types.Kawpow, tx, t.MerkleBranch())
		donor := types.NewBlockHeader(types.Kawpow, int32(t.Version()), t.PrevHash(), mr, t.SignatureTime()+1, t.Bits(), 0, t.Height())
		wo.WorkObjectHeader().SetAuxPow(types.NewAuxPow(types.Kawpow, donor, t.AuxPow2(), t.Sigs(), t.MerkleBranch(), tx))
	}
	s, order, err := hc.CalcOrder(wo)
	if err != nil {
		return map[string]interface{}{"error": err.Error()}
	}
	var rank interface{}
	if order == common.PRIME_CTX {
		r, e := hc.CalcRank(wo)
		if e != nil {
			panic(e)
		}
		rank = r
	}
	ws, e := hc.WorkShareLogEntropy(wo)
	if e != nil {
		panic(e)
	}
	if context != common.ZONE_CTX {
		ws = big.NewInt(0)
	}
	return map[string]interface{}{"intrinsic": s.String(), "order": order, "rank": rank, "total": hc.TotalLogEntropy(wo).String(), "delta": hc.DeltaLogEntropy(wo).String(), "uncled_delta": hc.UncledDeltaLogEntropy(wo).String(), "workshares": ws.String()}
}
func NativePowID(prime uint64, auxID int, share bool) bool {
	hc := nativeHC(2)
	defer hc.headerDb.Close()
	h := types.EmptyWorkObject(common.ZONE_CTX).WorkObjectHeader()
	h.SetPrimeTerminusNumber(new(big.Int).SetUint64(prime))
	if auxID >= 0 {
		a := new(types.AuxPow)
		a.SetPowID(types.PowID(auxID))
		h.SetAuxPow(a)
	}
	if share {
		return hc.CheckPowIdValidityForWorkshare(h) == nil
	}
	return hc.CheckPowIdValidity(h) == nil
}
