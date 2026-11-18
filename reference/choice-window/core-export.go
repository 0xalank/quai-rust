// Explicit controlled history through the unmodified token-choice controller.
package core

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus/misc"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"google.golang.org/protobuf/proto"
	"math/big"
)

func cwWire(w *types.WorkObject) string {
	p, e := w.ProtoEncode(types.BlockObject)
	if e != nil {
		panic(e)
	}
	b, e := proto.Marshal(p)
	if e != nil {
		panic(e)
	}
	return "0x" + hex.EncodeToString(b)
}
func cwTx(t *types.Transaction) string {
	p, e := t.ProtoEncode()
	if e != nil {
		panic(e)
	}
	b, e := proto.Marshal(p)
	if e != nil {
		panic(e)
	}
	return "0x" + hex.EncodeToString(b)
}
func cwDigest(set types.TokenChoiceSet) string {
	h := sha256.New()
	var row [48]byte
	for _, c := range set {
		binary.BigEndian.PutUint64(row[:8], c.Quai)
		binary.BigEndian.PutUint64(row[8:16], c.Qi)
		c.Diff.FillBytes(row[16:])
		h.Write(row[:])
	}
	return "0x" + hex.EncodeToString(h.Sum(nil))
}
func NativeChoiceWindowVectors() map[string]interface{} {
	db := rawdb.NewMemoryDatabase(log.Global)
	defer db.Close()
	cfg := *params.TestChainConfig
	cfg.Location = common.Location{}
	hc := &HeaderChain{config: &cfg, headerDb: db, bc: NewTestBodyDb(db), logger: log.Global}
	previous := types.NewTokenChoiceSet()
	previousKey := common.Hash{31: 55}
	seedKey := previousKey
	var fork types.TokenChoiceSet
	var forkKey common.Hash
	runs := []interface{}{}
	for branch := 0; branch < 2; branch++ {
		start, end := 0, 4208
		if branch == 1 {
			previous = fork
			previousKey = forkKey
			start = 4200
			end = 4208
		}
		rows := []interface{}{}
		for index := start; index < end; index++ {
			number := params.ControllerKickInBlock + uint64(index)
			parent := types.EmptyWorkObject(common.ZONE_CTX)
			parent.SetNumber(new(big.Int).SetUint64(number-1), common.PRIME_CTX)
			parent.WorkObjectHeader().SetNumber(new(big.Int).SetUint64(300000 + uint64(index)))
			parent.WorkObjectHeader().SetDifficulty(big.NewInt(17))
			parent.WorkObjectHeader().SetPrimeTerminusNumber(new(big.Int).SetUint64(number - 1))
			parent.WorkObjectHeader().SetLocation(common.Location{0, 0})
			pm := big.NewInt(int64(100000000 + index*997))
			parent.Header().SetMinerDifficulty(pm)
			parent.Header().SetExchangeRate(new(big.Int).Set(params.ExchangeRate))
			parent.WorkObjectHeader().SetHeaderHash(parent.Header().Hash())
			block := types.EmptyWorkObject(common.PRIME_CTX)
			block.SetNumber(new(big.Int).SetUint64(number), common.PRIME_CTX)
			block.Header().SetParentHash(previousKey, common.PRIME_CTX)
			block.Header().SetExtra([]byte{byte(branch)})
			miner := big.NewInt(int64(100000003 + index*991 + branch*701))
			if index < 4000 && index%997 == 0 {
				miner.SetUint64(0)
			}
			block.Header().SetMinerDifficulty(miner)
			block.WorkObjectHeader().SetHeaderHash(block.Header().Hash())
			qai := common.BytesToAddress([]byte{0, 0, 1}, common.Location{0, 0})
			qiBytes := make([]byte, 20)
			qiBytes[1] = 128
			qiBytes[19] = 2
			qi := common.BytesToAddress(qiBytes, common.Location{0, 0})
			recipient := qai
			if (index+branch)%2 == 0 {
				recipient = qi
			}
			txs := types.Transactions{types.NewTx(&types.ExternalTx{OriginatingTxHash: common.Hash{31: 1}, Gas: 100000, To: &recipient, Sender: qai, Value: big.NewInt(1), Data: []byte{}, AccessList: types.AccessList{}, EtxType: types.CoinbaseType})}
			chooseQi := (index+branch)%3 == 0
			recipient = qai
			reward := misc.CalculateQuaiConversionReward(parent, params.ExchangeRate, pm)
			if chooseQi {
				recipient = qi
				reward = misc.CalculateQiReward(parent.WorkObjectHeader(), pm)
			}
			count := new(big.Int).SetUint64(uint64(index%13 + branch + 1))
			if index%127 == 0 {
				count.Add(count, new(big.Int).Lsh(big.NewInt(1), 64))
			}
			value := new(big.Int).Mul(reward, count)
			txs = append(txs, types.NewTx(&types.ExternalTx{OriginatingTxHash: common.Hash{31: 2}, Gas: 100000, To: &recipient, Sender: qai, Value: value, Data: []byte{}, AccessList: types.AccessList{}, EtxType: types.ConversionType}))
			actual := big.NewInt(1000003)
			realized := new(big.Int).SetInt64([]int64{0, 333333, 999999, 1500001}[(index+branch)%4])
			if e := rawdb.WriteTokenChoicesSet(db, previousKey, &previous); e != nil {
				panic(e)
			}
			next, e := CalculateTokenChoicesSet(hc, block, parent, params.ExchangeRate, txs, actual, realized, miner)
			if e != nil {
				panic(e)
			}
			rate, e := CalculateBetaFromMiningChoiceAndConversions(hc, block, params.ExchangeRate, next)
			if e != nil {
				panic(e)
			}
			sum := big.NewInt(0)
			for _, c := range next {
				sum.Add(sum, c.Diff)
			}
			slot := number - params.ControllerKickInBlock - 1
			if index == 0 {
				slot = 0
			}
			if slot >= params.TokenChoiceSetSize {
				slot = params.TokenChoiceSetSize - 1
			}
			newest := next[slot]
			rows = append(rows, map[string]interface{}{"number": number, "block_key": block.Hash().Hex(), "parent_key": previousKey.Hex(), "parent": cwWire(parent), "etxs": []string{cwTx(txs[0]), cwTx(txs[1])}, "exchange": params.ExchangeRate.String(), "miner": miner.String(), "actual": actual.String(), "realized": realized.String(), "newest": map[string]interface{}{"quai": newest.Quai, "qi": newest.Qi, "difficulty": newest.Diff.String()}, "difficulty_sum": sum.String(), "window_sha256": cwDigest(next), "next_exchange": rate.String()})
			previous = next
			previousKey = block.Hash()
			if branch == 0 && index == 4199 {
				fork = next
				forkKey = previousKey
			}
			// Bound the offline database: retain the current predecessor only on the
			// next iteration. This happens after the unmodified calculation finishes.
			it := db.NewIterator(nil, nil)
			keys := [][]byte{}
			for it.Next() {
				keys = append(keys, append([]byte{}, it.Key()...))
			}
			it.Release()
			for _, k := range keys {
				if e := db.Delete(k); e != nil {
					panic(e)
				}
			}
		}
		runs = append(runs, map[string]interface{}{"branch": branch, "start_index": start, "rows": rows})
	}
	return map[string]interface{}{"seed_key": seedKey.Hex(), "seed_number": params.ControllerKickInBlock - 1, "runs": runs, "scope": "Controlled supplied pre-controller seed and ETX/history inputs; all 4000 token-choice rows match actual Go across warmup, rolling replacement and a competing suffix. No complete block, checkpoint or canonical admission authority."}
}
