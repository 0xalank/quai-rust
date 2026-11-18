// Controlled-state oracle using the production go-quai API, StateDB and EVM.
// go-quai retains its upstream copyright and license; no API handlers are copied.
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"math/big"
	"net"
	"net/http"
	"os"
	"reflect"
	"sort"
	"sync"
	"time"
	"unicode"

	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/bloombits"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/core/vm"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/ethdb"
	"github.com/dominant-strategies/go-quai/event"
	"github.com/dominant-strategies/go-quai/internal/quaiapi"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/dominant-strategies/go-quai/quai/filters"
	"github.com/dominant-strategies/go-quai/rpc"
	"github.com/dominant-strategies/go-quai/trie"
)

var referenceRevision string
var loc = common.Location{0, 0}
var account = common.HexToAddress("0x0010000000000000000000000000000000000001", loc)
var contract = common.HexToAddress("0x0010000000000000000000000000000000000002", loc)
var mutator = common.HexToAddress("0x0010000000000000000000000000000000000004", loc)
var reverting = common.HexToAddress("0x0010000000000000000000000000000000000003", loc)
var qi = common.HexToAddress("0x0080000000000000000000000000000000000001", loc)
var utxoHash = common.HexToHash("0x1234")

type snapshot struct {
	head  *types.WorkObject
	state *state.StateDB
	root  common.Hash
}
type backend struct {
	quaiapi.Backend // Unused handlers are not enabled by the oracle HTTP profile.
	disk            ethdb.Database
	cfg             params.ChainConfig
	snapshots       map[string]*snapshot
	selected        string
	nodeCtx         int
	processing      bool
	receipts        map[common.Hash]types.Receipts
}

