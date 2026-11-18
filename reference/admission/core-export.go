// Explicit offline oracle through native proof, header/manifest/body/state checks.
// Qualified scope is a pre-transaction local zone segment; higher-order admission is excluded.
package core

import (
	"bytes"
	"encoding/hex"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus/misc"
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

	hc.headerCache.Add(w.Hash(), *w)
	hc.bc.WriteBlock(w, 2)

}

type zoneLocatedDatabase struct{ ethdb.Database }

func (db zoneLocatedDatabase) Location() common.Location { return common.Location{0, 0} }

func nativeAdmissionBranch(branch uint64) map[string]interface{} {
	engine := progpow.New(params.PowConfig{CachesInMem: 2}, nil, false, log.Global)
	hc := nativeAdmissionHC(2, engine)
	hc.headerDb = zoneLocatedDatabase{hc.headerDb}
	defer hc.headerDb.Close()
	hc.config.ChainID = ebU(1337)
	hc.bc = NewTestBodyDb(hc.headerDb)
	hc.bc.chainConfig = hc.config
	hc.numberCache, _ = lru.New[common.Hash, uint64](32)
	senders, _ := lru.New[common.Hash, common.InternalAddress](32)
	hc.pool = &TxPool{senders: senders, signer: types.NewSigner(ebU(1337), common.Location{0, 0})}
	g := &Genesis{Config: hc.config, Timestamp: 1769112000, ExtraData: []byte("quai-rust-admission-v1"), GasLimit: 30000000, Difficulty: ebU(2)}
	parent := g.ToBlock(0)
	hc.AddGenesisHash(parent.Hash())
	ebKeep(hc, parent)
	genesis := parent
	termini := types.EmptyTermini()
	for i := 0; i < 16; i++ {
		termini.SetDomTerminiAtIndex(parent.Hash(), i)
		termini.SetSubTerminiAtIndex(parent.Hash(), i)
	}
	rawdb.WriteTermini(hc.headerDb, parent.Hash(), termini)
	hc.CalculateManifest(parent)
	sl := &Slice{hc: hc, config: hc.config, sliceDb: hc.headerDb, logger: hc.logger}
	validator := NewBlockValidator(hc.config, hc, hc.engine)
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
	rows := []interface{}{}
	negatives := []interface{}{}
	now := int64(1769112500)
	genesisWire := ebWire(parent)
	for number := uint64(1); number <= 16; number++ {
		block := types.EmptyWorkObject(common.ZONE_CTX)
		block.WorkObjectHeader().SetShaDiffAndCount(types.NewPowShareDiffAndCount(nil, nil, nil))
		block.WorkObjectHeader().SetScryptDiffAndCount(types.NewPowShareDiffAndCount(nil, nil, nil))
		block.WorkObjectHeader().SetShaShareTarget(nil)
		block.WorkObjectHeader().SetScryptShareTarget(nil)
		block.WorkObjectHeader().SetKawpowDifficulty(nil)
		block.WorkObjectHeader().SetLocation(common.Location{0, 0})
		block.SetNumber(ebU(number), 2)
		block.SetParentHash(parent.Hash(), 2)
		block.WorkObjectHeader().SetTime(parent.Time() + 5)
		block.WorkObjectHeader().SetDifficulty(hc.CalcDifficulty(parent.WorkObjectHeader(), parent.ExpansionNumber()))
		block.WorkObjectHeader().SetPrimeTerminusNumber(ebU(0))
		block.Header().SetPrimeTerminusHash(genesis.Hash())
		block.Header().SetGasLimit(CalcGasLimit(parent, 30000000))
		block.Header().SetStateLimit(misc.CalcStateLimit(parent, params.StateCeil))
		block.Header().SetBaseFee(hc.CalcBaseFee(parent))
		block.Header().SetAvgTxFees(ebU(0))
		block.Header().SetTotalFees(ebU(0))
		block.WorkObjectHeader().SetData([]byte{0})
		block.WorkObjectHeader().SetPrimaryCoinbase(sender)
		if branch == 1 && number > 8 {
			block.Header().SetExtra([]byte("alternate"))
		}
		block.Header().SetParentEntropy(hc.TotalLogEntropy(parent), 2)
		_, parentOrder, err := hc.CalcOrder(parent)
		ebMust(err)
		if parentOrder == common.ZONE_CTX {
			block.Header().SetParentDeltaEntropy(hc.DeltaLogEntropy(parent), 2)
			block.Header().SetParentUncledDeltaEntropy(hc.UncledDeltaLogEntropy(parent), 2)
		}
		block.Header().SetManifestHash(types.DeriveSha(hc.CalculateManifest(parent), trie.NewStackTrie(nil)), 2)
		inbound, err := sl.CollectNewlyConfirmedEtxs(block, common.ZONE_CTX)
		ebMust(err)
		if len(inbound) != 0 {
			panic("local zone cannot derive new inbound ETXs")
		}
		txs := types.Transactions{}
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
			if work[0] >= 64 && work[0] < 128 {
				found = true
				break
			}
		}
		if !found {
			panic("private fixture work search exhausted")
		}
		block.WorkObjectHeader().SetMixHash(mix)
		_, order, err := hc.CalcOrder(block)
		ebMust(err)
		if order != common.ZONE_CTX {
			panic("nonlocal candidate")
		}
		ebMust(hc.AppendHeader(block))
		ebMust(validator.ValidateBody(block))
		ebMust(validator.ValidateState(block, db, receipts, etxs, set, gasUsed, stateUsed))
		pcrcBatch := hc.headerDb.NewBatch()
		_, nextTermini, err := sl.pcrc(pcrcBatch, block, common.Hash{}, false)
		ebMust(err)
		ebMust(pcrcBatch.Write())
		if number == 9 && branch == 0 {
			negatives = admissionNegatives(hc, validator, engine, block, parent, processor, now)
		}
		_ = nextTermini
		rawdb.WriteMultiSet(hc.headerDb, block.Hash(), set)
		rawdb.WriteUTXOSetSize(hc.headerDb, block.Hash(), utxoSize)
		ebKeep(hc, block)
		hc.CalculateManifest(block)
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
		rows = append(rows, map[string]interface{}{"order": order, "header_valid": true, "body_valid": true, "state_valid": true, "manifest": hc.CalculateManifest(block), "termini": map[string]interface{}{"dom": nextTermini.DomTermini(), "sub": nextTermini.SubTermini()}, "work_hash": work.Hex(), "inbound": inboundWire, "id": fmt.Sprintf("private-zone-%d", number), "work_object": ebWire(block), "block_hash": block.Hash().Hex(), "evm_root": root.Hex(), "etx_root": etxRoot.Hex(), "utxo_root": set.Hash().Hex(), "utxo_set_size": utxoSize, "quai_state_size": db.GetQuaiTrieSize().Uint64(), "gas_used": gasUsed, "state_used": stateUsed, "receipt_rlp": receiptBytes, "receipt_root": block.ReceiptHash().Hex(), "outbound_count": len(etxs), "log_count": len(logs), "unlock_count": len(unlocks), "total_fees": ebHex(block.TotalFees().Bytes()), "average_fees": ebHex(block.AvgTxFees().Bytes())})
		parent = block
	}
	return map[string]interface{}{"genesis": genesisWire, "genesis_hash": genesis.Hash(), "genesis_body_hash": genesis.Header().Hash(), "vectors": rows, "negatives": negatives, "now": now, "scope": "Actual StateProcessor.Process on sixteen linked empty-allocation private blocks with signed CREATE/SSTORE, reward-depth payouts and incoming coinbase ETXs, all execution commitments and real ProgPoW kernels. Header/hierarchy admission and public network are excluded."}
}

func NativeAdmissionPipeline() map[string]interface{} {
	a := nativeAdmissionBranch(0)
	b := nativeAdmissionBranch(1)
	return map[string]interface{}{"genesis": a["genesis"], "genesis_hash": a["genesis_hash"], "genesis_body_hash": a["genesis_body_hash"], "branches": []interface{}{map[string]interface{}{"name": "main", "vectors": a["vectors"]}, map[string]interface{}{"name": "alternate", "vectors": b["vectors"]}}, "negatives": a["negatives"], "now": a["now"], "fork_height": 8, "scope": "Actual native ProgPoW, CalcOrder, AppendHeader, ValidateBody/ValidateState, PCRC, manifest and empty newly-confirmed zone-ETX collection on two local-zone pre-transaction branches. No fake engine or injected order cache. Full higher-order hierarchy, workshare/fork histories, PoEM selection and public sync remain unqualified."}
}
