// Filter and subscription oracle: go-quai v0.57.0's production filter system
// (quai/filters: PublicFilterAPI and its EventSystem) behind the production
// JSON-RPC server (rpc.Server over HTTP, and over WebSocket through
// Server.WebsocketHandler), over a test chain in a rawdb memory database:
// work objects with transactions, receipts with logs, and a side branch.
//
// The oracle plays the chain events as go-quai v0.57.0's core emits them
// (core/headerchain.go SetCurrentHeader and AppendBlock, core/slice.go
// GeneratePendingHeader): a head move rewrites the canonical index; each
// block that becomes canonical, oldest first, sends its logs (derived from
// the stored receipts, as Apply returns them) on the logs feed when it has
// any; then one ChainHeadEvent carries the new head, read back from the
// database. A jump over several blocks or a reorg therefore gives logs for
// every new block and one head event. Nothing in v0.57.0 sends
// RemovedLogsEvent (core/bodydb.go rmLogsFeed has no sender), so the oracle
// never sends it; it sends no ChainEvent either. Sends are paced (50 ms):
// the event system delivers with non-blocking sends into unbuffered
// channels, and a burst drops events.
//
// Every filter or subscription ID is checked against the ID pattern and
// replaced with a placeholder ($F<n>, $S<n>) in order of first appearance.
// Notifications are grouped by subscription (the order between two
// subscriptions on one connection depends on goroutine scheduling); the
// logs of multi-block getFilterLogs answers are sorted by block and index
// (Filter.Logs appends the results of up to 8 goroutines in completion
// order). Keys come from fixed seeds. No network, no real funds.
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
	"regexp"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/btcsuite/btcd/btcec/v2"
	"github.com/btcsuite/btcd/btcec/v2/schnorr"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/ethdb"
	"github.com/dominant-strategies/go-quai/ethdb/memorydb"
	"github.com/dominant-strategies/go-quai/event"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/dominant-strategies/go-quai/quai/filters"
	"github.com/dominant-strategies/go-quai/rpc"
	"github.com/dominant-strategies/go-quai/trie"
	"github.com/gorilla/websocket"
	"google.golang.org/protobuf/proto"
)

var referenceRevision string
var loc = common.Location{0, 0}
var chainID = big.NewInt(9)

const idPattern = `^0x(0|[1-9a-f][0-9a-f]{0,31})$`

var idRe = regexp.MustCompile(idPattern)

// pace is the wait after each feed send; quiet is the silence that ends a
// notifications step.
const (
	pace  = 50 * time.Millisecond
	quiet = 500 * time.Millisecond
)

type obj = map[string]interface{}

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

// locStore is a memory key-value store that reports the node's location,
// as a node's database does (ProtoDecode reads addresses in its scope).
type locStore struct{ *memorydb.Database }

func (locStore) Location() common.Location { return loc }

// --- keys -------------------------------------------------------------------

type key struct {
	prv  *ecdsa.PrivateKey
	btc  *btcec.PrivateKey
	addr common.Address
}

// grind derives a key from a fixed seed whose address passes want.
func grind(name string, want func(a []byte) bool) key {
	for i := 0; ; i++ {
		seed := crypto.Keccak256([]byte(fmt.Sprintf("filters-oracle/%s/%d", name, i)))
		prv, err := crypto.ToECDSA(seed)
		if err != nil {
			continue
		}
		pub := crypto.FromECDSAPub(&prv.PublicKey)
		a := crypto.Keccak256(pub[1:])[12:]
		if want(a) {
			btc, _ := btcec.PrivKeyFromBytes(seed)
			return key{prv, btc, common.BytesToAddress(a, loc)}
		}
	}
}
func quaiKey(name string) key {
	return grind(name, func(a []byte) bool { return a[0] == 0x00 && a[1] < 0x80 })
}
func qiKey(name string) key {
	return grind(name, func(a []byte) bool { return a[0] == 0x00 && a[1] >= 0x80 })
}

func addr(s string) common.Address { return common.HexToAddress(s, loc) }
func topic(n int) common.Hash {
	return crypto.Keccak256Hash([]byte(fmt.Sprintf("filters-oracle/topic/%d", n)))
}

// Log emitters, recipients and topics.
var (
	a1 = addr("0x00110000000000000000000000000000000000a1")
	a2 = addr("0x00110000000000000000000000000000000000a2")
	a3 = addr("0x00110000000000000000000000000000000000a3")
	t1 = topic(1)
	t2 = topic(2)
	t3 = topic(3)
	t4 = topic(4)
	t5 = topic(5)
)

func signQuai(k key, nonce uint64, to common.Address, value uint64, data []byte) *types.Transaction {
	inner := &types.QuaiTx{ChainID: chainID, Nonce: nonce, GasPrice: gwei(2), Gas: 90000, To: &to, Value: u(value), Data: data, AccessList: types.AccessList{}}
	tx, err := types.SignTx(types.NewTx(inner), types.NewSigner(chainID, loc), k.prv)
	must(err)
	return tx
}

// signQi signs a one-input Qi transaction with BIP-340 Schnorr.
func signQi(k key, in types.OutPoint, outs []types.TxOut) *types.Transaction {
	inner := &types.QiTx{ChainID: chainID, TxIn: types.TxIns{{PreviousOutPoint: in, PubKey: k.btc.PubKey().SerializeUncompressed()}}, TxOut: outs}
	h, err := types.NewSigner(chainID, loc).Hash(types.NewTx(inner))
	must(err)
	sig, err := schnorr.Sign(k.btc, h[:])
	must(err)
	inner.Signature = sig
	return types.NewTx(inner)
}

func etx(origin common.Hash, index uint16, to, sender common.Address, value uint64, etxType uint64) *types.Transaction {
	return types.NewTx(&types.ExternalTx{OriginatingTxHash: origin, ETXIndex: index, Gas: 21000, To: &to, Value: u(value), AccessList: types.AccessList{}, Sender: sender, EtxType: etxType})
}

func lg(a common.Address, data string, topics ...common.Hash) *types.Log {
	if topics == nil {
		topics = []common.Hash{}
	}
	return &types.Log{Address: a, Topics: topics, Data: common.FromHex(data)}
}

// --- chain ------------------------------------------------------------------

// entry is a block transaction with its receipt's status, gas and logs.
type entry struct {
	tx     *types.Transaction
	gas    uint64
	failed bool
	logs   []*types.Log
}

type blockSpec struct {
	name, parent string
	time         uint64
	coinbase     byte
	entries      []entry
	outbound     []*types.Transaction
	uncles       []string // names of uncle work objects
}

