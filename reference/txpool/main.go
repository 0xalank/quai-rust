// Transaction pool oracle: go-quai v0.57.0's production TxPool
// (core/tx_pool.go, core/tx_list.go) behind the production JSON-RPC server
// and quaiapi handlers (quai_sendRawTransaction,
// quai_receiveTxFromPoolSharingClient, quai_getTransactionCount "pending",
// quai_getTransactionByHash, txpool_*), over a test chain: committed state
// per head, a UTXO set in the database and work objects with transactions.
//
// Each scenario is a list of steps. A step submits transactions through the
// RPC (local, as AddLocal) or as a gossip batch (AddRemotes), runs one pool
// reorganisation (the production runReorg that the scheduler starts every
// second), moves the head (a reset from the old head to the new one, as the
// chain head event starts it), ages heartbeats or transaction times and lets
// the production eviction and Qi expiry loops run, or asks the RPC. After
// each step the oracle records the pool: pending and queued transactions per
// account in nonce order, the Qi pool in LRU order, locals, counts, slots and
// the pending nonce of the scenario's accounts.
//
// The scheduler's ticker is off (ReorgFrequency 10,000 hours): runs happen
// only at "run" and "head" steps, with the dirty accounts the adds since the
// last run reported (a sender whose transaction was added without replacing
// one). Sharing clients are local httptest servers that record each request.
// Keys are derived from fixed seeds. No network, no real funds.
package main

