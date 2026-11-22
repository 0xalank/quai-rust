// RPC parity oracle: go-quai v0.57.0 answers to the read methods that a
// default zone node serves and the native Rust RPC did not: the `quai_`
// methods quai_getRawTransactionByBlockNumberAndIndex,
// quai_getRawTransactionByBlockHashAndIndex, quai_getBlockOrCandidateByHash,
// quai_baseFee, quai_getLatestUTXOSetSize, quai_suggestFinalityDepth,
// quai_hashesPerQits, quai_getSupplyAnalyticsForBlock, quai_rewardAnalytics,
// quai_getKQuaiAndUpdateBit, quai_getContractSize,
// quai_getLockupsForContractAndMiner, quai_getProof and quai_clientVersion;
// net_peerCount and net_peerCountByDirection; and the `eth_` namespace that
// the default WebSocket endpoint serves (internal/quaiapi/api.go
// PublicBlockChainAPI, and the handlers it shares with `quai_`).
//
// The production handlers (internal/quaiapi: PublicQuaiAPI,
// PublicBlockChainAPI, PublicBlockChainQuaiAPI, PublicTransactionPoolAPI,
// PublicNetAPI) run behind the production JSON-RPC server (rpc.Server over
// HTTP), registered in quaiapi.GetAPIs' order, over a test chain in a rawdb
// memory database. The chain follows reference/filters: work objects with
// signed Quai transactions, a Qi transaction, an inbound and outbound ETXs,
// uncles, stored receipts with logs and a side branch. Added here: one
// committed state (accounts, a contract with storage, the burn address)
// whose root every block carries, a prime terminus with an exchange rate,
// UTXO set sizes, the address UTXO index of an indexing node, and coinbase
// lockup records.
//
// The backend mirrors quai/api_backend.go and core for each method the
// handlers call. Three values come from consensus code the oracle does not
// run and are inputs, recorded in the fixture: CalcOrder (2, a zone block),
// UncleWorkShareClassification (per uncle) and TotalLogEntropy (per block).
// GetKQuaiAndUpdateBit follows core/slice.go and core/state_processor.go:
// a stored block gives params.ExchangeRate and 1, an unknown hash
// ErrSubNotSyncedToDom. The genesis allocations, forfeiture and founder
// forfeiture lists of quai_rewardAnalytics are the production files of
// zone [0 0] (params/genesis_alloc.json and the forfeiture lists, checked
// against params.AllocHash). quai_clientVersion is go-quai's answer for a
// release build of the pinned commit (params.GitCommit set to it, VERSION
// from the checkout). No network, no keys with funds.
package main

import (
	"bytes"
	"context"
	"crypto/ecdsa"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"math/big"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/btcsuite/btcd/btcec/v2"
	"github.com/btcsuite/btcd/btcec/v2/schnorr"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus/misc"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/ethdb"
	"github.com/dominant-strategies/go-quai/ethdb/memorydb"
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
func hx(b []byte) string  { return "0x" + hex.EncodeToString(b) }
func w(v *big.Int) string { return hx(v.FillBytes(make([]byte, 32))) }
func gwei(v uint64) *big.Int {
	return new(big.Int).Mul(u(v), u(params.GWei))
}
func dec(s string) *big.Int {
	v, ok := new(big.Int).SetString(s, 10)
	if !ok {
		panic("bad decimal " + s)
	}
	return v
}

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
		seed := crypto.Keccak256([]byte(fmt.Sprintf("rpc-parity-oracle/%s/%d", name, i)))
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
	return crypto.Keccak256Hash([]byte(fmt.Sprintf("rpc-parity-oracle/topic/%d", n)))
}

var (
	a1       = addr("0x00110000000000000000000000000000000000a1")
	a2       = addr("0x00110000000000000000000000000000000000a2")
	a3       = addr("0x00110000000000000000000000000000000000a3")
	store    = addr("0x00330000000000000000000000000000000000f1") // contract with storage
	empty    = addr("0x0044000000000000000000000000000000000077") // no account
	other    = addr("0x01110000000000000000000000000000000000c1") // zone [0 1]
	qx       = addr("0x0183000000000000000000000000000000000c2e") // Qi, zone [0 1]
	burn     = addr("0x0050AF0000000000000000000000000000000000")
	owner    = addr("0x0066000000000000000000000000000000000a11") // lockup contract
	miner    = addr("0x0067000000000000000000000000000000000b22") // lockup beneficiary
	delegate = addr("0x0068000000000000000000000000000000000c33")
	qiMiner  = addr("0x0089000000000000000000000000000000000d44") // a Qi beneficiary
	t1       = topic(1)
	t2       = topic(2)
	t3       = topic(3)
)

func signQuai(k key, nonce uint64, to *common.Address, value uint64, data []byte) *types.Transaction {
	inner := &types.QuaiTx{ChainID: chainID, Nonce: nonce, GasPrice: gwei(2), Gas: 90000, To: to, Value: u(value), Data: data, AccessList: types.AccessList{}}
	tx, err := types.SignTx(types.NewTx(inner), types.NewSigner(chainID, loc), k.prv)
	must(err)
	return tx
}
func to(a common.Address) *common.Address { return &a }

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

// --- state ------------------------------------------------------------------

type account struct {
	a       common.Address
	balance *big.Int
	nonce   uint64
	code    []byte
	storage map[common.Hash]common.Hash
}

func slot(n uint64) common.Hash { return common.BigToHash(u(n)) }

func accounts(k1, k2 key) []account {
	slots := map[common.Hash]common.Hash{}
	for i := uint64(0); i < 24; i++ {
		slots[slot(i)] = common.BigToHash(u(1000 + i*i))
	}
	slots[crypto.Keccak256Hash([]byte("rpc-parity-oracle/slot"))] = common.BigToHash(new(big.Int).Lsh(u(1), 255))
	list := []account{
		{k1.addr, dec("5000000000000000000000"), 5, nil, nil},
		{k2.addr, dec("1234567890123456789"), 3, nil, nil},
		{store, u(77), 1, common.FromHex("6000548060010160015560005260206000f3"), slots},
		{burn, dec("987654321000000000000"), 0, nil, nil},
		{owner, u(0), 1, common.FromHex("00"), nil},
	}
	// More accounts give the account trie a few levels.
	for i := 0; i < 40; i++ {
		list = append(list, account{addr(fmt.Sprintf("0x0012%036x", i+1)), u(uint64(i + 1)), uint64(i), nil, nil})
	}
	return list
}

// --- chain ------------------------------------------------------------------

type entry struct {
	tx      *types.Transaction
	gas     uint64
	failed  bool
	logs    []*types.Log
	created *common.Address
	etxs    []*types.Transaction
}

type blockSpec struct {
	name, parent string
	time         uint64
	coinbase     byte
	difficulty   *big.Int
	entries      []entry
	outbound     []*types.Transaction
	uncles       []string
}

type built struct {
	name     string
	wo       *types.WorkObject
	receipts types.Receipts // nil for the genesis block
	record   obj
}

