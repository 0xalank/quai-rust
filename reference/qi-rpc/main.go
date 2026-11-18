// Qi and conversion RPC oracle: go-quai v0.57.0 answers to quai_qiToQuai,
// quai_quaiToQi, quai_calculateConversionAmount, quai_estimateFeeForQi,
// quai_getOutpointDeltasForAddressesInRange, quai_getLockedBalance,
// quai_listRunningChains and quai_getWrappedQiDeposit from the production
// handlers (internal/quaiapi/quai_api.go, PublicBlockChainQuaiAPI) behind the
// production JSON-RPC server (rpc.Server over HTTP), over work objects, a
// rawdb memory database and committed state that the oracle builds. The
// backend mirrors quai/api_backend.go for each method the handlers call.
//
// Block ids. A header whose prime terminus number is at or above the end of
// the KawPow transition (1,171,500 + 129,600) and that has no AuxPow hashes
// to blake3 of an empty AuxPow (core/types/wo.go:1452, 1474), so every such
// header has the same Hash(). Such blocks get a synthetic id
// (keccak256("qi-rpc-oracle/block/<name>")) that stands for the hash in the
// parent and prime terminus links and in every lookup; the handlers that use
// them never hash them, except quai_estimateFeeForQi, which reads the UTXO
// set size under the head's real Hash() (written there). One block, "d7" of
// the outpoint-delta chain, keeps its real (shared) hash: it is the only one
// in the fixture, and the delta handler compares real hashes.
//
// No network, no keys with funds; keys come from fixed seeds.
package main

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/big"
	"net/http/httptest"
	"os"
	"strconv"
	"strings"

	"github.com/btcsuite/btcd/btcec/v2"
	"github.com/btcsuite/btcd/btcec/v2/schnorr"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/common/hexutil"
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
	"github.com/dominant-strategies/go-quai/trie"
	"google.golang.org/protobuf/proto"
)

var referenceRevision string
var loc = common.Location{0, 0}
var chainID = big.NewInt(9)

type obj = map[string]interface{}

func must(e error) {
	if e != nil {
		panic(e)
	}
}
func u(v uint64) *big.Int { return new(big.Int).SetUint64(v) }
func dec(s string) *big.Int {
	v, ok := new(big.Int).SetString(s, 10)
	if !ok {
		panic("bad decimal " + s)
	}
	return v
}
func hx(b []byte) string    { return "0x" + hex.EncodeToString(b) }
func qty(v *big.Int) string { return (*hexutil.Big)(v).String() }
func q64(v uint64) string   { return hexutil.EncodeUint64(v) }
func pow2(n uint) *big.Int  { return new(big.Int).Lsh(big.NewInt(1), n) }
func mul(a *big.Int, b uint64) *big.Int {
	return new(big.Int).Mul(a, u(b))
}
func tag(s string) common.Hash { return crypto.Keccak256Hash([]byte("qi-rpc-oracle/" + s)) }

// --- addresses and keys -------------------------------------------------------

// addr is a deterministic address with the given first two bytes.
func addr(b0, b1 byte, name string) common.Address {
	h := crypto.Keccak256([]byte("qi-rpc-oracle/address/" + name))
	return common.BytesToAddress(append([]byte{b0, b1}, h[:18]...), loc)
}
func lower(a common.Address) string { return hx(a.Bytes()) }

type qiKey struct {
	btc  *btcec.PrivateKey
	addr common.Address
}

// key derives a Qi key of zone [0 0] from a fixed seed.
func key(name string) qiKey {
	for i := 0; ; i++ {
		seed := crypto.Keccak256([]byte(fmt.Sprintf("qi-rpc-oracle/key/%s/%d", name, i)))
		prv, err := crypto.ToECDSA(seed)
		if err != nil {
			continue
		}
		pub := crypto.FromECDSAPub(&prv.PublicKey)
		a := crypto.Keccak256(pub[1:])[12:]
		if a[0] == 0x00 && a[1] >= 0x80 {
			btc, _ := btcec.PrivKeyFromBytes(seed)
			return qiKey{btc, common.BytesToAddress(a, loc)}
		}
	}
}
func (k qiKey) pub() string { return hx(k.btc.PubKey().SerializeUncompressed()) }

// --- transactions ---------------------------------------------------------------

type out struct {
	denom uint8
	to    common.Address
	lock  *big.Int
}

// qiTx is a Qi transaction signed with BIP-340 Schnorr by k (the handlers
// do not check signatures; the encoding needs a parseable one).
func qiTx(k qiKey, ins []types.OutPoint, outs []out) *types.Transaction {
	txIns := make(types.TxIns, len(ins))
	for i := range ins {
		txIns[i] = types.TxIn{PreviousOutPoint: ins[i], PubKey: k.btc.PubKey().SerializeUncompressed()}
	}
	txOuts := make(types.TxOuts, len(outs))
	for i, o := range outs {
		lock := o.lock
		if lock == nil {
			lock = u(0)
		}
		txOuts[i] = types.TxOut{Denomination: o.denom, Address: o.to.Bytes(), Lock: lock}
	}
	inner := &types.QiTx{ChainID: chainID, TxIn: txIns, TxOut: txOuts, Data: []byte{}}
	h, err := types.NewSigner(chainID, loc).Hash(types.NewTx(inner))
	must(err)
	sig, err := schnorr.Sign(k.btc, h[:])
	must(err)
	inner.Signature = sig
	return types.NewTx(inner)
}

var etxSender = addr(0x00, 0x22, "etx-sender")

func etx(origin string, index uint16, typ uint64, to common.Address, value *big.Int, gas uint64, data []byte) *types.Transaction {
	return types.NewTx(&types.ExternalTx{
		OriginatingTxHash: tag("origin/" + origin),
		ETXIndex:          index,
		Gas:               gas,
		To:                &to,
		Value:             value,
		Data:              data,
		AccessList:        types.AccessList{},
		Sender:            etxSender,
		EtxType:           typ,
	})
}

// --- blocks -------------------------------------------------------------------

type spec struct {
	name                       string
	zone, prime                uint64
	parent                     string // block name, "" for none
	ptn                        uint64 // WorkObjectHeader prime terminus number
	terminus                   string // prime terminus block name
	terminusID                 *common.Hash
	difficulty, minerDiff      *big.Int
	exchange, avgFees          *big.Int
	flow, kqd, baseFee         *big.Int
	shaDiff, shaCount, scCount *big.Int
	evmRoot                    common.Hash
	txs                        types.Transactions
	realHash                   bool
}

type block struct {
	name string
	id   common.Hash // lookup key and link value
	wo   *types.WorkObject
	raw  []byte
}

var (
	blocks    = map[string]*block{}
	blockList []string
	ids       = map[common.Hash]string{}
)

func orZero(v *big.Int) *big.Int {
	if v == nil {
		return u(0)
	}
	return v
}

