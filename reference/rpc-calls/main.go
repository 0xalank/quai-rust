// RPC call oracle: go-quai v0.57.0 answers to quai_call, quai_estimateGas,
// quai_createAccessList and quai_gasPrice from the production handlers
// (internal/quaiapi: DoCall, DoEstimateGas, AccessList, GasPrice) behind the
// production JSON-RPC server, over controlled in-memory state and headers.
// The block context comes from core.NewEVMBlockContext and the base fee
// from HeaderChain.CalcBaseFee (core-export.go). No network and no keys.
//
// Two answers are made deterministic before they are recorded: the access
// list of quai_createAccessList comes from Go maps in random order and is
// sorted (addresses, then keys); "failed to apply transaction" prints a
// types.Message whose pointers change from run to run, and each pointer is
// replaced with 0xc000000000.
package main

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/big"
	"net/http/httptest"
	"os"
	"regexp"
	"sort"
	"strings"

	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/common/hexutil"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/core/vm"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/ethdb"
	"github.com/dominant-strategies/go-quai/internal/quaiapi"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/dominant-strategies/go-quai/rpc"
	"google.golang.org/protobuf/proto"
)

var referenceRevision string
var loc = common.Location{0, 0}

func must(e error) {
	if e != nil {
		panic(e)
	}
}
func u(v uint64) *big.Int { return new(big.Int).SetUint64(v) }
func hx(b []byte) string  { return "0x" + hex.EncodeToString(b) }
func w(v *big.Int) string { return hx(v.FillBytes(make([]byte, 32))) }
func addr(s string) common.Address {
	return common.HexToAddress(s, loc)
}
func code(parts ...string) []byte {
	return common.FromHex("0x" + strings.Join(parts, ""))
}
func a20(a common.Address) string { return hex.EncodeToString(a.Bytes()) }

// Accounts of the zone [0 0] scenarios.
var (
	alice    = addr("0x0011000000000000000000000000000000000001")
	bob      = addr("0x0011000000000000000000000000000000000002")
	store    = addr("0x00110000000000000000000000000000000000a1")
	caller   = addr("0x00110000000000000000000000000000000000b1")
	stat     = addr("0x00110000000000000000000000000000000000b2")
	deleg    = addr("0x00110000000000000000000000000000000000b3")
	creator  = addr("0x00110000000000000000000000000000000000c1")
	reverter = addr("0x00110000000000000000000000000000000000d1")
	rawrev   = addr("0x00110000000000000000000000000000000000d2")
	gasgate  = addr("0x00110000000000000000000000000000000000e1")
	gas63    = addr("0x00110000000000000000000000000000000000e2")
	probe    = addr("0x00110000000000000000000000000000000000f1")
	precall  = addr("0x00110000000000000000000000000000000000f2")
	selfie   = addr("0x00110000000000000000000000000000000000f3")
	gasslot  = addr("0x00110000000000000000000000000000000000f4")
	under    = addr("0x0011000000000000000000000000000000000101")
	over     = addr("0x0011000000000000000000000000000000000102")
	badop    = addr("0x0011000000000000000000000000000000000103")
	undef    = addr("0x0011000000000000000000000000000000000104")
	push0    = addr("0x0011000000000000000000000000000000000105")
	dupu     = addr("0x0011000000000000000000000000000000000106")
	call5    = addr("0x0011000000000000000000000000000000000107")
	qibal    = addr("0x0011000000000000000000000000000000000201")
	zonebal  = addr("0x0011000000000000000000000000000000000202")
	zonesd   = addr("0x0011000000000000000000000000000000000203")
	zonesize = addr("0x0011000000000000000000000000000000000204")
	refund   = addr("0x0011000000000000000000000000000000000301")
	loop     = addr("0x0011000000000000000000000000000000000501")
	other    = addr("0x1000000000000000000000000000000000000077")
	// Another zone's form of precompile 1: EVM.Call translates it.
	otherPre = addr("0x1000000000000000000000000000000000000001")
	qi       = addr("0x0080000000000000000000000000000000000001")
	qiOther  = addr("0x1080000000000000000000000000000000000001")
	kquai    = addr("0x00640d82ef6552085e494df2a2eaec18d8215913")
	coinbase = addr("0x0011000000000000000000000000000000000c0b")
)

// initRuntime0 is init code that deploys the one-byte runtime 0x00.
const initRuntime0 = "600060005360016000f3"

// initStore is init code that sets slot 3 to 9 and deploys runtime 0x00.
const initStore = "60096003556000600053" + "60016000f3"

type account struct {
	a       common.Address
	balance *big.Int
	nonce   uint64
	code    []byte
	storage map[common.Hash]common.Hash
}

func slot(n uint64) common.Hash { return common.BigToHash(u(n)) }