type chain struct {
	cfg     params.ChainConfig
	db      ethdb.Database
	cache   state.Database
	root    common.Hash
	blocks  map[string]*built
	byHash  map[common.Hash]*built
	order   []string
	uncles  map[string]*types.WorkObject
	prime   *types.WorkObject
	world   []interface{}
	keys    map[string]key
	entropy map[common.Hash]*big.Int
}

func makeWo(number uint64, parent common.Hash, time uint64, coinbase byte, difficulty *big.Int, root common.Hash, prime common.Hash, txs, outbound []*types.Transaction, uncles []*types.WorkObjectHeader, receipts types.Receipts) *types.WorkObject {
	wo := types.EmptyWorkObject(common.ZONE_CTX)
	wo.SetNumber(u(number), common.ZONE_CTX)
	wo.SetParentHash(parent, common.ZONE_CTX)
	wo.WorkObjectHeader().SetLocation(loc)
	wo.WorkObjectHeader().SetTime(time)
	wo.WorkObjectHeader().SetDifficulty(difficulty)
	wo.WorkObjectHeader().SetPrimeTerminusNumber(u(number / 4))
	wo.WorkObjectHeader().SetPrimaryCoinbase(common.BytesToAddress([]byte{0, 0x11, 0x0c, coinbase}, loc))
	wo.WorkObjectHeader().SetNonce(types.EncodeNonce(0x1122334455667700 + number))
	wo.WorkObjectHeader().SetMixHash(crypto.Keccak256Hash([]byte(fmt.Sprintf("rpc-parity-oracle/mix/%d/%d", number, time))))
	wo.Header().SetBaseFee(gwei(1 + number))
	wo.Header().SetGasLimit(30_000_000)
	wo.Header().SetQuaiStateSize(u(0))
	wo.Header().SetExchangeRate(u(1_000_000_000_000_000))
	wo.Header().SetAvgTxFees(u(0))
	wo.Header().SetEtxSetRoot(types.EmptyRootHash)
	wo.Header().SetPrimeTerminusHash(prime)
	wo.Header().SetExtra([]byte(fmt.Sprintf("rpc-parity %d", number)))
	wo.Body().SetTransactions(txs)
	wo.Body().SetOutboundEtxs(outbound)
	wo.Body().SetUncles(uncles)
	var used uint64
	if len(receipts) > 0 {
		used = receipts[len(receipts)-1].CumulativeGasUsed
	}
	wo.Header().SetGasUsed(used)
	wo.Header().SetEVMRoot(root)
	wo.Header().SetTxHash(types.DeriveSha(types.Transactions(txs), trie.NewStackTrie(nil)))
	wo.Header().SetOutboundEtxHash(types.DeriveSha(types.Transactions(outbound), trie.NewStackTrie(nil)))
	wo.Header().SetReceiptHash(types.DeriveSha(receipts, trie.NewStackTrie(nil)))
	wo.Header().SetUncleHash(types.CalcUncleHash(uncles))
	wo.WorkObjectHeader().SetTxHash(wo.Header().TxHash())
	wo.WorkObjectHeader().SetHeaderHash(wo.Header().Hash())
	return wo
}

// readBack stores a work object and returns it as the database gives it.
func readBack(db ethdb.Database, wo *types.WorkObject) (*types.WorkObject, []byte) {
	hash := wo.Hash()
	number := wo.NumberU64(common.ZONE_CTX)
	rawdb.WriteWorkObject(db, hash, wo, types.BlockObject, common.ZONE_CTX)
	read := rawdb.ReadWorkObject(db, number, hash, types.BlockObject)
	if read == nil || read.Hash() != hash {
		panic(fmt.Sprintf("block %d read back", number))
	}
	pb, err := read.ProtoEncode(types.BlockObject)
	must(err)
	raw, err := proto.Marshal(pb)
	must(err)
	return read, raw
}

