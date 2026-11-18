// Offline-only VM differential oracle. No network, wallets or runtime forwarding.
package main

import (
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/core/vm"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"math/big"
	"math/rand"
	"os"
	"sort"
	"time"
)

var referenceRevision string

func must(e error) {
	if e != nil {
		panic(e)
	}
}
func hx(b []byte) string     { return "0x" + hex.EncodeToString(b) }
func word(v *big.Int) []byte { return v.FillBytes(make([]byte, 32)) }
func u(v uint64) *big.Int    { return new(big.Int).SetUint64(v) }
func push(v []byte) []byte {
	if len(v) == 0 {
		v = []byte{0}
	}
	if len(v) > 32 {
		panic("push width")
	}
	return append([]byte{byte(0x5f + len(v))}, v...)
}
func p(v uint64) []byte { return push(u(v).Bytes()) }
func join(parts ...[]byte) []byte {
	var b []byte
	for _, part := range parts {
		b = append(b, part...)
	}
	return b
}
func output(code []byte) []byte { return join(code, p(0), []byte{0x52}, p(32), p(0), []byte{0xf3}) }

type trace struct {
	hash    common.Hash
	steps   uint64
	opcodes map[byte]bool
}

func (t *trace) CaptureStart(*vm.EVM, common.Address, common.Address, bool, []byte, uint64, *big.Int) {
}
func (t *trace) CaptureEnd([]byte, uint64, time.Duration, error) {}
func (t *trace) CaptureFault(*vm.EVM, uint64, vm.OpCode, uint64, uint64, *vm.ScopeContext, int, error) {
}
func (t *trace) CaptureState(env *vm.EVM, pc uint64, op vm.OpCode, gas, cost uint64, scope *vm.ScopeContext, _ []byte, _ int, err error, _ common.Location) {
	if err != nil {
		return
	}
	data := append([]byte{}, t.hash.Bytes()...)
	add := func(n uint64) { var b [8]byte; binary.BigEndian.PutUint64(b[:], n); data = append(data, b[:]...) }
	add(pc)
	data = append(data, byte(op))
	add(gas)
	add(scope.Contract.StateGas)
	add(env.StateDB.GetRefund())
	add(uint64(len(scope.Stack.Data())))
	for _, value := range scope.Stack.Data() {
		b := value.Bytes32()
		data = append(data, b[:]...)
	}
	add(uint64(scope.Memory.Len()))
	data = append(data, crypto.Keccak256(scope.Memory.Data())...)
	t.hash = crypto.Keccak256Hash(data)
	t.steps++
	if t.opcodes == nil {
		t.opcodes = map[byte]bool{}
	}
	t.opcodes[byte(op)] = true
}
func errorClass(err error) interface{} {
	if err == nil || errors.Is(err, vm.ErrExecutionReverted) {
		return nil
	}
	for _, v := range []struct {
		err  error
		name string
	}{{vm.ErrOutOfGas, "out_of_gas"}, {vm.ErrInvalidJump, "invalid_jump"}, {vm.ErrWriteProtection, "write_protection"}, {vm.ErrReturnDataOutOfBounds, "return_data_bounds"}, {vm.ErrGasUintOverflow, "gas_overflow"}, {vm.ErrInvalidAccessList, "invalid_access_list"}} {
		if errors.Is(err, v.err) {
			return v.name
		}
	}
	var under *vm.ErrStackUnderflow
	if errors.As(err, &under) {
		return "stack_underflow"
	}
	var over *vm.ErrStackOverflow
	if errors.As(err, &over) {
		return "stack_overflow"
	}
	var opcode *vm.ErrInvalidOpCode
	if errors.As(err, &opcode) {
		return "invalid_opcode"
	}
	panic(err)
}

type testCase struct {
	id                     string
	code, input            []byte
	gas                    uint64
	height                 uint64
	readonly, listed, warm bool
	initial                map[common.Hash]common.Hash
	txType                 byte
	stateSize              *big.Int
	location               common.Location
}