// built is one block of the chain: the work object as read back from the
// database, its receipts and the fixture record.
type built struct {
	name     string
	wo       *types.WorkObject
	receipts types.Receipts // nil for the genesis block
	record   obj
}

type chain struct {
	cfg    params.ChainConfig
	blocks map[string]*built
	byHash map[common.Hash]*built
	order  []string
	uncles map[string]*types.WorkObject
}

func makeWo(number uint64, parent common.Hash, time uint64, coinbase byte, txs, outbound []*types.Transaction, uncles []*types.WorkObjectHeader, receipts types.Receipts) *types.WorkObject {
	wo := types.EmptyWorkObject(common.ZONE_CTX)
	wo.SetNumber(u(number), common.ZONE_CTX)
	wo.SetParentHash(parent, common.ZONE_CTX)
	wo.WorkObjectHeader().SetLocation(loc)
	wo.WorkObjectHeader().SetTime(time)
	wo.WorkObjectHeader().SetDifficulty(u(1_000_000 + 1000*number))
	wo.WorkObjectHeader().SetPrimeTerminusNumber(u(number / 4))
	wo.WorkObjectHeader().SetPrimaryCoinbase(common.BytesToAddress([]byte{0, 0x11, 0x0c, coinbase}, loc))
	wo.Header().SetBaseFee(gwei(1))
	wo.Header().SetGasLimit(30_000_000)
	wo.Header().SetQuaiStateSize(u(0))
	wo.Header().SetExchangeRate(u(1_000_000_000_000_000))
	wo.Header().SetAvgTxFees(u(0))
	wo.Header().SetEtxSetRoot(types.EmptyRootHash)
	wo.Body().SetTransactions(txs)
	wo.Body().SetOutboundEtxs(outbound)
	wo.Body().SetUncles(uncles)
	// Commitments and gas, so that every block has its own body header.
	var used uint64
	if len(receipts) > 0 {
		used = receipts[len(receipts)-1].CumulativeGasUsed
	}
	wo.Header().SetGasUsed(used)
	wo.Header().SetEVMRoot(crypto.Keccak256Hash([]byte(fmt.Sprintf("filters-oracle/state/%d/%d", number, time))))
	wo.Header().SetTxHash(types.DeriveSha(types.Transactions(txs), trie.NewStackTrie(nil)))
	wo.Header().SetOutboundEtxHash(types.DeriveSha(types.Transactions(outbound), trie.NewStackTrie(nil)))
	wo.Header().SetReceiptHash(types.DeriveSha(receipts, trie.NewStackTrie(nil)))
	wo.Header().SetUncleHash(types.CalcUncleHash(uncles))
	wo.WorkObjectHeader().SetTxHash(wo.Header().TxHash())
	wo.WorkObjectHeader().SetHeaderHash(wo.Header().Hash())
	return wo
}