func newChain() *chain {
	db := rawdb.NewDatabase(locStore{memorydb.New(log.Global)})
	c := &chain{cfg: *params.TestChainConfig, db: db, cache: state.NewDatabase(db), blocks: map[string]*built{}, byHash: map[common.Hash]*built{}, uncles: map[string]*types.WorkObject{}, keys: map[string]key{}, entropy: map[common.Hash]*big.Int{}}
	c.cfg.Location = loc
	c.cfg.ChainID = chainID

	k1, k2, k3 := quaiKey("alice"), quaiKey("bob"), quaiKey("carol")
	qk, qe := qiKey("dave"), qiKey("erin")
	c.keys = map[string]key{"alice": k1, "bob": k2, "carol": k3, "dave": qk, "erin": qe}

	// The committed state.
	st, err := state.New(types.EmptyRootHash, types.EmptyRootHash, u(0), c.cache, c.cache, nil, loc, log.Global)
	must(err)
	for _, a := range accounts(k1, k2) {
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
	must(c.cache.TrieDB().Commit(root, false, nil))
	c.root = root
	st, err = state.New(root, types.EmptyRootHash, u(0), c.cache, c.cache, nil, loc, log.Global)
	must(err)
	for _, a := range accounts(k1, k2) {
		ia, _ := a.a.InternalAddress()
		keys := []common.Hash{}
		for k := range a.storage {
			keys = append(keys, k)
		}
		sort.Slice(keys, func(i, j int) bool { return bytes.Compare(keys[i][:], keys[j][:]) < 0 })
		storage := []interface{}{}
		for _, k := range keys {
			storage = append(storage, obj{"key": k.Hex(), "value": st.GetState(ia, k).Hex()})
		}
		c.world = append(c.world, obj{"address": hx(a.a.Bytes()), "nonce": st.GetNonce(ia), "balance": w(st.GetBalance(ia)), "code": hx(st.GetCode(ia)), "storage_size": w(st.GetSize(ia)), "storage": storage})
	}

	// The prime terminus of every block (a block that is not in the zone
	// chain; GetBlockByHash finds it by hash).
	pw := makeWo(0, crypto.Keccak256Hash([]byte("rpc-parity-oracle/prime-parent")), 1_699_999_990, 0x50, u(5_000_000_000_000), root, common.Hash{}, nil, nil, nil, nil)
	pw.Header().SetExchangeRate(dec("221077819000000000"))
	pw.Header().SetMinerDifficulty(u(4_000_000_000_000))
	pw.WorkObjectHeader().SetHeaderHash(pw.Header().Hash())
	c.prime, _ = readBack(db, pw)
	prime := c.prime.Hash()

	// Uncles: work object headers of blocks that are not in the chain.
	c.uncles["u1"] = makeWo(1, common.Hash{}, 1_700_000_011, 0x71, u(900_000), root, prime, nil, nil, nil, nil)
	c.uncles["u2"] = makeWo(3, common.Hash{}, 1_700_000_033, 0x72, u(910_000), root, prime, nil, nil, nil, nil)
	c.uncles["u3"] = makeWo(5, common.Hash{}, 1_700_000_055, 0x73, u(920_000), root, prime, nil, nil, nil, nil)

	qiTx := signQi(qk, types.OutPoint{TxHash: crypto.Keccak256Hash([]byte("rpc-parity-oracle/outpoint")), Index: 3}, []types.TxOut{
		{Denomination: 3, Address: qe.addr.Bytes(), Lock: big.NewInt(0)},
		{Denomination: 1, Address: qk.addr.Bytes(), Lock: big.NewInt(0)},
	})
	incoming := etx(crypto.Keccak256Hash([]byte("rpc-parity-oracle/origin")), 2, a3, other, 5, types.DefaultType)
	coinbaseEtx := etx(crypto.Keccak256Hash([]byte("rpc-parity-oracle/coinbase")), 0, k3.addr, k3.addr, 4_000, types.CoinbaseType)
	outbound := etx(common.Hash{}, 0, other, k1.addr, 7, types.DefaultType)
	outbound2 := etx(common.Hash{}, 1, other, k2.addr, 9, types.DefaultType)
	created := crypto.CreateAddress(k2.addr, 1, nil, loc)
	big1 := dec("2000000000000000")

	specs := []blockSpec{
		{name: "g", time: 1_700_000_000, coinbase: 0x00, difficulty: u(1_000_000)},
		{name: "b1", parent: "g", time: 1_700_000_010, coinbase: 0x01, difficulty: u(1_001_000), entries: []entry{
			{tx: signQuai(k1, 0, to(a1), 1, []byte{1}), gas: 30000, logs: []*types.Log{lg(a1, "0x01", t1)}},
			{tx: signQuai(k2, 0, to(a2), 2, nil), gas: 40000, logs: []*types.Log{lg(a2, "0x02", t2, t3)}},
		}},
		{name: "b2", parent: "b1", time: 1_700_000_020, coinbase: 0x02, difficulty: u(1_002_000), uncles: []string{"u1", "u2"}, entries: []entry{
			{tx: signQuai(k1, 1, to(a1), 0, []byte{2, 2}), gas: 50000, logs: []*types.Log{lg(a1, "0x0303", t1, t2), lg(a3, "0x")}},
			{tx: signQuai(k2, 1, nil, 0, common.FromHex("600060005360016000f3")), gas: 60000, created: &created},
		}},
		{name: "b3", parent: "b2", time: 1_700_000_030, coinbase: 0x03, difficulty: u(1_003_000), outbound: []*types.Transaction{outbound}, entries: []entry{
			{tx: signQuai(k3, 0, to(a2), 100, nil), gas: 21000, etxs: []*types.Transaction{outbound}},
			{tx: signQuai(k3, 1, to(a2), 0, nil), gas: 25000, failed: true},
		}},
		{name: "b4", parent: "b3", time: 1_700_000_040, coinbase: 0x04, difficulty: u(1_004_000), entries: []entry{
			{tx: incoming, gas: 21000, logs: []*types.Log{lg(a3, "0x0707", t3)}},
			{tx: qiTx, gas: 12000},
			{tx: coinbaseEtx, gas: 0},
		}},
		{name: "b5", parent: "b4", time: 1_700_000_050, coinbase: 0x05, difficulty: dec("120000000000000000")},
		{name: "c4", parent: "b3", time: 1_700_000_041, coinbase: 0x14, difficulty: big1, uncles: []string{"u3"}, outbound: []*types.Transaction{outbound2}, entries: []entry{
			{tx: signQuai(k1, 2, to(a3), 0, []byte{0x15}), gas: 31000, logs: []*types.Log{lg(a3, "0x15", t2)}, etxs: []*types.Transaction{outbound2}},
		}},
		{name: "c5", parent: "c4", time: 1_700_000_051, coinbase: 0x15, difficulty: big1},
	}

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
			r := &types.Receipt{Type: e.tx.Type(), Status: status, CumulativeGasUsed: cum, GasUsed: e.gas, TxHash: e.tx.Hash(), Logs: logs}
			if e.created != nil {
				r.ContractAddress = *e.created
			}
			if e.etxs != nil {
				r.OutboundEtxs = e.etxs
			}
			receipts = append(receipts, r)
		}
		outs := s.outbound
		if outs == nil {
			outs = []*types.Transaction{}
		}
		uncles := []*types.WorkObjectHeader{}
		for _, un := range s.uncles {
			uncles = append(uncles, types.CopyWorkObjectHeader(c.uncles[un].WorkObjectHeader()))
		}
		wo := makeWo(number, parent, s.time, s.coinbase, s.difficulty, root, prime, txs, outs, uncles, receipts)
		read, raw := readBack(db, wo)
		hash := read.Hash()
		b := &built{name: s.name, wo: read}
		if s.parent != "" {
			b.receipts = receipts
		}
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
			rawdb.WriteReceipts(db, hash, number, b.receipts)
			stored = hx(rawdb.ReadReceiptsProto(db, hash, number))
		}
		c.entropy[hash] = new(big.Int).Mul(u(number+1), u(1_000_003))
		b.record = obj{"hash": hash.Hex(), "number": number, "parent": parent.Hex(), "object": hx(raw), "size": uint64(read.Size()), "tx_hashes": txHashes, "etx_hashes": etxHashes, "receipts": stored, "total_entropy": hx(c.entropy[hash].Bytes())}
		if _, dup := c.byHash[hash]; dup {
			panic("duplicate block hash " + s.name)
		}
		c.blocks[s.name] = b
		c.byHash[hash] = b
		c.order = append(c.order, s.name)
	}
	return c
}

// uncleKinds is the oracle's UncleWorkShareClassification.
var uncleKinds = map[string]types.WorkShareValidity{"u1": types.Valid, "u2": types.Block, "u3": types.Invalid}

func (c *chain) uncleKind(h *types.WorkObjectHeader) types.WorkShareValidity {
	for name, u := range c.uncles {
		if u.WorkObjectHeader().Hash() == h.Hash() {
			return uncleKinds[name]
		}
	}
	panic("unknown uncle")
}

func (c *chain) out() obj {
	blocks := obj{}
	for _, n := range c.order {
		blocks[n] = c.blocks[n].record
	}
	kinds := obj{}
	for name, u := range c.uncles {
		kind := map[types.WorkShareValidity]string{types.Valid: "workshare", types.Block: "block", types.Invalid: "other"}[uncleKinds[name]]
		kinds[u.WorkObjectHeader().Hash().Hex()] = kind
	}
	_, primeRaw := readBack(c.db, c.prime)
	return obj{"chain_id": 9, "location": []int{0, 0}, "genesis": "g", "blocks": blocks, "world": c.world, "state_root": c.root.Hex(), "uncle_kinds": kinds, "prime": obj{"hash": c.prime.Hash().Hex(), "number": 0, "object": hx(primeRaw), "size": uint64(c.prime.Size())}}
}

// --- backend ----------------------------------------------------------------

// backend implements what the handlers call, with quai/api_backend.go's
// semantics. The oracle has no miner: the pending block is nil.
type backend struct {
	quaiapi.Backend
	c        *chain
	db       ethdb.Database
	canon    map[uint64]common.Hash
	head     *types.WorkObject
	peers    [2]uint
	fees     []*big.Int
	allocs   []params.GenesisAccount
	forfeit  map[common.AddressBytes]bool
	founders map[common.AddressBytes]bool
}

