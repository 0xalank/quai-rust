// Controlled inputs to unmodified official HeaderChain economic arithmetic.
package core

import (
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus/misc"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"math/big"
)

func NativeControllerVectors() map[string]interface{} {
	db := rawdb.NewMemoryDatabase(log.Global)
	defer db.Close()
	config := *params.TestChainConfig
	config.Location = common.Location{}
	hc := &HeaderChain{config: &config, headerDb: db, bc: NewTestBodyDb(db), logger: log.Global}
	flow := []interface{}{}
	difficulty := []interface{}{}
	discounts := []interface{}{}
	heights := []uint64{0, 261999, 262000, 262001, 262002, 265999, 266000, 266001, 1171500, 2444574}
	for i := 0; i < 640; i++ {
		previous := new(big.Int).Lsh(big.NewInt(int64(i+1)), uint(i%240))
		current := new(big.Int).Lsh(big.NewInt(int64(3*i+1)), uint((i*13)%240))
		if i%17 == 0 {
			previous.SetUint64(0)
		}
		if i%19 == 0 {
			current.SetUint64(0)
		}
		if i == 639 {
			previous.Sub(new(big.Int).Lsh(big.NewInt(1), 256), big.NewInt(1))
			current.Set(previous)
		}
		w := types.EmptyWorkObject(common.PRIME_CTX)
		w.Header().SetConversionFlowAmount(new(big.Int).Set(previous))
		flow = append(flow, map[string]interface{}{"previous": previous.String(), "current": current.String(), "expected": hc.ComputeConversionFlowAmount(w, current).String()})
		n := heights[i%len(heights)]
		w.SetNumber(new(big.Int).SetUint64(n), common.PRIME_CTX)
		w.WorkObjectHeader().SetDifficulty(new(big.Int).Set(current))
		w.Header().SetMinerDifficulty(new(big.Int).Set(previous))
		difficulty = append(difficulty, map[string]interface{}{"number": n, "previous": previous.String(), "difficulty": current.String(), "expected": hc.ComputeMinerDifficulty(w).String()})
		oldRate := new(big.Int).Lsh(big.NewInt(int64(i+3)), uint(i%220))
		rate := new(big.Int).Set(oldRate)
		switch i % 8 {
		case 0:
			rate.SetUint64(0)
		case 1:
			rate.Div(rate, big.NewInt(2))
		case 2:
			rate.Sub(rate, big.NewInt(1))
		case 4:
			rate.Add(rate, big.NewInt(1))
		case 5:
			rate.Div(rate.Mul(rate, big.NewInt(3)), big.NewInt(2))
		case 6:
			rate.Mul(rate, big.NewInt(2))
		case 7:
			rate.Mul(rate, big.NewInt(3))
		}
		prevDiscount := big.NewInt(int64((i * 7919) % 100001))
		w.Header().SetKQuaiDiscount(prevDiscount)
		if n > params.ControllerKickInBlock {
			old := types.EmptyWorkObject(common.PRIME_CTX)
			old.SetNumber(new(big.Int).SetUint64(n-params.MinerDifficultyWindow), common.PRIME_CTX)
			old.Header().SetExchangeRate(new(big.Int).Set(oldRate))
			old.WorkObjectHeader().SetHeaderHash(old.Header().Hash())
			rawdb.WriteCanonicalHash(db, old.Hash(), n-params.MinerDifficultyWindow)
			rawdb.WriteTermini(db, old.Hash(), types.EmptyTermini())
			hc.bc.blockCache.Add(old.Hash(), *old)
		}
		discounts = append(discounts, map[string]interface{}{"number": n, "previous": prevDiscount.String(), "old_exchange": oldRate.String(), "exchange": rate.String(), "expected": hc.ComputeKQuaiDiscount(w, rate).String()})
	}
	updates := []interface{}{}
	rates := []interface{}{}
	schedule := []uint64{265999, 266000, 266001, 751999, 752000, 752001, 771999, 772000, 1011199, 1011200, 1011201, 1031199, 1031200, 1171499, 1171500, 1171501, 1754999, 1755000, 1755001, 2236999, 2237000, 2237001, 2444574}
	for i := 0; i < 640; i++ {
		n := schedule[i%len(schedule)]
		base := new(big.Int).Lsh(big.NewInt(int64(i+4)), uint(i%180))
		miner := new(big.Int).Div(new(big.Int).Set(base), big.NewInt(2))
		if i%2 == 0 {
			miner.Mul(base, big.NewInt(2))
		}
		parent := new(big.Int).Lsh(new(big.Int).Add(params.ExchangeRate, big.NewInt(int64(i))), uint(i%120))
		xb := new(big.Int).Mul(new(big.Int).Set(base), big.NewInt(int64(i%5+1)))
		updates = append(updates, map[string]interface{}{"number": n, "parent": parent.String(), "miner": miner.String(), "xb_star": xb.String(), "expected": misc.CalculateKQuai(parent, miner, n, xb).String()})
		choices := types.NewTokenChoiceSet()
		for j := range choices {
			choices[j].Diff = new(big.Int).Set(base)
		}
		last := new(big.Int).Add(base, big.NewInt(17))
		choices[len(choices)-1].Diff = last
		block := types.EmptyWorkObject(common.PRIME_CTX)
		block.SetNumber(new(big.Int).SetUint64(n), common.PRIME_CTX)
		block.Header().SetMinerDifficulty(miner)
		rate, err := CalculateBetaFromMiningChoiceAndConversions(hc, block, parent, choices)
		if err != nil {
			panic(err)
		}
		rates = append(rates, map[string]interface{}{"number": n, "parent": parent.String(), "miner": miner.String(), "choice_base": base.String(), "choice_last": last.String(), "expected": rate.String()})
	}
	return map[string]interface{}{"flow": flow, "miner_difficulty": difficulty, "discounts": discounts, "updates": updates, "exchange_rates": rates}
}
