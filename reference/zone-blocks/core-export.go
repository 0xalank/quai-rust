// Whole multi-block execution oracle through actual StateProcessor.Process and native ProgPoW work.
// Header proof/order admission is outside this execution-only corpus.
package core

import (
	"bytes"
	"encoding/hex"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/consensus/progpow"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/core/vm"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/ethdb"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/dominant-strategies/go-quai/trie"
	lru "github.com/hashicorp/golang-lru/v2"
	"google.golang.org/protobuf/proto"
	"math/big"
)

func ebU(n uint64) *big.Int { return new(big.Int).SetUint64(n) }
func ebHex(b []byte) string { return "0x" + hex.EncodeToString(b) }
func ebMust(e error) {
	if e != nil {
		panic(e)
	}
}
func ebWire(w *types.WorkObject) string {
	p, e := w.ProtoEncode(types.BlockObject)
	ebMust(e)
	b, e := proto.Marshal(p)
	ebMust(e)
	return ebHex(b)
}
func ebKeep(hc *HeaderChain, w *types.WorkObject) {
	rawdb.WriteTermini(hc.headerDb, w.Hash(), types.EmptyTermini())
	hc.headerCache.Add(w.Hash(), *w)
	hc.bc.WriteBlock(w, 2)
	hc.AddToCalcOrderCache(w.Hash(), 0, ebU(1))
}

type zoneLocatedDatabase struct{ ethdb.Database }

func (db zoneLocatedDatabase) Location() common.Location { return common.Location{0, 0} }

