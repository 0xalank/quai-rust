// Scenarios of the transaction pool oracle (main.go).
package main

import (
	"math/big"
	"time"

	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus/misc"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/params"
)

// Fixed-seed keys: Quai senders of zone [0 0], Qi owners, a Quai-ledger
// key in zone [0 1] and a Qi-ledger key used as a Quai sender.
var (
	alice   = quaiKey("alice")
	bob     = quaiKey("bob")
	carol   = quaiKey("carol")
	dave    = quaiKey("dave")
	erin    = quaiKey("erin")
	frank   = quaiKey("frank")
	grace   = quaiKey("grace")
	heidi   = quaiKey("heidi")
	ivan    = quaiKey("ivan")
	judy    = quaiKey("judy")
	other   = zoneKey("other-zone", 0x01)
	qiSend  = qiKey("qi-sender")
	k1      = qiKey("k1")
	k2      = qiKey("k2")
	k3      = qiKey("k3")
	k4      = qiKey("k4")
	k5      = qiKey("k5")
	k6      = qiKey("k6")
	k7      = qiKey("k7")
	k8      = qiKey("k8")
	k9      = qiKey("k9")
	region1 = grind("region-1", func(a []byte) bool { return a[0] == 0x10 && a[1] >= 0x80 })
	region2 = grind("region-2", func(a []byte) bool { return a[0] == 0x20 && a[1] >= 0x80 })
	zone01  = grind("zone-0-1", func(a []byte) bool { return a[0] == 0x01 && a[1] >= 0x80 })
)

func addr(k key) *common.Address { a := k.addr; return &a }

// zoneOnly marks only zone [0 0] eligible for ETXs; withRegion1 adds [1 0].
var zoneOnly, withRegion1 common.Hash

func init() {
	zoneOnly[0] = 1
	withRegion1 = zoneOnly
	withRegion1[2] = 1
}

// spec is a zone head: pre-KawPow (prime terminus below 1,171,500) heads
// have distinct hashes; later heads hash their (absent) AuxPow.
func spec(number, prime uint64) head {
	return head{number: number, prime: prime, time: 1700000000 + number*5, difficulty: u(400000000000), baseFee: gwei(1), gasLimit: 30000000, exchange: params.ExchangeRate, avgFees: u(0), eligible: zoneOnly}
}
func polaris(number uint64) head {
	h := spec(number, 2500000)
	h.shaDiff = new(big.Int).Mul(u(167000), u(1000000000000))
	h.shaCount = u(1 << 31)
	h.scryptCount = u(1 << 30)
	h.avgFees = u(3000000000000000)
	return h
}

// world builds a prime terminus block and returns its hash.
func (e *env) prime(name string, h head, rate *big.Int, eligible common.Hash, root common.Hash) common.Hash {
	p := h
	p.number = h.number - 10
	// A pre-KawPow prime terminus number gives the block its own hash; the
	// pool reads only its exchange rate and eligible slices.
	p.prime = 100
	p.exchange = rate
	p.eligible = eligible
	return e.block(name, makeBlock(p, common.Hash{0x0f}, common.Hash{}, root, nil), "", 0)
}

func rich() *big.Int { return quai(1000) }

func quaiScenarios() []func() obj {
	return []func() obj{quaiValidate, quaiNonceGaps, quaiReplace, quaiAccountQueue, quaiGlobalSlots, quaiGlobalQueue(4), quaiGlobalQueue(5), quaiPoolFull, quaiPoolFullLocals, quaiLocals, quaiLocalsConfig, quaiNewHead, quaiReorg, quaiReorgDeep, quaiExpiry}
}

// quaiWorld: one head at zone 1000 with the given accounts.
func quaiWorld(id string, accounts []account, h head) *env {
	e := newEnv(id)
	root := e.commit("s1", accounts)
	ph := e.prime("prime", h, params.ExchangeRate, zoneOnly, root)
	e.block("h1", makeBlock(h, common.Hash{0x01}, ph, root, nil), "s1", 0)
	e.utxos("none", nil)
	return e
}
func payment(k key, nonce uint64, price *big.Int) []byte {
	return encode(signQuai(k, qtx{nonce: nonce, to: addr(bob), value: u(1), price: price, gas: 21000}))
}