func (b *backend) NodeCtx() int                                             { return common.ZONE_CTX }
func (b *backend) NodeLocation() common.Location                            { return loc }
func (b *backend) ProcessingState() bool                                    { return true }
func (b *backend) ChainConfig() *params.ChainConfig                         { return &b.c.cfg }
func (b *backend) Logger() *log.Logger                                      { return log.Global }
func (b *backend) Database() ethdb.Database                                 { return b.db }
func (b *backend) ChainDb() ethdb.Database                                  { return b.db }
func (b *backend) RpcVersion() string                                       { return "v1" }
func (b *backend) RPCGasCap() uint64                                        { return params.GasCeil }
func (b *backend) CurrentHeader() *types.WorkObject                         { return b.head }
func (b *backend) CurrentBlock() *types.WorkObject                          { return b.head }
func (b *backend) GenesisAllocs() []params.GenesisAccount                   { return b.allocs }
func (b *backend) ForfeitureAddresses() map[common.AddressBytes]bool        { return b.forfeit }
func (b *backend) FounderForfeitureAddresses() map[common.AddressBytes]bool { return b.founders }
func (b *backend) PeerCount() uint                                          { return b.peers[0] + b.peers[1] }

// GetRollingFeeInfo is the processor's in-memory window of the blocks this
// process executed (core/state_processor.go): nil until it executes a block
// with transactions.
func (b *backend) GetRollingFeeInfo() (min, max, avg *big.Int) {
	if b.fees == nil {
		return nil, nil, nil
	}
	return b.fees[0], b.fees[1], b.fees[2]
}
func (b *backend) PeerCountByDirection() (uint, uint) { return b.peers[0], b.peers[1] }

// lookup is core.GetBlockByHash: any stored block, canonical or not.
func (b *backend) lookup(h common.Hash) *types.WorkObject {
	if blk, ok := b.c.byHash[h]; ok {
		return blk.wo
	}
	if h == b.c.prime.Hash() {
		return b.c.prime
	}
	return nil
}
func (b *backend) GetBlockByHash(h common.Hash) *types.WorkObject         { return b.lookup(h) }
func (b *backend) GetHeaderByHash(h common.Hash) *types.WorkObject        { return b.lookup(h) }
func (b *backend) BlockOrCandidateByHash(h common.Hash) *types.WorkObject { return b.lookup(h) }
func (b *backend) HeaderByHash(_ context.Context, h common.Hash) (*types.WorkObject, error) {
	return b.lookup(h), nil
}
func (b *backend) BlockByHash(_ context.Context, h common.Hash) (*types.WorkObject, error) {
	return b.lookup(h), nil
}
func (b *backend) HeaderByNumber(_ context.Context, n rpc.BlockNumber) (*types.WorkObject, error) {
	switch n {
	case rpc.PendingBlockNumber:
		return nil, nil
	case rpc.LatestBlockNumber:
		return b.head, nil
	}
	if h, ok := b.canon[uint64(n)]; ok {
		return b.lookup(h), nil
	}
	return nil, nil
}
func (b *backend) BlockByNumber(ctx context.Context, n rpc.BlockNumber) (*types.WorkObject, error) {
	if n == rpc.PendingBlockNumber {
		return nil, nil // core.PendingBlock(): no miner
	}
	if n == rpc.LatestBlockNumber {
		n = rpc.BlockNumber(b.head.NumberU64(common.ZONE_CTX))
	}
	if h, ok := b.canon[uint64(n)]; ok {
		return b.lookup(h), nil
	}
	return nil, nil
}
func (b *backend) HeaderByNumberOrHash(ctx context.Context, n rpc.BlockNumberOrHash) (*types.WorkObject, error) {
	if number, ok := n.Number(); ok {
		return b.HeaderByNumber(ctx, number)
	}
	if hash, ok := n.Hash(); ok {
		header := b.lookup(hash)
		if header == nil {
			return nil, errors.New("header for hash not found")
		}
		if n.RequireCanonical && b.canon[header.NumberU64(common.ZONE_CTX)] != hash {
			return nil, errors.New("hash is not currently canonical")
		}
		return header, nil
	}
	return nil, errors.New("invalid arguments; neither block nor hash specified")
}
func (b *backend) BlockByNumberOrHash(ctx context.Context, n rpc.BlockNumberOrHash) (*types.WorkObject, error) {
	if number, ok := n.Number(); ok {
		return b.BlockByNumber(ctx, number)
	}
	return b.HeaderByNumberOrHash(ctx, n)
}
func (b *backend) stateAt(header *types.WorkObject) (*state.StateDB, error) {
	return state.New(header.EVMRoot(), header.EtxSetRoot(), header.QuaiStateSize(), b.c.cache, b.c.cache, nil, loc, log.Global)
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
		header := b.lookup(hash)
		if header == nil {
			return nil, nil, errors.New("header for hash not found")
		}
		if n.RequireCanonical && b.canon[header.NumberU64(common.ZONE_CTX)] != hash {
			return nil, nil, errors.New("hash is not currently canonical")
		}
		st, err := b.stateAt(header)
		return st, header, err
	}
	return nil, nil, errors.New("invalid arguments; neither block nor hash specified")
}

// GetTransaction as quai/api_backend.go over rawdb.ReadTransaction.
func (b *backend) GetTransaction(_ context.Context, h common.Hash) (*types.Transaction, common.Hash, uint64, uint64, error) {
	tx, blockHash, number, index := rawdb.ReadTransaction(b.db, h)
	if tx == nil {
		return nil, common.Hash{}, 0, 0, errors.New("transaction not found")
	}
	return tx, blockHash, number, index, nil
}
func (b *backend) GetPoolTransaction(common.Hash) *types.Transaction { return nil }
func (b *backend) GetPoolNonce(_ context.Context, a common.Address) (uint64, error) {
	st, err := b.stateAt(b.head)
	if err != nil {
		return 0, err
	}
	ia, err := a.InternalAndQuaiAddress()
	if err != nil {
		return 0, nil
	}
	return st.GetNonce(ia), nil
}
func (b *backend) GetReceipts(_ context.Context, h common.Hash) (types.Receipts, error) {
	number := rawdb.ReadHeaderNumber(b.db, h)
	if number == nil {
		return nil, nil
	}
	return rawdb.ReadReceipts(b.db, h, *number, &b.c.cfg), nil
}

// UTXOsByAddress as core.Core.GetUTXOsByAddress over the address index.
func (b *backend) UTXOsByAddress(_ context.Context, address common.Address) ([]*types.UtxoEntry, error) {
	outpoints, err := rawdb.ReadAddressUTXOs(b.db, address.Bytes20())
	if err != nil {
		return nil, err
	}
	utxos := make([]*types.UtxoEntry, 0, len(outpoints))
	for _, o := range outpoints {
		if entry := rawdb.GetUTXO(b.db, o.TxHash, o.Index); entry != nil {
			utxos = append(utxos, entry)
		}
	}
	return utxos, nil
}
func (b *backend) CalcOrder(*types.WorkObject) (*big.Int, int, error) {
	return nil, common.ZONE_CTX, nil
}
func (b *backend) UncleWorkShareClassification(h *types.WorkObjectHeader) types.WorkShareValidity {
	return b.c.uncleKind(h)
}
func (b *backend) TotalLogEntropy(wo *types.WorkObject) *big.Int {
	if e, ok := b.c.entropy[wo.Hash()]; ok {
		return e
	}
	return u(0)
}