func accounts() []account {
	rich := new(big.Int).Mul(u(1000000), u(1000000000000000000))
	revert := code("6308c379a060e01b600052", "6020600452", "6004602452", "63626f6f6d60e01b604452", "60646000fd")
	return []account{
		{alice, rich, 5, nil, nil},
		{bob, u(0), 0, nil, nil},
		{kquai, u(0), 0, nil, nil},
		// SLOAD 0, SSTORE 1 := slot0 + 1, return slot 0.
		{store, u(0), 1, code("6000548060010160015560005260206000f3"), map[common.Hash]common.Hash{slot(0): slot(42)}},
		// CALL store with all gas and return its output.
		{caller, u(0), 1, code("60206000600060006000", "73", a20(store), "5af1", "5060206000f3"), nil},
		// STATICCALL store (its SSTORE fails), return the success flag.
		{stat, u(0), 1, code("6000600060006000", "73", a20(store), "5afa", "60005260206000f3"), nil},
		// DELEGATECALL store: its code on this storage.
		{deleg, u(0), 1, code("6020600060006000", "73", a20(store), "5af4", "5060206000f3"), map[common.Hash]common.Hash{slot(0): slot(7)}},
		// CREATE the init code initRuntime0 and return the address.
		{creator, u(0), 1, code("69", initRuntime0, "600052", "600a60166000f0", "60005260206000f3"), nil},
		{reverter, u(0), 1, revert, nil},
		{rawrev, u(0), 1, code("60016000fd"), nil},
		// Revert unless GAS > 100000 at entry.
		{gasgate, u(0), 1, code("5a620186a010600e5760006000fd5b00"), nil},
		// CALL gasgate with all gas (63/64 rule); revert when it fails.
		{gas63, u(0), 1, code("60006000600060006000", "73", a20(gasgate), "5af1", "602957", "60006000fd5b00"), nil},
		// BALANCE bob, EXTCODESIZE store, EXTCODEHASH creator, EXTCODECOPY reverter.
		{probe, u(0), 1, code("73", a20(bob), "3150", "73", a20(store), "3b50", "73", a20(creator), "3f50", "600160006000", "73", a20(reverter), "3c00"), nil},
		// CALL the sha256 precompile on 32 bytes and return the hash.
		{precall, u(0), 1, code("6020600060206000600060025af1", "5060206000f3"), nil},
		// BALANCE of CALLER and ADDRESS, then SLOAD 5.
		{selfie, u(0), 1, code("333150303150600554", "5000"), nil},
		// SLOAD (GAS & 0xff): the list never converges.
		{gasslot, u(0), 1, code("5a60ff165400"), nil},
		{under, u(0), 1, code("01"), nil},
		// JUMPDEST PUSH1 1 PUSH1 0 JUMP: one more stack item per round.
		{over, u(0), 1, code("5b6001600056"), nil},
		{badop, u(0), 1, code("fe"), nil},
		{undef, u(0), 1, code("0c"), nil},
		{push0, u(0), 1, code("5f00"), nil},
		{dupu, u(0), 1, code("80"), nil},
		// CALL with five stack items: an underflow that still names bob.
		{call5, u(0), 1, code("600060006000", "73", a20(bob), "6000f1"), nil},
		{qibal, u(0), 1, code("73", a20(qi), "3100"), nil},
		{zonebal, u(0), 1, code("73", a20(other), "3100"), nil},
		{zonesd, u(0), 1, code("73", a20(other), "ff"), nil},
		{zonesize, u(0), 1, code("73", a20(other), "3b60005260206000f3"), nil},
		// SSTORE slot 0 := 0 (a clearing refund).
		{refund, u(0), 1, code("600060005500"), map[common.Hash]common.Hash{slot(0): slot(1)}},
		{loop, u(0), 1, code("5b600056"), nil},
	}
}

// --- backend ----------------------------------------------------------------

type backend struct {
	quaiapi.Backend // Methods the call handlers do not use stay nil.
	cfg             params.ChainConfig
	disk            ethdb.Database
	cache           state.Database
	blocks          map[common.Hash]*types.WorkObject
	numbers         map[uint64]*types.WorkObject
	head            *types.WorkObject
	genesis         common.Hash
	hc              *core.HeaderChain
}

func (b *backend) NodeCtx() int                                   { return common.ZONE_CTX }
func (b *backend) NodeLocation() common.Location                  { return loc }
func (b *backend) ProcessingState() bool                          { return true }
func (b *backend) ChainConfig() *params.ChainConfig               { return &b.cfg }
func (b *backend) RPCGasCap() uint64                              { return params.GasCeil }
func (b *backend) Logger() *log.Logger                            { return log.Global }
func (b *backend) CurrentHeader() *types.WorkObject               { return b.head }
func (b *backend) CurrentBlock() *types.WorkObject                { return b.head }
func (b *backend) GetBlockByHash(h common.Hash) *types.WorkObject { return b.blocks[h] }
func (b *backend) CalcBaseFee(wo *types.WorkObject) *big.Int      { return b.hc.CalcBaseFee(wo) }