import (
	"bytes"
	"context"
	"crypto/ecdsa"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"math/big"
	"net/http"
	"net/http/httptest"
	"os"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/btcsuite/btcd/btcec/v2"
	"github.com/btcsuite/btcd/btcec/v2/schnorr"
	"github.com/btcsuite/btcd/btcec/v2/schnorr/musig2"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/ethdb"
	"github.com/dominant-strategies/go-quai/event"
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
func gwei(v uint64) *big.Int {
	return new(big.Int).Mul(u(v), u(params.GWei))
}
func quai(v uint64) *big.Int {
	return new(big.Int).Mul(u(v), u(params.Ether))
}

type obj = map[string]interface{}

// --- keys -------------------------------------------------------------------

type key struct {
	name string
	prv  *ecdsa.PrivateKey
	btc  *btcec.PrivateKey
	addr common.Address
}

func (k key) bytes() []byte { return k.addr.Bytes() }
func (k key) internal() common.InternalAddress {
	var i common.InternalAddress
	copy(i[:], k.addr.Bytes())
	return i
}

// grind derives a key from a fixed seed whose address passes want.
func grind(name string, want func(a []byte) bool) key {
	for i := 0; ; i++ {
		seed := crypto.Keccak256([]byte(fmt.Sprintf("txpool-oracle/%s/%d", name, i)))
		prv, err := crypto.ToECDSA(seed)
		if err != nil {
			continue
		}
		pub := crypto.FromECDSAPub(&prv.PublicKey)
		a := crypto.Keccak256(pub[1:])[12:]
		if want(a) {
			btc, _ := btcec.PrivKeyFromBytes(seed)
			return key{name, prv, btc, common.BytesToAddress(a, loc)}
		}
	}
}
func quaiKey(name string) key {
	return grind(name, func(a []byte) bool { return a[0] == 0x00 && a[1] < 0x80 })
}
func qiKey(name string) key {
	return grind(name, func(a []byte) bool { return a[0] == 0x00 && a[1] >= 0x80 })
}
func zoneKey(name string, prefix byte) key {
	return grind(name, func(a []byte) bool { return a[0] == prefix && a[1] < 0x80 })
}

// --- transactions -----------------------------------------------------------

var chainID = u(9)

type qtx struct {
	nonce   uint64
	to      *common.Address
	value   *big.Int
	price   *big.Int
	gas     uint64
	data    []byte
	access  types.AccessList
	chainID *big.Int
}

func signQuai(k key, t qtx) *types.Transaction {
	id := t.chainID
	if id == nil {
		id = chainID
	}
	value := t.value
	if value == nil {
		value = u(0)
	}
	al := t.access
	if al == nil {
		al = types.AccessList{}
	}
	inner := &types.QuaiTx{ChainID: id, Nonce: t.nonce, GasPrice: t.price, Gas: t.gas, To: t.to, Value: value, Data: t.data, AccessList: al}
	tx, err := types.SignTx(types.NewTx(inner), types.NewSigner(id, loc), k.prv)
	must(err)
	return tx
}
func encode(tx *types.Transaction) []byte {
	pt, err := tx.ProtoEncode()
	must(err)
	b, err := proto.Marshal(pt)
	must(err)
	return b
}
func protoOf(raw []byte) *types.ProtoTransaction {
	pt := new(types.ProtoTransaction)
	must(proto.Unmarshal(raw, pt))
	return pt
}
func marshal(pt *types.ProtoTransaction) []byte {
	b, err := proto.Marshal(pt)
	must(err)
	return b
}

type out struct {
	denom uint8
	addr  []byte
	lock  *big.Int
}

// signQi signs a Qi transaction: one input with BIP-340 Schnorr, more with
// MuSig2 (deterministic nonces).
func signQi(keys []key, ins []types.OutPoint, outs []out, data []byte, id *big.Int) *types.Transaction {
	if id == nil {
		id = chainID
	}
	txIns := make(types.TxIns, len(ins))
	for i := range ins {
		txIns[i] = types.TxIn{PreviousOutPoint: ins[i], PubKey: keys[i].btc.PubKey().SerializeUncompressed()}
	}
	txOuts := make(types.TxOuts, len(outs))
	for i, o := range outs {
		txOuts[i] = types.TxOut{Denomination: o.denom, Address: o.addr, Lock: o.lock}
	}
	inner := &types.QiTx{ChainID: id, TxIn: txIns, TxOut: txOuts, Data: data}
	h, err := types.NewSigner(id, loc).Hash(types.NewTx(inner))
	must(err)
	inner.Signature = qiSign(keys, h)
	return types.NewTx(inner)
}
func qiSign(keys []key, h common.Hash) *schnorr.Signature {
	if len(keys) == 1 {
		sig, err := schnorr.Sign(keys[0].btc, h[:])
		must(err)
		return sig
	}
	pubs := make([]*btcec.PublicKey, len(keys))
	for i, k := range keys {
		pubs[i] = k.btc.PubKey()
	}
	sessions := make([]*musig2.Session, len(keys))
	for i, k := range keys {
		ctx, err := musig2.NewContext(k.btc, false, musig2.WithKnownSigners(pubs))
		must(err)
		seed := crypto.Keccak256(h[:], []byte{byte(i)})
		nonces, err := musig2.GenNonces(musig2.WithPublicKey(k.btc.PubKey()), musig2.WithCustomRand(bytes.NewReader(append(seed, seed...))))
		must(err)
		sessions[i], err = ctx.NewSession(musig2.WithPreGeneratedNonce(nonces))
		must(err)
	}
	for i, s := range sessions {
		for j, o := range sessions {
			if i != j {
				_, err := s.RegisterPubNonce(o.PublicNonce())
				must(err)
			}
		}
	}
	var msg [32]byte
	copy(msg[:], h[:])
	for i, s := range sessions {
		partial, err := s.Sign(msg)
		must(err)
		if i != 0 {
			_, err := sessions[0].CombineSig(partial)
			must(err)
		}
	}
	return sessions[0].FinalSig()
}

// --- chain ------------------------------------------------------------------

// chain is the TxPool's blockChain over the oracle's blocks and state.
type chain struct {
	mu      sync.Mutex
	current *types.WorkObject
	blocks  map[common.Hash]*types.WorkObject
	genesis common.Hash
	cache   state.Database
	hc      *core.HeaderChain
	feed    event.Feed
}

func (c *chain) CurrentBlock() *types.WorkObject {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.current
}
func (c *chain) set(wo *types.WorkObject) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.current = wo
}
func (c *chain) GetBlock(h common.Hash, n uint64) *types.WorkObject {
	c.mu.Lock()
	defer c.mu.Unlock()
	if b := c.blocks[h]; b != nil && b.NumberU64(common.ZONE_CTX) == n {
		return b
	}
	return nil
}
func (c *chain) StateAt(root, etxRoot common.Hash, size *big.Int) (*state.StateDB, error) {
	return state.New(root, etxRoot, size, c.cache, c.cache, nil, loc, log.Global)
}
func (c *chain) SubscribeChainHeadEvent(ch chan<- core.ChainHeadEvent) event.Subscription {
	return c.feed.Subscribe(ch)
}
func (c *chain) IsGenesisHash(h common.Hash) bool { return h == c.genesis }
func (c *chain) CheckIfEtxIsEligible(slices common.Hash, l common.Location) bool {
	return c.hc.CheckIfEtxIsEligible(slices, l)
}
func (c *chain) Engine(*types.WorkObjectHeader) consensus.Engine { return nil }
func (c *chain) GetHeaderOrCandidateByHash(h common.Hash) *types.WorkObject {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.blocks[h]
}
func (c *chain) NodeCtx() int { return common.ZONE_CTX }
func (c *chain) GetHeaderByHash(h common.Hash) *types.WorkObject {
	return c.GetHeaderOrCandidateByHash(h)
}
func (c *chain) GetBlockByHash(h common.Hash) *types.WorkObject {
	return c.GetHeaderOrCandidateByHash(h)
}
func (c *chain) GetMaxTxInWorkShare() uint64                          { return 1000 }
func (c *chain) CheckInCalcOrderCache(common.Hash) (*big.Int, int, bool) { return nil, 0, false }
func (c *chain) AddToCalcOrderCache(common.Hash, int, *big.Int)       {}
func (c *chain) CalcBaseFee(*types.WorkObject) *big.Int               { return u(0) }
func (c *chain) CalcOrder(*types.WorkObject) (*big.Int, int, error) {
	return big.NewInt(0), common.ZONE_CTX, nil
}