func mk(s spec) *block {
	wo := types.EmptyWorkObject(common.ZONE_CTX)
	wo.SetNumber(u(s.zone), common.ZONE_CTX)
	wo.SetNumber(u(s.prime), common.REGION_CTX)
	wo.SetNumber(u(s.prime), common.PRIME_CTX)
	parent := types.EmptyRootHash
	if s.parent != "" {
		parent = blocks[s.parent].id
	}
	wo.SetParentHash(parent, common.ZONE_CTX)
	wh := wo.WorkObjectHeader()
	wh.SetLocation(loc)
	wh.SetTime(1_700_000_000 + s.zone)
	wh.SetDifficulty(orZero(s.difficulty))
	wh.SetPrimeTerminusNumber(u(s.ptn))
	wh.SetPrimaryCoinbase(common.BytesToAddress([]byte{0, 0x11, 0x0c, 0x0b}, loc))
	if s.shaDiff != nil {
		wh.SetShaDiffAndCount(types.NewPowShareDiffAndCount(s.shaDiff, s.shaCount, u(0)))
		wh.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(u(1), orZero(s.scCount), u(0)))
	}
	h := wo.Header()
	root := s.evmRoot
	if root == (common.Hash{}) {
		root = types.EmptyRootHash
	}
	h.SetEVMRoot(root)
	h.SetEtxSetRoot(types.EmptyRootHash)
	h.SetBaseFee(orZero(s.baseFee))
	h.SetGasLimit(12_000_000)
	h.SetQuaiStateSize(u(0))
	h.SetAvgTxFees(orZero(s.avgFees))
	h.SetExchangeRate(orZero(s.exchange))
	h.SetMinerDifficulty(orZero(s.minerDiff))
	h.SetConversionFlowAmount(orZero(s.flow))
	h.SetKQuaiDiscount(orZero(s.kqd))
	terminus := types.EmptyRootHash
	if s.terminus != "" {
		terminus = blocks[s.terminus].id
	}
	if s.terminusID != nil {
		terminus = *s.terminusID
	}
	h.SetPrimeTerminusHash(terminus)
	if s.txs == nil {
		s.txs = types.Transactions{}
	}
	wo.Body().SetTransactions(s.txs)
	txHash := types.EmptyRootHash
	if len(s.txs) > 0 {
		txHash = types.DeriveSha(s.txs, trie.NewStackTrie(nil))
	}
	h.SetTxHash(txHash)
	wh.SetTxHash(txHash)
	wh.SetHeaderHash(wo.Header().Hash())

	// Keep the object as the database returns it: encoded and decoded.
	pb, err := wo.ProtoEncode(types.BlockObject)
	must(err)
	raw, err := proto.Marshal(pb)
	must(err)
	pb2 := new(types.ProtoWorkObject)
	must(proto.Unmarshal(raw, pb2))
	d := new(types.WorkObject)
	must(d.ProtoDecode(pb2, loc, types.BlockObject))
	if d.Hash() != wo.Hash() {
		panic("hash changed in the round trip: " + s.name)
	}
	id := d.Hash()
	synthetic := s.ptn >= params.KawPowForkBlock+params.KawPowTransitionPeriod
	if synthetic && !s.realHash {
		id = tag("block/" + s.name)
	}
	if prev, dup := ids[id]; dup {
		panic("duplicate block id " + s.name + " " + prev)
	}
	if _, dup := blocks[s.name]; dup {
		panic("duplicate block name " + s.name)
	}
	b := &block{name: s.name, id: id, wo: d, raw: raw}
	blocks[s.name] = b
	blockList = append(blockList, s.name)
	ids[id] = s.name
	return b
}

func blockJSON(b *block) obj {
	txs := []interface{}{}
	for _, t := range b.wo.Transactions() {
		txs = append(txs, t.Hash().Hex())
	}
	etxs := []interface{}{}
	for _, t := range b.wo.OutboundEtxs() {
		etxs = append(etxs, t.Hash().Hex())
	}
	return obj{
		"hash":       b.id.Hex(),
		"number":     b.wo.NumberU64(common.ZONE_CTX),
		"parent":     b.wo.ParentHash(common.ZONE_CTX).Hex(),
		"object":     hx(b.raw),
		"size":       uint64(b.wo.Size()),
		"tx_hashes":  txs,
		"etx_hashes": etxs,
	}
}

// --- backend --------------------------------------------------------------------

// backend implements what PublicBlockChainQuaiAPI's Qi and conversion
// handlers call, with quai/api_backend.go's semantics. Every block of the
// fixture is known by id (canonical or not); the canonical index is the
// scenario's. The oracle has no miner, so the pending block is nil.
type backend struct {
	quaiapi.Backend // methods these handlers do not call stay nil
	cfg             *params.ChainConfig
	db              ethdb.Database
	cache           state.Database
	head            *types.WorkObject
	byID            map[common.Hash]*types.WorkObject
	byNumber        map[uint64]*types.WorkObject
}

func (b *backend) NodeCtx() int                     { return common.ZONE_CTX }
func (b *backend) NodeLocation() common.Location    { return loc }
func (b *backend) ProcessingState() bool            { return true }
func (b *backend) ChainConfig() *params.ChainConfig { return b.cfg }
func (b *backend) Logger() *log.Logger              { return log.Global }
func (b *backend) Database() ethdb.Database         { return b.db }
func (b *backend) ChainDb() ethdb.Database          { return b.db }
func (b *backend) CurrentHeader() *types.WorkObject { return b.head }
func (b *backend) CurrentBlock() *types.WorkObject  { return b.head }
func (b *backend) GetSlicesRunning() []common.Location {
	return []common.Location{loc}
}
func (b *backend) GetBlockByHash(h common.Hash) *types.WorkObject   { return b.byID[h] }
func (b *backend) GetHeaderByHash(h common.Hash) *types.WorkObject  { return b.byID[h] }
func (b *backend) BlockOrCandidateByHash(h common.Hash) *types.WorkObject { return b.byID[h] }
func (b *backend) HeaderByHash(_ context.Context, h common.Hash) (*types.WorkObject, error) {
	return b.byID[h], nil
}
func (b *backend) BlockByHash(_ context.Context, h common.Hash) (*types.WorkObject, error) {
	return b.byID[h], nil
}
func (b *backend) HeaderByNumber(_ context.Context, n rpc.BlockNumber) (*types.WorkObject, error) {
	if n == rpc.PendingBlockNumber {
		return nil, nil // core.PendingBlock(): no miner
	}
	if n == rpc.LatestBlockNumber {
		return b.head, nil
	}
	return b.byNumber[uint64(n)], nil
}
func (b *backend) BlockByNumber(_ context.Context, n rpc.BlockNumber) (*types.WorkObject, error) {
	if n == rpc.PendingBlockNumber {
		return nil, nil
	}
	if n == rpc.LatestBlockNumber {
		n = rpc.BlockNumber(b.head.NumberU64(common.ZONE_CTX))
	}
	return b.byNumber[uint64(n)], nil
}
func (b *backend) stateAt(header *types.WorkObject) (*state.StateDB, error) {
	return state.New(header.EVMRoot(), header.EtxSetRoot(), header.QuaiStateSize(), b.cache, b.cache, nil, loc, log.Global)
}
func (b *backend) StateAndHeaderByNumber(ctx context.Context, n rpc.BlockNumber) (*state.StateDB, *types.WorkObject, error) {
	if n == rpc.PendingBlockNumber {
		return &state.StateDB{}, nil, nil // core.Pending() without a miner
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
		header := b.byID[hash]
		if header == nil {
			return nil, nil, errors.New("header for hash not found")
		}
		if n.RequireCanonical && rawdb.ReadCanonicalHash(b.db, header.NumberU64(common.ZONE_CTX)) != hash {
			return nil, nil, errors.New("hash is not currently canonical")
		}
		st, err := b.stateAt(header)
		return st, header, err
	}
	return nil, nil, errors.New("invalid arguments; neither block nor hash specified")
}

