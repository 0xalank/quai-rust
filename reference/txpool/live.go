// Live check of the transaction pool (scripts/check_txpool_live.py): the
// head of a replayed mainnet state, rebuilt from the header fields the pool
// reads (number, base fee, gas limit, difficulty, prime terminus number,
// expansion, average fees; the prime terminus's exchange rate and eligible
// slices) and the UTXOs the test transactions spend, with the empty state
// of keys that never held funds. The production pool and RPC answer each
// transaction; the script submits the same bytes to the Rust node and
// compares. Keys come from fixed seeds; no transaction leaves this process.
package main

import (
	"encoding/json"
	"math/big"
	"os"

	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/params"
)

type liveParams struct {
	Number      uint64      `json:"number"`
	BaseFee     string      `json:"base_fee"`
	GasLimit    uint64      `json:"gas_limit"`
	Difficulty  string      `json:"difficulty"`
	Prime       uint64      `json:"prime_terminus_number"`
	Expansion   uint8       `json:"expansion_number"`
	AvgFees     string      `json:"avg_tx_fees"`
	Exchange    string      `json:"exchange_rate"`
	Eligible    string      `json:"etx_eligible_slices"`
	UtxoSetSize uint64      `json:"utxo_set_size"`
	Utxos       []liveUtxo  `json:"utxos"`
}
type liveUtxo struct {
	Hash         string `json:"hash"`
	Index        uint16 `json:"index"`
	Denomination uint8  `json:"denomination"`
	Address      string `json:"address"`
	Lock         string `json:"lock"`
}

func bigHex(s string) *big.Int {
	v, ok := new(big.Int).SetString(s[2:], 16)
	if !ok {
		v = new(big.Int)
	}
	return v
}

func live(path string) {
	data, err := os.ReadFile(path)
	must(err)
	var p liveParams
	must(json.Unmarshal(data, &p))
	e := newEnv("live")
	root := e.commit("s1", nil)
	h := head{number: p.Number, prime: p.Prime, time: 1700000000, difficulty: bigHex(p.Difficulty), baseFee: bigHex(p.BaseFee), gasLimit: p.GasLimit, exchange: params.ExchangeRate, avgFees: bigHex(p.AvgFees), eligible: common.HexToHash(p.Eligible)}
	ph := e.prime("prime", h, bigHex(p.Exchange), common.HexToHash(p.Eligible), root)
	wo := makeBlock(h, common.Hash{0x01}, ph, root, nil)
	wo.Header().SetExpansionNumber(p.Expansion)
	wo.WorkObjectHeader().SetHeaderHash(wo.Header().Hash())
	e.block("h1", wo, "s1", p.UtxoSetSize)
	var us []utxo
	for _, x := range p.Utxos {
		var lock *big.Int
		if x.Lock != "0x" && x.Lock != "" {
			lock = bigHex(x.Lock)
		}
		us = append(us, utxo{types.OutPoint{TxHash: common.HexToHash(x.Hash), Index: x.Index}, &types.UtxoEntry{Denomination: x.Denomination, Address: common.FromHex(x.Address), Lock: lock}})
	}
	e.utxos("live", us)
	a, b := quaiKey("live/a"), quaiKey("live/b")
	q1, q2 := qiKey("live/q1"), qiKey("live/q2")
	far := zoneKey("live/far", 0x01)
	e.start("h1", "live", core.DefaultTxPoolConfig, a, b, q1, far)
	fee := h.baseFee
	low := new(big.Int).Sub(fee, u(1))
	if low.Sign() < 0 {
		low = u(0)
	}
	pay := func(k key, t qtx) []byte {
		if t.to == nil {
			t.to = addr(b)
		}
		return encode(signQuai(k, t))
	}
	e.send(pay(a, qtx{nonce: 0, price: fee, gas: 21000}), "base fee, no value, empty account")
	e.send(pay(a, qtx{nonce: 0, price: fee, gas: 21000, value: u(1)}), "one wei, empty account")
	e.send(pay(a, qtx{nonce: 1, price: low, gas: 21000}), "one below the base fee")
	e.send(pay(a, qtx{nonce: 1, price: fee, gas: p.GasLimit + 1}), "gas above the head's limit")
	e.send(pay(a, qtx{nonce: 1, price: fee, gas: 20000}), "gas below 21000")
	e.send(pay(a, qtx{nonce: 7, price: fee, gas: 21000}), "nonce 7")
	e.send(pay(a, qtx{nonce: 1, price: fee, gas: 21000, chainID: u(1)}), "chain ID 1")
	e.send(pay(a, qtx{nonce: 1, price: quai(1), gas: 10001}), "fee above the cap")
	e.send(pay(a, qtx{nonce: 1, price: fee, gas: 30000000, data: make([]byte, 131072)}), "oversized")
	e.send(pay(far, qtx{nonce: 0, price: fee, gas: 21000}), "sender in zone [0 1]")
	e.send(pay(q1, qtx{nonce: 0, price: fee, gas: 21000}), "sender in the Qi ledger")
	unsigned := protoOf(pay(a, qtx{nonce: 2, price: fee, gas: 21000}))
	unsigned.V, unsigned.R, unsigned.S = []byte{}, []byte{}, []byte{}
	e.send(marshal(unsigned), "unsigned")
	e.send([]byte{0xff}, "not a protobuf")
	etx := types.NewTx(&types.ExternalTx{OriginatingTxHash: common.Hash{0x0a}, Gas: 21000, To: addr(b), Value: u(1), Data: []byte{}, AccessList: types.AccessList{}, Sender: a.addr})
	e.send(encode(etx), "external transaction")
	missing := types.OutPoint{TxHash: common.HexToHash("0x00aa"), Index: 0}
	e.send(encode(signQi([]key{q1}, []types.OutPoint{missing}, []out{qo(q2, 0)}, nil, nil)), "Qi: missing UTXO")
	e.send(encode(signQi([]key{q1}, []types.OutPoint{missing}, []out{qo(far, 0)}, nil, nil)), "Qi: to zone [0 1]")
	e.send(encode(signQi([]key{q1}, []types.OutPoint{missing}, []out{qo(q2, 0)}, []byte{1, 2, 3}, nil)), "Qi: 3 bytes of data")
	for _, x := range us {
		e.send(encode(signQi([]key{q1}, []types.OutPoint{x.point}, []out{qo(q2, 0)}, nil, nil)), "Qi: another owner's UTXO")
	}
	noSig := protoOf(encode(signQi([]key{q1}, []types.OutPoint{missing}, []out{qo(q2, 0)}, nil, nil)))
	noSig.Signature = nil
	e.send(marshal(noSig), "Qi: no signature")
	e.run()
	for _, k := range []key{a, b, q1, far} {
		e.rpc("quai_getTransactionCount", k.addr.Hex(), "pending")
	}
	e.rpc("quai_getTransactionByHash", hashOf(pay(a, qtx{nonce: 0, price: fee, gas: 21000})).Hex())
	e.rpc("txpool_status")
	e.rpc("txpool_content")
	e.rpc("txpool_contentFrom", a.addr.Hex())
	e.stop()
	must(json.NewEncoder(os.Stdout).Encode(obj{"reference_revision": referenceRevision, "params": p, "steps": e.steps}))
}