// --- RPC backend --------------------------------------------------------------

type backend struct {
	quaiapi.Backend // Methods the pool handlers do not use stay nil.
	cfg             *params.ChainConfig
	chain           *chain
	pool            *core.TxPool
}

func (b *backend) NodeCtx() int                       { return common.ZONE_CTX }
func (b *backend) NodeLocation() common.Location      { return loc }
func (b *backend) ProcessingState() bool              { return true }
func (b *backend) ChainConfig() *params.ChainConfig   { return b.cfg }
func (b *backend) RPCTxFeeCap() float64               { return 10000 }
func (b *backend) Logger() *log.Logger                { return log.Global }
func (b *backend) CurrentHeader() *types.WorkObject   { return b.chain.CurrentBlock() }
func (b *backend) CurrentBlock() *types.WorkObject    { return b.chain.CurrentBlock() }
func (b *backend) SendTxToSharingClients(tx *types.Transaction) error {
	return b.pool.SendTxToSharingClients(tx)
}

// GetPoolNonce, GetPoolTransaction, TxPoolContent(From), Stats and
// GetTransaction as quai/api_backend.go and core.Core.
func (b *backend) GetPoolNonce(_ context.Context, addr common.Address) (uint64, error) {
	internal, err := addr.InternalAndQuaiAddress()
	if err != nil {
		return 0, nil
	}
	return b.pool.Nonce(internal), nil
}
func (b *backend) GetPoolTransaction(h common.Hash) *types.Transaction { return b.pool.Get(h) }
func (b *backend) TxPoolContent() (map[common.InternalAddress]types.Transactions, map[common.InternalAddress]types.Transactions) {
	return b.pool.Content()
}
func (b *backend) TxPoolContentFrom(addr common.Address) (types.Transactions, types.Transactions) {
	internal, err := addr.InternalAndQuaiAddress()
	if err != nil {
		return nil, nil
	}
	return b.pool.ContentFrom(internal)
}
func (b *backend) Stats() (int, int, int) { return b.pool.Stats() }
func (b *backend) GetTransaction(context.Context, common.Hash) (*types.Transaction, common.Hash, uint64, uint64, error) {
	return nil, common.Hash{}, 0, 0, fmt.Errorf("transaction not found")
}
func (b *backend) GetRollingFeeInfo() (min, max, avg *big.Int) { return nil, nil, nil }

// --- blocks -----------------------------------------------------------------

type head struct {
	number      uint64
	prime       uint64
	time        uint64
	difficulty  *big.Int
	baseFee     *big.Int
	gasLimit    uint64
	exchange    *big.Int
	avgFees     *big.Int
	eligible    common.Hash
	shaDiff     *big.Int
	shaCount    *big.Int
	scryptCount *big.Int
}