// HeaderByNumber as quai/api_backend.go: no pending block without a miner.
func (b *backend) HeaderByNumber(_ context.Context, n rpc.BlockNumber) (*types.WorkObject, error) {
	if n == rpc.PendingBlockNumber {
		return nil, nil
	}
	if n == rpc.LatestBlockNumber {
		return b.head, nil
	}
	return b.numbers[uint64(n)], nil
}
func (b *backend) HeaderByHash(_ context.Context, h common.Hash) (*types.WorkObject, error) {
	return b.blocks[h], nil
}
func (b *backend) HeaderByNumberOrHash(ctx context.Context, n rpc.BlockNumberOrHash) (*types.WorkObject, error) {
	if number, ok := n.Number(); ok {
		return b.HeaderByNumber(ctx, number)
	}
	if hash, ok := n.Hash(); ok {
		header := b.blocks[hash]
		if header == nil {
			return nil, errors.New("header for hash not found")
		}
		if n.RequireCanonical && b.numbers[header.NumberU64(common.ZONE_CTX)] != header {
			return nil, errors.New("hash is not currently canonical")
		}
		return header, nil
	}
	return nil, errors.New("invalid arguments; neither block nor hash specified")
}
func (b *backend) BlockByHash(_ context.Context, h common.Hash) (*types.WorkObject, error) {
	return b.blocks[h], nil
}
func (b *backend) stateAt(header *types.WorkObject) (*state.StateDB, error) {
	return state.New(header.EVMRoot(), header.EtxSetRoot(), header.QuaiStateSize(), b.cache, b.cache, nil, loc, log.Global)
}

// StateAndHeaderByNumber(OrHash) as quai/api_backend.go.
func (b *backend) StateAndHeaderByNumber(ctx context.Context, n rpc.BlockNumber) (*state.StateDB, *types.WorkObject, error) {
	if n == rpc.PendingBlockNumber {
		return &state.StateDB{}, nil, nil
	}
	header, err := b.HeaderByNumber(ctx, n)
	if err != nil {
		return nil, nil, err
	}
	if header == nil {
		return nil, nil, errors.New("header not found")
	}
	st, err := b.stateAt(header)
	return st, header, err
}
func (b *backend) StateAndHeaderByNumberOrHash(ctx context.Context, n rpc.BlockNumberOrHash) (*state.StateDB, *types.WorkObject, error) {
	if number, ok := n.Number(); ok {
		return b.StateAndHeaderByNumber(ctx, number)
	}
	if hash, ok := n.Hash(); ok {
		header := b.blocks[hash]
		if header == nil {
			return nil, nil, errors.New("header for hash not found")
		}
		if n.RequireCanonical && b.numbers[header.NumberU64(common.ZONE_CTX)] != header {
			return nil, nil, errors.New("hash is not currently canonical")
		}
		st, err := b.stateAt(header)
		return st, header, err
	}
	return nil, nil, errors.New("invalid arguments; neither block nor hash specified")
}

// GetEVM as quai/api_backend.go, with the production block context.
func (b *backend) GetEVM(_ context.Context, msg core.Message, st *state.StateDB, header *types.WorkObject, parent *types.WorkObject, vmConfig *vm.Config) (*vm.EVM, func() error, error) {
	vmError := func() error { return nil }
	if parent == nil {
		return nil, nil, errors.New("parent block not found, parenthash: " + header.ParentHash(common.ZONE_CTX).String())
	}
	txContext := core.NewEVMTxContext(msg)
	blockContext, err := core.NewEVMBlockContext(header, parent, chain{b}, nil)
	if err != nil {
		return nil, vmError, err
	}
	return vm.NewEVM(blockContext, txContext, st, &b.cfg, *vmConfig, nil), vmError, nil
}

// chain is the core.ChainContext of the block context.
type chain struct{ b *backend }

func (c chain) Engine(*types.WorkObjectHeader) consensus.Engine { return nil }
func (c chain) GetHeaderOrCandidateByHash(h common.Hash) *types.WorkObject {
	return c.b.blocks[h]
}
func (c chain) NodeCtx() int                                            { return common.ZONE_CTX }
func (c chain) IsGenesisHash(h common.Hash) bool                        { return h == c.b.genesis }
func (c chain) GetHeaderByHash(h common.Hash) *types.WorkObject         { return c.b.blocks[h] }
func (c chain) GetBlockByHash(h common.Hash) *types.WorkObject          { return c.b.blocks[h] }
func (c chain) CheckInCalcOrderCache(common.Hash) (*big.Int, int, bool) { return nil, 0, false }
func (c chain) AddToCalcOrderCache(common.Hash, int, *big.Int)          {}
func (c chain) CalcBaseFee(wo *types.WorkObject) *big.Int               { return c.b.hc.CalcBaseFee(wo) }
func (c chain) CheckIfEtxIsEligible(s common.Hash, l common.Location) bool {
	return c.b.hc.CheckIfEtxIsEligible(s, l)
}

// CalcOrder: the parent is a zone block, so the prime terminus header comes
// from its hash.
func (c chain) CalcOrder(*types.WorkObject) (*big.Int, int, error) {
	return big.NewInt(0), common.ZONE_CTX, nil
}

// --- headers ----------------------------------------------------------------

