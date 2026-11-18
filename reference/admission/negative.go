// Offline negative cases keep native work valid while changing admitted fields.
package core

import (
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/core/types"
	"math/big"
)

func admissionNegatives(hc *HeaderChain, v *BlockValidator, engine consensus.Engine, good, parent *types.WorkObject, processor *StateProcessor, now int64) []interface{} {
	tests := []struct {
		name   string
		mutate func(*types.WorkObject)
	}{
		{"extra", func(b *types.WorkObject) { b.Header().SetExtra(make([]byte, 33)) }},
		{"older_time", func(b *types.WorkObject) { b.WorkObjectHeader().SetTime(parent.Time() - 1) }},
		{"future_time", func(b *types.WorkObject) { b.WorkObjectHeader().SetTime(uint64(now + 16)) }},
		{"difficulty", func(b *types.WorkObject) { b.WorkObjectHeader().SetDifficulty(big.NewInt(3)) }},
		{"gas_limit", func(b *types.WorkObject) { b.Header().SetGasLimit(1) }},
		{"gas_used", func(b *types.WorkObject) { b.Header().SetGasUsed(1) }},
		{"state_limit", func(b *types.WorkObject) { b.Header().SetStateLimit(1) }},
		{"state_used", func(b *types.WorkObject) { b.Header().SetStateUsed(1) }},
		{"number", func(b *types.WorkObject) { b.SetNumber(ebU(b.NumberU64(2)+1), 2) }},
		{"parent_entropy", func(b *types.WorkObject) {
			b.Header().SetParentEntropy(new(big.Int).Add(b.ParentEntropy(2), big.NewInt(1)), 2)
		}},
		{"parent_delta", func(b *types.WorkObject) {
			b.Header().SetParentDeltaEntropy(new(big.Int).Add(b.ParentDeltaEntropy(2), big.NewInt(1)), 2)
		}},
		{"parent_uncled_delta", func(b *types.WorkObject) { b.Header().SetParentUncledDeltaEntropy(big.NewInt(1), 2) }},
		{"expansion", func(b *types.WorkObject) { b.Header().SetExpansionNumber(1) }},
		{"base_fee", func(b *types.WorkObject) { b.Header().SetBaseFee(new(big.Int).Add(b.BaseFee(), big.NewInt(1))) }},
		{"prime_hash", func(b *types.WorkObject) { b.Header().SetPrimeTerminusHash(common.HexToHash("0x1234")) }},
		{"prime_number", func(b *types.WorkObject) { b.WorkObjectHeader().SetPrimeTerminusNumber(big.NewInt(1)) }},
		{"manifest", func(b *types.WorkObject) { b.Header().SetManifestHash(common.HexToHash("0x1234"), 2) }},
		{"coinbase_scope", func(b *types.WorkObject) {
			b.WorkObjectHeader().SetPrimaryCoinbase(common.BytesToAddress(append([]byte{0x10}, make([]byte, 19)...), common.Location{0, 0}))
		}},
		{"coinbase_qi", func(b *types.WorkObject) {
			b.WorkObjectHeader().SetPrimaryCoinbase(common.BytesToAddress(append([]byte{0, 0x80}, make([]byte, 18)...), common.Location{0, 0}))
		}},
		{"lock", func(b *types.WorkObject) { b.WorkObjectHeader().SetLock(1) }},
		{"empty_data", func(b *types.WorkObject) { b.WorkObjectHeader().SetData([]byte{}) }},
		{"evm_root", func(b *types.WorkObject) { b.Header().SetEVMRoot(common.HexToHash("0x1234")) }},
		{"etx_root", func(b *types.WorkObject) { b.Header().SetEtxSetRoot(common.HexToHash("0x1234")) }},
		{"utxo_root", func(b *types.WorkObject) { b.Header().SetUTXORoot(common.HexToHash("0x1234")) }},
		{"receipt_root", func(b *types.WorkObject) { b.Header().SetReceiptHash(common.HexToHash("0x1234")) }},
		{"outbound_root", func(b *types.WorkObject) { b.Header().SetOutboundEtxHash(common.HexToHash("0x1234")) }},
		{"account_count", func(b *types.WorkObject) { b.Header().SetQuaiStateSize(big.NewInt(2)) }},
		{"uncled_entropy", func(b *types.WorkObject) { b.Header().SetUncledEntropy(big.NewInt(1)) }},
		{"average_fees", func(b *types.WorkObject) { b.Header().SetAvgTxFees(big.NewInt(1)) }},
		{"total_fees", func(b *types.WorkObject) { b.Header().SetTotalFees(big.NewInt(1)) }},
		{"unknown_parent", func(b *types.WorkObject) { b.SetParentHash(common.HexToHash("0x1234"), 2) }},
	}
	rows := []interface{}{}
	for i, tc := range tests {
		b := types.CopyWorkObject(good)
		tc.mutate(b)
		b.WorkObjectHeader().SetHeaderHash(b.Header().Hash())
		var work common.Hash
		found := false
		for nonce := uint64(1000 + i*1000); nonce < uint64(2000+i*1000); nonce++ {
			b.WorkObjectHeader().SetNonce(types.EncodeNonce(nonce))
			mix, w := engine.ComputePowLight(b.WorkObjectHeader())
			b.WorkObjectHeader().SetMixHash(mix)
			target := new(big.Int).Div(common.Big2e256, b.Difficulty())
			if w[0] >= 64 && w[0] < 128 && w.Big().Cmp(target) <= 0 {
				work = w
				found = true
				break
			}
		}
		if !found {
			panic("negative work search exhausted")
		}
		wire, e := b.ProtoEncode(types.BlockObject)
		ebMust(e)
		decoded := &types.WorkObject{}
		ebMust(decoded.ProtoDecode(wire, common.Location{0, 0}, types.BlockObject))
		b = decoded
		err := hc.verifyHeader(b, parent, false, now)
		stage := "header"
		if err == nil {
			err = hc.AppendHeader(b)
			stage = "append"
		}
		if err == nil {
			err = v.ValidateBody(b)
			stage = "body"
		}
		if err == nil {
			batch := hc.headerDb.NewBatch()
			rs, etxs, _, db, gas, used, _, set, _, e := processor.Process(b, batch)
			err = e
			stage = "execution"
			if err == nil {
				err = v.ValidateState(b, db, rs, etxs, set, gas, used)
				stage = "state"
			}
		}
		if err == nil {
			panic("negative case accepted: " + tc.name)
		}
		rows = append(rows, map[string]interface{}{"id": tc.name, "parent_hash": parent.Hash().Hex(), "work_object": ebWire(b), "work_hash": work.Hex(), "accepted": false, "rejected_stage": stage, "error": err.Error()})
	}
	return rows
}
