// Offline-only observation harness. The dispatch placeholder is replaced with
// byte-for-byte official StateProcessor.Process source by the explicit driver.
package core

import (
	"bytes"
	"encoding/hex"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus/misc"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/core/vm"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/crypto/multiset"
	"github.com/dominant-strategies/go-quai/params"
	"google.golang.org/protobuf/proto"
	"math/big"
	"time"
)

type nativeIncomingCase struct {
	ID                                      string
	Location                                common.Location
	Number, Parent, Prime, Gas, Pool, Value uint64
	Type                                    uint64
	Qi, Remote, Listed, Queued, Create      bool
	Data, Code                              []byte
	Repeat                                  int
}

func niU(n uint64) *big.Int { return new(big.Int).SetUint64(n) }
func niHex(b []byte) string { return "0x" + hex.EncodeToString(b) }
func niMust(e error) {
	if e != nil {
		panic(e)
	}
}
func niAddress(loc common.Location, n byte, qi bool) common.Address {
	var a [20]byte
	a[0] = loc.BytePrefix()
	a[18] = 0x11
	a[19] = n
	if qi {
		a[1] = 128
	}
	return common.BytesToAddress(a[:], loc)
}
func niOne(c nativeIncomingCase) map[string]interface{} {
	hc := nativeHC(2)
	defer hc.headerDb.Close()
	hc.config.Location = c.Location
	hc.config.ChainID = niU(1337)
	p := &StateProcessor{config: hc.config, hc: hc, logger: hc.logger}
	nodeLocation := c.Location
	nodeCtx := common.ZONE_CTX
	cache := state.NewDatabase(hc.headerDb)
	statedb, e := state.New(types.EmptyRootHash, types.EmptyRootHash, niU(0), cache, cache, nil, nodeLocation, hc.logger)
	niMust(e)
	zero := common.ZeroInternal(nodeLocation)
	target := niAddress(nodeLocation, 1, c.Qi)
	owner := niAddress(nodeLocation, 2, false)
	sender := niAddress(nodeLocation, 3, false)
	if c.Remote {
		other := common.Location{(nodeLocation[0] + 1) % 16, nodeLocation[1]}
		sender = niAddress(other, 3, false)
	}
	coinbase := niAddress(nodeLocation, 5, false)
	lockContract := common.BytesToAddress(append([]byte{nodeLocation.BytePrefix()}, append(make([]byte, 18), 10)...), nodeLocation)
	addresses := []common.Address{common.BytesToAddress(zero[:], nodeLocation), owner, lockContract}
	if !c.Qi {
		addresses = append(addresses, target)
	}
	for _, a := range addresses {
		ia, e := a.InternalAndQuaiAddress()
		niMust(e)
		statedb.CreateAccount(ia)
		statedb.SetNonce(ia, 1)
		statedb.SetBalance(ia, niU(100))
		if a.Equal(owner) {
			statedb.SetCode(ia, []byte{0})
		}
	}
	if !c.Qi && len(c.Code) > 0 {
		ia, _ := target.InternalAndQuaiAddress()
		statedb.SetCode(ia, c.Code)
	}
	before, e := statedb.Commit(false)
	niMust(e)
	niMust(cache.TrieDB().Commit(before, false, nil))
	statedb, e = state.New(before, types.EmptyRootHash, niU(0), cache, cache, nil, nodeLocation, hc.logger)
	niMust(e)
	world := []interface{}{}
	for _, a := range addresses {
		ia, _ := a.InternalAndQuaiAddress()
		world = append(world, map[string]interface{}{"address": niHex(a.Bytes()), "nonce": statedb.GetNonce(ia), "balance": niHex(statedb.GetBalance(ia).FillBytes(make([]byte, 32))), "code": niHex(statedb.GetCode(ia)), "storage_size": niHex(statedb.GetSize(ia).FillBytes(make([]byte, 32))), "storage": []interface{}{}})
	}
	data := c.Data
	if c.Type == 4 && data == nil {
		data = owner.Bytes()
	}
	if c.Type == 1 && data == nil {
		data = make([]byte, 33)
	}
	access := types.AccessList{}
	if c.Listed {
		access = append(access, types.AccessTuple{Address: target, StorageKeys: []common.Hash{{}}})
	}
	to := target
	if c.Create {
		// An ETX to the zone's zero address creates a contract from the zero
		// address (nonce 1); the access list must name the new address.
		to = common.ZeroAddress(nodeLocation)
		created := crypto.CreateAddress(to, 1, data, nodeLocation)
		if _, e := created.InternalAndQuaiAddress(); e != nil {
			created, _, e = vm.GrindContract(to, 1, 1<<40, 30+6*int64((len(data)+31)/32), crypto.Keccak256Hash(data), niU(c.Number), nodeLocation)
			niMust(e)
		}
		access = types.AccessList{{Address: created, StorageKeys: []common.Hash{}}}
	}
	tx := types.NewTx(&types.ExternalTx{Gas: c.Gas, To: &to, Value: niU(c.Value), Data: data, AccessList: access, Sender: sender, EtxType: c.Type, OriginatingTxHash: common.Hash{31: 99}, ETXIndex: 7})
	pb, e := tx.ProtoEncode()
	niMust(e)
	wire, e := proto.Marshal(pb)
	niMust(e)
	txs := types.Transactions{tx}
	for j := 1; j < c.Repeat; j++ {
		txs = append(txs, tx)
	}
	if c.Queued {
		niMust(statedb.PushETXs(txs))
	}
	beforeEtx := statedb.ETXRoot()
	block := types.EmptyWorkObject(nodeCtx)
	block.WorkObjectHeader().SetLocation(nodeLocation)
	block.SetNumber(niU(c.Number), nodeCtx)
	block.WorkObjectHeader().SetPrimeTerminusNumber(niU(c.Prime))
	block.WorkObjectHeader().SetTime(1769112000)
	block.WorkObjectHeader().SetDifficulty(niU(1))
	block.WorkObjectHeader().SetPrimaryCoinbase(coinbase)
	block.Header().SetGasLimit(30000000)
	block.Header().SetBaseFee(niU(13))
	block.Body().SetTransactions(txs)
	parent := types.EmptyWorkObject(nodeCtx)
	parent.SetNumber(niU(c.Parent), nodeCtx)
	blockNumber := niU(c.Number)
	blockHash := block.Hash()
	batch := hc.headerDb.NewBatch()
	batch.SetPending(true)
	vm.PrecompiledContracts = make(map[common.AddressBytes]vm.PrecompiledContract)
	vm.PrecompiledAddresses = make(map[string][]common.Address)
	vm.InitializePrecompiles(nodeLocation)
	ctx := vm.BlockContext{CanTransfer: CanTransfer, Transfer: Transfer, BlockNumber: blockNumber, Time: niU(1769112000), Difficulty: niU(1), BaseFee: niU(13), QuaiStateSize: niU(1 << 20), GasLimit: 30000000, PrimaryCoinbase: coinbase, PrimeTerminusNumber: c.Prime, GetHash: func(n uint64) common.Hash { return common.BigToHash(niU(n)) }}
	vmenv := vm.NewEVM(ctx, vm.TxContext{}, statedb, hc.config, vm.Config{}, batch)
	usedGas, usedState := new(uint64), new(uint64)
	gp := new(types.GasPool).AddGas(c.Pool)
	totalEtxGas := uint64(0)
	etxRLimit, etxPLimit := uint64(10000000), uint64(10000000)
	coinbaseLockupEpoch := uint32(c.Number/params.CoinbaseEpochBlocks + 1)
	gasUsedForCoinbase := uint64(params.TxGas)
	if c.Parent < params.TimeToStartTx {
		gasUsedForCoinbase = 0
	}
	receipts := types.Receipts{}
	allLogs := []*types.Log{}
	emittedEtxs := []*types.Transaction{}
	newLockedQuai := make(map[common.InternalAddress]*big.Int)
	quaiFees := niU(0)
	supplyAddedQi := niU(0)
	utxosCreatedDeleted := new(UtxosCreatedDeleted)
	etxCount := 0
	var timeQiToQuai, timeQuaiToQi, timeCoinbase time.Duration
	run := func() (types.Receipts, []*types.Transaction, []*types.Log, *state.StateDB, uint64, uint64, uint64, *multiset.MultiSet, []common.Unlock, error) {
		for i, tx := range block.Transactions() {
			msg, err := tx.AsMessage(types.MakeSigner(p.config, blockNumber), block.BaseFee())
			if err != nil {
				return nil, nil, nil, nil, 0, 0, 0, nil, nil, err
			}
			statedb.Prepare(tx.Hash(), i)
			var receipt *types.Receipt
			/* OFFICIAL_INCOMING_DISPATCH */
		}
		return nil, nil, nil, nil, 0, 0, 0, nil, nil, nil
	}
	_, _, _, _, _, _, _, _, _, err := run()
	_ = etxCount
	_ = timeQiToQuai
	_ = timeQuaiToQi
	_ = timeCoinbase
	result := map[string]interface{}{"id": c.ID, "location": []byte(nodeLocation), "number": c.Number, "parent_number": c.Parent, "prime_number": c.Prime, "gas_pool": c.Pool, "world": world, "before_root": before.Hex(), "before_etx_root": beforeEtx.Hex(), "transaction": niHex(wire), "repetitions": len(txs), "queued": c.Queued, "accepted": err == nil, "reference_error": nil}
	if err != nil {
		result["reference_error"] = err.Error()
		return result
	}
	root := statedb.IntermediateRoot(true)
	set := multiset.New()
	for _, h := range utxosCreatedDeleted.UtxosCreatedHashes {
		set.Add(h[:])
	}
	for _, h := range utxosCreatedDeleted.UtxosDeletedHashes {
		set.Remove(h[:])
	}
	receiptRows := []interface{}{}
	for i, r := range receipts {
		var b bytes.Buffer
		receipts.EncodeIndex(i, &b)
		receiptRows = append(receiptRows, map[string]interface{}{"status": r.Status, "gas_used": r.GasUsed, "cumulative_gas_used": r.CumulativeGasUsed, "rlp": niHex(b.Bytes())})
	}
	out := []string{}
	for _, t := range emittedEtxs {
		p, e := t.ProtoEncode()
		niMust(e)
		b, e := proto.Marshal(p)
		niMust(e)
		out = append(out, niHex(b))
	}
	result["after_root"] = root.Hex()
	result["etx_root"] = statedb.ETXRoot().Hex()
	result["utxo_root"] = set.Hash().Hex()
	result["utxo_size"] = len(utxosCreatedDeleted.UtxosCreatedHashes) - len(utxosCreatedDeleted.UtxosDeletedHashes)
	result["supply_added"] = niHex(supplyAddedQi.Bytes())
	result["gas_remaining"] = gp.Gas()
	result["gas_used"] = *usedGas
	result["state_used"] = *usedState
	result["etx_gas"] = totalEtxGas
	result["region_remaining"] = etxRLimit
	result["prime_remaining"] = etxPLimit
	result["receipts"] = receiptRows
	result["outbound"] = out
	return result
}
func NativeIncomingVectors() []interface{} {
	rows := []interface{}{}
	for _, loc := range []common.Location{{0, 0}, {2, 3}, {15, 15}} {
		for _, p := range []uint64{261999, 262000, 1754999, 1755000, 2236999, 2237000, 2316999, 2317000, 2444574} {
			for _, kind := range []uint64{0, 1, 2, 3, 4, 5, 6} {
				for _, qi := range []bool{false, true} {
					c := nativeIncomingCase{ID: fmt.Sprintf("incoming-%x-%d-%d-%t", []byte(loc), p, kind, qi), Location: loc, Number: 4000000, Parent: 3999999, Prime: p, Gas: 200000, Pool: 30000000, Value: 100006, Type: kind, Qi: qi, Listed: true, Queued: true}
					if kind == 0 && qi {
						c.Value = 14
						c.Remote = true
					}
					if kind == 5 && !qi {
						c.Data = append(make([]byte, 2), niAddress(loc, 8, true).Bytes()...)
					}
					rows = append(rows, niOne(c))
				}
			}
		}
		for _, variant := range []string{"empty-queue", "low-block-gas", "low-tx-gas", "empty-data", "contract-coinbase", "delegate-coinbase", "store-listed", "store-unlisted", "revert", "oversize", "intrinsic-short", "store-repeat", "wrap-repeat", "lock-repeat", "utxo-repeat"} {
			c := nativeIncomingCase{ID: fmt.Sprintf("incoming-%x-%s", []byte(loc), variant), Location: loc, Number: 4000000, Parent: 3999999, Prime: 2317000, Gas: 200000, Pool: 30000000, Value: 100006, Type: 3, Qi: true, Queued: true, Listed: true}
			switch variant {
			case "empty-queue":
				c.Queued = false
			case "low-block-gas":
				c.Pool = 8000
			case "low-tx-gas":
				c.Gas = 8999
			case "empty-data":
				c.Type = 1
				c.Data = []byte{}
			case "contract-coinbase":
				c.Type = 1
				c.Data = append(append([]byte{2}, niAddress(loc, 2, false).Bytes()...), make([]byte, 32)...)
			case "delegate-coinbase":
				c.Type = 1
				c.Data = append(append(append([]byte{1}, niAddress(loc, 2, false).Bytes()...), niAddress(loc, 9, false).Bytes()...), make([]byte, 32)...)
			case "store-listed", "store-unlisted":
				c.Type = 0
				c.Qi = false
				c.Code = []byte{0x60, 7, 0x60, 0, 0x55, 0}
				c.Listed = variant == "store-listed"
			case "revert":
				c.Type = 0
				c.Qi = false
				c.Code = []byte{0x60, 0, 0x60, 0, 0xfd}
			case "store-repeat":
				c.Type = 0
				c.Qi = false
				c.Code = []byte{0x60, 7, 0x60, 0, 0x55, 0}
				c.Repeat = 3
			case "wrap-repeat":
				c.Type = 4
				c.Qi = false
				c.Repeat = 3
			case "lock-repeat":
				c.Type = 1
				c.Data = append(append([]byte{2}, niAddress(loc, 2, false).Bytes()...), make([]byte, 32)...)
				c.Repeat = 3
			case "utxo-repeat":
				c.Type = 0
				c.Remote = true
				c.Value = 14
				c.Repeat = 3
			case "oversize":
				c.Type = 0
				c.Qi = false
				c.Gas = 6000001
			case "intrinsic-short":
				c.Type = 0
				c.Qi = false
				c.Gas = 20999
			}
			rows = append(rows, niOne(c))
		}
		// The same ETXs before SecurityHardeningForkBlock: an ETX gas limit above
		// GasLimit / MinimumEtxGasDivisor and an ETX below its intrinsic gas make
		// TransitionDb return an error, so the block is invalid
		// (core/state_transition.go:240, 329, 360).
		for _, variant := range []string{"oversize", "intrinsic-short"} {
			c := nativeIncomingCase{ID: fmt.Sprintf("incoming-%x-%s-prime-2316999", []byte(loc), variant), Location: loc, Number: 4000000, Parent: 3999999, Prime: 2316999, Gas: 6000001, Pool: 30000000, Value: 100006, Type: 0, Qi: false, Queued: true, Listed: true}
			if variant == "intrinsic-short" {
				c.Gas = 20999
			}
			rows = append(rows, niOne(c))
		}
	}
	// The lockup reward multiple (params.CalculateLockupByteRewardsMultiple,
	// params/protocol_params.go:389) is fixed in the first year, falls linearly
	// in years 1 to 4 and is fixed again from year 5 (BlocksPerYear = 6,307,200
	// zone blocks). Qi coinbases turn the reward into UTXOs; a contract coinbase
	// adds it to a coinbase lockup.
	loc := common.Location{0, 0}
	for _, number := range []uint64{6307199, 6307200, 9000000, 31535999, 31536000} {
		for _, lock := range []byte{1, 2, 3} {
			c := nativeIncomingCase{ID: fmt.Sprintf("incoming-%x-qi-coinbase-lock-%d-zone-%d", []byte(loc), lock, number), Location: loc, Number: number, Parent: number - 1, Prime: 2444574, Gas: 200000, Pool: 30000000, Value: 100006, Type: 1, Qi: true, Queued: true, Listed: true, Data: append([]byte{lock}, make([]byte, 32)...)}
			rows = append(rows, niOne(c))
		}
	}
	c := nativeIncomingCase{ID: fmt.Sprintf("incoming-%x-contract-coinbase-lock-2-zone-9000000", []byte(loc)), Location: loc, Number: 9000000, Parent: 8999999, Prime: 2444574, Gas: 200000, Pool: 30000000, Value: 100006, Type: 1, Qi: true, Queued: true, Listed: true, Data: append(append([]byte{2}, niAddress(loc, 2, false).Bytes()...), make([]byte, 32)...)}
	rows = append(rows, niOne(c))
	// ETX creations whose init code reaches a precompile that the access
	// list does not name: TransitionDb's PrepareAccessList adds the
	// precompiles to the ETX message too (core/state_transition.go:380).
	for _, loc := range []common.Location{{0, 0}, {2, 3}, {15, 15}} {
		precompile := func(first, n byte) []byte {
			b := make([]byte, 20)
			b[0] = first
			b[19] = n
			return b
		}
		for _, v := range []struct {
			name string
			code []byte
		}{
			// STATICCALL(GAS, prefix..02, 0, 32, 0, 32); SSTORE(0, success)
			{"static-local", append(append([]byte{0x60, 0x20, 0x60, 0, 0x60, 0x20, 0x60, 0, 0x73}, precompile(loc.BytePrefix(), 2)...), 0x5a, 0xfa, 0x60, 0, 0x55, 0)},
			// The same call to the zero-prefix form 0x00..04.
			{"static-zero", append(append([]byte{0x60, 0x20, 0x60, 0, 0x60, 0x20, 0x60, 0, 0x73}, precompile(0, 4)...), 0x5a, 0xfa, 0x60, 0, 0x55, 0)},
			// BALANCE(prefix..03); SSTORE(0, balance)
			{"balance-local", append(append([]byte{0x73}, precompile(loc.BytePrefix(), 3)...), 0x31, 0x60, 0, 0x55, 0)},
		} {
			c := nativeIncomingCase{ID: fmt.Sprintf("incoming-%x-create-%s", []byte(loc), v.name), Location: loc, Number: 4000000, Parent: 3999999, Prime: 2444574, Gas: 200000, Pool: 30000000, Value: 100006, Type: 0, Queued: true, Create: true, Data: v.code}
			rows = append(rows, niOne(c))
		}
	}
	return rows
}