// SuggestFinalityDepth as core.Core.SuggestFinalityDepth.
func (b *backend) SuggestFinalityDepth(_ context.Context, qiValue, correlatedRisk *big.Int) (*big.Int, error) {
	head := b.head
	reward := misc.CalculateQiReward(head.WorkObjectHeader(), head.Difficulty())
	return new(big.Int).Div(new(big.Int).Mul(qiValue, correlatedRisk), reward), nil
}

// GetKQuaiAndUpdateBit as core/slice.go for a zone: a stored block gives
// StateProcessor.GetKQuaiAndUpdateBit (params.ExchangeRate and 1 in
// v0.57.0), an unknown hash ErrSubNotSyncedToDom.
func (b *backend) GetKQuaiAndUpdateBit(h common.Hash) (*big.Int, uint8, error) {
	if b.lookup(h) == nil {
		return nil, 0, core.ErrSubNotSyncedToDom
	}
	return new(big.Int).Set(params.ExchangeRate), 1, nil
}

// --- scenarios --------------------------------------------------------------

type request struct {
	method string
	params []interface{}
}

func req(method string, params ...interface{}) request {
	if params == nil {
		params = []interface{}{}
	}
	return request{method, params}
}

type scenario struct {
	name     string
	head     string
	peers    [2]uint
	fees     []*big.Int
	utxoSize map[string]uint64
	requests []request
}

func (c *chain) hash(name string) string { return c.blocks[name].wo.Hash().Hex() }
func (c *chain) tx(block string, i int) string {
	return c.blocks[block].wo.Transactions()[i].Hash().Hex()
}

// ask sends one request through the production JSON-RPC server over HTTP.
func ask(server *rpc.Server, r request) interface{} {
	body, err := json.Marshal(obj{"jsonrpc": "2.0", "id": 1, "method": r.method, "params": r.params})
	must(err)
	hr := httptest.NewRequest("POST", "/", bytes.NewReader(body))
	hr.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	server.ServeHTTP(rec, hr)
	var answer map[string]interface{}
	d := json.NewDecoder(rec.Body)
	d.UseNumber()
	must(d.Decode(&answer))
	out := obj{}
	for _, k := range []string{"result", "error"} {
		if v, ok := answer[k]; ok {
			out[k] = v
		}
	}
	return out
}

func run(c *chain, root string, sc scenario) obj {
	b := &backend{c: c, db: c.db, canon: map[uint64]common.Hash{}, peers: sc.peers, fees: sc.fees}
	var err error
	b.allocs, err = params.VerifyGenesisAllocs(filepath.Join(root, "params/genesis_alloc.json"), params.AllocHash)
	must(err)
	b.forfeit, err = params.LoadForfeitureAddresses(filepath.Join(root, "params/forfeiture_addresses.json"))
	must(err)
	b.founders, err = params.LoadForfeitureAddresses(filepath.Join(root, "params/founder_forfeiture_addresses.json"))
	must(err)
	// The canonical chain: the head and its ancestors. Earlier scenarios'
	// canonical entries and transaction lookups are rewritten.
	canonical := []string{}
	for cur := c.blocks[sc.head]; ; {
		n := cur.wo.NumberU64(common.ZONE_CTX)
		b.canon[n] = cur.wo.Hash()
		canonical = append([]string{cur.name}, canonical...)
		if n == 0 {
			break
		}
		cur = c.byHash[cur.wo.ParentHash(common.ZONE_CTX)]
	}
	for _, name := range c.order {
		wo := c.blocks[name].wo
		for _, t := range wo.Transactions() {
			rawdb.DeleteTxLookupEntry(c.db, t.Hash())
		}
	}
	for _, name := range canonical {
		wo := c.blocks[name].wo
		rawdb.WriteCanonicalHash(c.db, wo.Hash(), wo.NumberU64(common.ZONE_CTX))
		rawdb.WriteTxLookupEntriesByBlock(c.db, wo, common.ZONE_CTX)
	}
	b.head = c.blocks[sc.head].wo
	rawdb.WriteHeadBlockHash(c.db, b.head.Hash())
	sizes := obj{}
	for name, n := range sc.utxoSize {
		rawdb.WriteUTXOSetSize(c.db, c.blocks[name].wo.Hash(), n)
		sizes[name] = n
	}

	server := rpc.NewServer(log.Global)
	defer server.Stop()
	nonceLock := new(quaiapi.AddrLocker)
	// quaiapi.GetAPIs' order for a zone node (the methods of these types).
	must(server.RegisterName("eth", quaiapi.NewPublicQuaiAPI(b)))
	must(server.RegisterName("quai", quaiapi.NewPublicQuaiAPI(b)))
	must(server.RegisterName("eth", quaiapi.NewPublicBlockChainAPI(b)))
	must(server.RegisterName("quai", quaiapi.NewPublicBlockChainQuaiAPI(b)))
	must(server.RegisterName("net", quaiapi.NewPublicNetAPI(c.cfg.ChainID.Uint64(), b)))
	must(server.RegisterName("eth", quaiapi.NewPublicTransactionPoolAPI(b, nonceLock)))
	must(server.RegisterName("quai", quaiapi.NewPublicTransactionPoolAPI(b, nonceLock)))
	must(server.RegisterName("txpool", quaiapi.NewPublicTxPoolAPI(b)))
	calls := []interface{}{}
	for i, r := range sc.requests {
		calls = append(calls, obj{"request": obj{"jsonrpc": "2.0", "id": i + 1, "method": r.method, "params": r.params}, "response": ask(server, r)})
	}
	fees := []interface{}{}
	for _, f := range sc.fees {
		fees = append(fees, hx(f.Bytes()))
	}
	return obj{"name": sc.name, "head": sc.head, "canonical": canonical, "peers": []uint{sc.peers[0], sc.peers[1]}, "rolling_fees": fees, "utxo_set_size": sizes, "calls": calls}
}

// --- RPCMarshalWorkObject ------------------------------------------------------

// auxPow decodes a KAWPOW AuxPow with a 120-byte donor header through the
// production ProtoDecode.
func auxPow(seed string) *types.AuxPow {
	header := make([]byte, 120)
	for i := range header {
		header[i] = byte(i) ^ seed[0]
	}
	id := uint32(types.Kawpow)
	p := &types.ProtoAuxPow{
		ChainId:      &id,
		Header:       header,
		Signature:    crypto.Keccak256([]byte("rpc-parity-oracle/signature/" + seed))[:16],
		MerkleBranch: [][]byte{crypto.Keccak256([]byte("branch/0/" + seed)), crypto.Keccak256([]byte("branch/1/" + seed))},
		Transaction:  append([]byte{1, 0, 0, 0}, crypto.Keccak256([]byte("coinbase/"+seed))...),
	}
	ap := &types.AuxPow{}
	must(ap.ProtoDecode(p))
	return ap
}