func (b *backend) current() *snapshot { return b.snapshots[b.selected] }
func (b *backend) NodeLocation() common.Location {
	if b.nodeCtx == common.REGION_CTX {
		return common.Location{0}
	}
	return loc
}
func (b *backend) NodeCtx() int                             { return b.nodeCtx }
func (b *backend) ProcessingState() bool                    { return b.processing }
func (b *backend) ChainConfig() *params.ChainConfig         { return &b.cfg }
func (b *backend) Config() *params.ChainConfig              { return &b.cfg }
func (b *backend) Database() ethdb.Database                 { return b.disk }
func (b *backend) ChainDb() ethdb.Database                  { return b.disk }
func (b *backend) CurrentHeader() *types.WorkObject         { return b.current().head }
func (b *backend) CurrentBlock() *types.WorkObject          { return b.CurrentHeader() }
func (b *backend) PendingBlock() *types.WorkObject          { return b.CurrentHeader() }
func (b *backend) RPCGasCap() uint64                        { return 1_000_000 }
func (b *backend) RPCTxFeeCap() float64                     { return 1 }
func (b *backend) Logger() *log.Logger                      { return log.Global }
func (b *backend) RpcVersion() string                       { return "v1" }
func (b *backend) GetSlicesRunning() []common.Location      { return []common.Location{b.NodeLocation()} }
func (b *backend) GetExpansionNumber() uint8                { return 0 }
func (b *backend) CalcBaseFee(h *types.WorkObject) *big.Int { return new(big.Int).Set(h.BaseFee()) }
func (b *backend) GetPoolNonce(context.Context, common.Address) (uint64, error) {
	if b.selected == "alternate" {
		return 10, nil
	}
	return 8, nil
}
func (b *backend) PeerCount() uint                    { return 0 }
func (b *backend) PeerCountByDirection() (uint, uint) { return 0, 0 }
func (b *backend) HeaderByNumber(_ context.Context, n rpc.BlockNumber) (*types.WorkObject, error) {
	if n == rpc.LatestBlockNumber || n == rpc.PendingBlockNumber {
		return b.CurrentHeader(), nil
	}
	if n == 0 {
		return b.snapshots["ancestor"].head, nil
	}
	if n == 1 {
		return b.snapshots["parent"].head, nil
	}
	if n == 2 {
		return b.CurrentHeader(), nil
	}
	return nil, errors.New("header not found")
}
func (b *backend) HeaderByHash(_ context.Context, hash common.Hash) (*types.WorkObject, error) {
	for _, s := range b.snapshots {
		if s.head.Hash() == hash {
			return s.head, nil
		}
	}
	return nil, errors.New("header not found")
}
func (b *backend) HeaderByNumberOrHash(ctx context.Context, selector rpc.BlockNumberOrHash) (*types.WorkObject, error) {
	if n, ok := selector.Number(); ok {
		return b.HeaderByNumber(ctx, n)
	}
	if h, ok := selector.Hash(); ok {
		head, err := b.HeaderByHash(ctx, h)
		if err != nil {
			return nil, err
		}
		if selector.RequireCanonical && h != b.CurrentHeader().Hash() && h != b.snapshots["parent"].head.Hash() {
			return nil, errors.New("hash is not currently canonical")
		}
		return head, nil
	}
	return nil, errors.New("missing block selector")
}
func (b *backend) BlockByHash(c context.Context, h common.Hash) (*types.WorkObject, error) {
	return b.HeaderByHash(c, h)
}
func (b *backend) BlockByNumber(c context.Context, n rpc.BlockNumber) (*types.WorkObject, error) {
	return b.HeaderByNumber(c, n)
}
func (b *backend) BlockByNumberOrHash(c context.Context, n rpc.BlockNumberOrHash) (*types.WorkObject, error) {
	return b.HeaderByNumberOrHash(c, n)
}
func (b *backend) GetBlockByHash(h common.Hash) *types.WorkObject {
	head, _ := b.HeaderByHash(context.Background(), h)
	return head
}
func (b *backend) StateAndHeaderByNumberOrHash(c context.Context, n rpc.BlockNumberOrHash) (*state.StateDB, *types.WorkObject, error) {
	h, err := b.HeaderByNumberOrHash(c, n)
	if err != nil {
		return nil, nil, err
	}
	for _, s := range b.snapshots {
		if s.head.Hash() == h.Hash() {
			return s.state.Copy(), h, nil
		}
	}
	return nil, nil, errors.New("state not found")
}
func (b *backend) StateAndHeaderByNumber(c context.Context, n rpc.BlockNumber) (*state.StateDB, *types.WorkObject, error) {
	return b.StateAndHeaderByNumberOrHash(c, rpc.BlockNumberOrHashWithNumber(n))
}
func (b *backend) GetEVM(_ context.Context, msg core.Message, db *state.StateDB, h, _ *types.WorkObject, cfg *vm.Config) (*vm.EVM, func() error, error) {
	block := vm.BlockContext{CanTransfer: core.CanTransfer, Transfer: core.Transfer, BlockNumber: h.Number(common.ZONE_CTX), Time: big.NewInt(1700000000), Difficulty: big.NewInt(1), BaseFee: h.BaseFee(), QuaiStateSize: new(big.Int), GasLimit: b.RPCGasCap(), PrimaryCoinbase: common.ZeroAddress(loc), PrimeTerminusNumber: 0}
	return vm.NewEVM(block, core.NewEVMTxContext(msg), db, &b.cfg, *cfg, nil), func() error { return nil }, nil
}
func (b *backend) UTXOsByAddress(_ context.Context, a common.Address) ([]*types.UtxoEntry, error) {
	var result []*types.UtxoEntry
	for i := uint16(0); i < 3; i++ {
		if u := rawdb.GetUTXO(b.disk, utxoHash, i); u != nil && bytes.Equal(u.Address, a.Bytes()) {
			result = append(result, u)
		}
	}
	return result, nil
}
func (b *backend) AddressOutpoints(_ context.Context, a common.Address) ([]*types.OutpointAndDenomination, error) {
	if !a.Equal(qi) {
		return []*types.OutpointAndDenomination{}, nil
	}
	return []*types.OutpointAndDenomination{{TxHash: utxoHash, Index: 0, Denomination: 7, Lock: big.NewInt(0)}, {TxHash: utxoHash, Index: 1, Denomination: 0, Lock: big.NewInt(105)}, {TxHash: utxoHash, Index: 2, Denomination: 1, Lock: big.NewInt(0)}}, nil
}
func (b *backend) GetOutpointsByAddressAndRange(c context.Context, a common.Address, start, end uint32) ([]*types.OutpointAndDenomination, error) {
	if start > 2 || end < 1 {
		return []*types.OutpointAndDenomination{}, nil
	}
	return b.AddressOutpoints(c, a)
}