func NativeZoneBlocks() map[string]interface{} {
	hc := nativeHC(2)
	hc.headerDb = zoneLocatedDatabase{hc.headerDb}
	engine := progpow.New(params.PowConfig{CachesInMem: 2}, nil, false, log.Global)
	hc.engine = []consensus.Engine{engine, nativeProjectionEngine{}}
	defer hc.headerDb.Close()
	hc.config.ChainID = ebU(1337)
	hc.bc = NewTestBodyDb(hc.headerDb)
	hc.bc.chainConfig = hc.config
	hc.numberCache, _ = lru.New[common.Hash, uint64](32)
	senders, _ := lru.New[common.Hash, common.InternalAddress](32)
	hc.pool = &TxPool{senders: senders, signer: types.NewSigner(ebU(1337), common.Location{0, 0})}
	g := &Genesis{Config: hc.config, Timestamp: 1769112000, ExtraData: []byte("quai-rust-private-v1"), GasLimit: 30000000, Difficulty: ebU(1)}
	parent := g.ToBlock(0)
	hc.AddGenesisHash(parent.Hash())
	ebKeep(hc, parent)
	vm.PrecompiledContracts = make(map[common.AddressBytes]vm.PrecompiledContract)
	vm.PrecompiledAddresses = make(map[string][]common.Address)
	vm.InitializePrecompiles(common.Location{0, 0})
	cache := state.NewDatabase(hc.headerDb)
	processor := &StateProcessor{config: hc.config, hc: hc, stateCache: cache, etxCache: cache, logger: hc.logger}
	var keyBytes []byte
	for n := uint64(1); ; n++ {
		k, e := crypto.ToECDSA(ebU(n).FillBytes(make([]byte, 32)))
		ebMust(e)
		a := crypto.PubkeyToAddress(k.PublicKey, common.Location{0, 0})
		if _, e = a.InternalAndQuaiAddress(); e == nil {
			keyBytes = crypto.FromECDSA(k)
			break
		}
	}
	key, e := crypto.ToECDSA(keyBytes)
	ebMust(e)
	sender := crypto.PubkeyToAddress(key.PublicKey, common.Location{0, 0})
	initCode, _ := hex.DecodeString("6007600c60003960076000f360003560005500")
	contract := crypto.CreateAddress(sender, 0, initCode, common.Location{0, 0})
	if _, e := contract.InternalAndQuaiAddress(); e != nil {
		contract, _, e = vm.GrindContract(sender, 0, 3000000, 30+6*int64((len(initCode)+31)/32), crypto.Keccak256Hash(initCode), ebU(1), common.Location{0, 0})
		ebMust(e)
	}
	rows := []interface{}{}
	genesisWire := ebWire(parent)
	for number := uint64(1); number <= 16; number++ {
		block := types.EmptyWorkObject(common.ZONE_CTX)
		block.WorkObjectHeader().SetLocation(common.Location{0, 0})
		block.SetNumber(ebU(number), 2)
		block.SetParentHash(parent.Hash(), 2)
		block.WorkObjectHeader().SetTime(parent.Time() + 5)
		block.WorkObjectHeader().SetDifficulty(ebU(1))
		block.WorkObjectHeader().SetPrimeTerminusNumber(ebU(0))
		block.Header().SetPrimeTerminusHash(parent.Hash())
		block.Header().SetGasLimit(30000000)
		block.Header().SetStateLimit(5000000)
		block.Header().SetBaseFee(ebU(0))
		block.Header().SetAvgTxFees(ebU(0))
		block.Header().SetTotalFees(ebU(0))
		block.WorkObjectHeader().SetData([]byte{0})
		block.WorkObjectHeader().SetPrimaryCoinbase(sender)
		data := ebU(number - 1).FillBytes(make([]byte, 32))
		to := &contract
		gas := uint64(300000)
		access := types.AccessList{{Address: contract, StorageKeys: []common.Hash{{}}}}
		if number == 1 {
			data = initCode
			to = nil
			gas = 3000000
		}
		tx := types.NewTx(&types.QuaiTx{ChainID: ebU(1337), Nonce: number - 1, GasPrice: ebU(0), Gas: gas, To: to, Value: ebU(0), Data: data, AccessList: access})
		tx, e = types.SignTx(tx, types.NewSigner(ebU(1337), common.Location{0, 0}), key)
		ebMust(e)
		inbound := rawdb.ReadInboundEtxs(hc.headerDb, parent.Hash())
		txs := types.Transactions{tx}
		txs = append(txs, inbound...)
		block.Body().SetTransactions(txs)
		block.Header().SetTxHash(types.DeriveSha(block.Transactions(), trie.NewStackTrie(nil)))
		block.WorkObjectHeader().SetHeaderHash(block.Header().Hash())
		batch := hc.headerDb.NewBatch()
		receipts, etxs, logs, db, gasUsed, stateUsed, utxoSize, set, unlocks, e := processor.Process(block, batch)
		ebMust(e)
		ebMust(batch.Write())
		root, e := db.Commit(true)
		ebMust(e)
		ebMust(cache.TrieDB().Commit(root, false, nil))
		etxRoot, e := db.CommitEtxs()
		ebMust(e)
		ebMust(cache.TrieDB().Commit(etxRoot, false, nil))
		block.Header().SetEVMRoot(root)
		block.Header().SetEtxSetRoot(etxRoot)
		block.Header().SetUTXORoot(set.Hash())
		block.Header().SetQuaiStateSize(db.GetQuaiTrieSize())
		block.Header().SetGasUsed(gasUsed)
		block.Header().SetStateUsed(stateUsed)
		block.Header().SetReceiptHash(types.DeriveSha(receipts, trie.NewStackTrie(nil)))
		block.Header().SetOutboundEtxHash(types.DeriveSha(types.Transactions(etxs), trie.NewStackTrie(nil)))
		block.Body().SetOutboundEtxs(etxs)
		block.WorkObjectHeader().SetHeaderHash(block.Header().Hash())
		var mix, work common.Hash
		found := false
		for nonce := uint64(0); nonce < 4096; nonce++ {
			block.WorkObjectHeader().SetNonce(types.EncodeNonce(nonce))
			mix, work = engine.ComputePowLight(block.WorkObjectHeader())
			if work[0] < 128 {
				found = true
				break
			}
		}
		if !found {
			panic("private fixture work search exhausted")
		}
		block.WorkObjectHeader().SetMixHash(mix)
		rawdb.WriteMultiSet(hc.headerDb, block.Hash(), set)
		rawdb.WriteUTXOSetSize(hc.headerDb, block.Hash(), utxoSize)
		ebKeep(hc, block)
		rawdb.WriteInboundEtxs(hc.headerDb, block.Hash(), types.Transactions(etxs))
		receiptBytes := []string{}
		for i := range receipts {
			var b bytes.Buffer
			receipts.EncodeIndex(i, &b)
			receiptBytes = append(receiptBytes, ebHex(b.Bytes()))
		}
		inboundWire := []string{}
		for _, item := range inbound {
			p, e := item.ProtoEncode()
			ebMust(e)
			b, e := proto.Marshal(p)
			ebMust(e)
			inboundWire = append(inboundWire, ebHex(b))
		}
		rows = append(rows, map[string]interface{}{"work_hash": work.Hex(), "inbound": inboundWire, "id": fmt.Sprintf("private-zone-%d", number), "work_object": ebWire(block), "block_hash": block.Hash().Hex(), "evm_root": root.Hex(), "etx_root": etxRoot.Hex(), "utxo_root": set.Hash().Hex(), "utxo_set_size": utxoSize, "quai_state_size": db.GetQuaiTrieSize().Uint64(), "gas_used": gasUsed, "state_used": stateUsed, "receipt_rlp": receiptBytes, "receipt_root": block.ReceiptHash().Hex(), "outbound_count": len(etxs), "log_count": len(logs), "unlock_count": len(unlocks), "total_fees": ebHex(block.TotalFees().Bytes()), "average_fees": ebHex(block.AvgTxFees().Bytes())})
		parent = block
	}
	return map[string]interface{}{"genesis": genesisWire, "vectors": rows, "scope": "Actual StateProcessor.Process on sixteen linked empty-allocation private blocks with signed CREATE/SSTORE, reward-depth payouts and incoming coinbase ETXs, all execution commitments and real ProgPoW kernels. Header/hierarchy admission and public network are excluded."}
}