func makeBlock(s head, parent, primeHash, root common.Hash, txs types.Transactions) *types.WorkObject {
	wo := types.EmptyWorkObject(common.ZONE_CTX)
	wo.SetNumber(u(s.number), common.ZONE_CTX)
	wo.SetParentHash(parent, common.ZONE_CTX)
	wo.WorkObjectHeader().SetLocation(loc)
	wo.WorkObjectHeader().SetTime(s.time)
	wo.WorkObjectHeader().SetDifficulty(s.difficulty)
	wo.WorkObjectHeader().SetPrimeTerminusNumber(u(s.prime))
	wo.WorkObjectHeader().SetPrimaryCoinbase(common.BytesToAddress([]byte{0, 0x11, 0x0c, 0x0b}, loc))
	if s.shaDiff != nil {
		wo.WorkObjectHeader().SetShaDiffAndCount(types.NewPowShareDiffAndCount(s.shaDiff, s.shaCount, u(0)))
		wo.WorkObjectHeader().SetScryptDiffAndCount(types.NewPowShareDiffAndCount(u(1), s.scryptCount, u(0)))
	}
	wo.Header().SetEVMRoot(root)
	wo.Header().SetEtxSetRoot(types.EmptyRootHash)
	wo.Header().SetBaseFee(s.baseFee)
	wo.Header().SetGasLimit(s.gasLimit)
	wo.Header().SetQuaiStateSize(u(0))
	wo.Header().SetAvgTxFees(s.avgFees)
	wo.Header().SetExchangeRate(s.exchange)
	wo.Header().SetEtxEligibleSlices(s.eligible)
	wo.Header().SetPrimeTerminusHash(primeHash)
	wo.Body().SetTransactions(txs)
	wo.WorkObjectHeader().SetHeaderHash(wo.Header().Hash())
	return wo
}
func encodeBlock(wo *types.WorkObject) string {
	pb, err := wo.ProtoEncode(types.BlockObject)
	must(err)
	b, err := proto.Marshal(pb)
	must(err)
	return hx(b)
}

// --- environment --------------------------------------------------------------

type account struct {
	k       key
	nonce   uint64
	balance *big.Int
}
type utxo struct {
	point types.OutPoint
	entry *types.UtxoEntry
}

// sharer is a local sharing client: an httptest server that records each
// request and answers from a list of replies (the last one repeats).
type sharer struct {
	srv     *httptest.Server
	mu      sync.Mutex
	got     []obj
	served  int
	replies []reply
}
type reply struct {
	status int
	body   string
	delay  time.Duration
}

func newSharer(replies ...reply) *sharer {
	s := &sharer{replies: replies}
	s.srv = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		s.mu.Lock()
		n := s.served
		s.served++
		s.got = append(s.got, obj{"method": r.Method, "path": r.URL.Path, "content_type": r.Header.Get("Content-Type"), "accept": r.Header.Get("Accept"), "user_agent": r.Header.Get("User-Agent"), "body": string(body)})
		rp := s.replies[len(s.replies)-1]
		if n < len(s.replies) {
			rp = s.replies[n]
		}
		s.mu.Unlock()
		time.Sleep(rp.delay)
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(rp.status)
		io.WriteString(w, rp.body)
	}))
	return s
}
func (s *sharer) take() []obj {
	s.mu.Lock()
	defer s.mu.Unlock()
	got := s.got
	s.got = nil
	return got
}

type env struct {
	id       string
	cfg      params.ChainConfig
	disk     ethdb.Database
	chain    *chain
	pool     *core.TxPool
	server   *rpc.Server
	names    map[common.Hash]string // block hash -> block name
	blockOut obj
	states   obj
	heads    obj
	utxoSets map[string][]utxo
	utxoOut  obj
	current  string // UTXO set in the database
	tracked  []key
	dirty    map[common.InternalAddress]bool
	steps    []interface{}
	sharers  []*sharer
	signer   types.Signer
	config   obj
	// The start head and UTXO set, and the genesis block's name (or nil).
	startHead, startUtxos string
	genesisName           interface{}
}