// UTXOsByAddress as core.Core.GetUTXOsByAddress.
func (b *backend) UTXOsByAddress(_ context.Context, address common.Address) ([]*types.UtxoEntry, error) {
	outpoints, err := rawdb.ReadAddressUTXOs(b.db, address.Bytes20())
	if err != nil {
		return nil, err
	}
	utxos := make([]*types.UtxoEntry, 0, len(outpoints))
	for _, o := range outpoints {
		entry := rawdb.GetUTXO(b.db, o.TxHash, o.Index)
		if entry == nil {
			continue
		}
		utxos = append(utxos, entry)
	}
	return utxos, nil
}

// --- scenarios ------------------------------------------------------------------

type utxo struct {
	hash  common.Hash
	index uint16
	denom uint8
	to    common.Address
	lock  *big.Int
}

func (x utxo) json() obj {
	lock := []byte{}
	if x.lock != nil {
		lock = x.lock.Bytes()
	}
	return obj{"tx_hash": x.hash.Hex(), "index": x.index, "denomination": x.denom, "address": lower(x.to), "lock": hx(lock)}
}
func (x utxo) entry() *types.UtxoEntry {
	return &types.UtxoEntry{Denomination: x.denom, Address: x.to.Bytes(), Lock: x.lock}
}

type unlock struct {
	to  common.Address
	amt *big.Int
}
type lockups struct {
	block   string
	locks   map[common.Address]*big.Int
	unlocks []unlock
}
type slot struct {
	address    common.Address
	key, value common.Hash
}
type call struct {
	method string
	params interface{} // nil: no "params" member
}

func c(method string, params ...interface{}) call {
	if params == nil {
		params = []interface{}{}
	}
	return call{method, params}
}
func raw(method string, params interface{}) call { return call{method, params} }

type scenario struct {
	name      string
	head      string
	canonical []string
	utxoSize  map[string]uint64
	utxos     []utxo
	spent     map[string][]utxo
	trimmed   map[string][]utxo
	lockups   []lockups
	storage   []slot
	calls     []call
}

func internal(a common.Address) common.InternalAddress {
	i, err := a.InternalAddress()
	must(err)
	return i
}

func run(sc scenario) obj {
	disk := rawdb.NewMemoryDatabase(log.Global)
	defer disk.Close()
	cache := state.NewDatabase(disk)
	cfg := *params.TestChainConfig
	cfg.Location = loc
	cfg.ChainID = chainID
	b := &backend{cfg: &cfg, db: disk, cache: cache, byID: map[common.Hash]*types.WorkObject{}, byNumber: map[uint64]*types.WorkObject{}}
	for _, name := range blockList {
		bl := blocks[name]
		b.byID[bl.id] = bl.wo
		rawdb.WriteWorkObject(disk, bl.id, bl.wo, types.BlockObject, common.ZONE_CTX)
	}
	canonical := obj{}
	for _, name := range sc.canonical {
		bl := blocks[name]
		n := bl.wo.NumberU64(common.ZONE_CTX)
		if _, dup := b.byNumber[n]; dup {
			panic("two canonical blocks at one number in " + sc.name)
		}
		b.byNumber[n] = bl.wo
		rawdb.WriteCanonicalHash(disk, bl.id, n)
		canonical[strconv.FormatUint(n, 10)] = name
	}
	head := blocks[sc.head]
	if b.byNumber[head.wo.NumberU64(common.ZONE_CTX)] != head.wo {
		panic("head is not canonical in " + sc.name)
	}
	b.head = head.wo
	sizes := obj{}
	for name, size := range sc.utxoSize {
		// Keyed by the real Hash(), as quai_estimateFeeForQi reads it.
		rawdb.WriteUTXOSetSize(disk, blocks[name].wo.Hash(), size)
		sizes[name] = size
	}
	utxoOut := []interface{}{}
	byAddress := map[[20]byte][]*types.OutpointAndDenomination{}
	for _, x := range sc.utxos {
		must(rawdb.CreateUTXO(disk, x.hash, x.index, x.entry()))
		a := x.to.Bytes20()
		byAddress[a] = append(byAddress[a], &types.OutpointAndDenomination{TxHash: x.hash, Index: x.index, Denomination: x.denom, Lock: x.lock})
		utxoOut = append(utxoOut, x.json())
	}
	must(rawdb.WriteAddressUTXOs(disk, disk, byAddress))
	journal := func(list map[string][]utxo, write func(ethdb.KeyValueWriter, common.Hash, []*types.SpentUtxoEntry) error) obj {
		o := obj{}
		for name, entries := range list {
			sutxos := []*types.SpentUtxoEntry{}
			js := []interface{}{}
			for _, x := range entries {
				sutxos = append(sutxos, &types.SpentUtxoEntry{OutPoint: types.OutPoint{TxHash: x.hash, Index: x.index}, UtxoEntry: x.entry()})
				js = append(js, x.json())
			}
			must(write(disk, blocks[name].wo.Hash(), sutxos))
			o[name] = js
		}
		return o
	}
	spent := journal(sc.spent, rawdb.WriteSpentUTXOs)
	trimmed := journal(sc.trimmed, rawdb.WriteTrimmedUTXOs)
	lockedAddresses := map[common.Address]bool{}
	for _, l := range sc.lockups {
		locks := map[common.InternalAddress]*big.Int{}
		for a, v := range l.locks {
			locks[internal(a)] = v
			lockedAddresses[a] = true
		}
		unlocks := []common.Unlock{}
		for _, x := range l.unlocks {
			unlocks = append(unlocks, common.Unlock{Addr: internal(x.to), Amt: x.amt})
			lockedAddresses[x.to] = true
		}
		rawdb.WriteNewLockups(disk, disk, blocks[l.block].wo.Hash(), locks, unlocks)
	}
	locked := obj{}
	for a := range lockedAddresses {
		locked[lower(a)] = hx(rawdb.ReadLockedBalance(disk, internal(a)).Bytes())
	}
	storage := []interface{}{}
	if len(sc.storage) > 0 {
		root := commitStorage(cache, sc.storage)
		if root != head.wo.EVMRoot() {
			panic("storage root differs from the head's EVM root in " + sc.name)
		}
		for _, s := range sc.storage {
			storage = append(storage, obj{"address": lower(s.address), "key": s.key.Hex(), "value": s.value.Hex()})
		}
	}
	server := rpc.NewServer(log.Global)
	must(server.RegisterName("quai", quaiapi.NewPublicBlockChainQuaiAPI(b)))
	defer server.Stop()
	calls := []interface{}{}
	for i, cl := range sc.calls {
		request := obj{"jsonrpc": "2.0", "id": i + 1, "method": cl.method}
		if cl.params != nil {
			request["params"] = cl.params
		}
		calls = append(calls, obj{"request": request, "response": ask(server, request)})
	}
	return obj{
		"name":          sc.name,
		"head":          sc.head,
		"canonical":     canonical,
		"utxo_set_size": sizes,
		"utxos":         utxoOut,
		"spent":         spent,
		"trimmed":       trimmed,
		"locked_quai":   locked,
		"storage":       storage,
		"calls":         calls,
	}
}