func receiptHashes(b *backend) []common.Hash {
	var ids []common.Hash
	for _, tx := range b.CurrentHeader().Transactions() {
		ids = append(ids, tx.Hash())
	}
	return ids
}
func (b *backend) CalcOrder(*types.WorkObject) (*big.Int, int, error) {
	return new(big.Int), common.ZONE_CTX, nil
}
func (b *backend) TotalLogEntropy(*types.WorkObject) *big.Int        { return new(big.Int) }
func (b *backend) GetPoolTransaction(common.Hash) *types.Transaction { return nil }
func (b *backend) GetTransaction(_ context.Context, hash common.Hash) (*types.Transaction, common.Hash, uint64, uint64, error) {
	for _, name := range []string{"ancestor", "parent", b.selected} {
		h := b.snapshots[name].head
		for i, tx := range h.Transactions() {
			if tx.Hash() == hash {
				return tx, h.Hash(), h.NumberU64(common.ZONE_CTX), uint64(i), nil
			}
		}
	}
	return nil, common.Hash{}, 0, 0, errors.New("transaction not found")
}
func (b *backend) GetReceipts(_ context.Context, hash common.Hash) (types.Receipts, error) {
	return b.receipts[hash], nil
}
func (b *backend) GetLogs(_ context.Context, hash common.Hash) ([][]*types.Log, error) {
	var logs [][]*types.Log
	for _, r := range b.receipts[hash] {
		logs = append(logs, r.Logs)
	}
	return logs, nil
}
func (b *backend) GetBloom(hash common.Hash) (*types.Bloom, error) {
	v := types.CreateBloom(b.receipts[hash])
	return &v, nil
}
func (b *backend) GetBlock(hash common.Hash, _ uint64) (*types.WorkObject, error) {
	return b.HeaderByHash(context.Background(), hash)
}
func (b *backend) BloomStatus() (uint64, uint64)                            { return 4096, 0 }
func (b *backend) ServiceFilter(context.Context, *bloombits.MatcherSession) {}
func subscription() event.Subscription {
	return event.NewSubscription(func(quit <-chan struct{}) error { <-quit; return nil })
}
func (b *backend) SubscribeChainEvent(chan<- core.ChainEvent) event.Subscription {
	return subscription()
}
func (b *backend) SubscribeChainHeadEvent(chan<- core.ChainHeadEvent) event.Subscription {
	return subscription()
}
func (b *backend) SubscribeRemovedLogsEvent(chan<- core.RemovedLogsEvent) event.Subscription {
	return subscription()
}
func (b *backend) SubscribeLogsEvent(chan<- []*types.Log) event.Subscription { return subscription() }
func (b *backend) SubscribePendingLogsEvent(chan<- []*types.Log) event.Subscription {
	return subscription()
}
func (b *backend) SubscribePendingHeaderEvent(chan<- *types.WorkObject) event.Subscription {
	return subscription()
}
func (b *backend) SubscribeNewWorkshareEvent(chan<- core.NewWorkshareEvent) event.Subscription {
	return subscription()
}
func (b *backend) SubscribeUnlocksEvent(chan<- core.UnlocksEvent) event.Subscription {
	return subscription()
}