func newChain() *chain {
	c := &chain{cfg: *params.TestChainConfig, blocks: map[string]*built{}, byHash: map[common.Hash]*built{}, uncles: map[string]*types.WorkObject{}}
	c.cfg.Location = loc
	c.cfg.ChainID = chainID

	k1, k2, k3 := quaiKey("alice"), quaiKey("bob"), quaiKey("carol")
	qk := qiKey("dave")
	recipient := addr("0x00110000000000000000000000000000000000b1")
	other := addr("0x01110000000000000000000000000000000000c1") // zone [0 1]

	// Uncles: work object headers of blocks that are not in the chain.
	c.uncles["u1"] = makeWo(1, common.Hash{}, 1_700_000_011, 0x71, nil, nil, nil, nil)
	c.uncles["u2"] = makeWo(3, common.Hash{}, 1_700_000_033, 0x72, nil, nil, nil, nil)
	c.uncles["u3"] = makeWo(5, common.Hash{}, 1_700_000_055, 0x73, nil, nil, nil, nil)

	qiTx := signQi(qk, types.OutPoint{TxHash: crypto.Keccak256Hash([]byte("filters-oracle/outpoint")), Index: 3}, []types.TxOut{
		{Denomination: 3, Address: qiKey("erin").addr.Bytes(), Lock: big.NewInt(0)},
		{Denomination: 1, Address: qk.addr.Bytes(), Lock: big.NewInt(0)},
	})
	incoming := etx(crypto.Keccak256Hash([]byte("filters-oracle/origin")), 2, a3, other, 5, types.DefaultType)
	outbound := etx(common.Hash{}, 0, other, k1.addr, 7, types.DefaultType)
	outbound2 := etx(common.Hash{}, 1, other, k2.addr, 9, types.DefaultType)

	specs := []blockSpec{
		{name: "g", time: 1_700_000_000, coinbase: 0x00},
		{name: "b1", parent: "g", time: 1_700_000_010, coinbase: 0x01, entries: []entry{
			{tx: signQuai(k1, 0, a1, 1, []byte{1}), gas: 30000, logs: []*types.Log{lg(a1, "0x01", t1)}},
			{tx: signQuai(k2, 0, a2, 2, nil), gas: 40000, logs: []*types.Log{lg(a2, "0x02", t2, t3)}},
		}},
		{name: "b2", parent: "b1", time: 1_700_000_020, coinbase: 0x02, uncles: []string{"u1"}, entries: []entry{
			{tx: signQuai(k1, 1, a1, 0, []byte{2, 2}), gas: 50000, logs: []*types.Log{lg(a1, "0x0303", t1, t2), lg(a3, "0x")}},
		}},
		{name: "b3", parent: "b2", time: 1_700_000_030, coinbase: 0x03, entries: []entry{
			{tx: signQuai(k3, 0, recipient, 100, nil), gas: 21000},
		}},
		{name: "b4", parent: "b3", time: 1_700_000_040, coinbase: 0x04, uncles: []string{"u2"}, outbound: []*types.Transaction{outbound}, entries: []entry{
			{tx: signQuai(k1, 2, a1, 0, []byte{4}), gas: 60000, logs: []*types.Log{lg(a1, "0x04", t1, t5, t3)}},
			{tx: signQuai(k2, 1, a2, 0, []byte{4, 4}), gas: 70000, logs: []*types.Log{lg(a2, "0x0404", t2, t4, t3, t1), lg(a3, "0x040404", t1)}},
			{tx: signQuai(k3, 1, a2, 0, nil), gas: 25000, failed: true},
		}},
		{name: "b5", parent: "b4", time: 1_700_000_050, coinbase: 0x05, entries: []entry{
			{tx: signQuai(k2, 2, a2, 0, []byte{5}), gas: 30000, logs: []*types.Log{lg(a2, "0x05", t1)}},
		}},
		{name: "b6", parent: "b5", time: 1_700_000_060, coinbase: 0x06},
		{name: "b7", parent: "b6", time: 1_700_000_070, coinbase: 0x07, entries: []entry{
			{tx: signQuai(k1, 3, a1, 0, []byte{7}), gas: 45000, logs: []*types.Log{lg(a1, "0x07", t2, t4, t3)}},
			{tx: incoming, gas: 21000, logs: []*types.Log{lg(a3, "0x0707", t3)}},
			{tx: qiTx, gas: 12000},
		}},
		{name: "b8", parent: "b7", time: 1_700_000_080, coinbase: 0x08, entries: []entry{
			{tx: signQuai(k3, 2, a1, 0, []byte{8}), gas: 33000, logs: []*types.Log{lg(a1, "0x08", t1)}},
		}},
		{name: "c5", parent: "b4", time: 1_700_000_051, coinbase: 0x15, entries: []entry{
			{tx: signQuai(k2, 2, a3, 0, []byte{0x15}), gas: 31000, logs: []*types.Log{lg(a3, "0x15", t2), lg(a1, "0x1515", t1, t4, t3)}},
		}},
		{name: "c6", parent: "c5", time: 1_700_000_061, coinbase: 0x16, uncles: []string{"u3"}, outbound: []*types.Transaction{outbound2}, entries: []entry{
			{tx: signQuai(k1, 3, a2, 0, []byte{0x16}), gas: 32000, logs: []*types.Log{lg(a2, "0x16", t1, t2)}},
		}},
		{name: "c7", parent: "c6", time: 1_700_000_071, coinbase: 0x17, entries: []entry{
			{tx: signQuai(k3, 2, a1, 0, []byte{0x17}), gas: 34000, logs: []*types.Log{lg(a1, "0x17", t2)}},
		}},
		{name: "c8", parent: "c7", time: 1_700_000_081, coinbase: 0x18, entries: []entry{
			{tx: signQuai(k1, 4, a3, 0, []byte{0x18}), gas: 35000, logs: []*types.Log{lg(a3, "0x18", t4)}},
		}},
	}

	// Build each block, write it and its receipts to a scratch database,
	// and keep the object as the database returns it.
	db := rawdb.NewDatabase(locStore{memorydb.New(log.Global)})
	for _, s := range specs {
		var parent common.Hash
		var number uint64
		if s.parent != "" {
			p := c.blocks[s.parent]
			parent = p.wo.Hash()
			number = p.wo.NumberU64(common.ZONE_CTX) + 1
		}
		txs := types.Transactions{}
		receipts := types.Receipts{}
		var cum uint64
		for _, e := range s.entries {
			txs = append(txs, e.tx)
			cum += e.gas
			status := types.ReceiptStatusSuccessful
			if e.failed {
				status = types.ReceiptStatusFailed
			}
			logs := e.logs
			if logs == nil {
				logs = []*types.Log{}
			}
			receipts = append(receipts, &types.Receipt{Type: e.tx.Type(), Status: status, CumulativeGasUsed: cum, GasUsed: e.gas, TxHash: e.tx.Hash(), Logs: logs})
		}
		outs := s.outbound
		if outs == nil {
			outs = []*types.Transaction{}
		}
		uncles := []*types.WorkObjectHeader{}
		for _, un := range s.uncles {
			uncles = append(uncles, types.CopyWorkObjectHeader(c.uncles[un].WorkObjectHeader()))
		}
		wo := makeWo(number, parent, s.time, s.coinbase, txs, outs, uncles, receipts)
		hash := wo.Hash()
		rawdb.WriteWorkObject(db, hash, wo, types.BlockObject, common.ZONE_CTX)
		read := rawdb.ReadWorkObject(db, number, hash, types.BlockObject)
		if read == nil || read.Hash() != hash {
			panic("block read back " + s.name)
		}
		b := &built{name: s.name, wo: read}
		if s.parent != "" {
			b.receipts = receipts
		}
		pb, err := read.ProtoEncode(types.BlockObject)
		must(err)
		raw, err := proto.Marshal(pb)
		must(err)
		txHashes := []interface{}{}
		for _, t := range read.Transactions() {
			txHashes = append(txHashes, t.Hash().Hex())
		}
		etxHashes := []interface{}{}
		for _, t := range read.OutboundEtxs() {
			etxHashes = append(etxHashes, t.Hash().Hex())
		}
		var stored interface{}
		if b.receipts != nil {
			// The bytes rawdb stores for the block's receipts.
			rawdb.WriteReceipts(db, hash, number, b.receipts)
			stored = hx(rawdb.ReadReceiptsProto(db, hash, number))
		}
		b.record = obj{"hash": hash.Hex(), "number": number, "parent": parent.Hex(), "object": hx(raw), "size": uint64(read.Size()), "tx_hashes": txHashes, "etx_hashes": etxHashes, "receipts": stored}
		if _, dup := c.byHash[hash]; dup {
			panic("duplicate block hash " + s.name)
		}
		c.blocks[s.name] = b
		c.byHash[hash] = b
		c.order = append(c.order, s.name)
	}
	return c
}

func (c *chain) out() obj {
	blocks := obj{}
	for _, n := range c.order {
		blocks[n] = c.blocks[n].record
	}
	return obj{"chain_id": 9, "location": []int{0, 0}, "genesis": "g", "blocks": blocks}
}

// --- backend ----------------------------------------------------------------

// backend is filters.Backend over the oracle's chain. Methods the filter
// system does not call for these scenarios stay nil.
type backend struct {
	filters.Backend
	cfg   *params.ChainConfig
	db    ethdb.Database
	chain *chain

	mu    sync.Mutex
	canon map[uint64]common.Hash
	head  *types.WorkObject

	chainFeed         event.Feed
	chainHeadFeed     event.Feed
	rmLogsFeed        event.Feed
	logsFeed          event.Feed
	pendingLogsFeed   event.Feed
	pendingHeaderFeed event.Feed
	workshareFeed     event.Feed
	unlocksFeed       event.Feed
}

func newBackend(c *chain, head string) *backend {
	b := &backend{cfg: &c.cfg, db: rawdb.NewDatabase(locStore{memorydb.New(log.Global)}), chain: c, canon: map[uint64]common.Hash{}}
	for _, n := range c.order {
		blk := c.blocks[n]
		h := blk.wo.Hash()
		rawdb.WriteWorkObject(b.db, h, blk.wo, types.BlockObject, common.ZONE_CTX)
		if blk.receipts != nil {
			rawdb.WriteReceipts(b.db, h, blk.wo.NumberU64(common.ZONE_CTX), blk.receipts)
		}
	}
	// The starting canonical chain: the head and its ancestors.
	for cur := c.blocks[head]; ; {
		n := cur.wo.NumberU64(common.ZONE_CTX)
		b.canon[n] = cur.wo.Hash()
		rawdb.WriteCanonicalHash(b.db, cur.wo.Hash(), n)
		if n == 0 {
			break
		}
		cur = c.byHash[cur.wo.ParentHash(common.ZONE_CTX)]
	}
	b.head = c.blocks[head].wo
	rawdb.WriteHeadBlockHash(b.db, b.head.Hash())
	return b
}