func quaiValidate() obj {
	poor := new(big.Int).Add(new(big.Int).Mul(gwei(1), u(21000)), u(1))
	e := quaiWorld("quai-validate", []account{{alice, 0, rich()}, {bob, 0, rich()}, {dave, 0, poor}, {erin, 5, rich()}, {frank, 0, u(0)}}, spec(1000, 50000))
	e.start("h1", "none", core.DefaultTxPoolConfig, alice, bob, dave, erin, frank, qiSend, other)
	t1 := payment(alice, 0, gwei(1))
	e.send(t1, "valid: queued until the next run")
	e.send(t1, "the same transaction again")
	e.run()
	e.send(encode(signQuai(alice, qtx{nonce: 0, to: addr(bob), value: u(2), price: gwei(1), gas: 21000})), "same nonce, same price")
	e.send(payment(erin, 4, gwei(1)), "nonce below the state nonce 5")
	e.send(payment(frank, 0, gwei(1)), "no balance")
	e.send(encode(signQuai(dave, qtx{nonce: 0, to: addr(bob), value: u(2), price: gwei(1), gas: 21000})), "cost one wei above the balance")
	e.send(encode(signQuai(dave, qtx{nonce: 0, to: addr(bob), value: u(1), price: gwei(1), gas: 21000})), "cost equal to the balance")
	e.send(payment(alice, 1, new(big.Int).Sub(gwei(1), u(1))), "gas price one below the base fee")
	e.send(encode(signQuai(alice, qtx{nonce: 1, to: addr(bob), price: gwei(1), gas: 30000001})), "gas above the block gas limit")
	e.send(encode(signQuai(alice, qtx{nonce: 1, to: addr(bob), price: gwei(1), gas: 20999})), "gas below 21000")
	e.send(encode(signQuai(alice, qtx{nonce: 1, price: gwei(1), gas: 52999, data: []byte{0x60, 0x00}})), "creation below 53000 plus data")
	e.send(encode(signQuai(alice, qtx{nonce: 1, price: gwei(1), gas: 53000 + 16 + 4, data: []byte{0x60, 0x00}})), "creation with exact intrinsic gas")
	data := []byte{0x00, 0x01, 0x00, 0x02}
	e.send(encode(signQuai(alice, qtx{nonce: 2, to: addr(bob), price: gwei(1), gas: 21000 + 39, data: data})), "data gas one short")
	e.send(encode(signQuai(alice, qtx{nonce: 2, to: addr(bob), price: gwei(1), gas: 21000 + 40, data: data})), "data gas exact")
	al := types.AccessList{{Address: bob.addr, StorageKeys: []common.Hash{{1}, {2}}}, {Address: carol.addr, StorageKeys: []common.Hash{{3}}}}
	e.send(encode(signQuai(alice, qtx{nonce: 3, to: addr(bob), price: gwei(1), gas: 31499, access: al})), "access list gas one short")
	e.send(encode(signQuai(alice, qtx{nonce: 3, to: addr(bob), price: gwei(1), gas: 31500, access: al})), "access list gas exact")
	e.send(encode(signQuai(alice, qtx{nonce: 4, to: addr(bob), price: gwei(1), gas: 30000000, data: make([]byte, 131072)})), "oversized data")
	// The data length whose transaction is exactly txMaxSize (131,072 bytes).
	limit := 130900
	for {
		tx := signQuai(alice, qtx{nonce: 4, to: addr(bob), price: gwei(1), gas: 21000 + 4*uint64(limit+1), data: make([]byte, limit+1)})
		if tx.Size() > 131072 {
			break
		}
		limit++
	}
	e.send(encode(signQuai(alice, qtx{nonce: 4, to: addr(bob), price: gwei(1), gas: 21000 + 4*uint64(limit+1), data: make([]byte, limit+1)})), "one byte above the size limit")
	e.send(encode(signQuai(alice, qtx{nonce: 4, to: addr(bob), price: gwei(1), gas: 21000 + 4*uint64(limit), data: make([]byte, limit)})), "exactly the size limit")
	e.send(encode(signQuai(alice, qtx{nonce: 5, to: addr(bob), price: quai(1), gas: 10001})), "fee 10001 QUAI above the cap")
	e.send(encode(signQuai(alice, qtx{nonce: 5, to: addr(bob), price: u(333333333333333333), gas: 30001})), "fee 10000.33 QUAI above the cap")
	e.send(encode(signQuai(alice, qtx{nonce: 5, to: addr(bob), price: quai(1), gas: 10000})), "fee exactly the cap")
	huge := new(big.Int).Add(new(big.Int).Lsh(u(1), 256), u(1))
	e.send(encode(signQuai(alice, qtx{nonce: 5, to: addr(bob), price: huge, gas: 0})), "gas price above 2^256, no gas")
	e.send(encode(signQuai(bob, qtx{nonce: 0, to: addr(alice), value: u(1), price: gwei(1), gas: 21000, chainID: u(1)})), "chain ID 1, funded")
	e.send(encode(signQuai(frank, qtx{nonce: 0, to: addr(alice), value: u(1), price: gwei(1), gas: 21000, chainID: u(1)})), "chain ID 1, no balance")
	e.send(payment(other, 0, gwei(1)), "sender in zone [0 1]")
	e.send(payment(qiSend, 0, gwei(1)), "sender in the Qi ledger")
	// Signature values.
	base := protoOf(payment(bob, 0, gwei(1)))
	mutate := func(f func(pt *types.ProtoTransaction)) []byte {
		pt := protoOf(marshal(base))
		f(pt)
		return marshal(pt)
	}
	e.send(mutate(func(pt *types.ProtoTransaction) { pt.R = []byte{} }), "R zero")
	n := new(big.Int).SetBytes(common.FromHex("0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141"))
	e.send(mutate(func(pt *types.ProtoTransaction) { pt.S = new(big.Int).Sub(n, new(big.Int).SetBytes(pt.S)).Bytes() }), "high S")
	e.send(mutate(func(pt *types.ProtoTransaction) { pt.V = []byte{2} }), "V 2")
	e.send(mutate(func(pt *types.ProtoTransaction) { pt.V = []byte{1, 0} }), "V 256")
	e.send(mutate(func(pt *types.ProtoTransaction) { pt.V, pt.R, pt.S = []byte{}, []byte{}, []byte{} }), "unsigned")
	for r := uint64(1); ; r++ {
		raw := mutate(func(pt *types.ProtoTransaction) { pt.R = u(r).Bytes() })
		if _, _, ok := sender(raw); !ok {
			e.send(raw, "an R that recovers no key")
			break
		}
	}
	e.send(mutate(func(pt *types.ProtoTransaction) { pt.Value = append([]byte{0}, pt.Value...) }), "value with a leading zero byte: the same transaction")
	etx := types.NewTx(&types.ExternalTx{OriginatingTxHash: common.Hash{0x0a}, ETXIndex: 1, Gas: 21000, To: addr(bob), Value: u(5), Data: []byte{}, AccessList: types.AccessList{}, Sender: alice.addr, EtxType: 0})
	e.send(encode(etx), "external transaction")
	// Decoding.
	fields := []struct {
		note string
		f    func(pt *types.ProtoTransaction)
	}{
		{"no type", func(pt *types.ProtoTransaction) { pt.Type = nil }},
		{"no nonce", func(pt *types.ProtoTransaction) { pt.Nonce = nil }},
		{"no gas", func(pt *types.ProtoTransaction) { pt.Gas = nil }},
		{"no access list", func(pt *types.ProtoTransaction) { pt.AccessList = nil }},
		{"no value", func(pt *types.ProtoTransaction) { pt.Value = nil }},
		{"no gas price", func(pt *types.ProtoTransaction) { pt.GasPrice = nil }},
		{"no data", func(pt *types.ProtoTransaction) { pt.Data = nil }},
		{"no chain ID", func(pt *types.ProtoTransaction) { pt.ChainId = nil }},
		{"no V", func(pt *types.ProtoTransaction) { pt.V = nil }},
		{"no R", func(pt *types.ProtoTransaction) { pt.R = nil }},
		{"no S", func(pt *types.ProtoTransaction) { pt.S = nil }},
		{"type 7", func(pt *types.ProtoTransaction) { t := uint64(7); pt.Type = &t }},
		{"no recipient: a creation with the original signature", func(pt *types.ProtoTransaction) { pt.To = nil }},
		{"a 21-byte recipient", func(pt *types.ProtoTransaction) { pt.To = append([]byte{0x77}, pt.To...) }},
	}
	for _, f := range fields {
		e.send(mutate(f.f), f.note)
	}
	ebase := protoOf(encode(etx))
	for _, f := range []struct {
		note string
		f    func(pt *types.ProtoTransaction)
	}{
		{"ETX without recipient", func(pt *types.ProtoTransaction) { pt.To = nil }},
		{"ETX without originating hash", func(pt *types.ProtoTransaction) { pt.OriginatingTxHash = nil }},
		{"ETX without type", func(pt *types.ProtoTransaction) { pt.EtxType = nil }},
	} {
		pt := protoOf(marshal(ebase))
		f.f(pt)
		e.send(marshal(pt), f.note)
	}
	e.send([]byte{0xff}, "one 0xff byte")
	e.send([]byte{0x08}, "a truncated varint field")
	e.send([]byte{0x0a, 0x05, 0x01}, "a truncated length-delimited field")
	e.send([]byte{}, "empty input")
	// protobuf-go keeps unknown and mistyped fields as unknown bytes.
	e.send(append(payment(bob, 1, gwei(1)), 0x98, 0x01, 0x05), "field 19 (parent hash) as a varint: unknown to Go")
	fieldTag := uint64(1<<29+1) << 3
	tagBytes := []byte{}
	for ; fieldTag >= 0x80; fieldTag >>= 7 {
		tagBytes = append(tagBytes, byte(fieldTag)|0x80)
	}
	tagBytes = append(tagBytes, byte(fieldTag), 0x01)
	e.send(append(payment(bob, 2, gwei(1)), tagBytes...), "field number 2^29+1")
	e.send(append(payment(bob, 3, gwei(1)), 0xf3, 0x01, 0x08, 0x01, 0xf4, 0x01), "an unknown group")
	e.send([]byte{0x0a, 0x00}, "the type field as bytes")
	e.send(append(payment(bob, 4, gwei(1)), 0x0c), "a stray end-group tag")
	e.sendText("0xzz", "not hex")
	e.sendText("1234", "no 0x prefix")
	e.sendText("0x123", "odd length")
	e.sendText(7, "a number")
	e.receive(payment(bob, 0, gwei(1)), "quai_receiveTxFromPoolSharingClient: valid")
	e.receive([]byte{0xff}, "quai_receiveTxFromPoolSharingClient: malformed")
	e.run()
	// Reads.
	for _, k := range []key{alice, bob, dave, erin, frank, qiSend, other} {
		e.rpc("quai_getTransactionCount", k.addr.Hex(), "pending")
	}
	e.rpc("quai_getTransactionByHash", common.Hash{0xde, 0xad}.Hex())
	tx1 := new(types.Transaction)
	must(tx1.ProtoDecode(protoOf(t1), loc))
	e.rpc("quai_getTransactionByHash", tx1.Hash().Hex())
	e.rpc("quai_getRawTransactionByHash", tx1.Hash().Hex())
	e.rpc("txpool_status")
	e.rpc("txpool_content")
	e.rpc("txpool_inspect")
	e.rpc("txpool_contentFrom", alice.addr.Hex())
	e.rpc("txpool_contentFrom", qiSend.addr.Hex())
	e.stop()
	return e.out()
}

