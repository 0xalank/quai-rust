// Offline v0.57.0 stateful execution oracle; deterministic private test accounts.
package main

import (
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/core/vm"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/crypto/bn256"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/dominant-strategies/go-quai/rlp"
	"google.golang.org/protobuf/proto"
	"math/big"
	"os"
	"strings"
)

var referenceRevision string

func must(e error) {
	if e != nil {
		panic(e)
	}
}
func hx(b []byte) string  { return "0x" + hex.EncodeToString(b) }
func u(v uint64) *big.Int { return new(big.Int).SetUint64(v) }
func w(v uint64) string   { return hx(u(v).FillBytes(make([]byte, 32))) }
func p(v uint64) []byte {
	b := u(v).Bytes()
	if len(b) == 0 {
		b = []byte{0}
	}
	return append([]byte{0x5f + byte(len(b))}, b...)
}
func join(parts ...[]byte) []byte {
	var b []byte
	for _, part := range parts {
		b = append(b, part...)
	}
	return b
}
func addr(loc common.Location, n byte) common.Address {
	a := make([]byte, 20)
	a[0] = loc.BytePrefix()
	a[18] = 0x11
	a[19] = n
	return common.BytesToAddress(a, loc)
}
func pushAddr(a common.Address) []byte { return append([]byte{0x73}, a.Bytes()...) }
func call(op byte, to common.Address, value, gas uint64) []byte {
	args := [][]byte{p(32), p(0), p(0), p(0)}
	if op == 0xf1 || op == 0xf2 {
		args = append(args, p(value))
	}
	args = append(args, pushAddr(to), p(gas), []byte{op})
	return join(args...)
}
func store(k, v uint64) []byte { return join(p(v), p(k), []byte{0x55}) }
func ret(v uint64) []byte      { return join(p(v), p(0), []byte{0x52}, p(32), p(0), []byte{0xf3}) }
func done() []byte             { return []byte{0x00} }
func revert() []byte           { return join(p(0), p(0), []byte{0xfd}) }
func errClass(err error) interface{} {
	if err == nil || errors.Is(err, vm.ErrExecutionReverted) {
		return nil
	}
	if errors.Is(err, vm.ErrCodeStoreOutOfGas) {
		return "code_store_out_of_gas"
	}
	if errors.Is(err, vm.ErrInvalidCode) {
		return "invalid_code"
	}
	if errors.Is(err, vm.ErrContractAddressCollision) {
		return "contract_collision"
	}
	if errors.Is(err, vm.ErrMaxCodeSizeExceeded) {
		return "max_code_size"
	}
	for _, v := range []struct {
		e error
		n string
	}{{vm.ErrOutOfGas, "out_of_gas"}, {vm.ErrWriteProtection, "write_protection"}, {vm.ErrInvalidAccessList, "invalid_access_list"}, {vm.ErrReturnDataOutOfBounds, "return_data_bounds"}, {vm.ErrDepth, "depth"}, {vm.ErrInsufficientBalance, "insufficient_balance"}, {vm.ErrGasUintOverflow, "gas_overflow"}} {
		if errors.Is(err, v.e) {
			return v.n
		}
	}
	var under *vm.ErrStackUnderflow
	if errors.As(err, &under) {
		return "stack_underflow"
	}
	var op *vm.ErrInvalidOpCode
	if errors.As(err, &op) {
		return "invalid_opcode"
	}
	panic(err)
}

type testcase struct {
	id                 string
	root, child, grand []byte
	listed, readonly   bool
	value, gas         uint64
	warm               bool
	extra              []common.Address
	collide            *common.Address
	target             *common.Address
	input              []byte
	precompile         bool
	prime              uint64
	rich               bool
	ineligible         bool
	lockup             string
	reset              *common.Address
	keys               int
}