type headerSpec struct {
	number      uint64
	prime       uint64
	time        uint64
	difficulty  *big.Int
	baseFee     *big.Int
	gasLimit    uint64
	stateSize   *big.Int
	avgFees     *big.Int
	exchange    *big.Int
	eligible    common.Hash
	shaDiff     *big.Int
	shaCount    *big.Int
	scryptCount *big.Int
}

func makeBlock(s headerSpec, parent common.Hash, primeHash common.Hash, root common.Hash) *types.WorkObject {
	wo := types.EmptyWorkObject(common.ZONE_CTX)
	wo.SetNumber(u(s.number), common.ZONE_CTX)
	wo.SetParentHash(parent, common.ZONE_CTX)
	wo.WorkObjectHeader().SetLocation(loc)
	wo.WorkObjectHeader().SetTime(s.time)
	wo.WorkObjectHeader().SetDifficulty(s.difficulty)
	wo.WorkObjectHeader().SetPrimeTerminusNumber(u(s.prime))
	wo.WorkObjectHeader().SetPrimaryCoinbase(coinbase)
	if s.shaDiff != nil {
		wo.WorkObjectHeader().SetShaDiffAndCount(types.NewPowShareDiffAndCount(s.shaDiff, s.shaCount, u(0)))
		wo.WorkObjectHeader().SetScryptDiffAndCount(types.NewPowShareDiffAndCount(u(1), s.scryptCount, u(0)))
	}
	wo.Header().SetEVMRoot(root)
	wo.Header().SetBaseFee(s.baseFee)
	wo.Header().SetGasLimit(s.gasLimit)
	wo.Header().SetQuaiStateSize(s.stateSize)
	wo.Header().SetAvgTxFees(s.avgFees)
	wo.Header().SetExchangeRate(s.exchange)
	wo.Header().SetEtxEligibleSlices(s.eligible)
	wo.Header().SetPrimeTerminusHash(primeHash)
	wo.WorkObjectHeader().SetHeaderHash(wo.Header().Hash())
	return wo
}
func encode(wo *types.WorkObject) string {
	pb, err := wo.ProtoEncode(types.BlockObject)
	must(err)
	b, err := proto.Marshal(pb)
	must(err)
	return hx(b)
}

// --- scenarios --------------------------------------------------------------

type request struct {
	method string
	params []interface{}
}
type scenario struct {
	id       string
	head     headerSpec
	parent   headerSpec
	prime    headerSpec
	genesis  bool // the head's parent is the genesis block
	requests []request
}

// register stores a block under its scenario ID. After the KawPow fork a
// work-object header hashes its AuxPow, and these headers have none, so
// every such header has the same Hash(); the ID stands for the hash in the
// parent and prime terminus links and in every lookup.
func (b *backend) register(h common.Hash, wo *types.WorkObject, canonical bool) {
	b.blocks[h] = wo
	if canonical {
		b.numbers[wo.NumberU64(common.ZONE_CTX)] = wo
	}
	rawdb.WriteHeaderNumber(b.disk, h, wo.NumberU64(common.ZONE_CTX))
	rawdb.WriteTermini(b.disk, h, types.EmptyTermini())
	rawdb.WriteWorkObject(b.disk, h, wo, types.BlockObject, common.ZONE_CTX)
}