// kawpow makes a work object after the KawPow fork, with an AuxPow, share
// counts and targets, and uncles.
func kawpow(c *chain, number uint64, coinbase byte, uncles []*types.WorkObjectHeader) *types.WorkObject {
	wo := makeWo(number, c.blocks["b5"].wo.Hash(), 1_700_000_000+10*number, coinbase, u(3_000_000_000_000), c.root, c.prime.Hash(), nil, nil, uncles, nil)
	wh := wo.WorkObjectHeader()
	wh.SetPrimeTerminusNumber(u(params.KawPowForkBlock + params.KawPowTransitionPeriod + number))
	wh.SetShaDiffAndCount(types.NewPowShareDiffAndCount(dec("167000000000000000"), new(big.Int).Lsh(u(3), 30), u(7)))
	wh.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(dec("5000000000000"), new(big.Int).Lsh(u(1), 29), u(0)))
	wh.SetShaShareTarget(new(big.Int).Lsh(u(1), 200))
	wh.SetScryptShareTarget(u(0))
	wh.SetKawpowDifficulty(dec("2900000000000"))
	wh.SetAuxPow(auxPow(fmt.Sprintf("k%d", number)))
	return wo
}

// marshals records RPCMarshalWorkObject("v1") and ("v2") of blocks before
// and after the KawPow fork, as read from the database.
func marshals(c *chain) []interface{} {
	uncle := kawpow(c, 20, 0x31, nil)
	k, raw := readBack(c.db, kawpow(c, 21, 0x32, []*types.WorkObjectHeader{types.CopyWorkObjectHeader(uncle.WorkObjectHeader())}))
	if k.WorkObjectHeader().AuxPow() == nil || k.Uncles()[0].AuxPow() == nil {
		panic("kawpow AuxPow did not round-trip")
	}
	type item struct {
		name string
		wo   *types.WorkObject
		raw  []byte
	}
	items := []item{{"k", k, raw}}
	for _, name := range []string{"b2", "b4"} {
		b := c.blocks[name]
		_, braw := readBack(c.db, b.wo)
		items = append(items, item{name, b.wo, braw})
	}
	out := []interface{}{}
	for _, it := range items {
		txs := []interface{}{}
		for _, t := range it.wo.Transactions() {
			txs = append(txs, t.Hash().Hex())
		}
		etxs := []interface{}{}
		for _, t := range it.wo.OutboundEtxs() {
			etxs = append(etxs, t.Hash().Hex())
		}
		out = append(out, obj{"name": it.name, "hash": it.wo.Hash().Hex(), "number": it.wo.NumberU64(common.ZONE_CTX),
			"object": hx(it.raw), "size": uint64(it.wo.Size()), "tx_hashes": txs, "etx_hashes": etxs,
			"v1": it.wo.RPCMarshalWorkObject("v1"), "v2": it.wo.RPCMarshalWorkObject("v2")})
	}
	return out
}

// --- lockups and the address index ----------------------------------------

type lockupRecord struct {
	owner, beneficiary common.Address
	lockupByte         byte
	epoch              uint32
	amount             *big.Int
	height             uint32
	elements           uint16
	delegate           common.Address
}

func lockups() []lockupRecord {
	return []lockupRecord{
		{owner, miner, 0, 1, dec("1000000000000000000"), 120, 1, common.Zero},
		{owner, miner, 0, 9, dec("2500000000000000000"), 900, 2, common.Zero},
		{owner, miner, 0, 10, dec("1"), 1000, 1, delegate},
		{owner, miner, 3, 1, dec("777000000000000000000"), 130, 7, delegate},
		{owner, qiMiner, 1, 2, dec("42"), 200, 1, common.Zero},
		{owner, a1, 2, 5, dec("0"), 0, 0, common.Zero},
	}
}

func writeLockups(db ethdb.Database) []interface{} {
	out := []interface{}{}
	for _, l := range lockups() {
		_, err := rawdb.WriteCoinbaseLockup(db, l.owner, l.beneficiary, l.lockupByte, l.epoch, l.amount, l.height, l.elements, l.delegate)
		must(err)
		raw, _ := db.Get(rawdb.CoinbaseLockupKey(l.owner, l.beneficiary, l.lockupByte, l.epoch))
		out = append(out, obj{"owner": hx(l.owner.Bytes()), "beneficiary": hx(l.beneficiary.Bytes()), "lockup_byte": l.lockupByte, "epoch": l.epoch, "record": hx(raw)})
	}
	return out
}

// writeUtxos writes UTXOs and the address index of an indexing node
// (`--node.index-address-utxos`) for the Qi balance reads.
func writeUtxos(db ethdb.Database, owners map[string]key) []interface{} {
	type utxoSpec struct {
		owner string
		index uint16
		denom uint8
		lock  *big.Int
	}
	list := []utxoSpec{{"dave", 0, 3, nil}, {"dave", 1, 5, u(2)}, {"dave", 2, 1, u(1_000)}, {"erin", 0, 7, nil}}
	index := map[[20]byte][]*types.OutpointAndDenomination{}
	out := []interface{}{}
	for i, x := range list {
		h := crypto.Keccak256Hash([]byte(fmt.Sprintf("rpc-parity-oracle/utxo/%d", i)))
		a := owners[x.owner].addr
		entry := types.NewUtxoEntry(types.NewTxOut(x.denom, a.Bytes(), x.lock))
		must(rawdb.CreateUTXO(db, h, x.index, entry))
		index[a.Bytes20()] = append(index[a.Bytes20()], &types.OutpointAndDenomination{TxHash: h, Index: x.index, Denomination: x.denom, Lock: x.lock})
		lock := "0x"
		if x.lock != nil {
			lock = hx(x.lock.Bytes())
		}
		out = append(out, obj{"tx_hash": h.Hex(), "index": x.index, "denomination": x.denom, "address": hx(a.Bytes()), "lock": lock})
	}
	must(rawdb.WriteAddressUTXOs(db, db, index))
	return out
}