func quaiNonceGaps() obj {
	e := quaiWorld("quai-nonce-gaps", []account{{alice, 0, rich()}, {bob, 3, rich()}}, spec(1000, 50000))
	e.start("h1", "none", core.DefaultTxPoolConfig, alice, bob)
	nonce := func(k key) { e.rpc("quai_getTransactionCount", k.addr.Hex(), "pending") }
	e.send(payment(alice, 0, gwei(1)), "")
	nonce(alice)
	e.run()
	nonce(alice)
	e.send(payment(alice, 2, gwei(1)), "gap at 1")
	e.run()
	nonce(alice)
	e.send(payment(alice, 3, gwei(1)), "")
	e.run()
	e.send(payment(alice, 1, gwei(1)), "fills the gap")
	nonce(alice)
	e.run()
	nonce(alice)
	e.send(payment(bob, 5, gwei(1)), "state nonce 3")
	e.run()
	nonce(bob)
	e.send(payment(bob, 3, gwei(2)), "")
	e.send(payment(bob, 4, gwei(3)), "")
	e.run()
	nonce(bob)
	e.rpc("txpool_status")
	e.rpc("txpool_content")
	e.rpc("txpool_inspect")
	e.rpc("txpool_contentFrom", bob.addr.Hex())
	e.stop()
	return e.out()
}

func quaiReplace() obj {
	h := spec(1000, 50000)
	h.baseFee = u(10)
	e := quaiWorld("quai-replace", []account{{alice, 0, rich()}, {bob, 0, rich()}, {carol, 0, u(21000*100 + 1)}}, h)
	e.start("h1", "none", core.DefaultTxPoolConfig, alice, bob, carol)
	e.send(payment(alice, 0, u(100)), "")
	e.run()
	e.send(payment(alice, 0, u(104)), "4% more")
	e.send(payment(alice, 0, u(105)), "5% more: replaces the pending transaction")
	e.send(encode(signQuai(alice, qtx{nonce: 0, to: addr(bob), value: u(7), price: u(105), gas: 21000})), "same price, other value")
	e.send(payment(alice, 0, u(90)), "lower price")
	e.run()
	e.send(payment(alice, 5, u(1000)), "queued behind a gap")
	e.send(payment(alice, 5, u(1049)), "4.9% more in the queue")
	e.send(payment(alice, 5, u(1050)), "5% more replaces the queued one")
	e.run()
	e.send(payment(bob, 0, u(19)), "")
	e.run()
	e.send(encode(signQuai(bob, qtx{nonce: 0, to: addr(alice), value: u(1), price: u(19), gas: 21000})), "19 again")
	e.send(payment(bob, 0, u(20)), "20: 19 * 105 / 100 rounds down to 19")
	e.send(payment(carol, 0, u(100)), "spends the whole balance")
	e.run()
	e.send(payment(carol, 0, u(106)), "a replacement that the balance cannot pay")
	e.rpc("quai_getTransactionByHash", hashOf(payment(alice, 0, u(100))).Hex())
	e.rpc("quai_getTransactionByHash", hashOf(payment(alice, 0, u(105))).Hex())
	e.rpc("txpool_content")
	e.stop()
	return e.out()
}
func hashOf(raw []byte) common.Hash {
	tx := new(types.Transaction)
	must(tx.ProtoDecode(protoOf(raw), loc))
	return tx.Hash()
}

func quaiAccountQueue() obj {
	e := quaiWorld("quai-account-queue", []account{{alice, 0, rich()}}, spec(1000, 50000))
	conf := core.DefaultTxPoolConfig
	conf.AccountQueue = 3
	e.start("h1", "none", conf, alice)
	for n := uint64(2); n <= 6; n++ {
		e.send(payment(alice, n, gwei(1)), "queued behind a gap")
	}
	e.run()
	e.send(payment(alice, 0, gwei(1)), "")
	e.run()
	e.send(payment(alice, 1, gwei(1)), "fills the gap")
	e.run()
	e.stop()
	return e.out()
}