func run(sc scenario) map[string]interface{} {
	disk := rawdb.NewMemoryDatabase(log.Global)
	defer disk.Close()
	cache := state.NewDatabase(disk)
	cfg := *params.TestChainConfig
	cfg.Location = loc
	cfg.ChainID = u(9)
	b := &backend{cfg: cfg, disk: disk, cache: cache, blocks: map[common.Hash]*types.WorkObject{}, numbers: map[uint64]*types.WorkObject{}, hc: core.NativeRPCCallsHeaderChain(disk)}
	st, err := state.New(types.EmptyRootHash, types.EmptyRootHash, u(0), cache, cache, nil, loc, log.Global)
	must(err)
	for _, a := range accounts() {
		ia, err := a.a.InternalAddress()
		must(err)
		st.CreateAccount(ia)
		st.SetBalance(ia, a.balance)
		st.SetNonce(ia, a.nonce)
		st.SetCode(ia, a.code)
		for k, v := range a.storage {
			st.SetState(ia, k, v)
		}
	}
	root, err := st.Commit(true)
	must(err)
	must(cache.TrieDB().Commit(root, false, nil))
	st, err = state.New(root, types.EmptyRootHash, u(0), cache, cache, nil, loc, log.Global)
	must(err)
	world := []interface{}{}
	for _, a := range accounts() {
		ia, _ := a.a.InternalAddress()
		if !st.Exist(ia) {
			continue
		}
		storage := []interface{}{}
		keys := []common.Hash{}
		for k := range a.storage {
			keys = append(keys, k)
		}
		sort.Slice(keys, func(i, j int) bool { return bytes.Compare(keys[i][:], keys[j][:]) < 0 })
		for _, k := range keys {
			storage = append(storage, map[string]interface{}{"key": k.Hex(), "value": st.GetState(ia, k).Hex()})
		}
		world = append(world, map[string]interface{}{"address": hx(a.a.Bytes()), "nonce": st.GetNonce(ia), "balance": w(st.GetBalance(ia)), "code": hx(st.GetCode(ia)), "storage_size": w(st.GetSize(ia)), "storage": storage})
	}
	// Blocks: grandparent (unless the parent is the genesis), parent, head
	// and the prime terminus, each under a scenario ID.
	id := func(role string) common.Hash { return crypto.Keccak256Hash([]byte(sc.id + "/" + role)) }
	primeID, grandID, parentID, headID := id("prime"), id("grandparent"), id("parent"), id("head")
	prime := makeBlock(sc.prime, id("prime-parent"), common.Hash{}, root)
	b.register(primeID, prime, false)
	grandSpec := sc.parent
	grandSpec.number = sc.parent.number - 1
	grandSpec.time = sc.parent.time - 5
	var grand *types.WorkObject
	parentLink := id("genesis-parent")
	if sc.parent.number > 0 {
		grand = makeBlock(grandSpec, id("grandparent-parent"), primeID, root)
		b.register(grandID, grand, true)
		parentLink = grandID
	}
	parent := makeBlock(sc.parent, parentLink, primeID, root)
	b.register(parentID, parent, true)
	if sc.genesis {
		b.genesis = parentID
		rawdb.WriteGenesisHashes(disk, common.Hashes{parentID})
	} else {
		rawdb.WriteGenesisHashes(disk, common.Hashes{id("genesis")})
	}
	head := makeBlock(sc.head, parentID, primeID, root)
	b.register(headID, head, true)
	b.head = head
	blockContext, err := core.NewEVMBlockContext(head, parent, chain{b}, nil)
	var context interface{}
	if err == nil {
		context = map[string]interface{}{"coinbase": hx(blockContext.PrimaryCoinbase.Bytes()), "block_number": w(blockContext.BlockNumber), "timestamp": w(blockContext.Time), "difficulty": w(blockContext.Difficulty), "base_fee": w(blockContext.BaseFee), "chain_id": w(cfg.ChainID), "state_size": w(blockContext.QuaiStateSize), "block_gas_limit": blockContext.GasLimit, "location": []int{int(loc[0]), int(loc[1])}, "prime_number": blockContext.PrimeTerminusNumber, "eligible_slices": blockContext.EtxEligibleSlices.Hex()}
	}
	hashes := map[string]interface{}{fmt.Sprint(sc.parent.number): parentID.Hex()}
	if grand != nil {
		hashes[fmt.Sprint(grandSpec.number)] = grandID.Hex()
	}
	server := rpc.NewServer(log.Global)
	defer server.Stop()
	// quaiapi.GetAPIs registers both namespaces: eth_gasPrice is the same
	// PublicQuaiAPI method, eth_call, eth_estimateGas and
	// eth_createAccessList are PublicBlockChainAPI's.
	must(server.RegisterName("eth", quaiapi.NewPublicQuaiAPI(b)))
	must(server.RegisterName("quai", quaiapi.NewPublicQuaiAPI(b)))
	must(server.RegisterName("eth", quaiapi.NewPublicBlockChainAPI(b)))
	must(server.RegisterName("quai", quaiapi.NewPublicBlockChainQuaiAPI(b)))
	vectors := []interface{}{}
	for _, r := range sc.requests {
		vectors = append(vectors, map[string]interface{}{"method": r.method, "params": r.params, "go": ask(server, r.method, r.params)})
	}
	block := func(h common.Hash, wo *types.WorkObject) map[string]interface{} {
		return map[string]interface{}{"number": wo.NumberU64(common.ZONE_CTX), "hash": h.Hex(), "object": encode(wo)}
	}
	out := map[string]interface{}{"id": sc.id, "world": world, "context": context, "block_hashes": hashes, "head": block(headID, head), "parent": block(parentID, parent), "prime": block(primeID, prime), "vectors": vectors}
	if grand != nil {
		out["grandparent"] = block(grandID, grand)
	}
	return out
}

var pointer = regexp.MustCompile(`\b0x[0-9a-f]{6,16}\b`)