func mustInternal(a common.Address) common.InternalAddress {
	v, e := a.InternalAndQuaiAddress()
	if e != nil {
		panic(e)
	}
	return v
}
func must(e error) {
	if e != nil {
		panic(e)
	}
}
func newBackend(nodeCtx int, processing bool) *backend {
	disk := rawdb.NewMemoryDatabase(log.Global)
	cfg := *params.TestChainConfig
	cfg.Location = loc
	cfg.ChainID = big.NewInt(1337)
	b := &backend{disk: disk, cfg: cfg, snapshots: map[string]*snapshot{}, selected: "canonical", nodeCtx: nodeCtx, processing: processing, receipts: map[common.Hash]types.Receipts{}}
	stateDB := state.NewDatabase(disk)
	for _, item := range []struct {
		name    string
		height  int64
		balance int64
		nonce   uint64
		slot    int64
	}{{"ancestor", 0, 1000000000, 6, 7}, {"parent", 1, 1000000000, 6, 7}, {"canonical", 2, 2000000000, 7, 42}, {"alternate", 2, 3000000000, 9, 99}} {
		db, err := state.New(common.Hash{}, common.Hash{}, new(big.Int), stateDB, state.NewDatabase(disk), nil, loc, log.Global)
		must(err)
		db.SetBalance(mustInternal(account), big.NewInt(item.balance))
		db.SetNonce(mustInternal(account), item.nonce)
		db.SetCode(mustInternal(contract), common.FromHex("60005460005260206000f3"))
		db.SetState(mustInternal(contract), common.Hash{}, common.BigToHash(big.NewInt(item.slot)))
		db.SetCode(mustInternal(mutator), common.FromHex("606360005560005460005260206000f3"))
		db.SetState(mustInternal(mutator), common.Hash{}, common.BigToHash(big.NewInt(7)))
		db.SetCode(mustInternal(reverting), common.FromHex("63deadbeef6000526004601cfd"))
		root, err := db.Commit(false)
		must(err)
		must(stateDB.TrieDB().Commit(root, false, nil))
		db, err = state.New(root, common.Hash{}, new(big.Int), stateDB, state.NewDatabase(disk), nil, loc, log.Global)
		must(err)
		head := types.EmptyWorkObject(common.ZONE_CTX)
		head.Body().SetInterlinkHashes(common.Hashes{})
		head.WorkObjectHeader().SetPrimaryCoinbase(common.ZeroAddress(loc))
		head.WorkObjectHeader().SetLocation(loc)
		head.Body().SetManifest(types.BlockManifest{})
		head.SetNumber(big.NewInt(item.height), common.ZONE_CTX)
		head.WorkObjectHeader().SetTime(uint64(1700000000 + item.height))
		head.Header().SetEVMRoot(root)
		head.Header().SetBaseFee(big.NewInt(10))
		head.Header().SetGasLimit(b.RPCGasCap())
		head.WorkObjectHeader().SetDifficulty(big.NewInt(1000000000))
		head.Header().SetExchangeRate(big.NewInt(1000000000))
		if item.height >= 1 {
			parentName := "parent"
			if item.name == "parent" {
				parentName = "ancestor"
			}
			head.SetParentHash(b.snapshots[parentName].head.Hash(), common.ZONE_CTX)
			head.Header().SetPrimeTerminusHash(b.snapshots[parentName].head.Hash())
		}
		if item.height > 0 {
			key, e := crypto.HexToECDSA(fmt.Sprintf("%064x", 1))
			must(e)
			for seed := int64(1); ; seed++ {
				key, e = crypto.HexToECDSA(fmt.Sprintf("%064x", seed))
				must(e)
				addr := crypto.PubkeyToAddress(key.PublicKey, loc)
				if addr.Bytes()[0] == 0 && addr.IsInQuaiLedgerScope() {
					break
				}
			}
			nonce := uint64(item.nonce)
			normal, e := types.SignTx(types.NewTx(&types.QuaiTx{ChainID: cfg.ChainID, Nonce: nonce, GasPrice: big.NewInt(10), Gas: 50000, To: &contract, Value: big.NewInt(item.slot), AccessList: types.AccessList{}}), types.NewSigner(cfg.ChainID, loc), key)
			must(e)
			ext := types.NewTx(&types.ExternalTx{OriginatingTxHash: utxoHash, ETXIndex: uint16(nonce), Gas: 30000, To: &contract, Value: big.NewInt(item.slot), Sender: account, EtxType: types.DefaultType})
			qiTx := types.NewTx(&types.QiTx{ChainID: cfg.ChainID, TxIn: types.TxIns{{PreviousOutPoint: types.OutPoint{TxHash: utxoHash, Index: uint16(nonce)}, PubKey: crypto.CompressPubkey(&key.PublicKey)}}, TxOut: types.TxOuts{{Denomination: 0, Address: qi.Bytes(), Lock: big.NewInt(0)}}})
			txs := types.Transactions{normal, ext, qiTx}
			head.Body().SetTransactions(txs)
			head.Body().SetOutboundEtxs(types.Transactions{ext})
			head.WorkObjectHeader().SetTxHash(types.DeriveSha(txs, trie.NewStackTrie(nil)))
		}
		head.WorkObjectHeader().SetHeaderHash(head.Header().Hash())
		b.snapshots[item.name] = &snapshot{head: head, state: db, root: root}
		rawdb.WriteUTXOSetSize(disk, head.Hash(), 1000)
		var rs types.Receipts
		for i, tx := range head.Transactions() {
			receipt := &types.Receipt{Type: tx.Type(), Status: types.ReceiptStatusSuccessful, TxHash: tx.Hash(), GasUsed: 21000, CumulativeGasUsed: uint64(i+1) * 21000, BlockHash: head.Hash(), BlockNumber: big.NewInt(item.height), TransactionIndex: uint(i), ContractAddress: common.ZeroAddress(loc), Logs: types.Logs{}}
			if i == 0 {
				receipt.Logs = types.Logs{{Address: contract, Topics: []common.Hash{common.HexToHash("0xabcd")}, Data: []byte{byte(item.slot)}, BlockNumber: uint64(item.height), TxHash: tx.Hash(), TxIndex: uint(i), BlockHash: head.Hash(), Index: 0}}
				receipt.OutboundEtxs = types.Transactions{head.OutboundEtxs()[0]}
			}
			if i == 1 {
				receipt.Status = types.ReceiptStatusFailed
			}
			receipt.Bloom = types.CreateBloom(types.Receipts{receipt})
			rs = append(rs, receipt)
		}
		b.receipts[head.Hash()] = rs
	}
	must(rawdb.CreateUTXO(disk, utxoHash, 0, &types.UtxoEntry{Address: qi.Bytes(), Denomination: 7, Lock: big.NewInt(0)}))
	must(rawdb.CreateUTXO(disk, utxoHash, 1, &types.UtxoEntry{Address: qi.Bytes(), Denomination: 0, Lock: big.NewInt(105)}))
	return b
}