func quaiGlobalSlots() obj {
	e := quaiWorld("quai-global-slots", []account{{alice, 0, rich()}, {bob, 0, rich()}, {carol, 0, rich()}}, spec(1000, 50000))
	conf := core.DefaultTxPoolConfig
	conf.GlobalSlots = 4
	conf.AccountSlots = 1
	e.start("h1", "none", conf, alice, bob, carol)
	for n := uint64(0); n < 3; n++ {
		e.send(payment(alice, n, gwei(1)), "")
	}
	for n := uint64(0); n < 2; n++ {
		e.send(payment(bob, n, gwei(1)), "")
	}
	e.send(payment(carol, 0, gwei(1)), "")
	e.run()
	e.rpc("txpool_status")
	e.stop()
	return e.out()
}

// quaiGlobalQueue: queued transactions above GlobalQueue are dropped from
// the accounts with the newest heartbeat first.
func quaiGlobalQueue(limit uint64) func() obj {
	return func() obj {
		e := quaiWorld("quai-global-queue-"+string(rune('0'+limit)), []account{{alice, 0, rich()}, {bob, 0, rich()}, {carol, 0, rich()}}, spec(1000, 50000))
		conf := core.DefaultTxPoolConfig
		conf.GlobalQueue = limit
		e.start("h1", "none", conf, alice, bob, carol)
		for _, k := range []key{alice, bob, carol} {
			for n := uint64(5); n <= 6; n++ {
				e.send(payment(k, n, gwei(1)), "queued behind a gap")
				time.Sleep(3 * time.Millisecond)
			}
		}
		e.run()
		e.stop()
		return e.out()
	}
}

func quaiPoolFull() obj {
	e := quaiWorld("quai-pool-full", []account{{alice, 0, rich()}, {bob, 0, rich()}, {carol, 0, rich()}, {dave, 0, rich()}, {erin, 0, rich()}, {grace, 0, rich()}}, spec(1000, 50000))
	conf := core.DefaultTxPoolConfig
	conf.GlobalSlots = 2
	conf.GlobalQueue = 2
	e.start("h1", "none", conf, alice, bob, carol, dave, erin, grace)
	e.remotes([][]byte{payment(alice, 0, gwei(10))}, "")
	e.remotes([][]byte{payment(bob, 0, gwei(20))}, "")
	e.run()
	e.remotes([][]byte{payment(carol, 0, gwei(30)), payment(dave, 0, gwei(40))}, "fills four slots")
	e.run()
	e.remotes([][]byte{payment(erin, 0, gwei(5))}, "cheaper than every remote")
	e.remotes([][]byte{payment(erin, 0, gwei(15))}, "evicts the cheapest remote")
	e.run()
	e.send(payment(grace, 0, gwei(1)), "local, cheaper than every remote")
	e.send(payment(grace, 0, gwei(50)), "local, evicts the cheapest remote")
	e.send(encode(signQuai(grace, qtx{nonce: 1, to: addr(bob), price: gwei(60), gas: 21000 + 4*40000, data: make([]byte, 40000)})), "two slots")
	e.run()
	e.rpc("txpool_status")
	e.stop()
	return e.out()
}

func quaiPoolFullLocals() obj {
	e := quaiWorld("quai-pool-full-locals", []account{{alice, 0, rich()}, {bob, 0, rich()}}, spec(1000, 50000))
	conf := core.DefaultTxPoolConfig
	conf.GlobalSlots = 2
	conf.GlobalQueue = 2
	e.start("h1", "none", conf, alice, bob)
	for n := uint64(0); n < 4; n++ {
		e.send(payment(alice, n, gwei(1)), "local")
	}
	e.send(payment(alice, 4, gwei(9)), "a fifth local: no remote to evict")
	e.remotes([][]byte{payment(bob, 0, gwei(9))}, "a remote: no remote to evict")
	e.run()
	e.stop()
	return e.out()
}

func quaiLocals() obj {
	e := quaiWorld("quai-locals", []account{{alice, 0, rich()}, {bob, 0, rich()}}, spec(1000, 50000))
	e.start("h1", "none", core.DefaultTxPoolConfig, alice, bob)
	e.remotes([][]byte{payment(alice, 0, gwei(1))}, "remote")
	e.send(payment(alice, 1, gwei(1)), "local: alice's remote becomes local")
	e.remotes([][]byte{payment(alice, 2, gwei(1)), payment(bob, 0, gwei(1))}, "alice's is local, bob's remote")
	e.run()
	e.stop()
	return e.out()
}

func quaiLocalsConfig() obj {
	e := quaiWorld("quai-locals-config", []account{{alice, 0, rich()}, {bob, 0, rich()}, {carol, 0, rich()}}, spec(1000, 50000))
	conf := core.DefaultTxPoolConfig
	conf.Locals = []common.InternalAddress{carol.internal()}
	conf.NoLocals = true
	e.start("h1", "none", conf, alice, bob, carol)
	e.remotes([][]byte{payment(carol, 0, gwei(1))}, "a configured local")
	e.send(payment(bob, 0, gwei(1)), "RPC with nolocals")
	e.run()
	e.stop()
	return e.out()
}

// quaiNewHead: the head advances to a child that includes alice's nonce 0,
// lowers her balance and the gas limit, and raises the base fee.
func quaiNewHead() obj {
	e := newEnv("quai-new-head")
	t0 := signQuai(alice, qtx{nonce: 0, to: addr(bob), value: u(1), price: gwei(1), gas: 21000})
	r1 := e.commit("s1", []account{{alice, 0, rich()}, {bob, 0, rich()}, {carol, 0, rich()}})
	r2 := e.commit("s2", []account{{alice, 1, quai(2)}, {bob, 0, rich()}, {carol, 0, rich()}})
	h1 := spec(1000, 50000)
	ph := e.prime("prime", h1, params.ExchangeRate, zoneOnly, r1)
	b1 := e.block("h1", makeBlock(h1, common.Hash{0x01}, ph, r1, nil), "s1", 0)
	h2 := spec(1001, 50000)
	h2.gasLimit = 50000
	h2.baseFee = gwei(2)
	e.block("h2", makeBlock(h2, b1, ph, r2, types.Transactions{t0}), "s2", 0)
	e.utxos("none", nil)
	e.start("h1", "none", core.DefaultTxPoolConfig, alice, bob, carol)
	e.send(encode(t0), "")
	e.send(payment(alice, 1, gwei(1)), "")
	e.send(encode(signQuai(alice, qtx{nonce: 2, to: addr(bob), value: quai(3), price: gwei(1), gas: 21000})), "costs more than her balance after the head")
	e.send(payment(alice, 3, gwei(1)), "")
	e.send(encode(signQuai(bob, qtx{nonce: 0, to: addr(alice), price: gwei(1), gas: 100000})), "gas above the next gas limit")
	e.send(payment(bob, 1, gwei(1)), "")
	e.send(payment(carol, 0, gwei(1)), "")
	e.run()
	e.head("h2", "none", "child of h1")
	e.send(payment(carol, 1, gwei(1)), "below the new base fee")
	e.send(payment(carol, 1, gwei(2)), "")
	e.run()
	e.rpc("quai_getTransactionCount", alice.addr.Hex(), "pending")
	e.stop()
	return e.out()
}