// commitStorage writes the slots into a fresh state and returns its root.
func commitStorage(cache state.Database, slots []slot) common.Hash {
	st, err := state.New(types.EmptyRootHash, types.EmptyRootHash, u(0), cache, cache, nil, loc, log.Global)
	must(err)
	for _, s := range slots {
		ia := internal(s.address)
		if !st.Exist(ia) {
			// A nonce keeps the account: Commit(true) deletes empty
			// accounts, storage and all.
			st.CreateAccount(ia)
			st.SetNonce(ia, 1)
		}
		st.SetState(ia, s.key, s.value)
	}
	root, err := st.Commit(true)
	must(err)
	must(cache.TrieDB().Commit(root, false, nil))
	return root
}

// ask sends one request through the production JSON-RPC server over HTTP and
// returns the decoded response.
func ask(server *rpc.Server, request obj) interface{} {
	body, err := json.Marshal(request)
	must(err)
	req := httptest.NewRequest("POST", "/", bytes.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	server.ServeHTTP(rec, req)
	var answer interface{}
	d := json.NewDecoder(rec.Body)
	d.UseNumber()
	must(d.Decode(&answer))
	return answer
}

// --- the chain ------------------------------------------------------------------

var (
	gwei = u(params.GWei)
	eth  = u(params.Ether)
	q32  = pow2(32)

	// Conversion parties.
	quaiParty  = addr(0x00, 0x1b, "conversion-quai")
	qiParty    = addr(0x00, 0x8b, "conversion-qi")
	quaiParty2 = addr(0x00, 0x3c, "conversion-quai-2")
	qiParty2   = addr(0x00, 0xc4, "conversion-qi-2")

	// Outputs and holders.
	q1     = addr(0x00, 0x81, "q1")
	q2     = addr(0x00, 0x91, "q2")
	q3base = addr(0x00, 0xa2, "q3")
	qx     = addr(0x01, 0x83, "qx")  // Qi, zone [0 1]
	qr     = addr(0x10, 0x84, "qr")  // Qi, zone [1 0]
	a1     = addr(0x00, 0x14, "a1")  // Quai, zone [0 0]
	ax     = addr(0x01, 0x15, "ax")  // Quai, zone [0 1]
	zero   = common.BytesToAddress(make([]byte, 20), loc)

	k1, k2, k3, k4 qiKey
)

func buildChain() {
	k1, k2, k3, k4 = key("k1"), key("k2"), key("k3"), key("k4")
	sha := func(s *spec, diff, count, scrypt *big.Int) {
		s.shaDiff, s.shaCount, s.scCount = diff, count, scrypt
	}

	// Pre-KawPow prime termini, the block one MinerDifficultyWindow behind
	// (read as a ZONE number), and heads.
	mk(spec{name: "rA", zone: 296_000, prime: 74_000, ptn: 73_999, difficulty: dec("1500000000000"), exchange: dec("221000000000000000")})
	mk(spec{name: "pA0", zone: 1_148_990, prime: 299_700, ptn: 299_699, difficulty: dec("1700000000000"), minerDiff: dec("1650000000000"), exchange: dec("200000000000000000"), avgFees: dec("2000000000000000"), flow: mul(eth, 10_000), kqd: u(100)})
	mk(spec{name: "hA0", zone: 1_149_000, prime: 299_700, parent: "pA0", ptn: 299_700, terminus: "pA0", difficulty: dec("1900000000000"), baseFee: gwei, avgFees: dec("2500000000000000")})
	mk(spec{name: "pA", zone: 1_150_000, prime: 300_000, parent: "hA0", ptn: 299_999, terminus: "pA0", difficulty: dec("2000000000000"), minerDiff: dec("1800000000000"), exchange: dec("221077819000000000"), avgFees: dec("3000000000000000"), flow: mul(eth, 10_000), kqd: u(100)})
	mk(spec{name: "hA", zone: 1_150_005, prime: 300_000, parent: "pA", ptn: 300_000, terminus: "pA", difficulty: dec("2100000000000"), baseFee: gwei, avgFees: dec("3100000000000000")})
	mk(spec{name: "hZ", zone: 1_150_006, prime: 300_000, parent: "hA", ptn: 300_000, terminus: "pA", difficulty: dec("2100000000000"), baseFee: u(0), avgFees: dec("3100000000000000")})
	mk(spec{name: "hH", zone: 1_150_008, prime: 300_000, parent: "hA", ptn: 300_000, terminus: "pA", difficulty: dec("2100000000000"), baseFee: mul(gwei, 100_000), avgFees: dec("3100000000000000")})
	missing := tag("missing-prime-terminus")
	mk(spec{name: "hX", zone: 1_150_007, prime: 300_000, parent: "hA", ptn: 300_000, terminusID: &missing, difficulty: dec("2100000000000"), baseFee: gwei})

	// KawPow (within the ProgPoW transition, so real hashes).
	s := spec{name: "rB", zone: 1_196_000, prime: 1_196_000, ptn: 1_195_999, difficulty: dec("1000000000000"), exchange: dec("260000000000000000")}
	sha(&s, dec("100000000000000000"), mul(q32, 2), q32)
	mk(s)
	s = spec{name: "pB", zone: 1_250_000, prime: 1_200_000, ptn: 1_199_999, difficulty: dec("1000000000000"), minerDiff: dec("1100000000000"), exchange: dec("250000000000000000"), avgFees: dec("4000000000000000"), flow: mul(eth, 8_000), kqd: u(25_000)}
	sha(&s, dec("100000000000000000"), mul(q32, 2), q32)
	mk(s)
	s = spec{name: "hB", zone: 1_250_010, prime: 1_200_000, parent: "pB", ptn: 1_200_000, terminus: "pB", difficulty: dec("1200000000000"), baseFee: mul(gwei, 50_000), avgFees: dec("4200000000000000")}
	sha(&s, dec("100000000000000000"), mul(q32, 2), q32)
	mk(s)

	// After KawPow a difficulty below KQuaiDifficultyDivisor (3e11) makes
	// the logarithm in CalculateQuaiReward negative.
	s = spec{name: "rN", zone: 1_196_500, prime: 1_196_500, ptn: 1_196_499, difficulty: dec("100000000000"), exchange: dec("240000000000000000")}
	sha(&s, dec("100000000000000000"), u(0), u(0))
	mk(s)
	s = spec{name: "pN", zone: 1_260_000, prime: 1_200_500, ptn: 1_200_499, difficulty: dec("100000000000"), minerDiff: dec("100000000000"), exchange: dec("250000000000000000"), avgFees: dec("1000000000000000"), flow: mul(eth, 10_000), kqd: u(100)}
	sha(&s, dec("100000000000000000"), u(0), u(0))
	mk(s)
	s = spec{name: "hN", zone: 1_260_005, prime: 1_200_500, parent: "pN", ptn: 1_200_500, terminus: "pN", difficulty: dec("100000000000"), baseFee: mul(gwei, 100_000), avgFees: dec("1000000000000000")}
	sha(&s, dec("100000000000000000"), u(0), u(0))
	mk(s)

	// SHA-equivalent difficulty (synthetic ids).
	s = spec{name: "rC", zone: 1_796_000, prime: 1_796_000, ptn: 1_795_999, difficulty: dec("1000000000000"), exchange: dec("300000000000000000")}
	sha(&s, dec("200000000000000000"), mul(q32, 3), q32)
	mk(s)
	s = spec{name: "pC", zone: 1_900_000, prime: 1_800_000, ptn: 1_799_999, difficulty: dec("1000000000000"), minerDiff: dec("1000000000000"), exchange: dec("300000000000000000"), avgFees: dec("5000000000000000"), flow: mul(eth, 6_000), kqd: u(100_000)}
	sha(&s, dec("200000000000000000"), mul(q32, 3), q32)
	mk(s)
	s = spec{name: "hC", zone: 1_900_020, prime: 1_800_000, parent: "pC", ptn: 1_800_000, terminus: "pC", difficulty: dec("1100000000000"), baseFee: mul(gwei, 70_000), avgFees: dec("5200000000000000")}
	sha(&s, dec("200000000000000000"), mul(q32, 3), q32)
	mk(s)

	// Conversion lock change (synthetic ids); pD's SHA difficulty is below
	// the floor of MinDifficultyForShaEquivalentDifficulty.
	s = spec{name: "rD", zone: 2_296_000, prime: 2_296_000, ptn: 2_295_999, difficulty: dec("900000000000"), exchange: dec("270000000000000000")}
	sha(&s, dec("10000000000000000"), q32, u(0))
	mk(s)
	s = spec{name: "pD", zone: 2_400_000, prime: 2_300_000, ptn: 2_299_999, difficulty: dec("900000000000"), minerDiff: dec("900000000000"), exchange: dec("280000000000000000"), avgFees: dec("5000000000000000"), flow: mul(eth, 5_000), kqd: u(75_000)}
	sha(&s, dec("10000000000000000"), q32, u(0))
	mk(s)
	s = spec{name: "hD", zone: 2_400_030, prime: 2_300_000, parent: "pD", ptn: 2_300_000, terminus: "pD", difficulty: dec("1000000000000"), baseFee: mul(gwei, 90_000), avgFees: dec("6000000000000000")}
	sha(&s, dec("300000000000000000"), q32, q32)
	mk(s)

	// A prime terminus at prime 3,000 (no rate check) and one whose
	// window block is missing.
	mk(spec{name: "pE", zone: 12_000, prime: 3_000, ptn: 2_999, difficulty: dec("500000000000"), minerDiff: dec("500000000000"), exchange: dec("221077819000000000"), avgFees: dec("1000000000000000"), flow: mul(eth, 10_000), kqd: u(100)})
	mk(spec{name: "hE", zone: 12_005, prime: 3_000, parent: "pE", ptn: 3_000, terminus: "pE", difficulty: dec("500000000000"), baseFee: gwei})
	mk(spec{name: "pF", zone: 1_100_000, prime: 500_000, ptn: 499_999, difficulty: dec("1500000000000"), minerDiff: dec("1500000000000"), exchange: dec("230000000000000000"), avgFees: dec("2000000000000000"), flow: mul(eth, 10_000), kqd: u(100)})
	mk(spec{name: "hF", zone: 1_100_005, prime: 500_000, parent: "pF", ptn: 500_000, terminus: "pF", difficulty: dec("1500000000000"), baseFee: gwei})

	// Locked balances and wrapped Qi.
	mk(spec{name: "hL", zone: 1_000_000, prime: 250_000, ptn: 250_000, terminus: "pE", difficulty: dec("1000000000000"), baseFee: gwei})
	mk(spec{name: "hW", zone: 1_000_500, prime: 250_100, ptn: 250_100, terminus: "pE", difficulty: dec("1000000000000"), baseFee: gwei, evmRoot: wrappedRoot()})

	buildDeltaChain()
}

// The outpoint-delta chain: d0..d7 (canonical, zone 1,110,000 + i), a side
// branch s4, s5 from d3, and dFar 1,001 blocks above d0.
const deltaBase = 1_110_000

var (
	t1, t2, t3, t4                     *types.Transaction
	c1, c2, c3, c4, c5                 *types.Transaction
	v1, v2, v3, unwrap1, unwrap2, rev1 *types.Transaction
	q3                                 common.Address
)

func buildDeltaChain() {
	z := func(i uint64) uint64 { return deltaBase + i }
	ptn := func(i uint64) uint64 { return 270_000 + i }
	plain := func(name string, i uint64, parent string, txs types.Transactions) {
		mk(spec{name: name, zone: z(i), prime: ptn(i), parent: parent, ptn: ptn(i), terminus: "pE", difficulty: dec("1000000000000"), baseFee: gwei, txs: txs})
	}
	prev := func(s string, i uint16) types.OutPoint { return types.OutPoint{TxHash: tag("prev/" + s), Index: i} }
	plain("d0", 0, "", nil)
	t1 = qiTx(k1, []types.OutPoint{prev("t1", 0)}, []out{
		{3, q1, nil}, {7, q1, nil}, {5, q2, nil}, {6, a1, nil}, {4, qx, nil}, {2, q1, u(0x10)},
	})
	plain("d1", 1, "d0", types.Transactions{t1})
	lockup := func(b byte) []byte { return append([]byte{b}, make([]byte, 32)...) }
	c1 = etx("c1", 0, types.CoinbaseType, q1, u(123_456_789), 0, lockup(0))
	c2 = etx("c2", 1, types.CoinbaseType, q1, u(1_000_000), 0, lockup(2))
	c3 = etx("c3", 2, types.CoinbaseType, q2, u(5_000), 0, []byte{})
	c5 = etx("c5", 3, types.CoinbaseType, a1, mul(eth, 2), 0, lockup(0))
	plain("d2", 2, "d1", types.Transactions{c1, c2, c3, c5})
	v1 = etx("v1", 0, types.ConversionType, q1, u(7_777), params.TxGas+3*params.CallValueTransferGas, []byte{})
	v2 = etx("v2", 1, types.ConversionType, q2, u(100), 20_000, []byte{})
	v3 = etx("v3", 2, types.ConversionType, q2, u(2_500), 1_000_000, []byte{})
	plain("d3", 3, "d2", types.Transactions{v1, v2, v3})
	unwrap1 = etx("unwrap1", 0, types.UnwrapQiType, q2, u(25_000_000), 5*params.CallValueTransferGas, []byte{})
	q3b := q3base.Bytes20()
	revData := append(append([]byte{0x00, 0x00}, q3b[:]...), make([]byte, 10)...)
	rev1 = etx("rev1", 1, types.ConversionRevertType, a1, u(12_345), params.TxGas+10*params.CallValueTransferGas, revData)
	plain("d4", 4, "d3", types.Transactions{unwrap1, rev1})
	q3a := q3base.Bytes20()
	binary.BigEndian.PutUint32(q3a[16:], uint32(z(4)))
	q3 = common.BytesToAddress(q3a[:], loc)
	t2 = qiTx(k2, []types.OutPoint{{TxHash: t1.Hash(), Index: 0}, {TxHash: t1.Hash(), Index: 2}}, []out{{4, q2, nil}})
	plain("d5", 5, "d4", types.Transactions{t2})
	t3 = qiTx(k3, []types.OutPoint{{TxHash: t1.Hash(), Index: 1}}, []out{{9, q1, nil}, {1, q1, nil}})
	plain("d6", 6, "d5", types.Transactions{t3})
	// d7 has a prime terminus number after ConversionLockChangeForkBlock
	// (unwrap lock 10) and keeps its real hash.
	unwrap2 = etx("unwrap2", 0, types.UnwrapQiType, q1, u(3_000_000), 10*params.CallValueTransferGas, []byte{})
	mk(spec{name: "d7", zone: z(7), prime: 2_300_001, parent: "d6", ptn: 2_300_001, terminus: "pE", difficulty: dec("1000000000000"), baseFee: gwei, txs: types.Transactions{unwrap2}, realHash: true})
	t4 = qiTx(k4, []types.OutPoint{{TxHash: v1.Hash(), Index: 0}}, []out{{8, q1, nil}})
	plain("s4", 4, "d3", types.Transactions{t4})
	c4 = etx("c4", 0, types.CoinbaseType, q1, u(50_000), 0, lockup(1))
	plain("s5", 5, "s4", types.Transactions{c4})
	plain("dFar", 1_001, "d7", nil)
}

// wrappedRoot is the state root with the wrapped-Qi deposits of the
// wrapped-qi scenario.
var lockupContract = common.HexToAddress("0x000000000000000000000000000000000000000A", loc)
var (
	wOwner  = addr(0x00, 0x2d, "wrapped-owner")
	wMiner  = addr(0x00, 0x3e, "wrapped-miner")
	wMiner2 = addr(0x00, 0x4f, "wrapped-miner-2")
)

func wrappedKey(owner, miner common.Address) common.Hash {
	var k common.Hash
	copy(k[:16], owner.Bytes()[:16])
	copy(k[16:], miner.Bytes()[:16])
	return k
}
func wrappedSlots() []slot {
	return []slot{
		{lockupContract, wrappedKey(wOwner, wMiner), common.BigToHash(dec("123456789000000000000"))},
		{lockupContract, common.BytesToHash(wOwner.Bytes()), common.BigToHash(dec("5000000000000000000"))},
	}
}
func wrappedRoot() common.Hash {
	disk := rawdb.NewMemoryDatabase(log.Global)
	defer disk.Close()
	return commitStorage(state.NewDatabase(disk), wrappedSlots())
}

// --- requests -------------------------------------------------------------------

func h(name string) string { return blocks[name].id.Hex() }
func n(name string) string { return q64(blocks[name].wo.NumberU64(common.ZONE_CTX)) }

func qiToQuaiCalls(head string, other string) []call {
	out := []call{}
	for _, amount := range []string{"0x0", "0x1", "0x3e8", "0xf4240", "0x3b9aca00", qty(pow2(200))} {
		out = append(out, c("quai_qiToQuai", amount, "latest"))
	}
	for _, amount := range []string{"0x0", "0x1", "0xde0b6b3a7640000", "0x3635c9adc5dea00000", qty(pow2(200))} {
		out = append(out, c("quai_quaiToQi", amount, "latest"))
	}
	out = append(out,
		c("quai_qiToQuai", "0x3e8", n(head)),
		c("quai_quaiToQi", "0xde0b6b3a7640000", n(head)),
	)
	if other != "" {
		out = append(out,
			c("quai_qiToQuai", "0x3e8", n(other)),
			c("quai_quaiToQi", "0xde0b6b3a7640000", n(other)),
			c("quai_qiToQuai", "0x3e8", h(other)),
			c("quai_quaiToQi", "0xde0b6b3a7640000", h(other)),
			c("quai_qiToQuai", "0x3e8", obj{"blockHash": h(other)}),
			c("quai_quaiToQi", "0xde0b6b3a7640000", obj{"blockHash": h(other)}),
			c("quai_qiToQuai", "0x3e8", obj{"blockNumber": n(other)}),
		)
	}
	return out
}

func conversionCalls() []call {
	out := []call{}
	conv := func(from, to common.Address, value string) call {
		return c("quai_calculateConversionAmount", obj{"from": from.Hex(), "to": to.Hex(), "value": value})
	}
	ether := func(v uint64) string { return qty(mul(eth, v)) }
	for _, v := range []string{"0x0", "0x1", ether(1), ether(1_000), ether(5_000), ether(10_000), ether(30_000), ether(99_900), ether(100_000), ether(200_000)} {
		out = append(out, conv(quaiParty, qiParty, v))
	}
	for _, v := range []string{"0x0", "0x1", "0x3e8", "0xf4240", "0x3b9aca00", "0xe8d4a51000", "0x38d7ea4c68000", "0x2386f26fc10000"} {
		out = append(out, conv(qiParty, quaiParty, v))
	}
	return out
}

func conversionErrorCalls() []call {
	return []call{
		c("quai_calculateConversionAmount", obj{"to": qiParty.Hex(), "value": "0x1"}),
		c("quai_calculateConversionAmount", obj{"from": quaiParty.Hex(), "value": "0x1"}),
		c("quai_calculateConversionAmount", obj{"from": quaiParty.Hex(), "to": qiParty.Hex()}),
		c("quai_calculateConversionAmount", obj{"from": quaiParty.Hex(), "to": quaiParty2.Hex(), "value": "0x1"}),
		c("quai_calculateConversionAmount", obj{"from": qiParty.Hex(), "to": qiParty2.Hex(), "value": "0x1"}),
		c("quai_calculateConversionAmount", obj{"from": qx.Hex(), "to": ax.Hex(), "value": "0x3e8"}),
		c("quai_calculateConversionAmount", obj{"from": strings.ToLower(quaiParty.Hex()), "to": qiParty.Hex(), "value": qty(mul(eth, 1)), "gas": "0x5208", "data": "0x"}),
		c("quai_calculateConversionAmount", "0x1"),
		c("quai_calculateConversionAmount"),
		raw("quai_calculateConversionAmount", nil),
		c("quai_calculateConversionAmount", obj{"from": "0x12", "to": qiParty.Hex(), "value": "0x1"}),
		c("quai_calculateConversionAmount", obj{"from": quaiParty.Hex(), "to": qiParty.Hex(), "value": 1}),
	}
}

func txIn(hash common.Hash, index string, pub string, quaisShape bool) obj {
	if quaisShape {
		// Quais.js writes "previousOutpoint" and "pubkey"; Go matches keys
		// without regard to case.
		return obj{"previousOutpoint": obj{"txHash": hash.Hex(), "index": index}, "pubkey": pub}
	}
	return obj{"previousOutPoint": obj{"txHash": hash.Hex(), "index": index}, "pubKey": pub}
}
func txOut(to common.Address, denom string) obj {
	return obj{"address": to.Hex(), "denomination": denom}
}

func feeCalls() []call {
	p := func(s string) common.Hash { return tag("fee-input/" + s) }
	in1 := []interface{}{txIn(p("a"), "0x0", k1.pub(), true)}
	in2 := []interface{}{txIn(p("a"), "0x0", k1.pub(), true), txIn(p("b"), "0x3", k2.pub(), true)}
	in3 := []interface{}{txIn(p("a"), "0x0", k1.pub(), false), txIn(p("b"), "0x1", k2.pub(), false), txIn(p("c"), "0x2", k3.pub(), false)}
	fee := func(args obj) call { return c("quai_estimateFeeForQi", args) }
	return []call{
		fee(obj{"txType": 2, "txIn": in1, "txOut": []interface{}{txOut(q1, "0x3")}}),
		fee(obj{"txType": 2, "txIn": []interface{}{txIn(p("a"), "0x0", k1.pub(), false)}, "txOut": []interface{}{obj{"denomination": "0x3", "address": q1.Hex(), "lock": "0x0"}}}),
		fee(obj{"txType": 2, "txIn": in2, "txOut": []interface{}{txOut(q1, "0x3"), txOut(qx, "0x2"), txOut(a1, "0x4")}}),
		fee(obj{"txType": 2, "txIn": in1, "txOut": []interface{}{txOut(qr, "0x5"), txOut(qx, "0x1")}}),
		fee(obj{"txType": 2, "txIn": in3, "txOut": []interface{}{txOut(zero, "0x6")}}),
		fee(obj{"txType": 2, "txIn": in3, "txOut": []interface{}{txOut(q1, "0x1"), txOut(q2, "0x1"), txOut(q3base, "0x1"), txOut(qx, "0x1"), txOut(qr, "0x1"), txOut(a1, "0x1"), txOut(ax, "0x1")}}),
		fee(obj{"txType": 2, "txIn": []interface{}{txIn(p("a"), "0xffff", k1.pub(), true)}, "txOut": []interface{}{txOut(q1, "0x3")}}),
		fee(obj{"txType": 2, "TXIN": in1, "TxOut": []interface{}{txOut(q1, "0x3")}}),
		fee(obj{"txType": 2, "txIn": []interface{}{txIn(p("a"), "0x10000", k1.pub(), true)}, "txOut": []interface{}{txOut(q1, "0x3")}}),
		fee(obj{"txIn": in1, "txOut": []interface{}{txOut(q1, "0x3")}}),
		fee(obj{"txType": 0, "txIn": in1, "txOut": []interface{}{txOut(q1, "0x3")}}),
		fee(obj{"txType": 1, "txIn": in1, "txOut": []interface{}{txOut(q1, "0x3")}}),
		fee(obj{"txType": "0x2", "txIn": in1, "txOut": []interface{}{txOut(q1, "0x3")}}),
		fee(obj{"txType": 2, "txIn": []interface{}{}, "txOut": []interface{}{txOut(q1, "0x3")}}),
		fee(obj{"txType": 2, "txIn": in1}),
		fee(obj{"txType": 2, "txIn": in1, "txOut": []interface{}{obj{"address": "0x1234", "denomination": "0x3"}}}),
		c("quai_estimateFeeForQi"),
		c("quai_estimateFeeForQi", "0x2"),
	}
}

func feeSubset() []call {
	all := feeCalls()
	return []call{all[0], all[2], all[4], all[5]}
}

func argumentErrorCalls(head string) []call {
	return []call{
		c("quai_qiToQuai", "0x3e8"),
		c("quai_qiToQuai", "0x", "latest"),
		c("quai_qiToQuai", "0x01", "latest"),
		c("quai_qiToQuai", 1000, "latest"),
		c("quai_qiToQuai", qty(pow2(256)), "latest"),
		c("quai_qiToQuai", "0x3e8", obj{"blockNumber": n(head), "blockHash": h(head)}),
		c("quai_qiToQuai", "0x3e8", "0xzz"),
		c("quai_qiToQuai", "0x3e8", "latest", true),
		c("quai_quaiToQi"),
		c("quai_qiToQuai", "0x3e8", "earliest"),
		c("quai_qiToQuai", "0x3e8", "0xffffff"),
		c("quai_quaiToQi", "0x3e8", "0xffffff"),
		c("quai_qiToQuai", "0x3e8", tag("unknown-block").Hex()),
		c("quai_quaiToQi", "0x3e8", obj{"blockHash": tag("unknown-block").Hex()}),
		c("quai_qiToQuai", "0x3e8", "pending"),
		c("quai_qiToQuai", "0x3e8", obj{"blockNumber": "latest"}),
		c("quai_qiToQuai", "0x3e8", true),
		c("quai_quaiToQi", "0x3e8", "earliest"),
		c("quai_quaiToQi", "0x3e8", "pending"),
		c("quai_listRunningChains"),
		c("quai_listRunningChains", 1),
		raw("quai_listRunningChains", nil),
	}
}

func headScenario(name, head string, canonical []string, size uint64, calls ...[]call) scenario {
	all := []call{}
	for _, l := range calls {
		all = append(all, l...)
	}
	return scenario{name: name, head: head, canonical: canonical, utxoSize: map[string]uint64{head: size}, calls: all}
}

func scenarios() []scenario {
	preKawPow := []string{"rA", "pA0", "hA0", "pA", "hA"}
	highFee := []string{"rA", "pA", "hA", "hH"}
	out := []scenario{
		headScenario("pre-kawpow", "hA", preKawPow, 1_000,
			qiToQuaiCalls("hA", "hA0"), conversionCalls(), conversionErrorCalls(), feeCalls(), argumentErrorCalls("hA")),
		headScenario("high-base-fee", "hH", highFee, 1_000, feeCalls()),
		headScenario("utxo-set-below-threshold", "hH", highFee, 3_269_017, feeSubset()),
		headScenario("utxo-set-at-threshold", "hH", highFee, 3_269_018, feeSubset()),
		headScenario("utxo-set-large", "hH", highFee, 1_000_000_000, feeSubset()),
		headScenario("utxo-set-huge", "hH", highFee, 1<<40, feeSubset()),
		headScenario("kawpow", "hB", []string{"rB", "pB", "hB"}, 50_000,
			qiToQuaiCalls("hB", ""), conversionCalls(), feeSubset()),
		headScenario("sha-equivalent-difficulty", "hC", []string{"rC", "pC", "hC"}, 50_000,
			qiToQuaiCalls("hC", ""), conversionCalls(), feeSubset()),
		headScenario("conversion-lock-change", "hD", []string{"rD", "pD", "hD"}, 50_000,
			qiToQuaiCalls("hD", ""), conversionCalls(), feeSubset()),
		headScenario("early-prime-terminus", "hE", []string{"pE", "hE"}, 10,
			qiToQuaiCalls("hE", ""), conversionCalls(), feeSubset()),
		headScenario("missing-window-block", "hF", []string{"pF", "hF"}, 10,
			qiToQuaiCalls("hF", ""), conversionCalls()[:3], feeSubset()[:1]),
		headScenario("zero-base-fee", "hZ", []string{"pA", "hA", "hZ"}, 1_000, feeSubset()),
		headScenario("unknown-prime-terminus", "hX", []string{"pA", "hA", "hX"}, 1_000,
			[]call{c("quai_qiToQuai", "0x3e8", "latest"), c("quai_quaiToQi", "0x3e8", "latest"), c("quai_qiToQuai", "0x3e8", n("hA"))},
			conversionCalls()[:3], feeSubset()[:1]),
		headScenario("negative-reward-logarithm", "hN", []string{"rN", "pN", "hN"}, 1_000,
			qiToQuaiCalls("hN", ""), conversionCalls(), feeSubset()),
		lockedScenario(),
		deltaScenario(),
		wrappedScenario(),
	}
	return out
}

func lockedScenario() scenario {
	head := uint64(1_000_000)
	a2 := addr(0x00, 0x25, "locked-a2")
	a3 := addr(0x00, 0x36, "locked-a3")
	q4 := addr(0x00, 0xb7, "locked-q4")
	q5 := addr(0x00, 0xc8, "locked-q5")
	q6 := addr(0x00, 0xd9, "locked-q6")
	x := func(s string, i uint16, d uint8, to common.Address, lock *big.Int) utxo {
		return utxo{tag("locked-utxo/" + s), i, d, to, lock}
	}
	gethex := func(a common.Address) string { return a.Hex() }
	return scenario{
		name: "locked-balance", head: "hL", canonical: []string{"hL"},
		utxos: []utxo{
			x("a", 0, 3, q4, nil),
			x("a", 1, 4, q4, u(0)),
			x("b", 0, 6, q4, u(head-1)),
			x("b", 1, 7, q4, u(head)),
			x("c", 0, 5, q4, u(head+1)),
			x("c", 1, 14, q4, u(2*head)),
			x("d", 0, 9, q5, u(10)),
			x("e", 0, 1, q6, u(head+100)),
			x("e", 1, 0, q6, u(head+100)),
		},
		lockups: []lockups{
			{block: "pA", locks: map[common.Address]*big.Int{a1: mul(eth, 5)}},
			{block: "hA", locks: map[common.Address]*big.Int{a2: mul(eth, 1)}, unlocks: []unlock{{a1, mul(eth, 2)}}},
			{block: "hZ", unlocks: []unlock{{a2, mul(eth, 3)}}},
		},
		calls: []call{
			c("quai_getLockedBalance", gethex(a1)),
			c("quai_getLockedBalance", gethex(a2)),
			c("quai_getLockedBalance", gethex(a3)),
			c("quai_getLockedBalance", lower(a1)),
			c("quai_getLockedBalance", strings.ToUpper("0x"+lower(a1)[2:])),
			c("quai_getLockedBalance", gethex(q4)),
			c("quai_getLockedBalance", gethex(q5)),
			c("quai_getLockedBalance", gethex(q6)),
			c("quai_getLockedBalance", gethex(q1)),
			c("quai_getLockedBalance", gethex(zero)),
			c("quai_getLockedBalance", gethex(ax)),
			c("quai_getLockedBalance", gethex(qx)),
			c("quai_getLockedBalance", "0x12"),
			c("quai_getLockedBalance", 5),
			c("quai_getLockedBalance"),
			c("quai_getLockedBalance", gethex(a1), "latest"),
			c("quai_listRunningChains"),
		},
	}
}

func deltaScenario() scenario {
	d := func(from, to string, addrs ...interface{}) call {
		if addrs == nil {
			addrs = []interface{}{}
		}
		return c("quai_getOutpointDeltasForAddressesInRange", addrs, h(from), h(to))
	}
	sp := func(tx *types.Transaction, i uint16, denom uint8, to common.Address, lock *big.Int) utxo {
		return utxo{tx.Hash(), i, denom, to, lock}
	}
	unknown := tag("unknown-delta-block").Hex()
	coinbaseLock := u(params.LockupByteToBlockDepth[0] + deltaBase + 2)
	conversionLock := u(params.ConversionLockPeriod + deltaBase + 3)
	Q1, Q2, Q3, QX, A1 := q1.Hex(), q2.Hex(), q3.Hex(), qx.Hex(), a1.Hex()
	return scenario{
		name: "outpoint-deltas", head: "d7",
		canonical: []string{"d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7", "dFar"},
		spent: map[string][]utxo{
			"d5": {sp(t1, 0, 3, q1, nil), sp(t1, 2, 5, q2, nil)},
			"d6": {sp(t1, 1, 7, q1, u(0))},
			"d7": {sp(t3, 0, 9, q1, nil)},
			"s4": {sp(v1, 0, 7, q1, conversionLock)},
		},
		trimmed: map[string][]utxo{
			"d5": {sp(c1, 25, 0, q1, coinbaseLock), sp(v3, 2, 5, q2, conversionLock)},
		},
		calls: []call{
			d("d0", "d7", Q1),
			d("d1", "d5", Q1, Q2, Q3, QX),
			d("d4", "d4", Q2),
			d("d2", "s5", Q1, Q2),
			d("s4", "d6", Q1, Q2),
			d("d5", "d5", Q1, Q2),
			d("d2", "d2", Q1, Q2, A1),
			d("d3", "d3", Q1, Q2),
			d("d1", "d4", A1),
			d("d1", "d1", strings.ToLower(Q1), Q1),
			d("d4", "d4", q3base.Hex(), Q3),
			d("d1", "d1", QX),
			d("d7", "d7", Q1),
			d("s5", "s4", Q1),
			d("s4", "s5", Q1, Q2),
			d("d4", "s5", Q1, Q2),
			d("s4", "s4", Q1),
			d("d0", "dFar", Q1),
			d("d1", "d6"),
			c("quai_getOutpointDeltasForAddressesInRange", []interface{}{Q1}, unknown, h("d5")),
			c("quai_getOutpointDeltasForAddressesInRange", []interface{}{Q1}, h("d1"), unknown),
			c("quai_getOutpointDeltasForAddressesInRange", []interface{}{"0x12"}, h("d1"), h("d2")),
			c("quai_getOutpointDeltasForAddressesInRange", []interface{}{Q1}, "0x1234", h("d2")),
			c("quai_getOutpointDeltasForAddressesInRange", Q1, h("d1"), h("d2")),
			c("quai_getOutpointDeltasForAddressesInRange", []interface{}{Q1}),
			c("quai_getOutpointDeltasForAddressesInRange"),
		},
	}
}

func wrappedScenario() scenario {
	w := func(owner, miner string, at interface{}) call {
		return c("quai_getWrappedQiDeposit", owner, miner, at)
	}
	O, M := wOwner.Hex(), wMiner.Hex()
	return scenario{
		name: "wrapped-qi", head: "hW", canonical: []string{"hL", "hW"},
		storage: wrappedSlots(),
		calls: []call{
			w(O, M, "latest"),
			w(O, M, n("hW")),
			w(O, M, h("hW")),
			w(O, M, obj{"blockHash": h("hW")}),
			w(O, M, obj{"blockHash": h("hW"), "requireCanonical": true}),
			w(O, wMiner2.Hex(), "latest"),
			w(O, M, n("hL")),
			w(qiParty.Hex(), M, "latest"),
			w(ax.Hex(), M, "latest"),
			w(O, ax.Hex(), "latest"),
			w(O, q1.Hex(), "latest"),
			w(O, M, "0xffffff"),
			w(O, M, tag("unknown-wrapped").Hex()),
			w(O, M, "pending"),
			c("quai_getWrappedQiDeposit", O, M),
		},
	}
}

func main() {
	log.Global.SetOutput(io.Discard)
	vm.InitializePrecompiles(loc)
	buildChain()
	chain := obj{}
	for _, name := range blockList {
		chain[name] = blockJSON(blocks[name])
	}
	out := []interface{}{}
	for _, sc := range scenarios() {
		out = append(out, run(sc))
	}
	must(json.NewEncoder(os.Stdout).Encode(obj{
		"reference_revision": referenceRevision,
		"scope":              "go-quai v0.57.0 production PublicBlockChainQuaiAPI handlers (quai_qiToQuai, quai_quaiToQi, quai_calculateConversionAmount, quai_estimateFeeForQi, quai_getOutpointDeltasForAddressesInRange, quai_getLockedBalance, quai_listRunningChains, quai_getWrappedQiDeposit) behind rpc.Server over HTTP, with a backend that mirrors quai/api_backend.go over oracle-built work objects (encoded and decoded as the database returns them), a rawdb memory database (canonical index, UTXO set sizes, UTXOs and the address index, spent and trimmed journals, address lockups written by rawdb.WriteNewLockups) and committed state. Every block of chain.blocks is known by hash in every scenario; the canonical index is per scenario. Blocks whose prime terminus number is at or after the end of the KawPow transition and that have no AuxPow all hash to blake3 of an empty AuxPow; they are linked and looked up by a synthetic id (the fixture hash), except d7, which keeps that real hash. The oracle has no miner: the pending block is nil. Fixed-seed keys; no network, no real funds.",
		"chain":              obj{"chain_id": 9, "location": []int{0, 0}, "blocks": chain},
		"scenarios":          out,
	}))
}
