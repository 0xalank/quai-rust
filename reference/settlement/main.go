// Actual signed transaction settlement on committed deterministic private state.
package main

import (
	"bytes"
	"crypto/ecdsa"
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
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"google.golang.org/protobuf/proto"
	"math/big"
	"os"
)

var referenceRevision string

func must(e error) {
	if e != nil {
		panic(e)
	}
}
func u(v uint64) *big.Int { return new(big.Int).SetUint64(v) }
func hx(b []byte) string  { return "0x" + hex.EncodeToString(b) }
func w(n uint64) string   { return hx(u(n).FillBytes(make([]byte, 32))) }
func addr(loc common.Location, n byte) common.Address {
	b := make([]byte, 20)
	b[0] = loc.BytePrefix()
	b[18] = 0x22
	b[19] = n
	return common.BytesToAddress(b, loc)
}
func boundsAddress(loc common.Location, i int) common.Address {
	b := make([]byte, 20)
	b[0] = loc.BytePrefix()
	b[1] = 0x33
	b[2] = byte(i >> 8)
	b[3] = byte(i)
	return common.BytesToAddress(b, loc)
}
func key(loc common.Location) *ecdsa.PrivateKey {
	for i := uint64(1); ; i++ {
		k, e := crypto.ToECDSA(u(i).FillBytes(make([]byte, 32)))
		must(e)
		a := crypto.PubkeyToAddress(k.PublicKey, loc)
		if _, e := a.InternalAndQuaiAddress(); e == nil {
			return k
		}
	}
}
func errorClass(e error) interface{} {
	if e == nil {
		return nil
	}
	for _, x := range []struct {
		e error
		n string
	}{{core.ErrNonceTooHigh, "nonce_too_high"}, {core.ErrNonceTooLow, "nonce_too_low"}, {core.ErrSenderNoEOA, "sender_not_eoa"}, {core.ErrFeeCapTooLow, "fee_below_base"}, {core.ErrInsufficientFunds, "insufficient_funds"}, {core.ErrIntrinsicGas, "intrinsic_gas"}, {types.ErrGasLimitReached, "block_gas"}} {
		if errors.Is(e, x.e) {
			return x.n
		}
	}
	return "consensus_error"
}
func vmClass(e error) interface{} {
	if e == nil || errors.Is(e, vm.ErrExecutionReverted) {
		return nil
	}
	for _, x := range []struct {
		e error
		n string
	}{{vm.ErrOutOfGas, "out_of_gas"}, {vm.ErrWriteProtection, "write_protection"}, {vm.ErrInvalidAccessList, "invalid_access_list"}, {vm.ErrCodeStoreOutOfGas, "code_store_out_of_gas"}, {vm.ErrInsufficientBalance, "insufficient_balance"}, {vm.ErrReturnDataOutOfBounds, "return_data_bounds"}} {
		if errors.Is(e, x.e) {
			return x.n
		}
	}
	var op *vm.ErrInvalidOpCode
	if errors.As(e, &op) {
		return "invalid_opcode"
	}
	if errors.Is(e, vm.ErrInvalidJump) {
		return "invalid_jump"
	}
	return "vm_error"
}
func run(kind string, loc common.Location, prime uint64, finalized bool) map[string]interface{} {
	k := key(loc)
	sender := crypto.PubkeyToAddress(k.PublicKey, loc)
	recipient := addr(loc, 1)
	child := addr(loc, 2)
	coinbase := addr(loc, 3)
	disk := rawdb.NewMemoryDatabase(log.Global)
	defer disk.Close()
	cache := state.NewDatabase(disk)
	s, e := state.New(types.EmptyRootHash, types.EmptyRootHash, u(0), cache, cache, nil, loc, log.Global)
	must(e)
	code := []byte{}
	data := []byte{}
	gas := uint64(100000)
	price := uint64(17)
	value := uint64(13)
	nonce := uint64(7)
	pool := uint64(20000000)
	creation := false
	switch kind {
	case "transfer":
		gas = 21000
	case "self":
		recipient = sender
	case "storage":
		code = []byte{0x60, 0, 0x60, 0, 0x55, 0x60, 7, 0x60, 1, 0x55, 0x00}
	case "revert":
		code = []byte{0x60, 7, 0x60, 1, 0x55, 0x60, 0, 0x60, 0, 0xfd}
	case "fault":
		code = []byte{0xfe}
	case "nonce-high":
		nonce = 8
	case "nonce-low":
		nonce = 6
	case "fee-low":
		price = 12
	case "funds":
		value = 10000000000000000
	case "block-gas":
		pool = 99999
	case "intrinsic":
		gas = 21000
		data = []byte{1}
	case "empty-recipient", "clear-empty", "empty-untouched":
	case "static-empty", "static-empty-revert":
		code = []byte{0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x73}
		code = append(code, child.Bytes()...)
		code = append(code, 0x62, 0x01, 0x86, 0xa0, 0xfa)
		if kind == "static-empty-revert" {
			code = append(code, 0x60, 0, 0x60, 0, 0xfd)
		} else {
			code = append(code, 0)
		}
		value = 0
	case "sender-code":
	case "precompile":
		b := make([]byte, 20)
		b[0] = loc.BytePrefix()
		b[19] = 2
		recipient = common.BytesToAddress(b, loc)
	case "create":
		creation = true
		data = []byte{0x60, 0, 0x60, 0, 0x52, 0x60, 1, 0x60, 0, 0xf3}
		gas = 1000000
	case "create-revert":
		creation = true
		data = []byte{0x60, 0, 0x60, 0, 0xfd}
		gas = 1000000
	case "create-deposit-oog":
		creation = true
		data = []byte{0x61, 0x4e, 0x20, 0x60, 0, 0xf3}
		gas = 1000000
	case "suicide-other":
		recipient = sender
		data = append([]byte("Suicide"), child.Bytes()...)
	case "suicide-self":
		recipient = sender
		data = append([]byte("Suicide"), sender.Bytes()...)
	case "suicide-lowercase":
		recipient = sender
		data = append([]byte("suicide"), sender.Bytes()...)
	case "bounds-data-log":
		// 140,000 bytes of call data, copied to memory and logged once.
		code = []byte{0x36, 0x60, 0, 0x60, 0, 0x37, 0x36, 0x60, 0, 0xa0, 0x00}
		data = make([]byte, 140000)
		for i := range data {
			data[i] = byte(i*7 + i/251)
		}
		gas = 5000000
	case "bounds-wire":
		// 1,048,577 zero bytes of call data: the signed transaction exceeds 1 MiB.
		code = []byte{0x36, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3}
		data = make([]byte, 1048577)
		gas = 5000000
	case "bounds-logs":
		// 4,100 LOG0 entries of one byte each.
		code = []byte{0x61, 0x10, 0x04, 0x5b, 0x80, 0x60, 0, 0x53, 0x60, 1, 0x60, 0, 0xa0, 0x60, 1, 0x90, 0x03, 0x80, 0x60, 3, 0x57, 0x00}
		gas = 3000000
	case "deep-calls":
		// CALL(GAS, ADDRESS, 0, 0, 0, 0, 0): the contract calls itself with all
		// the gas a frame may pass on (63/64), so the call chain only ends when a
		// frame runs out of gas, more than 500 frames deep.
		code = []byte{0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x30, 0x5a, 0xf1, 0x00}
		value = 0
		gas = 30000000
		pool = 30000000
	case "bounds-access":
		// 4,098 access-list tuples and 4,097 keys; the last tuple and key are read.
		code = append([]byte{0x61, 0x10, 0x00, 0x54, 0x50, 0x73}, boundsAddress(loc, 4096).Bytes()...)
		code = append(code, 0x31, 0x50, 0x00)
		gas = 20000000
		pool = 30000000
	}
	addresses := []common.Address{sender, child}
	codes := [][]byte{nil, nil}
	if !recipient.Equal(sender) {
		addresses = append(addresses, recipient)
		codes = append(codes, code)
	}
	for i, a := range addresses {
		ia, _ := a.InternalAndQuaiAddress()
		s.CreateAccount(ia)
		s.SetNonce(ia, 1)
		s.SetBalance(ia, u(1000000))
		s.SetCode(ia, codes[i])
		if !a.Equal(sender) {
			s.SetState(ia, common.Hash{}, common.Hash{31: 1})
		}
	}
	from, _ := sender.InternalAndQuaiAddress()
	s.SetBalance(from, u(1000000000000000))
	s.SetNonce(from, 7)
	if kind == "sender-code" {
		s.SetCode(from, []byte{0})
	}
	if kind == "empty-recipient" || kind == "clear-empty" {
		ia, _ := recipient.InternalAndQuaiAddress()
		s.SetNonce(ia, 0)
		s.SetBalance(ia, u(0))
		if kind == "clear-empty" {
			s.SetState(ia, common.Hash{}, common.Hash{})
		}
		value = 0
	}
	if kind == "empty-untouched" || kind == "static-empty" || kind == "static-empty-revert" {
		ia, _ := child.InternalAndQuaiAddress()
		s.SetNonce(ia, 0)
		s.SetBalance(ia, u(0))
		s.SetState(ia, common.Hash{}, common.Hash{})
	}
	before, e := s.Commit(false)
	must(e)
	must(cache.TrieDB().Commit(before, false, nil))
	s, e = state.New(before, types.EmptyRootHash, u(0), cache, cache, nil, loc, log.Global)
	must(e)
	world := []interface{}{}
	for _, a := range addresses {
		ia, _ := a.InternalAndQuaiAddress()
		storage := []interface{}{}
		v := s.GetState(ia, common.Hash{})
		if v != (common.Hash{}) {
			storage = append(storage, map[string]interface{}{"key": common.Hash{}.Hex(), "value": v.Hex()})
		}
		world = append(world, map[string]interface{}{"address": hx(a.Bytes()), "nonce": s.GetNonce(ia), "balance": hx(s.GetBalance(ia).FillBytes(make([]byte, 32))), "code": hx(s.GetCode(ia)), "storage_size": hx(s.GetSize(ia).FillBytes(make([]byte, 32))), "storage": storage})
	}
	list := types.AccessList{{Address: recipient, StorageKeys: []common.Hash{{}, {31: 1}}}}
	if kind == "transfer" || kind == "self" {
		list = nil
	}
	if kind == "bounds-access" {
		keys := []common.Hash{}
		for i := 0; i < 4097; i++ {
			keys = append(keys, common.BigToHash(u(uint64(i))))
		}
		list = types.AccessList{{Address: recipient, StorageKeys: keys}}
		for i := 0; i < 4097; i++ {
			list = append(list, types.AccessTuple{Address: boundsAddress(loc, i)})
		}
	}
	if kind == "static-empty" || kind == "static-empty-revert" {
		list = append(list, types.AccessTuple{Address: child})
	}
	var to *common.Address = &recipient
	if creation {
		to = nil
		created := crypto.CreateAddress(sender, nonce, data, loc)
		if _, e := created.InternalAndQuaiAddress(); e != nil {
			created, _, e = vm.GrindContract(sender, nonce, 100000000, 30+6*int64((len(data)+31)/32), crypto.Keccak256Hash(data), u(4000000), loc)
			must(e)
		}
		list = append(list, types.AccessTuple{Address: created})
	}
	tx := types.NewTx(&types.QuaiTx{ChainID: u(1337), Nonce: nonce, GasPrice: u(price), Gas: gas, To: to, Value: u(value), Data: data, AccessList: list})
	chain := *params.TestChainConfig
	chain.ChainID = u(1337)
	chain.Location = loc
	signer := types.MakeSigner(&chain, u(4000000))
	tx, e = types.SignTx(tx, signer, k)
	must(e)
	pb, e := tx.ProtoEncode()
	must(e)
	wire, e := proto.Marshal(pb)
	must(e)
	vm.PrecompiledContracts = make(map[common.AddressBytes]vm.PrecompiledContract)
	vm.PrecompiledAddresses = make(map[string][]common.Address)
	vm.InitializePrecompiles(loc)
	transfer := func(db vm.StateDB, f, t common.Address, v *big.Int) error {
		fi, e := f.InternalAndQuaiAddress()
		if e != nil {
			return e
		}
		ti, e := t.InternalAndQuaiAddress()
		if e != nil {
			return e
		}
		db.SubBalance(fi, v)
		db.AddBalance(ti, v)
		return nil
	}
	can := func(db vm.StateDB, a common.Address, v *big.Int) bool {
		ia, e := a.InternalAndQuaiAddress()
		return e == nil && db.GetBalance(ia).Cmp(v) >= 0
	}
	context := vm.BlockContext{CanTransfer: can, Transfer: transfer, BlockNumber: u(4000000), Time: u(1769112000), Difficulty: u(1), BaseFee: u(13), QuaiStateSize: u(1 << 20), GasLimit: 30000000, PrimaryCoinbase: coinbase, PrimeTerminusNumber: prime, GetHash: func(n uint64) common.Hash { return common.BigToHash(u(n)) }}
	msg, e := tx.AsMessage(signer, u(13))
	must(e)
	s.Prepare(tx.Hash(), 0)
	env := vm.NewEVM(context, vm.TxContext{Origin: sender, GasPrice: u(price), TxType: 0, Hash: tx.Hash()}, s, &chain, vm.Config{}, nil)
	gp := new(types.GasPool).AddGas(pool)
	revision := s.Snapshot()
	var result *core.ExecutionResult
	var receipt *types.Receipt
	var err error
	used, usedState, r, p := uint64(1000), uint64(11), uint64(1000000), uint64(1000000)
	if finalized {
		receipt, _, err = core.NativeApplySettlement(msg, &chain, gp, s, tx, env, &used, &usedState, &r, &p)
		result = core.NativeSettlementLast
	} else {
		result, err = core.ApplyMessage(env, msg, gp)
	}
	if err != nil {
		s.RevertToSnapshot(revision)
		gp = new(types.GasPool).AddGas(pool)
	}
	logs := []interface{}{}
	for _, l := range s.Logs() {
		topics := []string{}
		for _, h := range l.Topics {
			topics = append(topics, h.Hex())
		}
		logs = append(logs, map[string]interface{}{"address": hx(l.Address.Bytes()), "topics": topics, "data": hx(l.Data)})
	}
	out := map[string]interface{}{"id": fmt.Sprintf("%s-%d-%d-prime-%d-final-%t", kind, loc[0], loc[1], prime, finalized), "finalize_empty": finalized, "world": world, "transaction": hx(wire), "tx_hash": tx.Hash().Hex(), "context": map[string]interface{}{"address": hx(recipient.Bytes()), "caller": hx(sender.Bytes()), "origin": hx(sender.Bytes()), "coinbase": hx(coinbase.Bytes()), "etx_sender": hx(sender.Bytes()), "location": []int{int(loc[0]), int(loc[1])}, "tx_type": 0, "value": w(value), "gas_price": w(price), "block_number": w(4000000), "timestamp": w(1769112000), "difficulty": w(1), "base_fee": w(13), "chain_id": w(1337), "state_size": w(1 << 20), "block_gas_limit": 30000000, "address_listed": true, "warm_slots": []string{}, "read_only": false, "prime_number": prime}, "before_root": before.Hex(), "consensus_error": errorClass(err), "initial_gas_pool": pool, "initial_cumulative_gas": 1000, "initial_state_used": 11, "gas_pool": gp.Gas(), "logs": logs}
	if err == nil {
		out["gas_used"] = result.UsedGas
		out["state_used"] = result.UsedState
		out["fees"] = hx(result.QuaiFees.FillBytes(make([]byte, 32)))
		out["output"] = hx(result.ReturnData)
		out["error"] = vmClass(result.Err)
		out["reverted"] = errors.Is(result.Err, vm.ErrExecutionReverted)
		out["cumulative_gas"] = 1000 + result.UsedGas
		out["cumulative_state"] = 11 + result.UsedState
		var contract interface{}
		if result.ContractAddr != nil {
			contract = hx(result.ContractAddr.Bytes())
		}
		out["contract_address"] = contract
	}
	if receipt != nil {
		var buf bytes.Buffer
		types.Receipts{receipt}.EncodeIndex(0, &buf)
		out["receipt_rlp"] = hx(buf.Bytes())
		out["region_remaining"] = r
		out["prime_remaining"] = p
	}
	out["after_root"] = s.IntermediateRoot(false).Hex()
	return out
}
func main() {
	v := []interface{}{}
	for _, finalized := range []bool{false, true} {
		for _, loc := range []common.Location{{0, 0}, {1, 2}, {15, 15}} {
			for _, kind := range []string{"transfer", "self", "storage", "revert", "fault", "nonce-high", "nonce-low", "fee-low", "funds", "block-gas", "intrinsic", "sender-code", "precompile", "create", "create-revert", "create-deposit-oog", "suicide-lowercase", "empty-recipient", "clear-empty", "empty-untouched", "static-empty", "static-empty-revert"} {
				v = append(v, run(kind, loc, 2444574, finalized))
			}
			for _, p := range []uint64{1919499, 1919500, 2444573, 2444574, 2444575} {
				for _, kind := range []string{"suicide-other", "suicide-self"} {
					v = append(v, run(kind, loc, p, finalized))
				}
			}
		}
	}
	// Gas-only bounds: call data and log data above 128 KiB, a transaction
	// above 1 MiB, more than 4,096 access-list tuples and keys, more than
	// 4,096 logs in one receipt, and a call chain deeper than 500 frames.
	for _, kind := range []string{"bounds-data-log", "bounds-access", "bounds-logs", "bounds-wire", "deep-calls"} {
		v = append(v, run(kind, common.Location{0, 0}, 2444574, true))
	}
	must(json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"reference_revision": referenceRevision, "vectors": v, "scope": "Actual signed Quai transactions through core.ApplyMessage and actual applyTransaction finalization on identical committed private pre-state; consensus errors are rolled back as rejected block transactions. No full block admission or public network."}))
}