func newEnv(id string) *env {
	e := &env{id: id, cfg: *params.TestChainConfig, names: map[common.Hash]string{}, blockOut: obj{}, states: obj{}, heads: obj{}, utxoSets: map[string][]utxo{}, utxoOut: obj{}}
	e.cfg.Location = loc
	e.cfg.ChainID = chainID
	e.disk = rawdb.NewMemoryDatabase(log.Global)
	e.chain = &chain{blocks: map[common.Hash]*types.WorkObject{}, cache: state.NewDatabase(e.disk), hc: core.OracleHeaderChain(e.disk)}
	e.signer = types.LatestSigner(&e.cfg)
	return e
}

// commit writes the accounts into a fresh state and returns its root.
func (e *env) commit(name string, accounts []account) common.Hash {
	st, err := state.New(types.EmptyRootHash, types.EmptyRootHash, u(0), e.chain.cache, e.chain.cache, nil, loc, log.Global)
	must(err)
	list := []interface{}{}
	for _, a := range accounts {
		ia := a.k.internal()
		st.CreateAccount(ia)
		st.SetBalance(ia, a.balance)
		st.SetNonce(ia, a.nonce)
		list = append(list, obj{"address": hx(a.k.bytes()), "nonce": a.nonce, "balance": hx(a.balance.Bytes())})
	}
	root, err := st.Commit(true)
	must(err)
	must(e.chain.cache.TrieDB().Commit(root, false, nil))
	e.states[name] = list
	return root
}

// block registers a work object under a name. Heads carry the name of their
// state and their UTXO set size.
func (e *env) block(name string, wo *types.WorkObject, stateName string, utxoSetSize uint64) common.Hash {
	h := wo.Hash()
	if _, dup := e.names[h]; dup {
		panic("duplicate block hash " + name)
	}
	e.names[h] = name
	e.chain.blocks[h] = wo
	rawdb.WriteUTXOSetSize(e.disk, h, utxoSetSize)
	e.blockOut[name] = obj{"hash": h.Hex(), "number": wo.NumberU64(common.ZONE_CTX), "parent": wo.ParentHash(common.ZONE_CTX).Hex(), "object": encodeBlock(wo)}
	if stateName != "" {
		e.heads[name] = obj{"state": stateName, "utxo_set_size": utxoSetSize}
	}
	return h
}
func (e *env) utxos(name string, list []utxo) {
	e.utxoSets[name] = list
	out := []interface{}{}
	for _, x := range list {
		lock := []byte{}
		if x.entry.Lock != nil {
			lock = x.entry.Lock.Bytes()
		}
		out = append(out, obj{"hash": x.point.TxHash.Hex(), "index": x.point.Index, "denomination": x.entry.Denomination, "address": hx(x.entry.Address), "lock": hx(lock)})
	}
	e.utxoOut[name] = out
}
func (e *env) useUtxos(name string) {
	if e.current != "" {
		for _, x := range e.utxoSets[e.current] {
			rawdb.DeleteUTXO(e.disk, x.point.TxHash, x.point.Index)
		}
	}
	for _, x := range e.utxoSets[name] {
		must(rawdb.CreateUTXO(e.disk, x.point.TxHash, x.point.Index, x.entry))
	}
	e.current = name
}

// start creates the pool at a head, as NewTxPool does at the chain's
// current block, and the RPC server.
func (e *env) start(headName, utxoSet string, conf core.TxPoolConfig, tracked ...key) {
	e.tracked = tracked
	var h *types.WorkObject
	for hash, n := range e.names {
		if n == headName {
			h = e.chain.blocks[hash]
		}
	}
	e.chain.set(h)
	e.useUtxos(utxoSet)
	conf.ReorgFrequency = 10000 * time.Hour
	conf.Journal = ""
	core.OracleTxPoolTimers(2*time.Millisecond, 2*time.Millisecond)
	e.pool = core.NewTxPool(conf, &e.cfg, e.chain, log.Global, e.disk)
	for i := 0; i < 500 && !core.OracleSharingReady(e.pool); i++ {
		time.Sleep(2 * time.Millisecond)
	}
	b := &backend{cfg: &e.cfg, chain: e.chain, pool: e.pool}
	e.server = rpc.NewServer(log.Global)
	must(e.server.RegisterName("quai", quaiapi.NewPublicTransactionPoolAPI(b, new(quaiapi.AddrLocker))))
	must(e.server.RegisterName("txpool", quaiapi.NewPublicTxPoolAPI(b)))
	sharing := []interface{}{}
	for _, s := range conf.SharingClientsEndpoints {
		sharing = append(sharing, s)
	}
	e.config = obj{"account_slots": conf.AccountSlots, "global_slots": conf.GlobalSlots, "account_queue": conf.AccountQueue, "global_queue": conf.GlobalQueue, "price_bump": conf.PriceBump, "price_limit": conf.PriceLimit, "lifetime_s": int64(conf.Lifetime / time.Second), "qi_pool_size": conf.QiPoolSize, "qi_tx_lifetime_s": int64(conf.QiTxLifetime / time.Second), "no_locals": conf.NoLocals, "sync_tx_with_return": conf.SyncTxWithReturn, "sharing_clients": len(conf.SharingClientsEndpoints)}
	locals := []interface{}{}
	for _, l := range conf.Locals {
		locals = append(locals, hx(l[:]))
	}
	e.config["locals"] = locals
	e.startHead = headName
	e.startUtxos = utxoSet
	e.snapshot(obj{"op": "start"})
}