func main() {
	versionDir := flag.String("version-dir", "reference/chain", "directory containing the pinned Go VERSION file")
	listen := flag.String("listen", "127.0.0.1:0", "loopback address")
	ctx := flag.String("context", "zone", "zone or region")
	processing := flag.Bool("processing-state", true, "state API permission")
	flag.Parse()
	host, _, err := net.SplitHostPort(*listen)
	must(err)
	if net.ParseIP(host) == nil || !net.ParseIP(host).IsLoopback() {
		panic("oracle requires a loopback listener")
	}
	nodeCtx := common.ZONE_CTX
	if *ctx == "region" {
		nodeCtx = common.REGION_CTX
	} else if *ctx != "zone" {
		panic("unknown context")
	}
	log.Global.SetOutput(io.Discard)
	must(os.Chdir(*versionDir))
	params.GitCommit = referenceRevision
	params.InitVersion()
	b := newBackend(nodeCtx, *processing)
	defer b.disk.Close()
	server := rpc.NewServer(log.Global)
	defer server.Stop()
	services := []struct {
		namespace string
		service   interface{}
	}{{"quai", quaiapi.NewPublicQuaiAPI(b)}, {"eth", quaiapi.NewPublicQuaiAPI(b)}, {"quai", quaiapi.NewPublicBlockChainQuaiAPI(b)}, {"eth", quaiapi.NewPublicBlockChainAPI(b)}, {"net", quaiapi.NewPublicNetAPI(1337, b)}}
	if nodeCtx == common.ZONE_CTX {
		pool := quaiapi.NewPublicTransactionPoolAPI(b, new(quaiapi.AddrLocker))
		services = append(services, struct {
			namespace string
			service   interface{}
		}{"quai", pool}, struct {
			namespace string
			service   interface{}
		}{"eth", pool})
	}
	filterAPI := filters.NewPublicFilterAPI(b, time.Minute, 16)
	services = append(services, struct {
		namespace string
		service   interface{}
	}{"quai", filterAPI}, struct {
		namespace string
		service   interface{}
	}{"eth", filterAPI})
	var registered []string
	for _, s := range services {
		must(server.RegisterName(s.namespace, s.service))
		typ := reflect.TypeOf(s.service)
		for i := 0; i < typ.NumMethod(); i++ {
			m := typ.Method(i)
			name := []rune(m.Name)
			name[0] = unicode.ToLower(name[0])
			registered = append(registered, s.namespace+"_"+string(name))
		}
	}
	sort.Strings(registered)
	allowed := []string{"quai_chainId", "quai_nodeLocation", "quai_listRunningChains", "quai_getProtocolExpansionNumber", "quai_blockNumber", "quai_clientVersion", "quai_gasPrice", "quai_getBalance", "quai_getCode", "quai_getStorageAt", "quai_getBlockByNumber", "quai_getBlockByHash", "quai_getTransactionReceipt", "quai_getTransactionByHash", "quai_getLogs", "eth_getLogs", "quai_getUTXO", "quai_getLockedBalance", "quai_getOutpointsByAddress", "quai_getOutPointsByAddressAndRange", "quai_getTransactionCount", "quai_call", "quai_estimateGas", "quai_createAccessList", "quai_estimateFeeForQi", "eth_chainId", "eth_blockNumber", "eth_clientVersion", "eth_gasPrice", "eth_getBalance", "eth_getCode", "eth_getStorageAt", "eth_getTransactionCount", "eth_call", "eth_estimateGas", "eth_createAccessList", "net_version"}
	permitted := map[string]bool{}
	for _, m := range allowed {
		permitted[m] = true
	}
	var mu sync.RWMutex
	profile := func() map[string]interface{} {
		snapshots := map[string]interface{}{}
		for name, s := range b.snapshots {
			snapshots[name] = map[string]interface{}{"hash": s.head.Hash(), "height": s.head.NumberU64(common.ZONE_CTX), "state_root": s.root}
		}
		return map[string]interface{}{"oracle": "go-quai production handlers over controlled StateDB/UTXO/EVM", "scope": "Controlled API integration; headers/state seeded locally, no PoW validation, P2P or synchronized chain", "reference_revision": referenceRevision, "chain_config": b.cfg, "rpc_gas_cap": b.RPCGasCap(), "storage": "isolated in-memory rawdb and committed state trie", "chain_id": "0x539", "location": b.NodeLocation(), "context": *ctx, "processing_state": *processing, "scenario": b.selected, "head_hash": b.CurrentHeader().Hash(), "snapshots": snapshots, "account": account.Hex(), "contract": contract.Hex(), "funded": true, "genesis_hash": b.snapshots["ancestor"].head.Hash(), "receipt_hashes": receiptHashes(b), "reverting_contract": reverting.Hex(), "mutating_contract": mutator.Hex(), "qi_address": qi.Hex(), "utxo_hash": utxoHash, "enabled_methods": allowed, "registered_declarations": registered, "registration_note": "Reflected exported declarations are candidates; enabled methods are the HTTP profile. No complete endpoint coverage claim."}
	}
	mux := http.NewServeMux()
	mux.HandleFunc("/profile", func(w http.ResponseWriter, r *http.Request) {
		mu.RLock()
		defer mu.RUnlock()
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(profile())
	})
	mux.HandleFunc("/scenario", func(w http.ResponseWriter, r *http.Request) {
		if r.Method != "POST" {
			w.WriteHeader(405)
			return
		}
		var x struct {
			Name string `json:"name"`
		}
		if json.NewDecoder(io.LimitReader(r.Body, 1024)).Decode(&x) != nil || (x.Name != "canonical" && x.Name != "alternate") {
			w.WriteHeader(400)
			return
		}
		mu.Lock()
		defer mu.Unlock()
		b.selected = x.Name
		json.NewEncoder(w).Encode(profile())
	})
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, 1024*1024))
		if err != nil {
			w.WriteHeader(413)
			return
		}
		// Disable every handler outside the explicit controlled-state read profile.
		var value interface{}
		decoder := json.NewDecoder(bytes.NewReader(body))
		decoder.UseNumber()
		if json.Valid(body) && decoder.Decode(&value) == nil {
			var restrict func(interface{})
			restrict = func(v interface{}) {
				switch q := v.(type) {
				case []interface{}:
					for _, v := range q {
						restrict(v)
					}
				case map[string]interface{}:
					if method, ok := q["method"].(string); ok && !permitted[method] {
						q["method"] = "oracle_disabled"
					}
				}
			}
			restrict(value)
			body, err = json.Marshal(value)
			must(err)
		}
		r.Body = io.NopCloser(bytes.NewReader(body))
		r.ContentLength = int64(len(body))
		server.ServeHTTP(w, r)
	})
	listener, err := net.Listen("tcp", *listen)
	must(err)
	json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"ready": true, "address": listener.Addr().String(), "mode": "controlled-state"})
	fmt.Fprintln(os.Stderr, "controlled-state oracle ready")
	must((&http.Server{Handler: mux, ReadHeaderTimeout: 5 * time.Second}).Serve(listener))
}