// quaiReorg: the head moves from branch A to branch B. A1 holds alice's
// nonce 0 and an ETX; B1 holds bob's nonce 0, which also sits in A2.
func quaiReorg() obj {
	e := newEnv("quai-reorg")
	ta := signQuai(alice, qtx{nonce: 0, to: addr(carol), value: u(1), price: gwei(1), gas: 21000})
	tb := signQuai(bob, qtx{nonce: 0, to: addr(carol), value: u(1), price: gwei(1), gas: 21000})
	etx := types.NewTx(&types.ExternalTx{OriginatingTxHash: common.Hash{0x0b}, ETXIndex: 0, Gas: 21000, To: addr(carol), Value: u(5), Data: []byte{}, AccessList: types.AccessList{}, Sender: other.addr, EtxType: 0})
	r0 := e.commit("s0", []account{{alice, 0, rich()}, {bob, 0, rich()}, {carol, 0, rich()}})
	rA := e.commit("sA", []account{{alice, 1, rich()}, {bob, 1, rich()}, {carol, 0, rich()}})
	rB := e.commit("sB", []account{{alice, 0, rich()}, {bob, 1, rich()}, {carol, 0, rich()}})
	h0 := spec(1000, 50000)
	ph := e.prime("prime", h0, params.ExchangeRate, zoneOnly, r0)
	b0 := e.block("b0", makeBlock(h0, common.Hash{0x01}, ph, r0, nil), "s0", 0)
	a1 := e.block("a1", makeBlock(spec(1001, 50000), b0, ph, rA, types.Transactions{ta, etx}), "sA", 0)
	a2spec := spec(1002, 50000)
	e.block("a2", makeBlock(a2spec, a1, ph, rA, types.Transactions{tb}), "sA", 0)
	b1spec := spec(1001, 50000)
	b1spec.time++
	bb1 := e.block("bb1", makeBlock(b1spec, b0, ph, rB, types.Transactions{tb}), "sB", 0)
	b2spec := spec(1002, 50000)
	b2spec.time++
	bb2 := e.block("bb2", makeBlock(b2spec, bb1, ph, rB, nil), "sB", 0)
	b3spec := spec(1003, 50000)
	b3spec.time++
	e.block("bb3", makeBlock(b3spec, bb2, ph, rB, nil), "sB", 0)
	e.utxos("none", nil)
	e.start("b0", "none", core.DefaultTxPoolConfig, alice, bob, carol)
	e.send(encode(ta), "alice is local")
	e.send(payment(alice, 1, gwei(1)), "")
	e.run()
	e.head("a1", "none", "child: includes alice's nonce 0")
	e.head("a2", "none", "child: includes bob's nonce 0")
	e.send(payment(alice, 2, gwei(1)), "")
	e.run()
	e.head("bb3", "none", "reorg to branch B: alice's nonce 0 returns, bob's stays included")
	e.run()
	e.rpc("quai_getTransactionCount", alice.addr.Hex(), "pending")
	e.rpc("quai_getTransactionCount", bob.addr.Hex(), "pending")
	e.stop()
	return e.out()
}

// quaiReorgDeep: a reorg deeper than 64 blocks skips reinjection; a reset
// from a head the chain does not hold leaves the state as it was.
func quaiReorgDeep() obj {
	e := newEnv("quai-reorg-deep")
	ta := signQuai(alice, qtx{nonce: 0, to: addr(carol), value: u(1), price: gwei(1), gas: 21000})
	r0 := e.commit("s0", []account{{alice, 0, rich()}, {bob, 0, rich()}})
	rA := e.commit("sA", []account{{alice, 1, rich()}, {bob, 0, rich()}})
	rB := e.commit("sB", []account{{alice, 0, rich()}, {bob, 5, rich()}})
	h0 := spec(1000, 50000)
	ph := e.prime("prime", h0, params.ExchangeRate, zoneOnly, r0)
	b0 := e.block("b0", makeBlock(h0, common.Hash{0x01}, ph, r0, nil), "s0", 0)
	e.block("a1", makeBlock(spec(1100, 50000), b0, ph, rA, types.Transactions{ta}), "sA", 0)
	bs := spec(1001, 50000)
	bs.time++
	bb1 := e.block("bb1", makeBlock(bs, b0, ph, rB, nil), "sB", 0)
	cs := spec(1002, 50000)
	cs.time++
	e.block("bb2", makeBlock(cs, bb1, ph, r0, nil), "s0", 0)
	e.utxos("none", nil)
	e.start("b0", "none", core.DefaultTxPoolConfig, alice, bob)
	e.send(encode(ta), "")
	e.run()
	e.head("a1", "none", "a jump of 100 blocks: no reorg walk")
	e.head("bb1", "none", "reorg of depth 99: no reinjection")
	e.send(payment(bob, 5, gwei(1)), "")
	e.run()
	ghost := spec(1000, 50000)
	ghost.time += 7
	e.headFrom("ghost", ghost, "bb2", "none", "old head unknown to the chain: no reset")
	e.send(payment(bob, 6, gwei(1)), "")
	e.run()
	e.rpc("quai_getTransactionCount", bob.addr.Hex(), "pending")
	e.stop()
	return e.out()
}

func quaiExpiry() obj {
	e := quaiWorld("quai-expiry", []account{{alice, 0, rich()}, {bob, 0, rich()}, {carol, 0, rich()}}, spec(1000, 50000))
	e.start("h1", "none", core.DefaultTxPoolConfig, alice, bob, carol)
	e.send(payment(alice, 3, gwei(1)), "queued")
	e.send(payment(bob, 0, gwei(1)), "")
	e.send(payment(bob, 1, gwei(1)), "")
	e.send(payment(carol, 0, gwei(1)), "")
	e.send(payment(carol, 2, gwei(1)), "")
	e.run()
	e.evict()
	e.ageBeat(alice, 299)
	e.evict()
	e.ageBeat(alice, 301)
	e.evict()
	e.ageTx(hashOf(payment(bob, 0, gwei(1))), 301)
	e.ageTx(hashOf(payment(carol, 2, gwei(1))), 301)
	e.evict()
	e.ageBeat(carol, 400)
	e.evict()
	e.stop()
	return e.out()
}

// --- Qi -----------------------------------------------------------------------

func qiScenarios() []func() obj {
	return []func() obj{qiValidate, qiLru, qiHead, qiExpire, qiHold(params.KawPowForkBlock, "qi-hold-kawpow"), qiHold(params.ShaEquivalentDifficultyForkBlock, "qi-hold-sha"), qiNoPrime, remotesBatch}
}