func (b *backend) ChainDb() ethdb.Database { return b.db }
func (b *backend) HeaderByNumber(_ context.Context, n rpc.BlockNumber) (*types.WorkObject, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	switch n {
	case rpc.PendingBlockNumber:
		return nil, nil
	case rpc.LatestBlockNumber:
		return b.head, nil
	}
	h, ok := b.canon[uint64(n)]
	if !ok {
		return nil, nil
	}
	return b.chain.byHash[h].wo, nil
}
func (b *backend) HeaderByHash(_ context.Context, h common.Hash) (*types.WorkObject, error) {
	if blk, ok := b.chain.byHash[h]; ok {
		return blk.wo, nil
	}
	return nil, nil
}

// GetReceipts and GetLogs as quai/api_backend.go: the stored receipts with
// their derived fields (StateProcessor.GetReceiptsByHash).
func (b *backend) GetReceipts(_ context.Context, h common.Hash) (types.Receipts, error) {
	return b.receipts(h), nil
}
func (b *backend) receipts(h common.Hash) types.Receipts {
	number := rawdb.ReadHeaderNumber(b.db, h)
	if number == nil {
		return nil
	}
	return rawdb.ReadReceipts(b.db, h, *number, b.cfg)
}
func (b *backend) GetLogs(_ context.Context, h common.Hash) ([][]*types.Log, error) {
	receipts := b.receipts(h)
	if receipts == nil {
		return nil, nil
	}
	logs := make([][]*types.Log, len(receipts))
	for i, r := range receipts {
		logs[i] = r.Logs
	}
	return logs, nil
}
func (b *backend) GetBloom(common.Hash) (*types.Bloom, error) { return nil, nil }
func (b *backend) GetBlock(h common.Hash, n uint64) (*types.WorkObject, error) {
	if blk, ok := b.chain.byHash[h]; ok && blk.wo.NumberU64(common.ZONE_CTX) == n {
		return blk.wo, nil
	}
	return nil, nil
}
func (b *backend) SubscribeChainEvent(ch chan<- core.ChainEvent) event.Subscription {
	return b.chainFeed.Subscribe(ch)
}
func (b *backend) SubscribeChainHeadEvent(ch chan<- core.ChainHeadEvent) event.Subscription {
	return b.chainHeadFeed.Subscribe(ch)
}
func (b *backend) SubscribeRemovedLogsEvent(ch chan<- core.RemovedLogsEvent) event.Subscription {
	return b.rmLogsFeed.Subscribe(ch)
}
func (b *backend) SubscribeLogsEvent(ch chan<- []*types.Log) event.Subscription {
	return b.logsFeed.Subscribe(ch)
}
func (b *backend) SubscribePendingLogsEvent(ch chan<- []*types.Log) event.Subscription {
	return b.pendingLogsFeed.Subscribe(ch)
}
func (b *backend) SubscribePendingHeaderEvent(ch chan<- *types.WorkObject) event.Subscription {
	return b.pendingHeaderFeed.Subscribe(ch)
}
func (b *backend) SubscribeNewWorkshareEvent(ch chan<- core.NewWorkshareEvent) event.Subscription {
	return b.workshareFeed.Subscribe(ch)
}
func (b *backend) SubscribeUnlocksEvent(ch chan<- core.UnlocksEvent) event.Subscription {
	return b.unlocksFeed.Subscribe(ch)
}
func (b *backend) ProcessingState() bool         { return true }
func (b *backend) NodeLocation() common.Location { return loc }
func (b *backend) NodeCtx() int                  { return common.ZONE_CTX }
func (b *backend) RpcVersion() string            { return "v1" }
func (b *backend) Logger() *log.Logger           { return log.Global }

// moveHead moves the head to `to` as SetCurrentHeader and
// GeneratePendingHeader do: canonical entries of the old branch above the
// common ancestor go, the new blocks become canonical oldest first and send
// their logs, then one ChainHeadEvent carries the new head as read from the
// database. It returns the names of the new canonical blocks.
func (b *backend) moveHead(to string) []string {
	target := b.chain.blocks[to]
	var stack []*built
	b.mu.Lock()
	cur := target
	for {
		n := cur.wo.NumberU64(common.ZONE_CTX)
		if h, ok := b.canon[n]; ok && h == cur.wo.Hash() {
			break
		}
		stack = append(stack, cur)
		cur = b.chain.byHash[cur.wo.ParentHash(common.ZONE_CTX)]
	}
	common_ := cur.wo.NumberU64(common.ZONE_CTX)
	for n := range b.canon {
		if n > common_ {
			delete(b.canon, n)
			rawdb.DeleteCanonicalHash(b.db, n)
		}
	}
	b.head = cur.wo
	b.mu.Unlock()
	names := []string{}
	for i := len(stack) - 1; i >= 0; i-- {
		blk := stack[i]
		n := blk.wo.NumberU64(common.ZONE_CTX)
		b.mu.Lock()
		b.canon[n] = blk.wo.Hash()
		rawdb.WriteCanonicalHash(b.db, blk.wo.Hash(), n)
		b.mu.Unlock()
		// AppendBlock: the logs of Apply, sent before the head moves.
		var logs []*types.Log
		for _, r := range b.receipts(blk.wo.Hash()) {
			logs = append(logs, r.Logs...)
		}
		if len(logs) > 0 {
			b.logsFeed.Send(logs)
			time.Sleep(pace)
		}
		b.mu.Lock()
		b.head = blk.wo
		rawdb.WriteHeadBlockHash(b.db, blk.wo.Hash())
		b.mu.Unlock()
		names = append(names, blk.name)
	}
	head := rawdb.ReadWorkObject(b.db, target.wo.NumberU64(common.ZONE_CTX), target.wo.Hash(), types.BlockObject)
	b.chainHeadFeed.Send(core.ChainHeadEvent{Block: head})
	time.Sleep(pace)
	return names
}

// --- WebSocket connections ----------------------------------------------------

type wsConn struct {
	c      *websocket.Conn
	mu     sync.Mutex
	frames []string
	last   time.Time
	closed bool
}