var _ = sort.Strings

func (e *env) stop() {
	e.pool.Stop()
	e.server.Stop()
	for _, s := range e.sharers {
		s.srv.Close()
	}
}

// ask sends one request through the production JSON-RPC server.
func (e *env) ask(method string, params []interface{}) obj {
	body, err := json.Marshal(obj{"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
	must(err)
	req := httptest.NewRequest("POST", "/", bytes.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	e.server.ServeHTTP(rec, req)
	var answer obj
	dec := json.NewDecoder(rec.Body)
	dec.UseNumber()
	must(dec.Decode(&answer))
	result := obj{}
	for _, k := range []string{"result", "error"} {
		if v, ok := answer[k]; ok {
			result[k] = v
		}
	}
	return result
}

// sender is the sender and nonce of a Quai transaction as tx.Hash() recovers
// it (with the transaction's own chain ID), if any.
func sender(raw []byte) (common.InternalAddress, uint64, bool) {
	tx := new(types.Transaction)
	pt := new(types.ProtoTransaction)
	if proto.Unmarshal(raw, pt) != nil || tx.ProtoDecode(pt, loc) != nil || tx.Type() != types.QuaiTxType {
		return common.InternalAddress{}, 0, false
	}
	from, err := types.Sender(types.NewSigner(tx.ChainId(), loc), tx)
	if err != nil {
		return common.InternalAddress{}, 0, false
	}
	var i common.InternalAddress
	copy(i[:], from.Bytes())
	return i, tx.Nonce(), true
}

type pendingAdd struct {
	addr common.InternalAddress
	had  bool
	ok   bool
}

func (e *env) before(raws [][]byte) []pendingAdd {
	out := make([]pendingAdd, len(raws))
	seen := map[[2]interface{}]bool{}
	for i, raw := range raws {
		addr, nonce, ok := sender(raw)
		if !ok {
			continue
		}
		k := [2]interface{}{addr, nonce}
		out[i] = pendingAdd{addr, core.OracleHas(e.pool, addr, nonce) || seen[k], true}
		seen[k] = true
	}
	return out
}
func (e *env) after(p []pendingAdd, errs []bool) {
	any := false
	for i, a := range p {
		if a.ok {
			any = true
		}
		if a.ok && errs[i] && !a.had {
			if e.dirty == nil {
				e.dirty = map[common.InternalAddress]bool{}
			}
			e.dirty[a.addr] = true
		}
	}
	if any && e.dirty == nil {
		e.dirty = map[common.InternalAddress]bool{}
	}
}

// send submits a raw transaction through quai_sendRawTransaction.
func (e *env) send(raw []byte, note string) obj {
	return e.submit("quai_sendRawTransaction", "send", raw, note)
}
func (e *env) receive(raw []byte, note string) obj {
	return e.submit("quai_receiveTxFromPoolSharingClient", "receive", raw, note)
}
func (e *env) submit(method, op string, raw []byte, note string) obj {
	p := e.before([][]byte{raw})
	answer := e.ask(method, []interface{}{hx(raw)})
	_, failed := answer["error"]
	e.after(p, []bool{!failed})
	// The mock sharing clients listen on random ports: name them.
	if er, ok := answer["error"].(map[string]interface{}); ok {
		if m, ok := er["message"].(string); ok {
			for i, s := range e.sharers {
				m = strings.ReplaceAll(m, s.srv.URL, fmt.Sprintf("http://sharing-client-%d", i))
			}
			er["message"] = m
		}
	}
	step := obj{"op": op, "raw": hx(raw), "answer": answer, "note": note}
	tx := new(types.Transaction)
	if pt := new(types.ProtoTransaction); proto.Unmarshal(raw, pt) == nil && tx.ProtoDecode(pt, loc) == nil && tx.Type() == types.QuaiTxType {
		step["size"] = uint64(tx.Size())
	}
	e.shared(step)
	e.snapshot(step)
	return answer
}
func (e *env) sendText(param interface{}, note string) {
	answer := e.ask("quai_sendRawTransaction", []interface{}{param})
	e.snapshot(obj{"op": "rpc", "method": "quai_sendRawTransaction", "params": []interface{}{param}, "answer": answer, "note": note})
}

// remotes adds a gossip batch with AddRemotes, as core.Core's remote queue
// does every two seconds.
func (e *env) remotes(raws [][]byte, note string) {
	txs := make([]*types.Transaction, len(raws))
	for i, raw := range raws {
		tx := new(types.Transaction)
		must(tx.ProtoDecode(protoOf(raw), loc))
		txs[i] = tx
	}
	p := e.before(raws)
	errs := e.pool.AddRemotes(txs)
	ok := make([]bool, len(errs))
	out := []interface{}{}
	hexes := []interface{}{}
	for i, err := range errs {
		ok[i] = err == nil
		if err == nil {
			out = append(out, nil)
		} else {
			out = append(out, err.Error())
		}
		hexes = append(hexes, hx(raws[i]))
	}
	e.after(p, ok)
	e.snapshot(obj{"op": "remotes", "raw": hexes, "errors": out, "note": note})
}

// run is one runReorg without a reset.
func (e *env) run() {
	core.OracleRunReorg(e.pool, nil, nil, e.dirtyList())
	e.dirty = nil
	e.snapshot(obj{"op": "run"})
}
func (e *env) dirtyList() []common.InternalAddress {
	if e.dirty == nil {
		return nil
	}
	list := []common.InternalAddress{}
	for a := range e.dirty {
		list = append(list, a)
	}
	return list
}

// head moves the chain to a block and runs the reset, as the chain head
// event does; utxos is the UTXO set of the database after it.
func (e *env) head(name, utxoSet string, note string) {
	old := e.chain.CurrentBlock()
	var nh *types.WorkObject
	for hash, n := range e.names {
		if n == name {
			nh = e.chain.blocks[hash]
		}
	}
	e.chain.set(nh)
	e.useUtxos(utxoSet)
	core.OracleRunReorg(e.pool, old, nh, e.dirtyList())
	e.dirty = nil
	e.snapshot(obj{"op": "head", "head": name, "utxos": utxoSet, "note": note})
}

// headFrom resets from a head that the chain does not hold.
func (e *env) headFrom(ghost string, gs head, name, utxoSet string, note string) {
	g := makeBlock(gs, common.Hash{0x0e}, common.Hash{}, common.Hash{}, nil)
	e.blockOut[ghost] = obj{"hash": g.Hash().Hex(), "number": g.NumberU64(common.ZONE_CTX), "parent": g.ParentHash(common.ZONE_CTX).Hex(), "object": encodeBlock(g), "stored": false}
	var nh *types.WorkObject
	for hash, n := range e.names {
		if n == name {
			nh = e.chain.blocks[hash]
		}
	}
	e.chain.set(nh)
	e.useUtxos(utxoSet)
	core.OracleRunReorg(e.pool, g, nh, e.dirtyList())
	e.dirty = nil
	e.snapshot(obj{"op": "head", "head": name, "old": ghost, "utxos": utxoSet, "note": note})
}

// ageBeat, ageTx and ageQi move a time back; evict and expireQi wait for the
// production loops (every 2 ms) to reach a steady state.
func (e *env) ageBeat(k key, seconds int64) {
	ok := core.OracleAgeBeat(e.pool, k.internal(), time.Duration(seconds)*time.Second)
	e.snapshot(obj{"op": "age_beat", "account": hx(k.bytes()), "seconds": seconds, "found": ok})
}
func (e *env) ageTx(h common.Hash, seconds int64) {
	ok := core.OracleAgeTx(e.pool, h, time.Duration(seconds)*time.Second)
	e.snapshot(obj{"op": "age_tx", "hash": h.Hex(), "seconds": seconds, "found": ok})
}
func (e *env) ageQi(h common.Hash, seconds int64) {
	ok := core.OracleAgeQi(e.pool, h, time.Duration(seconds)*time.Second)
	e.snapshot(obj{"op": "age_qi", "hash": h.Hex(), "seconds": seconds, "found": ok})
}
func (e *env) evict() {
	time.Sleep(150 * time.Millisecond)
	e.snapshot(obj{"op": "evict"})
}
func (e *env) expireQi() {
	time.Sleep(300 * time.Millisecond)
	e.snapshot(obj{"op": "expire_qi"})
}

// rpc records an RPC answer.
func (e *env) rpc(method string, params ...interface{}) obj {
	if params == nil {
		params = []interface{}{}
	}
	answer := e.ask(method, params)
	e.snapshot(obj{"op": "rpc", "method": method, "params": params, "answer": answer})
	return answer
}

// shared waits briefly for asynchronous sharing requests and adds them to
// the step.
func (e *env) shared(step obj) {
	if len(e.sharers) == 0 {
		return
	}
	time.Sleep(120 * time.Millisecond)
	got := []interface{}{}
	for i, s := range e.sharers {
		for _, r := range s.take() {
			r["client"] = i
			got = append(got, r)
		}
	}
	step["shared"] = got
}

func (e *env) snapshot(step obj) {
	s := core.OracleSnapshot(e.pool)
	byAddr := func(m map[common.InternalAddress][]common.Hash) obj {
		out := obj{}
		for a, hs := range m {
			list := []interface{}{}
			for _, h := range hs {
				list = append(list, h.Hex())
			}
			out[hx(a[:])] = list
		}
		return out
	}
	qi := []interface{}{}
	for _, h := range s.Qi {
		qi = append(qi, h.Hex())
	}
	locals := []interface{}{}
	for _, a := range s.Locals {
		locals = append(locals, hx(a[:]))
	}
	beats := []interface{}{}
	for _, a := range s.Beats {
		beats = append(beats, hx(a[:]))
	}
	nonces := obj{}
	for _, k := range e.tracked {
		if k.addr.IsInQuaiLedgerScope() {
			if _, err := k.addr.InternalAndQuaiAddress(); err == nil {
				nonces[hx(k.bytes())] = e.pool.Nonce(k.internal())
			}
		}
	}
	step["pool"] = obj{"pending": byAddr(s.Pending), "queued": byAddr(s.Queued), "qi": qi, "locals": locals, "local_count": s.LocalCount, "remote_count": s.RemoteCount, "slots": s.Slots, "beats": beats, "nonces": nonces}
	e.steps = append(e.steps, step)
}

func (e *env) out() obj {
	return obj{"id": e.id, "config": e.config, "blocks": e.blockOut, "states": e.states, "heads": e.heads, "utxo_sets": e.utxoOut, "genesis": e.genesisName, "start": obj{"head": e.startHead, "utxos": e.startUtxos}, "steps": e.steps}
}

func main() {
	log.Global.SetOutput(io.Discard)
	scenarios := []func() obj{}
	scenarios = append(scenarios, quaiScenarios()...)
	scenarios = append(scenarios, qiScenarios()...)
	scenarios = append(scenarios, sharingScenarios()...)
	out := []interface{}{}
	for _, s := range scenarios {
		out = append(out, s())
	}
	must(json.NewEncoder(os.Stdout).Encode(obj{"reference_revision": referenceRevision, "location": []int{0, 0}, "chain_id": 9, "rpc_tx_fee_cap": 10000, "scenarios": out, "scope": "Production core.TxPool (core/tx_pool.go, core/tx_list.go) and quaiapi pool handlers behind rpc.Server over a test chain: committed state per head, the UTXO set in the database, work objects with transactions. Runs (runReorg) at explicit steps instead of the 1-second ticker; eviction and Qi expiry by the production loops after aging. Sharing clients are local httptest servers. Fixed-seed keys; no network, no real funds."}))
}