func point(seed string, index uint16) types.OutPoint {
	return types.OutPoint{TxHash: common.BytesToHash(crypto.Keccak256([]byte("utxo/" + seed))), Index: index}
}

func entry(k key, denom uint8, lock uint64) *types.UtxoEntry {
	var l *big.Int
	if lock > 0 {
		l = u(lock)
	}
	return &types.UtxoEntry{Denomination: denom, Address: k.bytes(), Lock: l}
}

// qiWorld: a head with a base fee that QiToQuai of 5 qits pays for 21,000
// gas, and a UTXO set.
func qiWorld(id string, h head, utxos []utxo, accounts []account, setSize uint64) (*env, *types.WorkObject) {
	e := newEnv(id)
	root := e.commit("s1", accounts)
	rate := new(big.Int).Mul(params.ExchangeRate, u(7))
	ph := e.prime("prime", h, rate, withRegion1, root)
	probe := makeBlock(h, common.Hash{0x01}, ph, root, nil)
	h.baseFee = new(big.Int).Div(misc.QiToQuai(probe, rate, probe.Difficulty(), u(5)), u(21000))
	if h.baseFee.Sign() == 0 {
		h.baseFee = u(1)
	}
	wo := makeBlock(h, common.Hash{0x01}, ph, root, nil)
	e.block("h1", wo, "s1", setSize)
	e.utxos("u1", utxos)
	return e, wo
}
func qo(k key, denom uint8) out { return out{denom: denom, addr: k.bytes()} }

func qiValidate() obj {
	u1, u2, u3, u4, u5, u6, u7 := point("u1", 0), point("u2", 0), point("u3", 1), point("u4", 2), point("u5", 0), point("u6", 0), point("u7", 0)
	h := polaris(3000000)
	e, _ := qiWorld("qi-validate", h, []utxo{{u1, entry(k1, 2, 0)}, {u2, entry(k1, 3, 3000010)}, {u3, entry(k1, 6, 0)}, {u4, entry(k2, 6, 0)}, {u5, entry(k5, 8, 0)}, {u6, entry(alice, 4, 0)}, {u7, entry(k5, 2, 0)}}, []account{{alice, 0, rich()}}, 30000000)
	e.start("h1", "u1", core.DefaultTxPoolConfig)
	q1 := encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k3, 1)}, nil, nil))
	e.send(q1, "10 qits in, 5 out")
	e.send(q1, "again")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k4, 0)}, nil, nil)), "spends the same UTXO: no double-spend check in the pool")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{point("missing", 0)}, []out{qo(k3, 0)}, nil, nil)), "missing UTXO")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u2}, []out{qo(k3, 0)}, nil, nil)), "locked UTXO")
	e.send(encode(signQi([]key{k2}, []types.OutPoint{u1}, []out{qo(k3, 0)}, nil, nil)), "another key's pubkey")
	e.send(encode(signQi([]key{alice}, []types.OutPoint{u6}, []out{qo(k3, 0)}, nil, nil)), "a Quai key owns the UTXO")
	e.send(encode(signQi([]key{k1, k1}, []types.OutPoint{u1, u1}, []out{qo(k3, 0)}, nil, nil)), "duplicate input")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k3, 0)}, nil, u(1))), "chain ID 1")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k3, 0)}, []byte{1, 2, 3, 4, 5}, nil)), "5 bytes of data")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k3, 0)}, k4.bytes(), nil)), "wrapping into a Qi address")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k3, 0)}, append([]byte{0, 0}, alice.bytes()...), nil)), "conversion refund to a Quai address")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k3, 15)}, nil, nil)), "denomination 15")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{{denom: 0, addr: k3.bytes(), lock: u(5)}}, nil, nil)), "output lock")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{{denom: 0, addr: k3.bytes()[:19]}}, nil, nil)), "19-byte output address")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k1, 0)}, nil, nil)), "pays the input's owner")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k3, 0), qo(k3, 0)}, nil, nil)), "two outputs to one address")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(bob, 0)}, nil, nil)), "to a Quai address without data")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(zone01, 0)}, nil, nil)), "to zone [0 1]")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u3}, []out{qo(region1, 5)}, nil, nil)), "to region 1 zone 0: an ETX, 500 qits fee")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(region1, 1)}, nil, nil)), "an ETX with 5 qits fee")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(region2, 0)}, nil, nil)), "to region 2: not eligible")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k3, 3)}, nil, nil)), "50 out of 10")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k3, 1), qo(k4, 0), qo(k6, 0), qo(k7, 0)}, nil, nil)), "2 qits fee")
	bad := signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k8, 1)}, nil, nil)
	pt := protoOf(encode(bad))
	pt.Signature = encodeSig(qiSign([]key{k2}, common.Hash{0x55}))
	e.send(marshal(pt), "a signature over another message")
	e.send(encode(signQi([]key{k1, k2}, []types.OutPoint{u3, u4}, []out{qo(k9, 6)}, nil, nil)), "two inputs, MuSig2")
	conv := append([]byte{0, 50}, k6.bytes()...)
	e.send(encode(signQi([]key{k5}, []types.OutPoint{u5}, []out{{denom: 7, addr: carol.bytes()}}, conv, nil)), "conversion with 5000 qits fee")
	e.send(encode(signQi([]key{k5}, []types.OutPoint{u7}, []out{{denom: 1, addr: carol.bytes()}}, conv, nil)), "conversion with 5 qits fee")
	e.send(encode(signQi([]key{k5}, []types.OutPoint{u5}, []out{{denom: 7, addr: dave.bytes()}}, carol.bytes(), nil)), "wrapping into carol's contract")
	// Decoding.
	qbase := protoOf(q1)
	for _, f := range []struct {
		note string
		f    func(pt *types.ProtoTransaction)
	}{
		{"Qi without inputs", func(pt *types.ProtoTransaction) { pt.TxIns = nil }},
		{"Qi with an empty input list", func(pt *types.ProtoTransaction) { pt.TxIns = &types.ProtoTxIns{} }},
		{"Qi without outputs", func(pt *types.ProtoTransaction) { pt.TxOuts = nil }},
		{"Qi without signature", func(pt *types.ProtoTransaction) { pt.Signature = nil }},
		{"Qi without chain ID", func(pt *types.ProtoTransaction) { pt.ChainId = nil }},
		{"Qi without data", func(pt *types.ProtoTransaction) { pt.Data = nil }},
		{"input without outpoint", func(pt *types.ProtoTransaction) { pt.TxIns.TxIns[0].PreviousOutPoint = nil }},
		{"outpoint without hash", func(pt *types.ProtoTransaction) { pt.TxIns.TxIns[0].PreviousOutPoint.Hash = nil }},
		{"outpoint without index", func(pt *types.ProtoTransaction) { pt.TxIns.TxIns[0].PreviousOutPoint.Index = nil }},
		{"10-byte public key", func(pt *types.ProtoTransaction) { pt.TxIns.TxIns[0].PubKey = make([]byte, 10) }},
		{"33-byte key off the curve", func(pt *types.ProtoTransaction) { k := make([]byte, 33); k[0] = 2; k[32] = 5; pt.TxIns.TxIns[0].PubKey = k }},
		{"65-byte key off the curve", func(pt *types.ProtoTransaction) { k := make([]byte, 65); k[0] = 4; k[64] = 1; pt.TxIns.TxIns[0].PubKey = k }},
		{"uncompressed public key", func(pt *types.ProtoTransaction) { pt.TxIns.TxIns[0].PubKey = k1.btc.PubKey().SerializeUncompressed() }},
		{"output without denomination", func(pt *types.ProtoTransaction) { pt.TxOuts.TxOuts[0].Denomination = nil }},
		{"denomination 256", func(pt *types.ProtoTransaction) { d := uint32(256); pt.TxOuts.TxOuts[0].Denomination = &d }},
		{"63-byte signature", func(pt *types.ProtoTransaction) { pt.Signature = pt.Signature[:63] }},
		{"signature R above the field", func(pt *types.ProtoTransaction) {
			for i := 0; i < 32; i++ {
				pt.Signature[i] = 0xff
			}
		}},
		{"signature S above the order", func(pt *types.ProtoTransaction) {
			for i := 32; i < 64; i++ {
				pt.Signature[i] = 0xff
			}
		}},
	} {
		pt := protoOf(marshal(qbase))
		f.f(pt)
		e.send(marshal(pt), f.note)
	}
	e.rpc("quai_getTransactionByHash", hashOf(q1).Hex())
	e.rpc("txpool_status")
	e.rpc("txpool_content")
	e.stop()
	return e.out()
}
func encodeSig(s interface{ Serialize() []byte }) []byte { return s.Serialize() }