// ask sends one request through the production JSON-RPC server and keeps
// the result or the error object.
func ask(server *rpc.Server, method string, params []interface{}) map[string]interface{} {
	body, err := json.Marshal(map[string]interface{}{"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
	must(err)
	req := httptest.NewRequest("POST", "/", bytes.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	server.ServeHTTP(rec, req)
	var answer map[string]interface{}
	dec := json.NewDecoder(rec.Body)
	dec.UseNumber()
	must(dec.Decode(&answer))
	out := map[string]interface{}{}
	for _, k := range []string{"result", "error"} {
		if v, ok := answer[k]; ok {
			out[k] = v
		}
	}
	if e, ok := out["error"].(map[string]interface{}); ok {
		if m, ok := e["message"].(string); ok && strings.HasPrefix(m, "failed to apply transaction: {") {
			end := strings.LastIndex(m, "} err: ")
			e["message"] = pointer.ReplaceAllString(m[:end], "0xc000000000") + m[end:]
		}
	}
	if r, ok := out["result"].(map[string]interface{}); ok && (method == "quai_createAccessList" || method == "eth_createAccessList") {
		if list, ok := r["accessList"].([]interface{}); ok {
			for _, t := range list {
				keys := t.(map[string]interface{})["storageKeys"].([]interface{})
				sort.Slice(keys, func(i, j int) bool { return keys[i].(string) < keys[j].(string) })
			}
			sort.Slice(list, func(i, j int) bool {
				return strings.ToLower(list[i].(map[string]interface{})["address"].(string)) < strings.ToLower(list[j].(map[string]interface{})["address"].(string))
			})
		}
	}
	return out
}

type obj = map[string]interface{}

func a(x common.Address) string { return hx(x.Bytes()) }
func gas(n uint64) string       { return hexutil.EncodeUint64(n) }

func callRequests(at interface{}) []request {
	var r []request
	add := func(method string, params ...interface{}) { r = append(r, request{method, params}) }
	for _, method := range []string{"quai_call", "quai_estimateGas", "quai_createAccessList"} {
		data := "0x01"
		for _, args := range []obj{
			{"from": a(alice), "to": a(bob), "value": "0x1"},
			{"from": a(alice), "to": a(store), "data": data},
			{"from": a(alice), "to": a(store), "data": data, "accessList": []obj{{"address": a(bob), "storageKeys": []string{}}}},
			{"from": a(alice), "to": a(store), "data": data, "accessList": []obj{{"address": a(store), "storageKeys": []string{slot(0).Hex()}}}},
			{"from": a(alice), "to": a(store), "data": data, "accessList": []obj{{"address": a(bob), "storageKeys": []string{}}, {"address": a(alice), "storageKeys": []string{}}, {"address": a(store), "storageKeys": []string{slot(5).Hex()}}}},
			{"from": a(alice), "to": a(caller), "data": data},
			{"from": a(alice), "to": a(stat), "data": data},
			{"from": a(alice), "to": a(deleg), "data": data},
			{"from": a(alice), "to": a(creator), "data": data},
			{"from": a(alice), "data": "0x" + initRuntime0},
			{"from": a(alice), "data": "0x" + initStore},
			{"from": a(alice), "to": a(reverter), "data": data},
			{"from": a(alice), "to": a(rawrev), "data": data},
			{"from": a(alice), "to": a(gasgate), "data": data},
			{"from": a(alice), "to": a(gas63), "data": data},
			{"from": a(alice), "to": a(probe), "data": data},
			{"from": a(alice), "to": a(precall), "data": data},
			{"from": a(alice), "to": a(selfie), "data": data},
			{"from": a(alice), "to": a(refund), "data": data},
			{"from": a(alice), "to": a(under), "data": data},
			{"from": a(alice), "to": a(over), "data": data},
			{"from": a(alice), "to": a(badop), "data": data},
			{"from": a(alice), "to": a(undef), "data": data},
			{"from": a(alice), "to": a(push0), "data": data},
			{"from": a(alice), "to": a(dupu), "data": data},
			{"from": a(alice), "to": a(call5), "data": data},
			{"from": a(alice), "to": a(qibal), "data": data},
			{"from": a(alice), "to": a(zonebal), "data": data},
			{"from": a(alice), "to": a(zonesd), "data": data},
			{"from": a(alice), "to": a(zonesize), "data": data},
			{"from": a(alice), "to": a(loop), "data": data, "gas": gas(0x30000)},
			{"from": a(alice), "to": a(other), "value": "0x1"},
			{"from": a(alice), "to": a(other), "value": "0x1", "gas": gas(30000)},
			{"from": a(alice), "to": a(other), "value": "0x1", "gas": gas(50000)},
			{"from": a(alice), "to": a(other), "value": "0x1", "data": data},
			{"from": a(alice), "to": a(otherPre), "value": "0x1"},
			{"from": a(alice), "to": a(otherPre), "value": "0x1", "gas": gas(30000)},
			{"from": a(alice), "to": a(qi), "value": "0x1"},
			{"from": a(alice), "to": a(qi), "value": "0x8ac7230489e80000"},
			{"from": a(alice), "to": a(qiOther), "value": "0x1"},
			{"from": a(alice), "to": a(alice), "data": "0x" + hex.EncodeToString(append([]byte("Suicide"), bob.Bytes()...))},
			{"from": a(alice), "to": a(alice), "data": "0x" + hex.EncodeToString(append([]byte("Suicide"), qi.Bytes()...))},
			{"from": a(kquai), "to": a(store), "data": "0x" + hex.EncodeToString(append([]byte("update"), 1))},
			{"from": a(kquai), "to": a(store), "data": "0x" + hex.EncodeToString([]byte("freeze"))},
			{"from": a(kquai), "to": a(store), "data": "0x" + hex.EncodeToString([]byte("nope"))},
			{"from": a(store), "to": a(bob)},
			{"from": a(bob), "to": a(store), "value": "0x1", "data": data},
			{"from": a(alice), "to": a(store), "data": data, "gas": gas(0x100)},
			{"from": a(alice), "to": a(store), "gas": gas(21000)},
			{"from": a(alice), "to": a(store), "data": data, "gas": gas(0x5000)},
			{"from": a(alice), "to": a(store), "data": data, "gasPrice": "0x1"},
			{"from": a(bob), "to": a(bob), "gasPrice": "0x1"},
			{"from": a(alice), "to": a(gasslot), "data": data},
			{"from": a(qi), "to": a(bob)},
			{"from": a(other), "to": a(bob)},
			{"to": a(store), "data": data},
			{"from": a(alice), "to": a(store), "data": "0x01", "input": "0x02"},
			{"from": a(alice)},
			{"from": a(alice), "input": "0x" + initRuntime0},
		} {
			if method == "quai_estimateGas" && (args["to"] == a(qi) || args["to"] == a(qiOther)) {
				// A conversion estimate (EstimateGas's own branch).
				continue
			}
			add(method, args, at)
		}
		add(method, obj{"from": a(alice), "to": a(store), "data": data}, "pending")
		add(method, obj{"from": a(alice), "to": a(store), "data": data, "gas": gas(0x30000)}, "pending")
		add(method, obj{"from": a(alice), "to": a(store), "data": data}, "0x7fffffff")
		add(method, obj{"from": a(alice), "to": a(store), "data": data}, obj{"blockHash": common.Hash{0x11}.Hex()})
	}
	add("quai_estimateGas", obj{"from": a(alice), "to": a(store), "data": "0x01"})
	add("quai_createAccessList", obj{"from": a(alice), "to": a(store), "data": "0x01"})
	add("quai_gasPrice")
	add("quai_gasPrice", 1)
	return r
}

// ethRequests mirrors the call requests in the eth namespace, with the
// cases where PublicBlockChainAPI differs: EstimateGas has no conversion or
// Qi branch (DoEstimateGas runs them), and CreateAccessList defaults to the
// pending block.
func ethRequests(at interface{}) []request {
	var r []request
	for _, q := range callRequests(at) {
		r = append(r, request{"eth_" + strings.TrimPrefix(q.method, "quai_"), q.params})
	}
	add := func(method string, params ...interface{}) { r = append(r, request{method, params}) }
	add("eth_estimateGas", obj{"from": a(alice), "to": a(qi), "value": "0x1"}, at)
	add("eth_estimateGas", obj{"from": a(alice), "to": a(qi), "value": "0x8ac7230489e80000"}, at)
	add("eth_estimateGas", obj{"from": a(alice), "to": a(qiOther), "value": "0x1"}, at)
	add("eth_estimateGas", obj{"from": a(alice), "to": a(bob), "value": "0x1", "txType": 2}, at)
	add("eth_estimateGas", obj{"from": a(alice), "to": a(bob), "value": "0x1", "txType": 1}, at)
	add("quai_estimateGas", obj{"from": a(alice), "to": a(bob), "value": "0x1", "txType": 1}, at)
	add("eth_createAccessList", obj{"from": a(alice), "to": a(store), "data": "0x01"}, nil)
	add("eth_call", obj{"from": a(alice), "to": a(store), "data": "0x01"}, at, obj{})
	return r
}

func main() {
	log.Global.SetOutput(io.Discard)
	vm.PrecompiledContracts = make(map[common.AddressBytes]vm.PrecompiledContract)
	vm.PrecompiledAddresses = make(map[string][]common.Address)
	vm.InitializePrecompiles(loc)
	eligible := common.Hash{}
	eligible[0] = 1 // only zone [0 0]
	everywhere := common.Hash{}
	for i := range everywhere {
		everywhere[i] = 0xff
	}
	base := func(number, prime uint64) headerSpec {
		return headerSpec{number: number, prime: prime, time: 1700000000 + number*5, difficulty: u(400000000000), baseFee: u(1000000000), gasLimit: 30000000, stateSize: u(1 << 20), avgFees: u(0), exchange: params.ExchangeRate, eligible: eligible, shaDiff: u(0), shaCount: u(0), scryptCount: u(0)}
	}
	mainHead := base(2000000, 2444574)
	mainHead.shaDiff = new(big.Int).Mul(u(167000), u(1000000000000))
	mainHead.shaCount = u(1 << 31)
	mainHead.scryptCount = u(1 << 30)
	mainHead.avgFees = u(3000000000000000)
	mainParent := mainHead
	mainParent.number--
	mainParent.time -= 5
	mainParent.stateSize = u(1<<20 - 7)
	mainPrime := mainHead
	mainPrime.number = 1999990
	mainPrime.exchange = u(1500000000000000000)
	mainPrime.eligible = eligible
	early := base(27000, 5000)
	early.stateSize = u(30000)
	earlyParent := early
	earlyParent.number--
	earlyParent.time -= 5
	earlyParent.stateSize = u(29990)
	earlyPrime := early
	earlyPrime.number = 26990
	earlyPrime.exchange = u(221077819000000000)
	openEtx := mainHead
	openEtx.eligible = everywhere
	openParent := mainParent
	openParent.eligible = everywhere
	openPrime := mainPrime
	openPrime.eligible = everywhere
	fork := base(3500000, 2500000)
	forkParent := fork
	forkParent.number--
	forkParent.time -= 5
	forkPrime := fork
	forkPrime.number = 3499990
	year := base(6307200, 2600000)
	yearParent := year
	yearParent.number--
	yearParent.time -= 5
	yearPrime := year
	yearPrime.number = 6307190
	scenarios := []scenario{
		{"main", mainHead, mainParent, mainPrime, false, callRequests("0x1e8480")},
		{"early", early, earlyParent, earlyPrime, false, callRequests("latest")},
		{"etx-eligible", openEtx, openParent, openPrime, false, []request{
			{"quai_call", []interface{}{obj{"from": a(alice), "to": a(other), "value": "0x1"}, "latest"}},
			{"quai_estimateGas", []interface{}{obj{"from": a(alice), "to": a(other), "value": "0x1"}, "latest"}},
			{"quai_createAccessList", []interface{}{obj{"from": a(alice), "to": a(other), "value": "0x1"}, "latest"}},
			{"quai_estimateGas", []interface{}{obj{"from": a(alice), "to": a(other), "value": "0x1", "data": "0x0102"}, "latest"}},
		}},
		{"max-code-size", fork, forkParent, forkPrime, false, []request{
			{"quai_call", []interface{}{obj{"from": a(alice), "to": a(push0), "data": "0x01"}, "latest"}},
			{"quai_estimateGas", []interface{}{obj{"from": a(alice), "to": a(push0), "data": "0x01"}, "latest"}},
			{"quai_gasPrice", []interface{}{}},
		}},
		{"after-first-year", year, yearParent, yearPrime, false, []request{
			{"quai_call", []interface{}{obj{"from": a(kquai), "to": a(store), "data": "0x" + hex.EncodeToString([]byte("freeze"))}, "latest"}},
			{"quai_createAccessList", []interface{}{obj{"from": a(kquai), "to": a(store), "data": "0x" + hex.EncodeToString([]byte("freeze"))}, "latest"}},
		}},
	}
	// quai_gasPrice over heads whose base fee goes up, down and stays flat,
	// and the genesis head and its child.
	fee := func(id string, h headerSpec, primeExchange *big.Int) scenario {
		p := h
		p.number--
		p.time -= 5
		pr := h
		pr.number = h.number - 3
		pr.exchange = primeExchange
		return scenario{id, h, p, pr, false, []request{{"quai_gasPrice", []interface{}{}}, {"quai_call", []interface{}{obj{"from": a(alice), "to": a(bob), "value": "0x1"}, "latest"}}}}
	}
	feeA := base(100000, 20000)
	feeB := feeA
	feeB.number++
	feeB.difficulty = u(200000000000) // lower difficulty: fee up
	feeC := feeB
	feeC.number++
	feeC.difficulty = u(800000000000) // higher difficulty: fee down
	feeD := feeC
	feeD.number++ // flat
	kaw := base(1500000, 1200000)
	kaw.shaDiff = u(5000000000000)
	kaw.shaCount = u(1 << 30)
	kaw.scryptCount = u(1 << 29)
	lock := base(5000000, 2300000)
	lock.shaDiff = new(big.Int).Mul(u(167000), u(2000000000000))
	lock.shaCount = u(3 << 30)
	lock.scryptCount = u(1 << 30)
	lock.avgFees = u(7000000000000000)
	scenarios = append(scenarios,
		fee("fee-a", feeA, u(221077819000000000)),
		fee("fee-b-up", feeB, u(221077819000000000)),
		fee("fee-c-down", feeC, u(221077819000000000)),
		fee("fee-d-flat", feeD, u(221077819000000000)),
		fee("fee-exchange-up", feeD, u(442155638000000000)),
		fee("fee-kawpow", kaw, u(900000000000000000)),
		fee("fee-conversion-lock", lock, u(1300000000000000000)),
	)
	one := base(1, 0)
	gen := base(0, 0)
	onePrime := one
	onePrime.number = 0
	scenarios = append(scenarios,
		scenario{"fee-child-of-genesis", one, gen, onePrime, true, []request{{"quai_gasPrice", []interface{}{}}}},
		scenario{"eth", mainHead, mainParent, mainPrime, false, ethRequests("0x1e8480")},
	)
	out := []interface{}{}
	for _, sc := range scenarios {
		out = append(out, run(sc))
	}
	must(json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"reference_revision": referenceRevision, "location": []int{0, 0}, "chain_id": 9, "rpc_gas_cap": params.GasCeil, "scenarios": out, "scope": "Production quaiapi handlers (DoCall, DoEstimateGas, AccessList, CreateAccessList, GasPrice) and rpc.Server over controlled committed state and headers; block context from core.NewEVMBlockContext, base fee from HeaderChain.CalcBaseFee. Access lists sorted and message pointers masked for determinism. No network, keys or proof-of-work."}))
}