func dial(url string) *wsConn {
	c, _, err := websocket.DefaultDialer.Dial(url, nil)
	must(err)
	w := &wsConn{c: c, last: time.Now()}
	go func() {
		for {
			_, data, err := c.ReadMessage()
			w.mu.Lock()
			if err != nil {
				w.closed = true
				w.mu.Unlock()
				return
			}
			w.frames = append(w.frames, string(data))
			w.last = time.Now()
			w.mu.Unlock()
		}
	}()
	return w
}

func isNotification(text string) bool {
	var m obj
	if json.Unmarshal([]byte(text), &m) != nil {
		return false
	}
	_, hasID := m["id"]
	return m["method"] != nil && !hasID
}

// answer waits for the first frame that is not a notification and takes
// it out of the buffer.
func (w *wsConn) answer() string {
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		w.mu.Lock()
		for i, f := range w.frames {
			if !isNotification(f) {
				w.frames = append(w.frames[:i], w.frames[i+1:]...)
				w.mu.Unlock()
				return f
			}
		}
		w.mu.Unlock()
		time.Sleep(2 * time.Millisecond)
	}
	panic("no answer on the WebSocket connection")
}

// drain waits until no frame arrived for `quiet` (at most 5 s) and takes
// every buffered frame.
func (w *wsConn) drain() []string {
	start := time.Now()
	for {
		w.mu.Lock()
		idle := time.Since(w.last)
		w.mu.Unlock()
		if (idle >= quiet && time.Since(start) >= quiet) || time.Since(start) > 5*time.Second {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}
	w.mu.Lock()
	defer w.mu.Unlock()
	out := w.frames
	w.frames = nil
	return out
}

// --- scenarios ----------------------------------------------------------------

type env struct {
	name    string
	timeout time.Duration
	limit   int
	start   string
	chain   *chain
	b       *backend
	srv     *rpc.Server
	httpURL string
	wsURL   string
	servers []*httptest.Server
	conns   map[int]*wsConn
	steps   []interface{}
	real    map[string]string // placeholder -> ID
	place   map[string]string // ID -> placeholder
	nf, ns  int
	reqID   int
	framed  map[string]bool
}

func newEnv(c *chain, name string, timeout time.Duration, limit int, head string) *env {
	e := &env{name: name, timeout: timeout, limit: limit, start: head, chain: c, conns: map[int]*wsConn{}, real: map[string]string{}, place: map[string]string{}, framed: map[string]bool{}}
	e.b = newBackend(c, head)
	e.srv = rpc.NewServer(log.Global)
	must(e.srv.RegisterName("quai", filters.NewPublicFilterAPI(e.b, timeout, limit)))
	h := httptest.NewServer(e.srv)
	ws := httptest.NewServer(e.srv.WebsocketHandler([]string{"*"}))
	e.servers = append(e.servers, h, ws)
	e.httpURL = h.URL
	e.wsURL = "ws" + strings.TrimPrefix(ws.URL, "http")
	return e
}

func (e *env) close() {
	for _, c := range e.conns {
		c.c.Close()
	}
	for _, s := range e.servers {
		s.Close()
	}
}

// substitute replaces placeholders in a request with the real IDs.
func (e *env) substitute(v interface{}) interface{} {
	switch t := v.(type) {
	case string:
		if r, ok := e.real[t]; ok {
			return r
		}
		return t
	case []interface{}:
		out := make([]interface{}, len(t))
		for i, x := range t {
			out[i] = e.substitute(x)
		}
		return out
	case obj:
		out := obj{}
		for k, x := range t {
			out[k] = e.substitute(x)
		}
		return out
	}
	return v
}

func (e *env) register(id string, kind string) string {
	if p, ok := e.place[id]; ok {
		return p
	}
	if !idRe.MatchString(id) {
		panic(fmt.Sprintf("ID %q does not match %s", id, idPattern))
	}
	var p string
	if kind == "F" {
		e.nf++
		p = fmt.Sprintf("$F%d", e.nf)
	} else {
		e.ns++
		p = fmt.Sprintf("$S%d", e.ns)
	}
	e.place[id] = p
	e.real[p] = id
	return p
}

// creates reports which ID kind a successful call returns.
func creates(method string) string {
	switch method {
	case "quai_newFilter", "quai_newBlockFilter", "quai_newPendingTransactionFilter":
		return "F"
	}
	if strings.HasSuffix(method, "_subscribe") {
		return "S"
	}
	return ""
}

// mask replaces the IDs in one response object.
func (e *env) mask(resp obj, method string) {
	r, ok := resp["result"].(string)
	if !ok {
		return
	}
	if kind := creates(method); kind != "" {
		resp["result"] = e.register(r, kind)
	} else if p, ok := e.place[r]; ok {
		resp["result"] = p
	}
}

func decode(text string) interface{} {
	d := json.NewDecoder(strings.NewReader(text))
	d.UseNumber()
	var v interface{}
	must(d.Decode(&v))
	return v
}

// maskAnswer masks a single or batch answer against its request.
func (e *env) maskAnswer(req interface{}, text string) interface{} {
	v := decode(text)
	methods := map[string]string{}
	collect := func(r interface{}) {
		if m, ok := r.(obj); ok {
			if id, ok := m["id"]; ok {
				methods[fmt.Sprint(id)] = fmt.Sprint(m["method"])
			}
		}
	}
	if list, ok := req.([]interface{}); ok {
		for _, r := range list {
			collect(r)
		}
	} else {
		collect(req)
	}
	apply := func(x interface{}) {
		if m, ok := x.(obj); ok {
			e.mask(m, methods[fmt.Sprint(m["id"])])
		}
	}
	if list, ok := v.([]interface{}); ok {
		for _, x := range list {
			apply(x)
		}
	} else {
		apply(v)
	}
	return v
}

// maskText replaces every known real ID in a raw frame.
func (e *env) maskText(text string) string {
	for id, p := range e.place {
		text = strings.ReplaceAll(text, `"`+id+`"`, `"`+p+`"`)
	}
	return text
}

func (e *env) req(method string, params ...interface{}) obj {
	e.reqID++
	r := obj{"jsonrpc": "2.0", "id": e.reqID, "method": method}
	if params != nil {
		r["params"] = params
	}
	return r
}

// emptyParams is a request with "params": [].
func (e *env) emptyParams(method string) obj {
	e.reqID++
	return obj{"jsonrpc": "2.0", "id": e.reqID, "method": method, "params": []interface{}{}}
}

// noParams is a request without "params".
func (e *env) noParams(method string) obj {
	e.reqID++
	return obj{"jsonrpc": "2.0", "id": e.reqID, "method": method}
}

func (e *env) http(req interface{}) interface{} {
	body, err := json.Marshal(e.substitute(req))
	must(err)
	resp, err := http.Post(e.httpURL, "application/json", bytes.NewReader(body))
	must(err)
	raw, err := io.ReadAll(resp.Body)
	must(err)
	resp.Body.Close()
	answer := e.maskAnswer(req, string(raw))
	e.steps = append(e.steps, obj{"op": "call", "via": "http", "request": req, "response": answer})
	return answer
}

func (e *env) conn(k int) *wsConn {
	c, ok := e.conns[k]
	if !ok {
		c = dial(e.wsURL)
		e.conns[k] = c
	}
	return c
}

// ws sends a request on connection k and records it with its answer; with
// frame, the raw answer text is recorded too.
func (e *env) ws(k int, req interface{}, frame bool) interface{} {
	c := e.conn(k)
	body, err := json.Marshal(e.substitute(req))
	must(err)
	must(c.c.WriteMessage(websocket.TextMessage, body))
	text := c.answer()
	answer := e.maskAnswer(req, text)
	e.steps = append(e.steps, obj{"op": "call", "via": "ws", "conn": k, "request": req, "response": answer})
	if frame {
		e.steps = append(e.steps, obj{"op": "frame", "via": "ws", "conn": k, "text": e.maskText(text)})
	}
	return answer
}

func (e *env) head(name string) {
	names := e.b.moveHead(name)
	list := []interface{}{}
	for _, n := range names {
		list = append(list, n)
	}
	e.steps = append(e.steps, obj{"op": "head", "head": name, "new_blocks": list})
}

// settle waits for the filter goroutines after a head step of a scenario
// without WebSocket connections.
func (e *env) settle() { time.Sleep(4 * pace) }

func (e *env) notifications(k int) {
	c := e.conns[k]
	frames := c.drain()
	by := obj{}
	other := []interface{}{}
	// The raw frames of each subscription, in arrival order.
	raw := map[string][]string{}
	for _, f := range frames {
		if !isNotification(f) {
			other = append(other, decode(e.maskText(f)))
			continue
		}
		m := decode(f).(obj)
		params := m["params"].(obj)
		id := params["subscription"].(string)
		p, ok := e.place[id]
		if !ok {
			panic("notification for an unknown subscription " + id)
		}
		list, _ := by[p].([]interface{})
		by[p] = append(list, params["result"])
		raw[p] = append(raw[p], f)
	}
	step := obj{"op": "notifications", "conn": k, "by_subscription": by}
	if len(other) > 0 {
		step["other"] = other
	}
	e.steps = append(e.steps, step)
	// The raw text of one notification of each kind not recorded yet: the
	// first one of the lowest-numbered subscription that has one (the order
	// between subscriptions is not deterministic).
	places := make([]string, 0, len(raw))
	for p := range raw {
		places = append(places, p)
	}
	number := func(p string) int {
		var n int
		fmt.Sscanf(p, "$S%d", &n)
		return n
	}
	sort.Slice(places, func(i, j int) bool { return number(places[i]) < number(places[j]) })
	for _, kind := range []string{"newHeads", "logs"} {
		if e.framed[kind] {
			continue
		}
		for _, p := range places {
			f := raw[p][0]
			r, _ := decode(f).(obj)["params"].(obj)["result"].(obj)
			_, head := r["woHeader"]
			if (kind == "newHeads") == head {
				e.framed[kind] = true
				e.steps = append(e.steps, obj{"op": "frame", "via": "ws", "conn": k, "kind": kind, "text": e.maskText(f)})
				break
			}
		}
	}
}

func (e *env) sleep(ms int) {
	time.Sleep(time.Duration(ms) * time.Millisecond)
	e.steps = append(e.steps, obj{"op": "sleep", "ms": ms})
}

func (e *env) closeConn(k int) {
	c := e.conns[k]
	must(c.c.WriteMessage(websocket.CloseMessage, websocket.FormatCloseMessage(websocket.CloseNormalClosure, "")))
	c.c.Close()
	delete(e.conns, k)
	e.steps = append(e.steps, obj{"op": "close", "conn": k})
	time.Sleep(4 * pace)
}

// sortLogs orders a multi-block getFilterLogs answer by block number and
// log index (Go appends per-goroutine results in completion order).
func sortLogs(answer interface{}) {
	m, ok := answer.(obj)
	if !ok {
		return
	}
	list, ok := m["result"].([]interface{})
	if !ok {
		return
	}
	n := func(x interface{}, k string) uint64 {
		s := x.(obj)[k].(string)
		v, err := hexDecodeUint(s)
		must(err)
		return v
	}
	sort.SliceStable(list, func(i, j int) bool {
		bi, bj := n(list[i], "blockNumber"), n(list[j], "blockNumber")
		if bi != bj {
			return bi < bj
		}
		return n(list[i], "logIndex") < n(list[j], "logIndex")
	})
}
func hexDecodeUint(s string) (uint64, error) {
	var v uint64
	_, err := fmt.Sscanf(s, "0x%x", &v)
	return v, err
}

// httpSorted records a getFilterLogs (or getLogs) call whose logs come from
// several blocks: the answer is sorted and the step says so.
func (e *env) httpSorted(req interface{}) {
	e.http(req)
	step := e.steps[len(e.steps)-1].(obj)
	sortLogs(step["response"])
	step["sorted"] = true
}

func (e *env) out() obj {
	return obj{"name": e.name, "timeout_ms": e.timeout.Milliseconds(), "subscription_limit": e.limit, "head": e.start, "steps": e.steps}
}

func hash(c *chain, name string) string { return c.blocks[name].wo.Hash().Hex() }

// --- A: filters over HTTP -----------------------------------------------------

func filtersScenario(c *chain) obj {
	e := newEnv(c, "filters", 5*time.Minute, 1000, "b3")
	defer e.close()
	A1, A2, A3 := a1.Hex(), a2.Hex(), a3.Hex()
	T1, T2, T3, T4 := t1.Hex(), t2.Hex(), t3.Hex(), t4.Hex()
	e.http(e.req("quai_newBlockFilter"))
	e.http(e.req("quai_newPendingTransactionFilter"))
	logFilters := []string{}
	newFilter := func(crit interface{}) {
		answer := e.http(e.req("quai_newFilter", crit)).(obj)
		if p, ok := answer["result"].(string); ok {
			logFilters = append(logFilters, p)
		}
	}
	newFilter(obj{})
	newFilter(obj{"address": A1})
	newFilter(obj{"address": []interface{}{A1, A2}})
	newFilter(obj{"topics": []interface{}{[]interface{}{T1, T2}, nil, T3}})
	newFilter(obj{"fromBlock": "0x5"})
	newFilter(obj{"fromBlock": "0x2", "toBlock": "0x6"})
	newFilter(obj{"fromBlock": "pending", "toBlock": "pending"})
	newFilter(obj{"fromBlock": "latest", "toBlock": "pending"})
	newFilter(obj{"fromBlock": "earliest"})
	newFilter(obj{"blockHash": hash(c, "b2")})
	newFilter(obj{"fromBlock": "0x5", "toBlock": "latest"})
	newFilter(obj{"fromBlock": "0x1", "toBlock": "0x2711"})
	newFilter(obj{"fromBlock": "0x0", "toBlock": "0x2710"})
	newFilter(obj{"topics": []interface{}{nil, T2}})
	newFilter(obj{"topics": []interface{}{[]interface{}{}}})
	newFilter(obj{"topics": []interface{}{[]interface{}{nil, T4}, T4}})
	newFilter(obj{"address": A3, "topics": []interface{}{[]interface{}{T1}}})
	newFilter(obj{"fromBlock": "latest", "toBlock": "latest", "address": []interface{}{}})
	// Errors.
	for _, crit := range []interface{}{
		obj{"fromBlock": "0x6", "toBlock": "0x2"},
		obj{"fromBlock": "0x0", "toBlock": "0x2711"},
		obj{"fromBlock": "pending", "toBlock": "0x5"},
		obj{"fromBlock": "latest", "toBlock": "0x5"},
		obj{"blockHash": hash(c, "b2"), "fromBlock": "0x1"},
		obj{"address": "0x12"},
		obj{"address": 5},
		obj{"address": []interface{}{A1, 5}},
		obj{"topics": "x"},
		obj{"topics": []interface{}{"0x12"}},
		obj{"topics": []interface{}{[]interface{}{5}}},
		obj{"fromBlock": "0xg"},
		"x",
	} {
		e.http(e.req("quai_newFilter", crit))
	}
	e.http(e.noParams("quai_newFilter"))
	e.http(e.req("quai_newFilter", obj{}, obj{}))
	e.http(e.req("quai_newBlockFilter", 1))

	all := append([]string{"$F1", "$F2"}, logFilters...)
	changes := func() {
		for _, f := range all {
			e.http(e.req("quai_getFilterChanges", f))
		}
	}
	e.head("b4")
	e.settle()
	changes()
	e.head("b7")
	e.settle()
	changes()
	e.head("c6")
	e.settle()
	changes()
	changes()
	// A filter on a block the reorg removed.
	answer := e.http(e.req("quai_newFilter", obj{"blockHash": hash(c, "b5")})).(obj)
	removedFilter, _ := answer["result"].(string)
	for _, f := range append(logFilters, removedFilter) {
		e.httpSorted(e.req("quai_getFilterLogs", f))
	}
	e.http(e.req("quai_getFilterLogs", "$F1"))
	e.http(e.req("quai_getFilterLogs", "$F2"))
	e.http(e.req("quai_getFilterLogs", "0x1234"))
	e.http(e.req("quai_getFilterChanges", "0x1234"))
	e.http(e.req("quai_uninstallFilter", logFilters[1]))
	e.http(e.req("quai_uninstallFilter", logFilters[1]))
	e.http(e.req("quai_getFilterChanges", logFilters[1]))
	e.http(e.req("quai_getFilterLogs", logFilters[1]))
	e.http(e.req("quai_uninstallFilter", "$F1"))
	e.http(e.req("quai_getFilterChanges", "$F1"))
	for _, m := range []string{"quai_getFilterChanges", "quai_getFilterLogs", "quai_uninstallFilter"} {
		e.http(e.noParams(m))
		e.http(e.emptyParams(m))
		e.http(e.req(m, 1))
		e.http(e.req(m, nil))
		e.http(e.req(m, "$F2", "x"))
	}
	// The head moves again: the remaining filters see it.
	e.head("c7")
	e.settle()
	changes()
	return e.out()
}

// --- B: expiry ------------------------------------------------------------------

func expiryScenario(c *chain) obj {
	// Timeout 2 s: the timeout loop ticks at 2, 4 and 6 s after the API
	// starts; a filter goes at the first tick after its deadline (the last
	// poll plus the timeout) passed.
	e := newEnv(c, "expiry", 2*time.Second, 1000, "b3")
	defer e.close()
	e.http(e.req("quai_newBlockFilter"))
	e.http(e.req("quai_newFilter", obj{}))
	e.sleep(600)
	e.http(e.req("quai_newFilter", obj{"address": a1.Hex()}))
	for i := 0; i < 6; i++ {
		e.sleep(400)
		e.http(e.req("quai_getFilterChanges", "$F1"))
	}
	// $F3's deadline passed at about 2.6 s and the next tick is at 4 s: a
	// poll now keeps it.
	e.http(e.req("quai_getFilterChanges", "$F3"))
	for i := 0; i < 4; i++ {
		e.sleep(400)
		e.http(e.req("quai_getFilterChanges", "$F1"))
	}
	// The tick at 2 s or 4 s removed $F2, never polled.
	e.http(e.req("quai_getFilterChanges", "$F2"))
	e.http(e.req("quai_getFilterChanges", "$F3"))
	e.http(e.req("quai_uninstallFilter", "$F2"))
	e.http(e.req("quai_getFilterLogs", "$F2"))
	return e.out()
}

// --- C: subscriptions over WebSocket ----------------------------------------------

func subscriptionsScenario(c *chain) obj {
	e := newEnv(c, "subscriptions", 5*time.Minute, 1000, "b3")
	defer e.close()
	A1, T1, T2, T4 := a1.Hex(), t1.Hex(), t2.Hex(), t4.Hex()
	// sub subscribes on connection k and returns the placeholder.
	sub := func(k int, frame bool, params ...interface{}) string {
		answer := e.ws(k, e.req("quai_subscribe", params...), frame).(obj)
		p, _ := answer["result"].(string)
		return p
	}
	sub(0, true, "newHeads")
	sub(0, false, "newPendingTransactions")
	all := sub(0, false, "logs", obj{})
	addressTopics := sub(0, false, "logs", obj{"address": A1, "topics": []interface{}{[]interface{}{T1, T2}}})
	sub(0, false, "logs", obj{"fromBlock": "0x2", "toBlock": "0x5"})
	sub(0, false, "logs", obj{"fromBlock": "pending", "toBlock": "pending"})
	sub(0, false, "logs", obj{"fromBlock": "latest", "toBlock": "pending"})
	sub(0, false, "logs", obj{"blockHash": hash(c, "b2")})
	sub(0, false, "logs", obj{"fromBlock": "0x6"})
	nullCriteria := sub(0, false, "logs", nil)
	// Errors.
	e.ws(0, e.req("quai_subscribe", "nope"), true)
	e.ws(0, e.emptyParams("quai_subscribe"), false)
	e.ws(0, e.noParams("quai_subscribe"), false)
	e.ws(0, e.req("quai_subscribe", "newHeads", obj{}), false)
	e.ws(0, e.req("quai_subscribe", "logs"), false)
	e.ws(0, e.req("quai_subscribe", "logs", obj{"fromBlock": "0x6", "toBlock": "0x2"}), false)
	e.ws(0, e.req("quai_subscribe", "logs", obj{"fromBlock": "0x0", "toBlock": "0x2711"}), false)
	e.ws(0, e.req("quai_subscribe", "logs", obj{"address": 5}), false)
	e.ws(0, e.req("quai_subscribe", "logs", obj{}, obj{}), false)
	e.ws(0, e.req("quai_subscribe", 1), false)
	e.ws(0, e.req("quai_subscribe", nil), false)
	e.ws(0, e.req("quai_subscribe", "newPendingTransactions", 1), false)
	e.ws(0, e.req("quai_subscribe", "NewHeads"), false)
	e.ws(0, e.req("eth_subscribe", "newHeads"), false)
	e.ws(0, e.req("foo_subscribe", "newHeads"), false)
	other := sub(1, false, "newHeads")
	sub(1, false, "logs", obj{"topics": []interface{}{nil, T2}})
	sub(1, false, "newPendingTransactions")
	time.Sleep(4 * pace)

	both := func() {
		e.notifications(0)
		e.notifications(1)
	}
	e.head("b4")
	both()
	e.head("b7")
	both()
	e.head("c6")
	both()
	// Unsubscribe: any "_unsubscribe" method cancels a subscription of the
	// same connection, whatever its namespace.
	e.ws(0, e.req("quai_unsubscribe", all), true)
	e.ws(0, e.req("quai_unsubscribe", all), true)
	e.ws(0, e.req("quai_unsubscribe", other), false)
	e.ws(0, e.emptyParams("quai_unsubscribe"), false)
	e.ws(0, e.noParams("quai_unsubscribe"), false)
	e.ws(0, e.req("quai_unsubscribe", 1), false)
	e.ws(0, e.req("quai_unsubscribe", nil), false)
	e.ws(0, e.req("quai_unsubscribe", addressTopics, "x"), false)
	e.ws(0, e.req("eth_unsubscribe", addressTopics), false)
	e.ws(0, e.req("foo_unsubscribe", "0x1"), false)
	e.ws(0, e.req("foo_unsubscribe", nullCriteria), false)
	time.Sleep(4 * pace)
	e.head("c7")
	both()
	e.closeConn(1)
	e.head("c8")
	e.notifications(0)
	// A batch with two subscriptions and an error.
	e.ws(0, []interface{}{e.req("quai_subscribe", "newHeads"), e.req("quai_subscribe", "logs", obj{"topics": []interface{}{T4}}), e.req("quai_subscribe", "nope")}, true)
	time.Sleep(4 * pace)
	// Back to the first branch: b5..b8 become canonical again.
	e.head("b8")
	e.notifications(0)
	// Filters and subscriptions share the API: a filter made over the
	// WebSocket connection is polled over HTTP.
	f := e.ws(0, e.req("quai_newBlockFilter"), false).(obj)["result"]
	e.head("c6")
	e.notifications(0)
	e.http(e.req("quai_getFilterChanges", f))
	// Over HTTP there are no subscriptions.
	e.http(e.req("quai_subscribe", "newHeads"))
	e.http(e.req("quai_subscribe", "nope"))
	e.http(e.req("quai_unsubscribe", "$S1"))
	e.http(e.req("quai_unsubscribe", "0x1"))
	return e.out()
}

// --- D: the subscription limit ---------------------------------------------------

func limitScenario(c *chain) obj {
	e := newEnv(c, "limit", 5*time.Minute, 2, "b3")
	defer e.close()
	step := func(k int, req obj) {
		e.ws(k, req, false)
		e.sleep(100)
	}
	step(0, e.req("quai_subscribe", "newHeads"))
	step(0, e.req("quai_subscribe", "logs", obj{}))
	step(0, e.req("quai_subscribe", "newPendingTransactions"))
	step(0, e.req("quai_subscribe", "logs", obj{"fromBlock": "0x6", "toBlock": "0x2"}))
	step(1, e.req("quai_subscribe", "newHeads"))
	step(0, e.req("quai_unsubscribe", "$S1"))
	step(1, e.req("quai_subscribe", "newHeads"))
	step(0, e.req("quai_subscribe", "newHeads"))
	// Filters do not count.
	e.http(e.req("quai_newBlockFilter"))
	e.closeConn(1)
	e.sleep(100)
	step(0, e.req("quai_subscribe", "newHeads"))
	e.head("b4")
	e.notifications(0)
	return e.out()
}

func main() {
	log.Global.SetOutput(io.Discard)
	c := newChain()
	scenarios := []interface{}{
		filtersScenario(c),
		expiryScenario(c),
		subscriptionsScenario(c),
		limitScenario(c),
	}
	must(json.NewEncoder(os.Stdout).Encode(obj{
		"reference_revision": referenceRevision,
		"id_pattern":         idPattern,
		"chain":              c.out(),
		"scenarios":          scenarios,
		"scope": "Production quai/filters PublicFilterAPI and EventSystem behind one rpc.Server (namespace quai) over HTTP (Server.ServeHTTP) and WebSocket (Server.WebsocketHandler), with raw gorilla/websocket clients, over a test chain in a rawdb memory database (work objects with Quai, external and Qi transactions, stored receipts with logs, uncles, a side branch from b4). Events as go-quai v0.57.0 core emits them: a head move rewrites the canonical index; each block that becomes canonical, oldest first, sends its logs (the stored receipts' logs with derived fields) on the logs feed when it has any; then one ChainHeadEvent carries the new head read back from the database; no RemovedLogsEvent and no ChainEvent (nothing in v0.57.0 sends them for a head move). Sends are paced by 50 ms; a notifications step waits for 500 ms without frames. IDs are replaced by $F<n> and $S<n> in order of first appearance; by_subscription lists every subscription that received a notification since the connection's previous notifications step (others received none); steps marked sorted had their multi-block log list sorted by block number and log index (Filter.Logs appends goroutine results in completion order). Fixed-seed keys; no network, no real funds.",
	}))
}