func qiLru() obj {
	ps := []types.OutPoint{point("l1", 0), point("l2", 0), point("l3", 0), point("l4", 0)}
	var us []utxo
	for _, p := range ps {
		us = append(us, utxo{p, entry(k1, 2, 0)})
	}
	e, _ := qiWorld("qi-lru", polaris(3000000), us, nil, 1000)
	conf := core.DefaultTxPoolConfig
	conf.QiPoolSize = 3
	e.start("h1", "u1", conf)
	q := func(i int) []byte {
		return encode(signQi([]key{k1}, []types.OutPoint{ps[i]}, []out{qo(k3, 1)}, nil, nil))
	}
	e.send(q(0), "")
	e.send(q(1), "")
	e.send(q(2), "")
	e.send(q(0), "known: refreshes it")
	e.send(q(3), "evicts the least recently used")
	e.rpc("txpool_status")
	e.stop()
	return e.out()
}

// qiHead: Qi transactions leave the pool when the next head includes them,
// stay when a jump or a reorg includes them, and return on a reorg.
func qiHead() obj {
	e := newEnv("qi-head")
	ps := []types.OutPoint{point("h1", 0), point("h2", 0), point("h3", 0), point("h4", 0), point("h5", 0)}
	us := []utxo{}
	for _, p := range ps {
		us = append(us, utxo{p, entry(k1, 2, 0)})
	}
	q := func(i int, to key) *types.Transaction {
		return signQi([]key{k1}, []types.OutPoint{ps[i]}, []out{qo(to, 1)}, nil, nil)
	}
	q0, q1, q2, q3 := q(0, k3), q(1, k3), q(2, k3), q(3, k3)
	// Never sent to this pool: a reorg validates them again.
	qx, qy := q(4, k4), q(2, k4)
	root := e.commit("s0", nil)
	rate := new(big.Int).Mul(params.ExchangeRate, u(7))
	h0 := spec(1000, 50000)
	ph := e.prime("prime", h0, rate, withRegion1, root)
	probe := makeBlock(h0, common.Hash{0x01}, ph, root, nil)
	fee := new(big.Int).Div(misc.QiToQuai(probe, rate, probe.Difficulty(), u(5)), u(21000))
	at := func(n uint64, t uint64) head {
		h := spec(n, 50000)
		h.time += t
		h.baseFee = fee
		h.exchange = rate
		return h
	}
	b0 := e.block("b0", makeBlock(at(1000, 0), common.Hash{0x01}, ph, root, nil), "s0", 4)
	a1 := e.block("a1", makeBlock(at(1001, 0), b0, ph, root, types.Transactions{q0, qx, qy}), "s0", 4)
	a2 := e.block("a2", makeBlock(at(1002, 0), a1, ph, root, types.Transactions{q1}), "s0", 4)
	e.block("a3", makeBlock(at(1003, 0), a2, ph, root, nil), "s0", 4)
	x1 := e.block("x1", makeBlock(at(1001, 1), b0, ph, root, types.Transactions{q2, q3}), "s0", 4)
	x2 := e.block("x2", makeBlock(at(1002, 1), x1, ph, root, nil), "s0", 4)
	e.block("x3", makeBlock(at(1003, 1), x2, ph, root, nil), "s0", 4)
	e.utxos("all", us)
	e.utxos("spent-0", us[1:])
	e.utxos("spent-01", us[2:])
	e.utxos("spent-23", []utxo{us[0], us[1], us[4]})
	e.start("b0", "all", core.DefaultTxPoolConfig)
	e.send(encode(q0), "")
	e.send(encode(q1), "")
	e.send(encode(q2), "")
	e.head("a1", "spent-0", "child includes q0: removed")
	e.head("a3", "spent-01", "jump over a2 (includes q1): q1 stays")
	e.head("x3", "spent-23", "reorg: q0 returns (cached fee), qx is validated and returns, qy's input is spent, x1's q2 and q3 stay as they were")
	e.rpc("txpool_status")
	e.stop()
	return e.out()
}

func qiExpire() obj {
	var ps []types.OutPoint
	var us []utxo
	for i := 0; i < 7; i++ {
		p := point("e", uint16(i))
		ps = append(ps, p)
		us = append(us, utxo{p, entry(k1, 2, 0)})
	}
	e, _ := qiWorld("qi-expire", polaris(3000000), us, nil, 1000)
	e.start("h1", "u1", core.DefaultTxPoolConfig)
	var hs []common.Hash
	for i := range ps {
		raw := encode(signQi([]key{k1}, []types.OutPoint{ps[i]}, []out{qo(k3, 1)}, nil, nil))
		hs = append(hs, hashOf(raw))
		e.send(raw, "")
	}
	e.expireQi()
	for i := 0; i < 4; i++ {
		e.ageQi(hs[i], 1801)
	}
	e.expireQi()
	e.stop()
	return e.out()
}