func main() {
	root, err := filepath.Abs(os.Args[1])
	must(err)
	log.SetGlobalLogger("", "fatal")
	params.GitCommit = referenceRevision
	c := newChain()
	locks := writeLockups(c.db)
	utxos := writeUtxos(c.db, c.keys)

	k1, k2 := c.keys["alice"], c.keys["bob"]
	qk := c.keys["dave"]
	m := func(a common.Address) string { return a.Hex() }
	lower := func(a common.Address) string { return hx(a.Bytes()) }
	latest, pending, earliest := "latest", "pending", "earliest"
	keys := []interface{}{slot(0).Hex(), slot(7).Hex(), "0x3", "0x" + strings.Repeat("0", 63) + "5", crypto.Keccak256Hash([]byte("rpc-parity-oracle/slot")).Hex(), slot(99).Hex(), "0xzz", ""}

	common1 := []request{
		req("quai_clientVersion"), req("eth_clientVersion"), req("quai_clientVersion", 1),
		req("txpool_getRollingFeeInfo"), req("txpool_getRollingFeeInfo", 1),
		req("net_peerCount"), req("net_peerCountByDirection"),
		// raw transactions by block and index
		req("quai_getRawTransactionByBlockNumberAndIndex", "0x1", "0x0"),
		req("quai_getRawTransactionByBlockNumberAndIndex", "0x1", "0x1"),
		req("quai_getRawTransactionByBlockNumberAndIndex", "0x1", "0x2"),
		req("quai_getRawTransactionByBlockNumberAndIndex", "0x4", "0x1"),
		req("quai_getRawTransactionByBlockNumberAndIndex", "0x4", "0x2"),
		req("quai_getRawTransactionByBlockNumberAndIndex", latest, "0x0"),
		req("quai_getRawTransactionByBlockNumberAndIndex", pending, "0x0"),
		req("quai_getRawTransactionByBlockNumberAndIndex", earliest, "0x0"),
		req("quai_getRawTransactionByBlockNumberAndIndex", "0x63", "0x0"),
		req("quai_getRawTransactionByBlockNumberAndIndex", "0x1"),
		req("quai_getRawTransactionByBlockNumberAndIndex", "0x1", 0),
		req("eth_getRawTransactionByBlockNumberAndIndex", "0x2", "0x1"),
		req("quai_getRawTransactionByBlockHashAndIndex", c.hash("b2"), "0x1"),
		req("quai_getRawTransactionByBlockHashAndIndex", c.hash("c4"), "0x0"),
		req("quai_getRawTransactionByBlockHashAndIndex", c.hash("b3"), "0x5"),
		req("quai_getRawTransactionByBlockHashAndIndex", c.prime.Hash().Hex(), "0x0"),
		req("quai_getRawTransactionByBlockHashAndIndex", crypto.Keccak256Hash([]byte("nope")).Hex(), "0x0"),
		req("eth_getRawTransactionByBlockHashAndIndex", c.hash("b4"), "0x0"),
		req("eth_getRawTransactionByHash", c.tx("b3", 1)),
		// blocks by hash, canonical or not
		req("quai_getBlockOrCandidateByHash", c.hash("b2"), false),
		req("quai_getBlockOrCandidateByHash", c.hash("b2"), true),
		req("quai_getBlockOrCandidateByHash", c.hash("c4"), true),
		req("quai_getBlockOrCandidateByHash", c.hash("b4"), true),
		req("quai_getBlockOrCandidateByHash", c.prime.Hash().Hex(), false),
		req("quai_getBlockOrCandidateByHash", crypto.Keccak256Hash([]byte("nope")).Hex(), true),
		req("quai_getBlockOrCandidateByHash", c.hash("b2")),
		// fees, sizes, quotes
		req("quai_baseFee", true), req("quai_baseFee", false), req("quai_baseFee"), req("quai_baseFee", "true"),
		req("quai_getLatestUTXOSetSize"),
		req("quai_suggestFinalityDepth", "0x0", "0x0"),
		req("quai_suggestFinalityDepth", "0x3e8", "0x2"),
		req("quai_suggestFinalityDepth", "0x8000000000000000", "0x3"),
		req("quai_suggestFinalityDepth", "0xffffffffffffffff", "0xffffffffffffffff"),
		req("quai_suggestFinalityDepth", "0x1"),
		req("quai_hashesPerQits", "0x0", latest),
		req("quai_hashesPerQits", "0x5", latest),
		req("quai_hashesPerQits", "0x5", "0x0"),
		req("quai_hashesPerQits", "0x64", "0x12a05f200"),
		req("quai_hashesPerQits", "0x64", "0x7fffffffffffffff"),
		req("quai_hashesPerQits", "0x64", pending),
		req("quai_hashesPerQits", "0x64", earliest),
		req("quai_hashesPerQits", "0x64", obj{"blockHash": c.hash("b1")}),
		req("quai_hashesPerQits", "-0x5", latest),
		req("quai_hashesPerQits", "0xde0b6b3a7640000", "0x1171c0"),
		req("quai_hashesPerQits", "0xde0b6b3a7640000", "0x1171c1"),
		req("quai_hashesPerQits", "0xde0b6b3a7640000", "0x2e5f080"),
		// supply analytics (no index: zeros less the burn balance)
		req("quai_getSupplyAnalyticsForBlock", latest),
		req("quai_getSupplyAnalyticsForBlock", "0x2"),
		req("quai_getSupplyAnalyticsForBlock", pending),
		req("quai_getSupplyAnalyticsForBlock", "0x63"),
		req("quai_getSupplyAnalyticsForBlock", obj{"blockHash": c.hash("c4")}),
		req("quai_getSupplyAnalyticsForBlock", obj{"blockHash": crypto.Keccak256Hash([]byte("nope")).Hex()}),
		req("quai_rewardAnalytics"),
		req("quai_getKQuaiAndUpdateBit", c.hash("b3")),
		req("quai_getKQuaiAndUpdateBit", c.hash("c4")),
		req("quai_getKQuaiAndUpdateBit", crypto.Keccak256Hash([]byte("nope")).Hex()),
		// contract size
		req("quai_getContractSize", lower(store), latest),
		req("quai_getContractSize", lower(k1.addr), "0x1"),
		req("quai_getContractSize", lower(empty), latest),
		req("quai_getContractSize", lower(qk.addr), latest),
		req("quai_getContractSize", lower(other), latest),
		req("quai_getContractSize", lower(store), pending),
		req("quai_getContractSize", lower(store), "0x63"),
		req("quai_getContractSize", lower(store), obj{"blockHash": c.hash("b1")}),
		req("quai_getContractSize", m(store)[:20], latest),
		// coinbase lockups
		req("quai_getLockupsForContractAndMiner", lower(owner), lower(miner)),
		req("quai_getLockupsForContractAndMiner", lower(owner), lower(qiMiner)),
		req("quai_getLockupsForContractAndMiner", lower(owner), lower(a1)),
		req("quai_getLockupsForContractAndMiner", lower(owner), lower(a2)),
		req("quai_getLockupsForContractAndMiner", lower(qk.addr), lower(miner)),
		req("quai_getLockupsForContractAndMiner", lower(other), lower(miner)),
		req("quai_getLockupsForContractAndMiner", lower(owner), lower(other)),
		// proofs
		req("quai_getProof", lower(store), keys[:5], latest),
		req("quai_getProof", lower(store), keys, latest),
		req("quai_getProof", lower(k1.addr), []interface{}{}, "0x2"),
		req("quai_getProof", lower(k1.addr), keys[:2], latest),
		req("quai_getProof", lower(k2.addr), nil, obj{"blockHash": c.hash("b1")}),
		req("quai_getProof", lower(empty), keys[:2], latest),
		req("quai_getProof", lower(burn), []interface{}{slot(1).Hex()}, latest),
		req("quai_getProof", lower(qk.addr), []interface{}{}, latest),
		req("quai_getProof", lower(other), []interface{}{}, latest),
		req("quai_getProof", lower(store), keys[:1], pending),
		req("quai_getProof", lower(store), keys[:1], "0x63"),
		req("quai_getProof", m(store), keys[:1], latest),
		req("eth_getProof", lower(store), keys[:3], latest),
		req("eth_getProof", lower(empty), keys[:1], latest),
		req("eth_getProof", lower(qk.addr), []interface{}{}, latest),
		// eth: chain and head
		req("eth_chainId"), req("eth_blockNumber"),
		// eth: state reads
		req("eth_getBalance", lower(k1.addr), latest),
		req("eth_getBalance", m(k1.addr), "0x2"),
		req("eth_getBalance", lower(empty), latest),
		req("eth_getBalance", lower(burn), obj{"blockHash": c.hash("b1")}),
		req("eth_getBalance", lower(qk.addr), latest),
		req("eth_getBalance", lower(c.keys["erin"].addr), latest),
		req("eth_getBalance", lower(qiMiner), latest),
		req("eth_getBalance", lower(qx), latest),
		req("eth_getBalance", lower(qk.addr), "0x1"),
		req("eth_getBalance", lower(other), latest),
		req("eth_getBalance", lower(k1.addr), pending),
		req("eth_getBalance", lower(k1.addr), "0x63"),
		req("eth_getCode", lower(store), latest),
		req("eth_getCode", m(store), latest),
		req("eth_getCode", lower(empty), "0x1"),
		req("eth_getCode", lower(qk.addr), latest),
		req("eth_getCode", lower(other), latest),
		req("eth_getCode", lower(store), pending),
		req("eth_getStorageAt", lower(store), "0x0", latest),
		req("eth_getStorageAt", lower(store), slot(7).Hex(), "0x3"),
		req("eth_getStorageAt", lower(store), "0x99", latest),
		req("eth_getStorageAt", lower(empty), "0x0", latest),
		req("eth_getStorageAt", lower(qk.addr), "0x0", latest),
		req("eth_getStorageAt", lower(other), "0x0", latest),
		req("eth_getTransactionCount", m(k1.addr), latest),
		req("eth_getTransactionCount", m(k1.addr), pending),
		// eth: headers and blocks (RPCMarshalETHHeader and RPCMarshalETHBlock)
		req("eth_getHeaderByNumber", "0x2"), req("eth_getHeaderByNumber", latest), req("eth_getHeaderByNumber", pending),
		req("eth_getHeaderByNumber", "0x0"), req("eth_getHeaderByNumber", "0x63"),
		req("eth_getHeaderByHash", c.hash("b4")), req("eth_getHeaderByHash", c.hash("c4")),
		req("eth_getHeaderByHash", c.prime.Hash().Hex()), req("eth_getHeaderByHash", crypto.Keccak256Hash([]byte("nope")).Hex()),
		req("eth_getBlockByNumber", "0x1", false), req("eth_getBlockByNumber", "0x2", true),
		req("eth_getBlockByNumber", "0x3", true), req("eth_getBlockByNumber", "0x4", true),
		req("eth_getBlockByNumber", latest, false), req("eth_getBlockByNumber", pending, true),
		req("eth_getBlockByNumber", "0x63", true), req("eth_getBlockByNumber", "0x0", true),
		req("eth_getBlockByHash", c.hash("b2"), true), req("eth_getBlockByHash", c.hash("c4"), false),
		req("eth_getBlockByHash", crypto.Keccak256Hash([]byte("nope")).Hex(), true),
		req("eth_getBlockByHash", c.hash("b3")),
		// eth: uncles and counts
		req("eth_getUncleByBlockNumberAndIndex", "0x2", "0x0"), req("eth_getUncleByBlockNumberAndIndex", "0x2", "0x1"),
		req("eth_getUncleByBlockNumberAndIndex", "0x2", "0x2"), req("eth_getUncleByBlockNumberAndIndex", "0x63", "0x0"),
		req("eth_getUncleByBlockHashAndIndex", c.hash("c4"), "0x0"), req("eth_getUncleByBlockHashAndIndex", c.hash("b2"), "0x5"),
		req("eth_getUncleByBlockHashAndIndex", crypto.Keccak256Hash([]byte("nope")).Hex(), "0x0"),
		req("eth_getUncleCountByBlockNumber", "0x2"), req("eth_getUncleCountByBlockNumber", "0x63"),
		req("eth_getUncleCountByBlockHash", c.hash("c4")), req("eth_getUncleCountByBlockHash", crypto.Keccak256Hash([]byte("nope")).Hex()),
		req("eth_getBlockTransactionCountByNumber", "0x4"), req("eth_getBlockTransactionCountByHash", c.hash("b2")),
		// eth: transactions and receipts
		req("eth_getTransactionByHash", c.tx("b1", 1)), req("eth_getTransactionByHash", c.tx("b4", 0)),
		req("eth_getTransactionByHash", c.tx("b4", 1)), req("eth_getTransactionByHash", crypto.Keccak256Hash([]byte("nope")).Hex()),
		req("eth_getTransactionByBlockNumberAndIndex", "0x3", "0x0"),
		req("eth_getTransactionByBlockHashAndIndex", c.hash("b2"), "0x1", []int{0, 0}),
		req("eth_getTransactionReceipt", c.tx("b1", 0)), req("eth_getTransactionReceipt", c.tx("b2", 1)),
		req("eth_getTransactionReceipt", c.tx("b3", 0)), req("eth_getTransactionReceipt", c.tx("b3", 1)),
		req("eth_getTransactionReceipt", c.tx("b4", 0)), req("eth_getTransactionReceipt", c.tx("b4", 1)),
		req("eth_getTransactionReceipt", c.tx("b4", 2)),
		req("eth_getTransactionReceipt", crypto.Keccak256Hash([]byte("nope")).Hex()),
		req("quai_getTransactionReceipt", c.tx("b4", 2)),
	}
	scenarios := []scenario{
		{name: "head-b5", head: "b5", peers: [2]uint{0, 0}, utxoSize: map[string]uint64{"b5": 1_234_567}, requests: common1},
		{name: "head-c5", head: "c5", peers: [2]uint{3, 5}, fees: []*big.Int{u(1), new(big.Int).Lsh(u(1), 70), u(12345)}, utxoSize: map[string]uint64{"c5": 2}, requests: append([]request{
			req("quai_getBlockOrCandidateByHash", c.hash("b4"), false),
			req("quai_getBlockOrCandidateByHash", c.hash("c5"), true),
			req("eth_getTransactionReceipt", c.tx("c4", 0)),
			req("eth_getTransactionByHash", c.tx("b4", 0)),
			req("quai_getRawTransactionByBlockNumberAndIndex", "0x4", "0x0"),
			req("eth_getBlockByNumber", "0x4", true),
			req("eth_getBlockByNumber", "0x5", true),
		}, common1...)},
	}
	out := []interface{}{}
	for _, sc := range scenarios {
		out = append(out, run(c, root, sc))
	}
	result := obj{
		"reference_revision": referenceRevision,
		"scope": "go-quai v0.57.0 production quaiapi handlers (PublicQuaiAPI, PublicBlockChainAPI, " +
			"PublicBlockChainQuaiAPI, PublicTransactionPoolAPI, PublicNetAPI) behind rpc.Server over HTTP, " +
			"over a rawdb memory database: blocks, receipts, a side branch, one committed state, a prime " +
			"terminus, UTXO set sizes, the address UTXO index and coinbase lockup records",
		"chain":     c.out(),
		"marshal":   marshals(c),
		"lockups":   locks,
		"utxos":     utxos,
		"scenarios": out,
	}
	encoded, err := json.MarshalIndent(result, "", " ")
	must(err)
	os.Stdout.Write(append(encoded, '\n'))
}