func run(c testcase, loc common.Location) map[string]interface{} {
	disk := rawdb.NewMemoryDatabase(log.Global)
	defer disk.Close()
	cache := state.NewDatabase(disk)
	s, e := state.New(types.EmptyRootHash, types.EmptyRootHash, u(0), cache, cache, nil, loc, log.Global)
	must(e)
	root, child, grand, sender, coinbase := addr(loc, 1), addr(loc, 2), addr(loc, 3), addr(loc, 4), addr(loc, 5)
	addresses := []common.Address{root, child, grand, sender}
	codes := [][]byte{c.root, c.child, c.grand, nil}
	if c.collide != nil {
		addresses = append(addresses, *c.collide)
		codes = append(codes, []byte{0})
	}
	for i, a := range addresses {
		ia, e := a.InternalAndQuaiAddress()
		must(e)
		s.CreateAccount(ia)
		s.SetNonce(ia, 1)
		s.SetBalance(ia, u(1000000))
		s.SetCode(ia, codes[i])
		s.SetState(ia, common.Hash{}, common.Hash{31: 1})
	}
	if c.rich {
		ia, _ := root.InternalAndQuaiAddress()
		s.SetBalance(ia, new(big.Int).Lsh(u(1), 128))
	}
	watch := []map[string]interface{}{}
	slotKeys := []common.Hash{{}}
	batch := disk.NewBatch()
	batch.SetPending(true)
	if c.reset != nil {
		a, _ := c.reset.InternalAndQuaiAddress()
		s.CreateAccount(a)
		s.SetNonce(a, 0)
		s.SetState(a, common.Hash{}, common.Hash{31: 1})
		addresses = append(addresses, *c.reset)
		codes = append(codes, nil)
	}
	if c.lockup != "" {
		lockAddr := common.HexToAddress(fmt.Sprintf("%02x0000000000000000000000000000000000000a", loc.BytePrefix()), loc)
		li, _ := lockAddr.InternalAndQuaiAddress()
		s.CreateAccount(li)
		s.SetNonce(li, 1)
		addresses = append(addresses, lockAddr)
		codes = append(codes, nil)
		owner := sender
		if strings.HasPrefix(c.lockup, "nested-") {
			owner = root
		}
		benef := addr(loc, 6)
		delegate := addr(loc, 7)
		epoch := uint32(1)
		unlock := uint32(3900000)
		elements := uint16(7)
		if strings.Contains(c.lockup, "locked") {
			unlock = 4000001
		}
		if strings.Contains(c.lockup, "missing") {
			unlock = 0
			elements = 0
		}
		key, e := rawdb.WriteCoinbaseLockup(disk, owner, benef, 2, epoch, u(12345), unlock, elements, delegate)
		must(e)
		_ = key
		ownerKey := common.BytesToHash(owner.Bytes())
		depositKey := common.Hash{}
		copy(depositKey[:16], owner.Bytes()[:16])
		copy(depositKey[16:], benef.Bytes()[:16])
		s.SetState(li, ownerKey, common.BigToHash(u(5000)))
		s.SetState(li, depositKey, common.BigToHash(u(3000)))
		slotKeys = append(slotKeys, ownerKey, depositKey)
		watch = append(watch, map[string]interface{}{"owner": hx(owner.Bytes()), "beneficiary": hx(benef.Bytes()), "lock": 2, "epoch": epoch, "balance": w(12345), "unlock_height": unlock, "elements": elements, "delegate": hx(delegate.Bytes())})
	}
	before, e := s.Commit(false)
	must(e)
	must(cache.TrieDB().Commit(before, false, nil))
	s, e = state.New(before, types.EmptyRootHash, u(0), cache, cache, nil, loc, log.Global)
	must(e)
	world := []interface{}{}
	for i, a := range addresses {
		ia, _ := a.InternalAndQuaiAddress()
		world = append(world, map[string]interface{}{"address": hx(a.Bytes()), "nonce": s.GetNonce(ia), "balance": hx(s.GetBalance(ia).FillBytes(make([]byte, 32))), "code": hx(codes[i]), "storage_size": hx(s.GetSize(ia).FillBytes(make([]byte, 32))), "storage": func() []interface{} {
			out := []interface{}{}
			for _, k := range slotKeys {
				value := s.GetState(ia, k)
				if value != (common.Hash{}) {
					out = append(out, map[string]interface{}{"key": k.Hex(), "value": value.Hex()})
				}
			}
			return out
		}()})
	}
	chain := *params.TestChainConfig
	chain.Location = loc
	chain.ChainID = u(1337)
	vm.PrecompiledContracts = make(map[common.AddressBytes]vm.PrecompiledContract)
	vm.PrecompiledAddresses = make(map[string][]common.Address)
	vm.InitializePrecompiles(loc)
	if c.target != nil {
		root = *c.target
	}
	list := types.AccessList{}
	access := []interface{}{}
	for _, a := range c.extra {
		list = append(list, types.AccessTuple{Address: a, StorageKeys: nil})
		access = append(access, map[string]interface{}{"address": hx(a.Bytes()), "keys": []string{}})
	}
	if c.listed {
		for _, a := range []common.Address{root, child, grand} {
			keys := []common.Hash{}
			if c.warm {
				keys = append(keys, common.Hash{}, common.Hash{31: 1}, common.Hash{31: 2})
			}
			if a.Equal(root) {
				for i := 0; i < c.keys; i++ {
					keys = append(keys, common.BigToHash(u(uint64(i))))
				}
			}
			list = append(list, types.AccessTuple{Address: a, StorageKeys: keys})
			encoded := []string{}
			for _, k := range keys {
				encoded = append(encoded, k.Hex())
			}
			access = append(access, map[string]interface{}{"address": hx(a.Bytes()), "keys": encoded})
		}
	}
	s.Prepare(common.Hash{31: 77}, 0)
	s.PrepareAccessList(sender, &root, vm.ActivePrecompiles(chain.Rules(u(4000000)), loc), list, false)
	prime := c.prime
	if prime == 0 {
		prime = 2444574
	}
	tr := new(trace)
	transfer := func(db vm.StateDB, from, to common.Address, v *big.Int) error {
		f, e := from.InternalAndQuaiAddress()
		if e != nil {
			return e
		}
		t, e := to.InternalAndQuaiAddress()
		if e != nil {
			return e
		}
		db.SubBalance(f, v)
		db.AddBalance(t, v)
		return nil
	}
	can := func(db vm.StateDB, a common.Address, v *big.Int) bool {
		ia, e := a.InternalAndQuaiAddress()
		return e == nil && db.GetBalance(ia).Cmp(v) >= 0
	}
	eligible := common.Hash{}
	if !c.ineligible {
		for i := range eligible {
			eligible[i] = 255
		}
	}
	env := vm.NewEVM(vm.BlockContext{CheckIfEtxEligible: func(bits common.Hash, to common.Location) bool {
		n := int(to[0])*16 + int(to[1])
		return bits[n/8]&(1<<uint(n%8)) != 0
	}, EtxEligibleSlices: eligible, CanTransfer: can, Transfer: transfer, BlockNumber: u(4000000), Time: u(1769112000), Difficulty: u(1), BaseFee: u(13), QuaiStateSize: u(1 << 20), GasLimit: 30000000, PrimaryCoinbase: coinbase, PrimeTerminusNumber: prime, GetHash: func(n uint64) common.Hash { return common.BigToHash(u(n)) }}, vm.TxContext{Origin: sender, GasPrice: u(17), TxType: 0, Hash: common.Hash{31: 77}}, s, &chain, vm.Config{Debug: true, Tracer: tr}, batch)
	var output []byte
	var left, stateGas uint64
	var err error
	if c.readonly {
		output, left, err = env.StaticCall(vm.AccountRef(sender), root, c.input, c.gas)
	} else {
		output, left, stateGas, err = env.Call(vm.AccountRef(sender), root, c.input, c.gas, u(c.value))
	}
	refund := s.GetRefund()
	logs := []interface{}{}
	for _, l := range s.Logs() {
		topics := []string{}
		for _, h := range l.Topics {
			topics = append(topics, h.Hex())
		}
		logs = append(logs, map[string]interface{}{"address": hx(l.Address.Bytes()), "topics": topics, "data": hx(l.Data)})
	}
	after := s.IntermediateRoot(false)
	var message interface{}
	if err != nil {
		message = err.Error()
	}
	return map[string]interface{}{"id": fmt.Sprintf("%s-%d-%d", c.id, loc[0], loc[1]), "world": world, "lockups_before": watch, "lockups_after": func() []interface{} {
		out := []interface{}{}
		for _, v := range watch {
			owner := common.HexToAddress(v["owner"].(string), loc)
			benef := common.HexToAddress(v["beneficiary"].(string), loc)
			balance, height, elements, delegate := rawdb.ReadCoinbaseLockup(disk, batch, owner, benef, 2, 1)
			if height != 0 || balance.Sign() != 0 || elements != 0 {
				out = append(out, map[string]interface{}{"owner": v["owner"], "beneficiary": v["beneficiary"], "lock": 2, "epoch": 1, "balance": hx(balance.FillBytes(make([]byte, 32))), "unlock_height": height, "elements": elements, "delegate": hx(delegate.Bytes()), "commitment": types.CoinbaseLockupHash(owner, benef, delegate, 2, 1, balance, height, elements).Hex()})
			}
		}
		return out
	}(), "context": map[string]interface{}{"address": hx(root.Bytes()), "caller": hx(sender.Bytes()), "origin": hx(sender.Bytes()), "coinbase": hx(coinbase.Bytes()), "etx_sender": hx(sender.Bytes()), "location": []int{int(loc[0]), int(loc[1])}, "tx_type": 0, "value": w(c.value), "gas_price": w(17), "block_number": w(4000000), "timestamp": w(1769112000), "difficulty": w(1), "base_fee": w(13), "chain_id": w(1337), "state_size": w(1 << 20), "block_gas_limit": 30000000, "address_listed": true, "warm_slots": []string{}, "read_only": c.readonly, "prime_number": prime}, "transaction_hash": common.Hash{31: 77}.Hex(), "eligible_slices": eligible.Hex(), "outbound_etxs": func() []string {
		out := []string{}
		for _, tx := range env.ETXCache {
			pb, e := tx.ProtoEncode()
			must(e)
			b, e := proto.Marshal(pb)
			must(e)
			out = append(out, hx(b))
		}
		return out
	}(), "block_hashes": []interface{}{map[string]interface{}{"number": 3999999, "hash": common.BigToHash(u(3999999)).Hex()}}, "access_list": access, "input": hx(c.input), "gas": c.gas, "before_root": before.Hex(), "after_root": after.Hex(), "output": hx(output), "error": func() interface{} {
		if c.precompile && err != nil && !errors.Is(err, vm.ErrOutOfGas) && !errors.Is(err, vm.ErrInsufficientBalance) {
			return "precompile"
		}
		if c.lockup != "" && err != nil && !errors.Is(err, vm.ErrExecutionReverted) && !errors.Is(err, vm.ErrOutOfGas) && !errors.Is(err, vm.ErrWriteProtection) {
			return "vm_error"
		}
		return errClass(err)
	}(), "reference_error": message, "reverted": errors.Is(err, vm.ErrExecutionReverted), "gas_remaining": left, "state_gas": stateGas, "refund": refund, "logs": logs, "trace_hash": tr.hash.Hex(), "trace_steps": tr.steps}
}
func main() {
	vectors := []interface{}{}
	for _, loc := range []common.Location{{0, 0}, {1, 2}, {15, 15}} {
		child, grand := addr(loc, 2), addr(loc, 3)
		for _, op := range []byte{0xf1, 0xf2, 0xf4, 0xfa} {
			for _, warm := range []bool{false, true} {
				base := testcase{id: fmt.Sprintf("op-%x-warm-%v", op, warm), root: join(store(2, 3), call(op, child, 0, 150000), p(1), []byte{0x55}, ret(99)), child: join(store(1, 7), ret(42)), listed: true, warm: warm, gas: 1000000}
				vectors = append(vectors, run(base, loc))
				base.id += "-child-revert"
				base.child = join(store(1, 7), p(42), p(0), []byte{0x52}, p(32), p(0), []byte{0xfd})
				vectors = append(vectors, run(base, loc))
				base.id += "-outer-revert"
				base.child = join(store(1, 7), ret(42))
				base.root = join(store(2, 3), call(op, child, 0, 150000), revert())
				vectors = append(vectors, run(base, loc))
			}
		}
		cases := []testcase{
			{id: "nested-success", root: join(call(0xf1, child, 11, 200000), done()), child: join(call(0xf1, grand, 3, 100000), done()), grand: join(store(2, 19), ret(51)), value: 17},
			{id: "child-out-of-gas", root: join(call(0xf1, child, 0, 20), store(2, 1), done()), child: join(store(1, 8), done())},
			{id: "access-list-failure", root: join(store(2, 3), call(0xf1, child, 0, 150000), done()), child: done()},
			{id: "return-copy", root: join(call(0xf1, child, 0, 150000), []byte{0x50}, p(32), p(0), p(0), []byte{0x3e}, p(32), p(0), []byte{0xf3}), child: ret(77)},
			{id: "return-copy-bounds", root: join(store(2, 3), call(0xf1, child, 0, 150000), []byte{0x50}, p(33), p(0), p(0), []byte{0x3e}), child: ret(77)},
			{id: "transient-delegate", root: join(p(12), p(0), []byte{0x5d}, call(0xf4, child, 0, 150000), []byte{0x50}, p(0), []byte{0x5c}, p(0), []byte{0x52}, p(32), p(0), []byte{0xf3}), child: join(p(33), p(0), []byte{0x5d}, done())},
			{id: "transient-delegate-revert", root: join(p(12), p(0), []byte{0x5d}, call(0xf4, child, 0, 150000), []byte{0x50}, p(0), []byte{0x5c}, p(0), []byte{0x52}, p(32), p(0), []byte{0xf3}), child: join(p(33), p(0), []byte{0x5d}, revert())},
			{id: "refund-across-delegate", root: join(store(0, 0), call(0xf4, child, 0, 150000), done()), child: join(store(0, 1), done())},
			{id: "refund-child-revert", root: join(store(0, 0), call(0xf4, child, 0, 150000), done()), child: join(store(0, 1), revert())},
			{id: "static-parent-value", root: join(call(0xf1, child, 7, 150000), done()), child: done(), readonly: true},
		}
		for _, opcode := range []byte{0xf0, 0xf5} {
			for _, variant := range []string{"success", "revert", "invalid-code", "no-access", "deposit-oog", "collision"} {
				init := ret(0)
				if variant == "revert" {
					init = join(store(1, 7), revert())
				}
				if variant == "invalid-code" {
					init = join(p(0xef), p(0), []byte{0x53}, p(1), p(0), []byte{0xf3})
				}
				if variant == "deposit-oog" {
					init = join(p(20000), p(0), []byte{0xf3})
				}
				rootAddr := addr(loc, 1)
				created := crypto.CreateAddress(rootAddr, 1, init, loc)
				var salt [32]byte
				if opcode == 0xf0 {
					if _, e := created.InternalAndQuaiAddress(); e != nil {
						var e error
						created, _, e = vm.GrindContract(rootAddr, 1, 100000000, 30+6*int64((len(init)+31)/32), crypto.Keccak256Hash(init), u(4000000), loc)
						must(e)
					}
				} else {
					for i := uint64(0); ; i++ {
						copy(salt[:], u(i).FillBytes(make([]byte, 32)))
						created = crypto.CreateAddress2(rootAddr, salt, crypto.Keccak256(init), loc)
						if _, e := created.InternalAndQuaiAddress(); e == nil {
							break
						}
					}
				}
				code := []byte{}
				for i, x := range init {
					code = join(code, p(uint64(x)), p(uint64(i)), []byte{0x53})
				}
				if opcode == 0xf5 {
					code = join(code, append([]byte{0x7f}, salt[:]...))
				}
				code = join(code, p(uint64(len(init))), p(0), p(13), []byte{opcode}, p(0), []byte{0x52}, p(32), p(0), []byte{0xf3})
				c := testcase{id: fmt.Sprintf("create-%x-%s", opcode, variant), root: code, listed: true, gas: 1000000, extra: []common.Address{created}}
				if variant == "no-access" {
					c.extra = nil
				}
				if variant == "collision" {
					c.collide = &created
				}
				cases = append(cases, c)
			}
		}
		for _, opcode := range []byte{0x31, 0x3b, 0x3f} {
			for _, listed := range []bool{false, true} {
				c := testcase{id: fmt.Sprintf("account-op-%x-listed-%v", opcode, listed), root: join(pushAddr(child), []byte{opcode}, p(0), []byte{0x52}, p(32), p(0), []byte{0xf3}), child: ret(42), listed: listed, gas: 1000000}
				vectors = append(vectors, run(c, loc))
			}
		}
		for _, listed := range []bool{false, true} {
			c := testcase{id: fmt.Sprintf("extcodecopy-listed-%v", listed), root: join(p(17), p(0), p(0), pushAddr(child), []byte{0x3c}, p(17), p(0), []byte{0xf3}), child: ret(42), listed: listed, gas: 1000000}
			vectors = append(vectors, run(c, loc))
		}
		for _, requested := range []uint64{3999999, 4000000, 3999743} {
			c := testcase{id: fmt.Sprintf("blockhash-%d", requested), root: join(p(requested), []byte{0x40}, p(0), []byte{0x52}, p(32), p(0), []byte{0xf3}), listed: true, gas: 1000000}
			vectors = append(vectors, run(c, loc))
		}
		for _, prime := range []uint64{1919499, 1919500, 2444574} {
			for _, repeat := range []bool{false, true} {
				root := join(call(0xf1, child, 0, 150000), done())
				if repeat {
					root = join(call(0xf1, child, 0, 150000), []byte{0x50}, call(0xf1, child, 0, 150000), done())
				}
				c := testcase{id: fmt.Sprintf("suicide-%d-repeat-%v", prime, repeat), root: root, child: join(pushAddr(grand), []byte{0xff}), listed: true, gas: 1000000, prime: prime}
				vectors = append(vectors, run(c, loc))
			}
		}
		for _, prime := range []uint64{261999, 262000, 1171500, 1191500, 1755000, 1775000, 1919499, 1919500, 2316999, 2317000, 2444574} {
			for _, mode := range []string{"eligible", "ineligible", "invalid-access", "qi-remote", "local"} {
				to := addr(common.Location{loc[0], (loc[1] + 1) % 16}, 8)
				if mode == "local" {
					to = child
				}
				if mode == "qi-remote" {
					b := to.Bytes()
					b[1] |= 128
					to = common.BytesToAddress(b, loc)
				}
				access := byte(0xc0)
				if mode == "invalid-access" {
					access = 0x80
				}
				code := join(p(uint64(access)), p(32), []byte{0x53}, p(1), p(32), p(0), p(0), p(2), p(3), p(21000), p(13), pushAddr(to), p(0), []byte{0xf6, 0x00})
				c := testcase{id: fmt.Sprintf("etx-%s-prime-%d", mode, prime), root: code, listed: true, gas: 1000000, prime: prime, ineligible: mode == "ineligible"}
				vectors = append(vectors, run(c, loc))
				c.id += "-reverted"
				c.root = join(code[:len(code)-1], revert())
				vectors = append(vectors, run(c, loc))
			}
			to := addr(loc, 9)
			b := to.Bytes()
			b[1] |= 128
			to = common.BytesToAddress(b, loc)
			code := join(p(21000), p(10_000_000_000_000_000_000), pushAddr(to), p(0), []byte{0xf8}, p(0), []byte{0x52}, p(32), p(0), []byte{0xf3})
			c := testcase{id: fmt.Sprintf("convert-prime-%d", prime), root: code, listed: true, gas: 1000000, prime: prime, rich: true}
			vectors = append(vectors, run(c, loc))
			c.id += "-reverted"
			c.root = join(code[:len(code)-1], revert())
			vectors = append(vectors, run(c, loc))
		}
		for i, c := range cases {
			c.gas = 1000000
			c.listed = i != 2
			vectors = append(vectors, run(c, loc))
		}
		for id := byte(1); id <= 9; id++ {
			inputs := [][]byte{{}, {1}, make([]byte, 32), make([]byte, 128)}
			switch id {
			case 1:
				key, e := crypto.ToECDSA(append(make([]byte, 31), byte(1)))
				must(e)
				msg := crypto.Keccak256([]byte("public deterministic stateful fixture"))
				sig, e := crypto.Sign(msg, key)
				must(e)
				encoded := join(msg, make([]byte, 31), []byte{27 + sig[64]}, sig[:64])
				inputs = append(inputs, encoded)
				high := append([]byte{}, encoded...)
				n := crypto.S256().Params().N
				new(big.Int).Sub(n, new(big.Int).SetBytes(encoded[96:128])).FillBytes(high[96:128])
				high[63] = 27 + ((high[63] - 27) ^ 1)
				inputs = append(inputs, high)
			case 5:
				build := func(base, exp, mod []byte) []byte {
					return join(u(uint64(len(base))).FillBytes(make([]byte, 32)), u(uint64(len(exp))).FillBytes(make([]byte, 32)), u(uint64(len(mod))).FillBytes(make([]byte, 32)), base, exp, mod)
				}
				inputs = append(inputs, build([]byte{2}, []byte{10}, []byte{17}), build([]byte{5}, make([]byte, 33), []byte{23}), build([]byte{5}, []byte{1}, []byte{0}), build(make([]byte, 33), []byte{1}, make([]byte, 32)))
			case 6:
				one := new(bn256.G1).ScalarBaseMult(u(1)).Marshal()
				inputs = append(inputs, join(one, one), join(one, make([]byte, 64)), join(make([]byte, 63), []byte{3}))
			case 7:
				one := new(bn256.G1).ScalarBaseMult(u(1)).Marshal()
				inputs = append(inputs, join(one, u(2).FillBytes(make([]byte, 32))), join(one, make([]byte, 32)), join(one, new(big.Int).Sub(new(big.Int).Lsh(u(1), 256), u(1)).FillBytes(make([]byte, 32))))
			case 8:
				one := new(bn256.G1).ScalarBaseMult(u(1))
				neg := new(bn256.G1).Neg(one)
				two := new(bn256.G2).ScalarBaseMult(u(1))
				pair := join(one.Marshal(), two.Marshal())
				inputs = append(inputs, pair, join(pair, neg.Marshal(), two.Marshal()), make([]byte, 191))
			case 9:
				b := make([]byte, 213)
				binary.BigEndian.PutUint32(b[:4], 12)
				for i := 4; i < 212; i++ {
					b[i] = byte(i)
				}
				inputs = append(inputs, b)
				final := append([]byte{}, b...)
				final[212] = 1
				inputs = append(inputs, final)
				bad := append([]byte{}, b...)
				bad[212] = 2
				inputs = append(inputs, bad)
			}
			for i, input := range inputs {
				for _, gas := range []uint64{1000000, 25020} {
					for _, alias := range []bool{false, true} {
						bytes := make([]byte, 20)
						if !alias {
							bytes[0] = loc.BytePrefix()
						}
						bytes[19] = id
						target := common.BytesToAddress(bytes, loc)
						c := testcase{id: fmt.Sprintf("precompile-%d-%d-gas-%d-alias-%v", id, i, gas, alias), target: &target, input: input, gas: gas, listed: true, precompile: true}
						vectors = append(vectors, run(c, loc))
					}
				}
			}
		}
	}
	for _, loc := range []common.Location{{0, 0}, {1, 2}, {15, 15}} {
		rootAddr, child := addr(loc, 1), addr(loc, 2)
		remote := addr(common.Location{(loc[0] + 1) % 16, loc[1]}, 2)
		for _, value := range []uint64{0, 13} {
			for _, listed := range []bool{false, true} {
				vectors = append(vectors, run(testcase{id: fmt.Sprintf("external-nested-call-%d-listed-%t", value, listed), root: join(call(0xf1, remote, value, 100000), done()), listed: listed, gas: 1000000, extra: []common.Address{remote}}, loc))
			}
		}
		_ = child
		lock := common.HexToAddress(fmt.Sprintf("%02x0000000000000000000000000000000000000a", loc.BytePrefix()), loc)
		benef := addr(loc, 6)
		toQiBytes := benef.Bytes()
		toQiBytes[1] |= 128
		toQi := common.BytesToAddress(toQiBytes, loc)
		for _, prime := range []uint64{1754999, 1755000, 2316999, 2317000} {
			for _, mode := range []string{"get", "latest", "claim", "claim-locked", "claim-missing", "deposit", "unwrap", "unwrap-zero", "unwrap-high", "invalid", "static"} {
				for _, nested := range []bool{false, true} {
					input := []byte{}
					switch mode {
					case "get", "claim-locked", "claim-missing", "claim":
						input = append(input, benef.Bytes()...)
						if strings.HasPrefix(mode, "claim") {
							input = append(input, benef.Bytes()...)
							input = append(input, 2)
							input = append(input, 0, 0, 0, 1)
							input = append(input, u(100000).FillBytes(make([]byte, 8))...)
						} else {
							input = append(input, 2, 0, 0, 0, 1)
						}
					case "latest":
						input = append(benef.Bytes(), 2)
					case "deposit":
						input = benef.Bytes()
					case "unwrap", "unwrap-zero", "unwrap-high":
						input = append(input, toQi.Bytes()...)
						v := uint64(2000)
						if mode == "unwrap-zero" {
							v = 0
						}
						if mode == "unwrap-high" {
							v = 10000
						}
						input = append(input, u(v).FillBytes(make([]byte, 32))...)
						input = append(input, u(100000).FillBytes(make([]byte, 8))...)
					case "invalid", "static":
						input = []byte{1, 2, 3}
					}
					name := mode
					var target = &lock
					code := []byte{}
					if nested {
						name = "nested-" + name
						target = nil
						code = join(p(uint64(len(input))), p(0), p(0), []byte{0x37}, p(128), p(256), p(uint64(len(input))), p(0), p(0), pushAddr(lock), p(500000), []byte{0xf1}, p(128), p(256), []byte{0xf3})
					}
					c := testcase{id: fmt.Sprintf("lockup-%s-%d", name, prime), lockup: name, target: target, input: input, root: code, listed: true, extra: []common.Address{lock}, gas: 1000000, prime: prime}
					if mode == "static" {
						c.readonly = true
					}
					vectors = append(vectors, run(c, loc))
					if nested && (mode == "claim" || mode == "unwrap" || mode == "deposit") {
						c.id += "-outer-revert"
						c.root = join(code[:len(code)-6], revert())
						vectors = append(vectors, run(c, loc))
					}
				}
			}
		}
		// Existing storage/account objects are replaced by CREATE, including their original-storage baseline.
		init := join(store(0, 9), p(0), p(0), []byte{0xf3})
		created := crypto.CreateAddress(rootAddr, 1, init, loc)
		if _, e := created.InternalAndQuaiAddress(); e != nil {
			var e error
			created, _, e = vm.GrindContract(rootAddr, 1, 100000000, 30+6*int64((len(init)+31)/32), crypto.Keccak256Hash(init), u(4000000), loc)
			must(e)
		}
		code := []byte{}
		for i, x := range init {
			code = join(code, p(uint64(x)), p(uint64(i)), []byte{0x53})
		}
		code = join(code, p(uint64(len(init))), p(0), p(0), []byte{0xf0}, done())
		vectors = append(vectors, run(testcase{id: "create-reset-storage", root: code, listed: true, extra: []common.Address{created}, reset: &created, gas: 1000000}, loc))
	}

	// Gas-only bounds on the call path. Each case passes a cap that the native
	// code had and go-quai never had: 131,072 bytes of call input, 4,096
	// access-list tuples or keys, 4,096 opETX access-list entries, 20-byte
	// opETX access-list addresses, 1 MiB of precompile input and 1 MiB of
	// modexp operands, and 64-bit modexp lengths.
	{
		loc := common.Location{0, 0}
		remote := addr(common.Location{0, 1}, 8)
		precompile := func(id byte) common.Address {
			b := make([]byte, 20)
			b[0] = loc.BytePrefix()
			b[19] = id
			return common.BytesToAddress(b, loc)
		}
		input := make([]byte, 140000)
		for i := range input {
			input[i] = byte(i*7 + i/251)
		}
		hashInput := join([]byte{0x36}, p(0), p(0), []byte{0x37, 0x36}, p(0), []byte{0x20}, p(0), []byte{0x52}, p(139968), []byte{0x35}, p(32), []byte{0x52, 0x36}, p(64), []byte{0x52}, p(96), p(0), []byte{0xf3})
		vectors = append(vectors, run(testcase{id: "bounds-call-input-140000", root: hashInput, input: input, listed: true, gas: 1000000}, loc))
		gasLeft := join([]byte{0x5a}, p(0), []byte{0x52}, p(32), p(0), []byte{0xf3})
		vectors = append(vectors, run(testcase{id: "bounds-access-keys-4097", root: join(p(4096), []byte{0x54, 0x50}, gasLeft), listed: true, keys: 4097, gas: 1000000}, loc))
		extra := []common.Address{}
		for i := 0; i < 4097; i++ {
			b := make([]byte, 20)
			b[0] = loc.BytePrefix()
			b[1] = 0x33
			b[2] = byte(i >> 8)
			b[3] = byte(i)
			extra = append(extra, common.BytesToAddress(b, loc))
		}
		vectors = append(vectors, run(testcase{id: "bounds-access-tuples-4100", root: join(pushAddr(extra[4096]), []byte{0x31, 0x50}, gasLeft), listed: true, extra: extra, gas: 1000000}, loc))
		// opETX reads its access list from calldata: no fee, value 13.
		etx := func(limit uint64) []byte {
			return join([]byte{0x36}, p(0), p(0), []byte{0x37, 0x36}, p(0), p(0), p(0), p(0), p(0), p(limit), p(13), pushAddr(remote), p(0), []byte{0xf6}, p(0), []byte{0x52}, p(32), p(0), []byte{0xf3})
		}
		keys := []common.Hash{}
		for i := 0; i < 4097; i++ {
			keys = append(keys, common.BigToHash(u(uint64(i))))
		}
		list, e := rlp.EncodeToBytes(types.AccessList{{Address: addr(loc, 2), StorageKeys: keys}})
		must(e)
		vectors = append(vectors, run(testcase{id: "bounds-etx-access-keys-4097", root: etx(8000000), input: list, listed: true, gas: 1000000}, loc))
		short := addr(loc, 2).Bytes()[1:]
		long := append([]byte{0x77}, addr(loc, 3).Bytes()...)
		odd, e := rlp.EncodeToBytes([]interface{}{[]interface{}{short, []common.Hash{}}, []interface{}{long, []common.Hash{{31: 5}}}, []interface{}{[]byte{}, []common.Hash{}}})
		must(e)
		vectors = append(vectors, run(testcase{id: "bounds-etx-access-address-lengths", root: etx(100000), input: odd, listed: true, gas: 1000000}, loc))
		identity := join(p(0xabcdef), p(0x10ffe0), []byte{0x52}, p(0), p(0), p(0x110000), p(0), p(0), pushAddr(precompile(4)), []byte{0x5a, 0xf1, 0x50, 0x3d}, p(0), p(0), []byte{0x3e, 0x3d}, p(0), []byte{0x20}, p(0), []byte{0x52, 0x3d}, p(32), []byte{0x52}, p(64), p(0), []byte{0xf3})
		vectors = append(vectors, run(testcase{id: "bounds-identity-input-1114112", root: identity, listed: true, gas: 5000000}, loc))
		modexp := join(p(1), p(0), []byte{0x52}, p(1048577), p(32), []byte{0x52}, p(1), p(64), []byte{0x52}, p(3), p(96), []byte{0x53}, p(1), p(97), []byte{0x53}, p(11), p(1048674), []byte{0x53}, p(1), p(0), p(1048675), p(0), pushAddr(precompile(5)), []byte{0x5a, 0xfa}, p(32), []byte{0x52}, p(64), p(0), []byte{0xf3})
		vectors = append(vectors, run(testcase{id: "bounds-modexp-exponent-1048577", root: modexp, listed: true, gas: 8000000}, loc))
		huge := join(make([]byte, 32), new(big.Int).Lsh(u(1), 70).FillBytes(make([]byte, 32)), make([]byte, 32))
		target := precompile(5)
		vectors = append(vectors, run(testcase{id: "bounds-modexp-zero-lengths-huge-exponent", target: &target, input: huge, listed: true, precompile: true, gas: 1000000}, loc))
	}

	chain := *params.TestChainConfig
	chain.ChainID = u(1337)
	genesis := []interface{}{}
	for _, loc := range []common.Location{{}, {0}, {0, 0}} {
		chain.Location = loc
		g := &core.Genesis{Config: &chain, Timestamp: 1769112000, ExtraData: []byte("quai-rust-private-v1"), GasLimit: 30000000, Difficulty: u(1)}
		b := g.ToBlock(0)
		wire, e := json.Marshal(b.Hash())
		must(e)
		_ = wire
		genesis = append(genesis, map[string]interface{}{"location": func() []int {
			v := []int{}
			for _, b := range loc {
				v = append(v, int(b))
			}
			return v
		}(), "hash": b.Hash().Hex(), "body_hash": b.Header().Hash().Hex()})
	}
	out := map[string]interface{}{"reference_revision": referenceRevision, "scope": "Actual v0.57.0 EVM calls on committed deterministic private pre-state. No transaction settlement, public peers or full block authority.", "vectors": vectors, "private_genesis": genesis}
	must(json.NewEncoder(os.Stdout).Encode(out))
}