func runCase(c testCase, rng *rand.Rand) map[string]interface{} {
	location := c.location
	if location == nil {
		location = common.Location{0, 0}
	}
	makeAddr := func(n byte) common.Address {
		b := make([]byte, 20)
		b[0] = location.BytePrefix()
		b[18] = 0x11
		b[19] = n
		return common.BytesToAddress(b, location)
	}
	address, caller, coinbase := makeAddr(1), makeAddr(2), makeAddr(3)
	a, _ := address.InternalAndQuaiAddress()
	ca, _ := caller.InternalAndQuaiAddress()
	disk := rawdb.NewMemoryDatabase(log.Global)
	defer disk.Close()
	cache := state.NewDatabase(disk)
	s, e := state.New(types.EmptyRootHash, types.EmptyRootHash, big.NewInt(0), cache, cache, nil, location, log.Global)
	must(e)
	s.CreateAccount(a)
	s.SetCode(a, c.code)
	s.SetNonce(a, 7)
	s.SetBalance(a, big.NewInt(987654321))
	s.CreateAccount(ca)
	s.SetNonce(ca, 1)
	s.SetBalance(ca, big.NewInt(123456789))
	for k, v := range c.initial {
		s.SetState(a, k, v)
	}
	before, e := s.Commit(false)
	must(e)
	must(cache.TrieDB().Commit(before, false, nil))
	s, e = state.New(before, types.EmptyRootHash, big.NewInt(0), cache, cache, nil, location, log.Global)
	must(e)
	keys := []common.Hash{}
	for k := range c.initial {
		keys = append(keys, k)
	}
	sort.Slice(keys, func(i, j int) bool { return keys[i].Hex() < keys[j].Hex() })
	account := func(addr common.Address, code []byte, slots []common.Hash) map[string]interface{} {
		internal, _ := addr.InternalAndQuaiAddress()
		storage := []interface{}{}
		for _, key := range slots {
			v := s.GetState(internal, key)
			if v != (common.Hash{}) {
				storage = append(storage, map[string]interface{}{"key": key.Hex(), "value": v.Hex()})
			}
		}
		return map[string]interface{}{"address": hx(addr.Bytes()), "nonce": s.GetNonce(internal), "balance": hx(word(s.GetBalance(internal))), "code": hx(code), "storage_size": hx(word(s.GetSize(internal))), "storage": storage}
	}
	world := []interface{}{account(address, c.code, keys), account(caller, nil, nil)}
	s.Prepare(common.Hash{31: 7}, 0)
	if c.listed {
		s.AddAddressToAccessList(address.Bytes20())
	}
	warmKeys := []string{}
	if c.warm {
		for _, key := range []common.Hash{{}, {31: 1}, {31: 2}, {31: 3}} {
			s.AddSlotToAccessList(address.Bytes20(), key)
			warmKeys = append(warmKeys, key.Hex())
		}
	}
	snapshot := s.Snapshot()
	tr := new(trace)
	stateSize := c.stateSize
	if stateSize == nil {
		stateSize = big.NewInt(1 << 20)
	}
	etxSender := makeAddr(9)
	value := big.NewInt(1234)
	timestamp := new(big.Int).SetUint64(rng.Uint64())
	difficulty := new(big.Int).Lsh(big.NewInt(1), 128)
	baseFee := big.NewInt(13)
	gasPrice := big.NewInt(17)
	blockGasLimit := uint64(30000000)
	chain := *params.TestChainConfig
	chain.Location = location
	chain.ChainID = big.NewInt(9)
	env := vm.NewEVM(vm.BlockContext{BlockNumber: u(c.height), Time: timestamp, Difficulty: difficulty, BaseFee: baseFee, QuaiStateSize: stateSize, GasLimit: blockGasLimit, PrimaryCoinbase: coinbase, PrimeTerminusNumber: c.height}, vm.TxContext{Origin: caller, GasPrice: gasPrice, TxType: c.txType, ETXSender: etxSender}, s, &chain, vm.Config{Debug: true, Tracer: tr}, nil)
	contract := vm.NewContract(vm.AccountRef(caller), vm.AccountRef(address), value, c.gas)
	contract.SetCallCode(&address, crypto.Keccak256Hash(c.code), c.code)
	ret, err := env.Interpreter().Run(contract, c.input, c.readonly)
	left := contract.Gas
	reverted := errors.Is(err, vm.ErrExecutionReverted)
	if err != nil {
		s.RevertToSnapshot(snapshot)
		if !reverted {
			left = 0
		}
	}
	refund := s.GetRefund()
	logs := []interface{}{}
	for _, log := range s.Logs() {
		topics := []string{}
		for _, h := range log.Topics {
			topics = append(topics, h.Hex())
		}
		logs = append(logs, map[string]interface{}{"address": hx(log.Address.Bytes()), "topics": topics, "data": hx(log.Data)})
	}
	after := s.IntermediateRoot(false)
	// Query every initial slot plus low keys used by the generated programs.
	probes := append([]common.Hash{}, keys...)
	for i := byte(0); i < 8; i++ {
		key := common.Hash{31: i}
		found := false
		for _, k := range probes {
			if k == key {
				found = true
			}
		}
		if !found {
			probes = append(probes, key)
		}
	}
	sort.Slice(probes, func(i, j int) bool { return probes[i].Hex() < probes[j].Hex() })
	storage := []interface{}{}
	for _, k := range probes {
		v := s.GetState(a, k)
		storage = append(storage, map[string]interface{}{"key": k.Hex(), "value": v.Hex()})
	}
	var message interface{}
	if err != nil {
		message = err.Error()
	}
	loc := []int{int(location[0]), int(location[1])}
	observed := []int{}
	for op := range tr.opcodes {
		observed = append(observed, int(op))
	}
	sort.Ints(observed)
	return map[string]interface{}{"id": c.id, "world": world, "context": map[string]interface{}{"address": hx(address.Bytes()), "caller": hx(caller.Bytes()), "origin": hx(caller.Bytes()), "coinbase": hx(coinbase.Bytes()), "etx_sender": hx(etxSender.Bytes()), "location": loc, "tx_type": c.txType, "value": hx(word(value)), "gas_price": hx(word(gasPrice)), "block_number": hx(word(u(c.height))), "timestamp": hx(word(timestamp)), "difficulty": hx(word(difficulty)), "base_fee": hx(word(baseFee)), "chain_id": hx(word(big.NewInt(9))), "state_size": hx(word(stateSize)), "block_gas_limit": blockGasLimit, "address_listed": c.listed, "warm_slots": warmKeys, "read_only": c.readonly}, "input": hx(c.input), "gas": c.gas, "before_root": before.Hex(), "after_root": after.Hex(), "output": hx(ret), "error": errorClass(err), "reference_error": message, "reverted": reverted, "gas_remaining": left, "state_gas": contract.StateGas, "refund": refund, "logs": logs, "storage": storage, "storage_size": hx(word(s.GetSize(a))), "trace_hash": tr.hash.Hex(), "trace_steps": tr.steps, "trace_opcode_values": observed}
}
func main() {
	rng := rand.New(rand.NewSource(2026110911))
	cases := []testCase{}
	add := func(id string, code []byte) {
		cases = append(cases, testCase{id: id, code: code, gas: 3000000, height: 4000000, listed: true})
	}
	words := [][]byte{{}, {1}, {2}, {255}, {1, 0}, word(new(big.Int).Sub(new(big.Int).Lsh(big.NewInt(1), 64), big.NewInt(1))), word(new(big.Int).Lsh(big.NewInt(1), 128)), word(new(big.Int).Sub(new(big.Int).Lsh(big.NewInt(1), 255), big.NewInt(1))), word(new(big.Int).Lsh(big.NewInt(1), 255)), word(new(big.Int).Sub(new(big.Int).Lsh(big.NewInt(1), 256), big.NewInt(1)))}
	randomWord := func() []byte { v := make([]byte, 32); rng.Read(v); return v }
	operations := []byte{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d}
	for _, op := range operations {
		for i := 0; i < 48; i++ {
			a, b, m := randomWord(), randomWord(), randomWord()
			if i < 30 {
				a = words[i%len(words)]
				b = words[(i*7+1)%len(words)]
				m = words[(i*3)%len(words)]
			}
			var code []byte
			if op == 8 || op == 9 {
				code = append(code, push(m)...)
			}
			if op != 0x15 && op != 0x19 {
				code = append(code, push(b)...)
			}
			code = append(code, push(a)...)
			code = append(code, op)
			add(fmt.Sprintf("arith-%02x-%d", op, i), output(code))
		}
	}
	for _, op := range []byte{0x30, 0x32, 0x33, 0x34, 0x36, 0x38, 0x3a, 0x3d, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x58, 0x59, 0x5a} {
		for kind := 0; kind < 3; kind++ {
			add(fmt.Sprintf("environment-%02x-%d", op, kind), output([]byte{op}))
			cases[len(cases)-1].txType = byte(kind)
			cases[len(cases)-1].input = []byte{1, 2, 3, 0, 255}
		}
	}
	for _, n := range []uint64{0, 1, 31, 32, 33, 63, 64, 65, 1024} {
		add(fmt.Sprintf("memory-load-%d", n), output(join(push(randomWord()), p(n), []byte{0x52}, p(n), []byte{0x51})))
		add(fmt.Sprintf("memory-store8-%d", n), join(p(0x1234), p(n), []byte{0x53}, p(n+1), p(0), []byte{0xf3}))
		add(fmt.Sprintf("calldata-%d", n), output(join(p(n), []byte{0x35})))
		cases[len(cases)-1].input = append(randomWord(), randomWord()...)
		add(fmt.Sprintf("calldatacopy-%d", n), join(p(n), p(3), p(5), []byte{0x37}, p(n+5), p(0), []byte{0xf3}))
		cases[len(cases)-1].input = append(randomWord(), randomWord()...)
		add(fmt.Sprintf("codecopy-%d", n), join(p(n), p(0), p(0), []byte{0x39}, p(n), p(0), []byte{0xf3}))
		add(fmt.Sprintf("sha3-%d", n), output(join(push(randomWord()), p(0), []byte{0x52}, p(n), p(0), []byte{0x20})))
	}
	for n := 1; n <= 32; n++ {
		add(fmt.Sprintf("push-width-%d", n), output(join(append([]byte{byte(0x5f + n)}, randomWord()[:n]...))))
	}
	add("truncated-push", []byte{0x7f, 1, 2, 3})
	add("empty-code", nil)
	add("explicit-stop", []byte{0})
	add("implicit-stop", p(42))
	for depth := 1; depth <= 16; depth++ {
		code := []byte{}
		for j := 1; j <= depth+1; j++ {
			code = append(code, p(uint64(j))...)
		}
		code = append(code, byte(0x7f+depth))
		add(fmt.Sprintf("dup-%d", depth), output(code))
		code = code[:len(code)-1]
		code = append(code, byte(0x8f+depth))
		add(fmt.Sprintf("swap-%d", depth), output(code))
	}
	add("jump-valid", []byte{0x60, 4, 0x56, 0xfe, 0x5b, 0x60, 42, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3})
	add("jump-push-data", []byte{0x60, 4, 0x56, 0x60, 0x5b, 0})
	add("jumpi-false-invalid-target", join(p(0), push(words[9]), []byte{0x57, 0}))
	add("jump-overflow", join(push(words[9]), []byte{0x56}))
	add("loop-out-of-gas", []byte{0x5b, 0x60, 0, 0x56})
	cases[len(cases)-1].gas = 90
	for _, op := range []byte{0x01, 0x08, 0x20, 0x37, 0x50, 0x51, 0x52, 0x54, 0x55, 0x57, 0x8f, 0x9f, 0xa4, 0xf3, 0xfd, 0xf7} {
		add(fmt.Sprintf("underflow-%02x", op), []byte{op})
	}
	add("stack-overflow", func() []byte {
		code := []byte{}
		for i := 0; i < 1025; i++ {
			code = append(code, 0x60, 1)
		}
		return code
	}())
	for _, op := range []byte{0x0c, 0x0f, 0x21, 0x49, 0xb0, 0xf9, 0xfe} {
		add(fmt.Sprintf("invalid-%02x", op), []byte{op})
	}
	for _, gas := range []uint64{0, 2, 3, 5, 8, 20, 100} {
		add(fmt.Sprintf("out-of-gas-%d", gas), output(join(p(42), p(7), []byte{1})))
		cases[len(cases)-1].gas = gas
	}
	for _, op := range []byte{0x51, 0x52, 0x53, 0x20, 0xf3, 0xfd} {
		code := join(push(words[9]), []byte{op})
		if op == 0x52 || op == 0x53 {
			code = join(p(1), code)
		}
		if op == 0x20 || op == 0xf3 || op == 0xfd {
			code = join(p(1), push(words[9]), []byte{op})
		}
		add(fmt.Sprintf("memory-overflow-%02x", op), code)
	}
	add("zero-size-huge-offset-return", join(p(0), push(words[9]), []byte{0xf3}))
	add("zero-size-huge-offset-sha3", output(join(p(0), push(words[9]), []byte{0x20})))
	add("return-data-bounds", join(p(1), p(0), p(0), []byte{0x3e}))
	add("return-data-empty", join(p(0), p(0), p(0), []byte{0x3e, 0}))
	add("return-data-zero-length-bad-offset", join(p(0), p(1), p(0), []byte{0x3e}))
	for i := 0; i < 96; i++ {
		initial := map[common.Hash]common.Hash{}
		if i%3 != 0 {
			initial[common.Hash{}] = common.Hash{31: 7}
			initial[common.Hash{31: 1}] = common.Hash{31: 9}
		}
		code := join(p(11), p(0), []byte{0x55}, p(0), []byte{0x54}, p(0), []byte{0x52}, p(13), p(2), []byte{0x55}, p(0), p(1), []byte{0x55})
		if i%4 == 0 {
			code = join(code, p(0), p(0), []byte{0x55})
		}
		if i%4 == 1 {
			code = join(code, p(7), p(0), []byte{0x55})
		}
		if i%4 == 2 {
			code = join(code, p(11), p(0), []byte{0x55})
		}
		code = join(code, p(32), p(0), []byte{0xf3})
		if i%8 == 0 {
			code[len(code)-1] = 0xfd
		}
		if i%8 == 1 {
			code[len(code)-1] = 0xfe
		}
		add(fmt.Sprintf("storage-%d", i), code)
		c := &cases[len(cases)-1]
		c.initial = initial
		c.warm = i%2 == 1 && i%13 != 0
		c.readonly = i%11 == 0
		c.listed = i%13 != 0
		c.stateSize = new(big.Int).Lsh(big.NewInt(1), uint(i%128))
		if i%17 == 0 {
			c.gas = 2500
		}
	}

	for i := 0; i < 64; i++ {
		initial := map[common.Hash]common.Hash{}
		code := []byte{}
		for j := 0; j < i%17; j++ {
			k := common.BytesToHash(randomWord())
			v := common.BytesToHash(randomWord())
			initial[k] = v
			code = join(code, push(k.Bytes()), []byte{0x54, 0x50})
			if j%2 == 0 {
				code = join(code, p(0), push(k.Bytes()), []byte{0x55})
			}
		}
		code = join(code, push(randomWord()), push(randomWord()), []byte{0x55, 0})
		add(fmt.Sprintf("storage-random-%d", i), code)
		c := &cases[len(cases)-1]
		c.initial = initial
		c.stateSize = new(big.Int).SetBytes(randomWord())
	}
	for topics := 0; topics <= 4; topics++ {
		for mode := 0; mode < 3; mode++ {
			code := join(push(randomWord()), p(0), []byte{0x52})
			for j := topics; j > 0; j-- {
				code = append(code, p(uint64(j))...)
			}
			code = join(code, p(32), p(0), []byte{byte(0xa0 + topics)}, p(32), p(0), []byte{0xf3})
			if mode == 1 {
				code[len(code)-1] = 0xfd
			}
			add(fmt.Sprintf("log-%d-%d", topics, mode), code)
			cases[len(cases)-1].readonly = mode == 2
		}
	}
	for _, height := range []uint64{params.MaxCodeSizeForkHeight - 1, params.MaxCodeSizeForkHeight, params.MaxCodeSizeForkHeight + 1} {
		add(fmt.Sprintf("push0-fork-%d", height), output([]byte{0x5f}))
		cases[len(cases)-1].height = height
		add(fmt.Sprintf("transient-fork-%d", height), output(join(p(123), p(0), []byte{0x5d}, p(0), []byte{0x5c})))
		cases[len(cases)-1].height = height
		add(fmt.Sprintf("mcopy-fork-%d", height), join(push(randomWord()), p(0), []byte{0x52}, p(31), p(0), p(1), []byte{0x5e}, p(32), p(0), []byte{0xf3}))
		cases[len(cases)-1].height = height
	}
	for i := 0; i < 48; i++ {
		data := make([]byte, 20)
		data[0] = byte(i % 16)
		if i >= 32 {
			data[0] ^= 0x10
		}
		data[1] = byte((i/16)%2) * 128
		add(fmt.Sprintf("is-internal-%d", i), output(join(push(data), []byte{0xf7})))
		cases[len(cases)-1].location = common.Location{0, byte(i & 15)}
	}
	vectors := []interface{}{}
	for _, c := range cases {
		vectors = append(vectors, runCase(c, rng))
	}
	sizes := []*big.Int{big.NewInt(0), big.NewInt(1), big.NewInt(2), big.NewInt(3), big.NewInt(4), big.NewInt(5), big.NewInt(16), big.NewInt(17), big.NewInt(31), big.NewInt(1 << 20), new(big.Int).Lsh(big.NewInt(1), 64), new(big.Int).Lsh(big.NewInt(1), 128), new(big.Int).Lsh(big.NewInt(1), 255), new(big.Int).Sub(new(big.Int).Lsh(big.NewInt(1), 256), big.NewInt(1))}
	for i := 0; i < 8; i++ {
		sizes = append(sizes, new(big.Int).SetBytes(randomWord()))
	}
	gasVectors := []interface{}{}
	for _, stateSize := range sizes {
		for _, contract := range sizes {
			for _, rate := range []uint64{0, 100, 2100, 2300, 2600, 5000, 20000, 25000} {
				value := params.CalculateGasWithStateScaling(stateSize, contract, rate)
				gasVectors = append(gasVectors, map[string]interface{}{"state": hx(word(stateSize)), "contract": hx(word(contract)), "base": rate, "gas": value})
			}
		}
	}
	valid := []int{}
	table := vm.NewInstructionSet()
	for i := 0; i < 256; i++ {
		if table[vm.OpCode(i)] != nil {
			valid = append(valid, i)
		}
	}
	result := map[string]interface{}{"schema": 1, "reference_revision": referenceRevision, "kind": "serial-vm", "scope": "Bounded isolated interpreter frames with account/storage roots, gas/refund/logs and trace hashes. No calls/creation/precompiles/value-transfer/transaction-fees or full block validity.", "new_ops_height": params.MaxCodeSizeForkHeight, "go_instruction_set": valid, "vectors": vectors, "state_gas": gasVectors}
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	must(encoder.Encode(result))
}