// qiHold: conversions are refused for 20,000 prime blocks after the KawPow
// and ShaEquivalentDifficulty forks.
func qiHold(fork uint64, id string) func() obj {
	return func() obj {
		u1 := point("hold", 0)
		h := polaris(3000000)
		h.prime = fork + 5
		e, _ := qiWorld(id, h, []utxo{{u1, entry(k5, 8, 0)}}, nil, 1000)
		e.start("h1", "u1", core.DefaultTxPoolConfig)
		conv := append([]byte{0, 50}, k6.bytes()...)
		e.send(encode(signQi([]key{k5}, []types.OutPoint{u1}, []out{{denom: 7, addr: carol.bytes()}}, conv, nil)), "conversion inside the hold")
		e.send(encode(signQi([]key{k5}, []types.OutPoint{u1}, []out{qo(k3, 7)}, nil, nil)), "a plain Qi transaction")
		e.stop()
		return e.out()
	}
}

// qiNoPrime: the head's prime terminus is not in the chain.
func qiNoPrime() obj {
	u1 := point("np", 0)
	e := newEnv("qi-no-prime")
	root := e.commit("s1", nil)
	e.block("h1", makeBlock(polaris(3000000), common.Hash{0x01}, common.Hash{0x99}, root, nil), "s1", 1000)
	e.utxos("u1", []utxo{{u1, entry(k1, 2, 0)}})
	e.start("h1", "u1", core.DefaultTxPoolConfig)
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k3, 1)}, nil, nil)), "")
	e.stop()
	return e.out()
}

// remotesBatch: one gossip batch with Quai, Qi, ETX and invalid entries.
func remotesBatch() obj {
	u1, u2 := point("rb", 0), point("rb", 1)
	e, _ := qiWorld("remotes-batch", polaris(3000000), []utxo{{u1, entry(k1, 2, 0)}, {u2, entry(k2, 2, 0)}}, []account{{alice, 0, rich()}, {bob, 0, rich()}}, 1000)
	e.start("h1", "u1", core.DefaultTxPoolConfig, alice, bob)
	price := new(big.Int).Mul(e.chain.CurrentBlock().BaseFee(), u(2))
	etx := types.NewTx(&types.ExternalTx{OriginatingTxHash: common.Hash{0x0c}, ETXIndex: 0, Gas: 21000, To: addr(bob), Value: u(5), Data: []byte{}, AccessList: types.AccessList{}, Sender: alice.addr, EtxType: 0})
	q1 := encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k3, 1)}, nil, nil))
	e.remotes([][]byte{
		payment(alice, 0, price),
		payment(alice, 0, new(big.Int).Mul(price, u(2))),
		encode(etx),
		q1,
		encode(signQi([]key{k1}, []types.OutPoint{u2}, []out{qo(k3, 1)}, nil, nil)),
		payment(bob, 1, price),
		payment(alice, 0, price),
		q1,
	}, "replacement inside the batch, ETX, Qi, wrong owner, gap, stale duplicate, Qi duplicate")
	e.run()
	e.stop()
	return e.out()
}

// --- sharing clients ------------------------------------------------------------

func sharingScenarios() []func() obj {
	return []func() obj{sharingSync, sharingAsync, sharingDead}
}

// sharingDead: the first sharing client does not listen.
func sharingDead() obj {
	e := quaiWorld("sharing-dead", []account{{alice, 0, rich()}}, spec(1000, 50000))
	conf := core.DefaultTxPoolConfig
	conf.SyncTxWithReturn = true
	conf.SharingClientsEndpoints = []string{"http://127.0.0.1:1"}
	e.start("h1", "none", conf, alice)
	e.send(payment(alice, 0, gwei(1)), "connection refused")
	e.stop()
	return e.out()
}

const ok = `{"jsonrpc":"2.0","id":1,"result":null}`

func sharingSync() obj {
	s0 := newSharer(reply{200, ok, 0}, reply{200, `{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"nope"}}`, 0}, reply{500, "boom", 0}, reply{200, ok, 0}, reply{200, ok, 0}, reply{200, ok, 0}, reply{200, "not json", 0}, reply{200, ok, 2500 * time.Millisecond})
	s1 := newSharer(reply{200, ok, 0})
	u1 := point("share", 0)
	e, _ := qiWorld("sharing-sync", polaris(3000000), []utxo{{u1, entry(k1, 2, 0)}}, []account{{alice, 0, rich()}}, 1000)
	e.sharers = []*sharer{s0, s1}
	conf := core.DefaultTxPoolConfig
	conf.SyncTxWithReturn = true
	conf.SharingClientsEndpoints = []string{s0.srv.URL, s1.srv.URL}
	e.start("h1", "u1", conf, alice, frank)
	price := new(big.Int).Mul(e.chain.CurrentBlock().BaseFee(), u(2))
	e.send(payment(alice, 0, price), "first client answers null")
	e.send(payment(alice, 1, price), "first client answers an error")
	e.send(payment(alice, 2, price), "first client answers HTTP 500")
	e.send(payment(frank, 0, price), "no local validation: forwarded")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k3, 1)}, nil, nil)), "Qi")
	e.send([]byte{0xff}, "decoding fails before forwarding")
	e.send(encode(signQuai(alice, qtx{nonce: 3, to: addr(bob), price: quai(1), gas: 10001})), "fee cap fails before forwarding")
	e.receive(payment(alice, 4, price), "from a sharing client: forwarded again")
	e.send(payment(alice, 5, price), "first client answers no JSON")
	e.send(payment(alice, 6, price), "first client answers after 2.5 s")
	e.rpc("txpool_status")
	e.stop()
	return e.out()
}

func sharingAsync() obj {
	s0 := newSharer(reply{200, ok, 0}, reply{200, `{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"nope"}}`, 0})
	u1 := point("share", 0)
	e, _ := qiWorld("sharing-async", polaris(3000000), []utxo{{u1, entry(k1, 2, 0)}}, []account{{alice, 0, rich()}}, 1000)
	e.sharers = []*sharer{s0}
	conf := core.DefaultTxPoolConfig
	conf.SyncTxWithReturn = false
	conf.SharingClientsEndpoints = []string{s0.srv.URL}
	e.start("h1", "u1", conf, alice, frank)
	price := new(big.Int).Mul(e.chain.CurrentBlock().BaseFee(), u(2))
	e.send(payment(alice, 0, price), "added, then forwarded")
	e.send(payment(frank, 0, price), "refused locally: not forwarded")
	e.send(encode(signQi([]key{k1}, []types.OutPoint{u1}, []out{qo(k3, 1)}, nil, nil)), "Qi added, then forwarded; the client's error is only logged")
	e.run()
	e.stop()
	return e.out()
}
