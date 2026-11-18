// Offline native admission from an explicit synthetic activation checkpoint.
// Checkpoint ancestry/state and historical empty slices are supplied test inputs.
package core

import (
	"bytes"
	"encoding/hex"
	"fmt"
	"github.com/btcsuite/btcd/btcec/v2"
	"github.com/btcsuite/btcd/btcec/v2/schnorr"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus/misc"
	"github.com/dominant-strategies/go-quai/consensus/progpow"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/core/vm"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/crypto/multiset"
	"github.com/dominant-strategies/go-quai/ethdb"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/dominant-strategies/go-quai/trie"
	lru "github.com/hashicorp/golang-lru/v2"
	"google.golang.org/protobuf/proto"
	"math/big"
	"strings"
)

func actU(n uint64) *big.Int { return new(big.Int).SetUint64(n) }
func actMust(e error) {
	if e != nil {
		panic(e)
	}
}
func actHex(b []byte) string { return "0x" + hex.EncodeToString(b) }
func actWire(w *types.WorkObject) string {
	p, e := w.ProtoEncode(types.BlockObject)
	actMust(e)
	b, e := proto.Marshal(p)
	actMust(e)
	return actHex(b)
}
func actTx(t *types.Transaction) string {
	p, e := t.ProtoEncode()
	actMust(e)
	b, e := proto.Marshal(p)
	actMust(e)
	return actHex(b)
}

type actDatabase struct{ ethdb.Database }

func (d actDatabase) Location() common.Location { return common.Location{0, 0} }
func actStore(h *HeaderChain, w *types.WorkObject) {
	if rawdb.ReadTermini(h.headerDb, w.Hash()) == nil {
		rawdb.WriteTermini(h.headerDb, w.Hash(), types.EmptyTermini())
	}
	h.headerCache.Add(w.Hash(), *w)
	h.bc.WriteBlock(w, 2)
	rawdb.WriteCanonicalHash(h.headerDb, w.Hash(), w.NumberU64(2))
}
func actSeal(h *HeaderChain, w *types.WorkObject) common.Hash {
	w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
	for n := uint64(0); n < 65536; n++ {
		w.WorkObjectHeader().SetNonce(types.EncodeNonce(n))
		mix, pow := h.Engine(w.WorkObjectHeader()).ComputePowLight(w.WorkObjectHeader())
		w.WorkObjectHeader().SetMixHash(mix)
		if pow.Big().Cmp(new(big.Int).Div(common.Big2e256, w.Difficulty())) > 0 {
			continue
		}
		_, order, e := h.CalcOrder(w)
		actMust(e)
		if order == 2 {
			return pow
		}
	}
	panic("activation nonce search exhausted")
}
func actHeader(h *HeaderChain, parent, genesis *types.WorkObject, number uint64, sender common.Address) *types.WorkObject {
	w := types.EmptyWorkObject(2)
	w.SetNumber(actU(number), 2)
	w.SetParentHash(parent.Hash(), 2)
	w.WorkObjectHeader().SetLocation(common.Location{0, 0})
	w.WorkObjectHeader().SetTime(parent.Time() + 5)
	w.WorkObjectHeader().SetDifficulty(h.CalcDifficulty(parent.WorkObjectHeader(), 0))
	w.WorkObjectHeader().SetPrimaryCoinbase(sender)
	w.WorkObjectHeader().SetData([]byte{0})
	w.WorkObjectHeader().SetPrimeTerminusNumber(actU(0))
	w.Header().SetPrimeTerminusHash(genesis.Hash())
	w.Header().SetGasLimit(CalcGasLimit(parent, 30000000))
	w.Header().SetStateLimit(misc.CalcStateLimit(parent, params.StateCeil))
	w.Header().SetBaseFee(h.CalcBaseFee(parent))
	w.Header().SetAvgTxFees(actU(0))
	w.Header().SetTotalFees(actU(0))
	w.Header().SetExchangeRate(params.ExchangeRate)
	w.Header().SetKQuaiDiscount(params.StartingKQuaiDiscount)
	w.Header().SetConversionFlowAmount(params.StartingConversionFlowAmount)
	w.Header().SetPrimeStateRoot(types.EmptyRootHash)
	w.Header().SetRegionStateRoot(types.EmptyRootHash)
	w.Header().SetMinerDifficulty(genesis.Difficulty())
	w.Header().SetEtxEligibleSlices(genesis.EtxEligibleSlices())
	w.Header().SetParentEntropy(h.TotalLogEntropy(parent), 2)
	_, order, e := h.CalcOrder(parent)
	actMust(e)
	if order == 2 {
		w.Header().SetParentDeltaEntropy(h.DeltaLogEntropy(parent), 2)
		w.Header().SetParentUncledDeltaEntropy(h.UncledDeltaLogEntropy(parent), 2)
	}
	manifest := h.CalculateManifest(parent)
	w.Header().SetManifestHash(types.DeriveSha(manifest, trie.NewStackTrie(nil)), 2)
	rollup := types.Transactions{}
	for _, id := range manifest {
		prior := h.GetBlockByHash(id)
		if prior == nil {
			panic("missing checkpoint rollup block")
		}
		for _, tx := range prior.OutboundEtxs() {
			if tx.To().Location().Region() != 0 || types.IsCoinBaseTx(tx) || types.IsConversionTx(tx) {
				rollup = append(rollup, tx)
			}
		}
	}
	w.Header().SetEtxRollupHash(types.DeriveSha(rollup, trie.NewStackTrie(nil)))
	w.WorkObjectHeader().SetShaDiffAndCount(types.NewPowShareDiffAndCount(nil, nil, nil))
	w.WorkObjectHeader().SetScryptDiffAndCount(types.NewPowShareDiffAndCount(nil, nil, nil))
	w.WorkObjectHeader().SetShaShareTarget(nil)
	w.WorkObjectHeader().SetScryptShareTarget(nil)
	w.WorkObjectHeader().SetKawpowDifficulty(nil)
	return w
}

func NativeActivationAdmission() map[string]interface{} {
	engine := progpow.New(params.PowConfig{CachesInMem: 2}, nil, false, log.Global)
	hc := nativeAdmissionHC(2, engine)
	hc.headerDb = actDatabase{hc.headerDb}
	defer hc.headerDb.Close()
	hc.config.ChainID = actU(1337)
	hc.bc = NewTestBodyDb(hc.headerDb)
	hc.bc.chainConfig = hc.config
	hc.numberCache, _ = lru.New[common.Hash, uint64](512)
	senders, _ := lru.New[common.Hash, common.InternalAddress](64)
	hc.pool = &TxPool{senders: senders, signer: types.NewSigner(actU(1337), common.Location{0, 0})}
	hc.fetchPrimeBlock = func(id common.Hash) *types.WorkObject { return hc.GetBlockByHash(id) }
	genesis := (&Genesis{Config: hc.config, Timestamp: 1769112000, ExtraData: []byte("quai-rust-admission-v1"), GasLimit: 30000000, Difficulty: actU(2)}).ToBlock(0)
	hc.AddGenesisHash(genesis.Hash())
	actStore(hc, genesis)
	terms := types.EmptyTermini()
	for i := 0; i < 16; i++ {
		terms.SetDomTerminiAtIndex(genesis.Hash(), i)
		terms.SetSubTerminiAtIndex(genesis.Hash(), i)
	}
	rawdb.WriteTermini(hc.headerDb, genesis.Hash(), terms)
	hc.CalculateManifest(genesis)
	var quaiKey *btcec.PrivateKey
	var sender common.Address
	for n := uint64(1); ; n++ {
		k, _ := btcec.PrivKeyFromBytes(actU(n).FillBytes(make([]byte, 32)))
		a := crypto.PubkeyBytesToAddress(k.PubKey().SerializeUncompressed(), common.Location{0, 0})
		if _, e := a.InternalAndQuaiAddress(); e == nil {
			quaiKey = k
			sender = a
			break
		}
	}
	var qiKey *btcec.PrivateKey
	var owner common.Address
	for n := uint64(1); ; n++ {
		k, _ := btcec.PrivKeyFromBytes(actU(n).FillBytes(make([]byte, 32)))
		a := crypto.PubkeyBytesToAddress(k.PubKey().SerializeUncompressed(), common.Location{0, 0})
		if a.IsInQiLedgerScope() && a.Location().Equal(common.Location{0, 0}) {
			qiKey = k
			owner = a
			break
		}
	}
	vm.PrecompiledContracts = make(map[common.AddressBytes]vm.PrecompiledContract)
	vm.PrecompiledAddresses = make(map[string][]common.Address)
	vm.InitializePrecompiles(common.Location{0, 0})
	cache := state.NewDatabase(hc.headerDb)
	initial, e := state.New(types.EmptyRootHash, types.EmptyRootHash, actU(0), cache, cache, nil, common.Location{0, 0}, log.Global)
	actMust(e)
	ia, e := sender.InternalAndQuaiAddress()
	actMust(e)
	balance := new(big.Int).Exp(actU(10), actU(30), nil)
	initial.SetBalance(ia, balance)
	lockAddr := common.BytesToAddress(append(make([]byte, 19), byte(10)), common.Location{0, 0})
	la, e := lockAddr.InternalAndQuaiAddress()
	actMust(e)
	initial.SetNonce(la, 1)
	to := sender
	queued := types.NewTx(&types.ExternalTx{OriginatingTxHash: common.Hash{31: 77}, ETXIndex: 0, Gas: 100000, To: &to, Value: new(big.Int).Exp(actU(10), actU(24), nil), Data: []byte{}, AccessList: types.AccessList{}, Sender: sender, EtxType: types.DefaultType})
	actMust(initial.PushETXs(types.Transactions{queued}))
	root, e := initial.Commit(true)
	actMust(e)
	actMust(cache.TrieDB().Commit(root, false, nil))
	etxRoot, e := initial.CommitEtxs()
	actMust(e)
	actMust(cache.TrieDB().Commit(etxRoot, false, nil))
	size := initial.GetQuaiTrieSize()
	point := types.OutPoint{TxHash: common.Hash{31: 99}, Index: 7}
	entry := &types.UtxoEntry{Denomination: 14, Address: owner.Bytes(), Lock: actU(0)}
	actMust(rawdb.CreateUTXO(hc.headerDb, point.TxHash, point.Index, entry))
	set := multiset.New()
	set.Add(types.UTXOHash(point.TxHash, point.Index, entry).Bytes())
	parent := genesis
	seed := []interface{}{}
	snapshots := []interface{}{}
	for n := params.TimeToStartTx - 3; n <= params.TimeToStartTx; n++ {
		w := actHeader(hc, parent, genesis, n, sender)
		w.Header().SetEVMRoot(root)
		w.Header().SetEtxSetRoot(etxRoot)
		w.Header().SetUTXORoot(set.Hash())
		w.Header().SetQuaiStateSize(size)
		actSeal(hc, w)
		b := hc.headerDb.NewBatch()
		sl := &Slice{hc: hc, sliceDb: hc.headerDb, logger: log.Global}
		_, t, e := sl.pcrc(b, w, common.Hash{}, false)
		actMust(e)
		actMust(b.Write())
		actStore(hc, w)
		hc.CalculateManifest(w)
		manifest := rawdb.ReadManifest(hc.headerDb, w.Hash())
		rawdb.WriteMultiSet(hc.headerDb, w.Hash(), set)
		rawdb.WriteUTXOSetSize(hc.headerDb, w.Hash(), 1)
		seed = append(seed, actWire(w))
		snapshots = append(snapshots, map[string]interface{}{"hash": w.Hash().Hex(), "manifest": manifest, "termini": map[string]interface{}{"dom": t.DomTermini(), "sub": t.SubTermini()}})
		parent = w
	}
	processor := &StateProcessor{config: hc.config, hc: hc, stateCache: cache, etxCache: cache, logger: log.Global}
	rows := []interface{}{}
	negatives := []interface{}{}
	historicalRows := []interface{}{}
	knownHistory := map[uint64]bool{}
	initCode, _ := hex.DecodeString("6007600c60003960076000f360003560005500")
	contract := crypto.CreateAddress(sender, 0, initCode, common.Location{0, 0})
	if _, e := contract.InternalAndQuaiAddress(); e != nil {
		contract, _, e = vm.GrindContract(sender, 0, 3000000, 30+6*int64((len(initCode)+31)/32), crypto.Keccak256Hash(initCode), size, common.Location{0, 0})
		actMust(e)
	}
	for i := uint64(0); i < 4; i++ {
		n := params.TimeToStartTx + 1 + i
		for _, depth := range params.LockupByteToBlockDepth {
			if n <= depth {
				continue
			}
			height := n - depth
			if !knownHistory[height] {
				old := types.CopyWorkObject(genesis)
				old.SetNumber(actU(height), 2)
				old.Header().SetExtra([]byte(fmt.Sprintf("activation-empty-history-%d", height)))
				old.WorkObjectHeader().SetHeaderHash(old.Header().Hash())
				actStore(hc, old)
				knownHistory[height] = true
				historicalRows = append(historicalRows, map[string]interface{}{"height": height, "work_object": actWire(old), "incoming": []string{}, "created": []interface{}{}})
			}
		}
		w := actHeader(hc, parent, genesis, n, sender)
		data := actU(i).FillBytes(make([]byte, 32))
		dest := &contract
		gas := uint64(300000)
		if i == 0 {
			data = initCode
			dest = nil
			gas = 3000000
		}
		ecdsaKey, e := crypto.ToECDSA(quaiKey.Serialize())
		actMust(e)
		tx := types.NewTx(&types.QuaiTx{ChainID: actU(1337), Nonce: i, GasPrice: w.BaseFee(), Gas: gas, To: dest, Value: actU(0), Data: data, AccessList: types.AccessList{{Address: contract, StorageKeys: []common.Hash{{}}}}})
		tx, e = types.SignTx(tx, types.NewSigner(actU(1337), common.Location{0, 0}), ecdsaKey)
		actMust(e)
		txs := types.Transactions{}
		if i == 0 {
			out := owner.Bytes()
			out[19] ^= 1
			q := &types.QiTx{ChainID: actU(1337), TxIn: types.TxIns{{PreviousOutPoint: point, PubKey: qiKey.PubKey().SerializeUncompressed()}}, TxOut: types.TxOuts{{Denomination: 13, Address: out, Lock: actU(0)}}, Signature: new(schnorr.Signature)}
			qt := types.NewTx(q)
			digest, e := types.NewSigner(actU(1337), common.Location{0, 0}).Hash(qt)
			actMust(e)
			q.Signature, e = schnorr.Sign(qiKey, digest[:], schnorr.CustomNonce([32]byte{}))
			actMust(e)
			txs = append(txs, types.NewTx(q))
		}
		txs = append(txs, tx)
		if i == 0 {
			txs = append(txs, queued)
		}
		w.Body().SetTransactions(txs)
		w.Header().SetTxHash(types.DeriveSha(txs, trie.NewStackTrie(nil)))
		w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
		var receipts types.Receipts
		var emitted []*types.Transaction
		var db *state.StateDB
		var usedGas, usedState, utxoSize uint64
		var nextSet *multiset.MultiSet
		var unlocks []common.Unlock
		var batch ethdb.Batch
		assembled := false
		for attempt := 0; attempt < 4; attempt++ {
			batch = hc.headerDb.NewBatch()
			receipts, emitted, _, db, usedGas, usedState, utxoSize, nextSet, unlocks, e = processor.Process(w, batch)
			if e == nil {
				assembled = true
				break
			}
			msg := e.Error()
			var remote, local string
			if strings.HasPrefix(msg, "invalid avgTxFees used") {
				_, e = fmt.Sscanf(msg, "invalid avgTxFees used (remote: %s local: %s", &remote, &local)
				actMust(e)
				v, ok := new(big.Int).SetString(strings.TrimSuffix(local, ")"), 10)
				if !ok {
					panic(msg)
				}
				w.Header().SetAvgTxFees(v)
			} else if strings.HasPrefix(msg, "invalid totalFees used") {
				_, e = fmt.Sscanf(msg, "invalid totalFees used (remote: %s local: %s", &remote, &local)
				actMust(e)
				v, ok := new(big.Int).SetString(strings.TrimSuffix(local, ")"), 10)
				if !ok {
					panic(msg)
				}
				w.Header().SetTotalFees(v)
			} else {
				panic(e)
			}
			w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
		}
		if !assembled {
			panic("fee assembly exhausted")
		}

		if i == 0 {
			badNonce := types.NewTx(&types.QuaiTx{ChainID: actU(1337), Nonce: i + 1, GasPrice: w.BaseFee(), Gas: gas, To: dest, Value: actU(0), Data: data, AccessList: types.AccessList{{Address: contract, StorageKeys: []common.Hash{{}}}}})
			badNonce, e = types.SignTx(badNonce, types.NewSigner(actU(1337), common.Location{0, 0}), ecdsaKey)
			actMust(e)
			q := txs[0]
			badSig := types.NewTx(&types.QiTx{ChainID: actU(1337), TxIn: q.TxIn(), TxOut: q.TxOut(), Data: q.Data(), Signature: new(schnorr.Signature)})
			wrongEtx := types.NewTx(&types.ExternalTx{OriginatingTxHash: common.Hash{31: 78}, ETXIndex: 0, Gas: 100000, To: &to, Value: new(big.Int).Exp(actU(10), actU(24), nil), Data: []byte{}, AccessList: types.AccessList{}, Sender: sender, EtxType: types.DefaultType})
			variants := []struct {
				name string
				txs  types.Transactions
			}{{"signed-quai-bad-nonce-after-qi", types.Transactions{q, badNonce, queued}}, {"qi-bad-signature", types.Transactions{badSig, tx, queued}}, {"qi-double-spend", types.Transactions{q, q, tx, queued}}, {"fee-order", types.Transactions{tx, q, queued}}, {"wrong-queued-etx-after-qi-quai", types.Transactions{q, tx, wrongEtx}}}
			for _, variant := range variants {
				bad := types.CopyWorkObject(w)
				bad.Body().SetTransactions(variant.txs)
				bad.Header().SetTxHash(types.DeriveSha(variant.txs, trie.NewStackTrie(nil)))
				pow := actSeal(hc, bad)
				actMust(hc.AppendHeader(bad))
				actMust(NewBlockValidator(hc.config, hc, hc.engine).ValidateBody(bad))
				_, _, _, _, _, _, _, _, _, err := processor.Process(bad, hc.headerDb.NewBatch())
				if err == nil {
					panic("bad activation transaction accepted: " + variant.name)
				}
				negatives = append(negatives, map[string]interface{}{"id": variant.name, "parent_height": parent.NumberU64(2), "work_object": actWire(bad), "work_hash": pow.Hex(), "header_body_accepted": true, "accepted": false, "rejection_stage": "execution", "error": err.Error()})
			}
		}
		actMust(batch.Write())
		root, e = db.Commit(true)
		actMust(e)
		actMust(cache.TrieDB().Commit(root, false, nil))
		etxRoot, e = db.CommitEtxs()
		actMust(e)
		actMust(cache.TrieDB().Commit(etxRoot, false, nil))
		w.Header().SetEVMRoot(root)
		w.Header().SetEtxSetRoot(etxRoot)
		w.Header().SetUTXORoot(nextSet.Hash())
		w.Header().SetQuaiStateSize(db.GetQuaiTrieSize())
		w.Header().SetGasUsed(usedGas)
		w.Header().SetStateUsed(usedState)
		w.Header().SetReceiptHash(types.DeriveSha(receipts, trie.NewStackTrie(nil)))
		w.Header().SetOutboundEtxHash(types.DeriveSha(types.Transactions(emitted), trie.NewStackTrie(nil)))
		w.Body().SetOutboundEtxs(emitted)
		pow := actSeal(hc, w)
		actMust(hc.AppendHeader(w))
		validator := NewBlockValidator(hc.config, hc, hc.engine)
		actMust(validator.ValidateBody(w))
		committedRollup := types.Transactions{}
		for _, id := range hc.CalculateManifest(parent) {
			prior := hc.GetBlockByHash(id)
			for _, tx := range prior.OutboundEtxs() {
				if tx.To().Location().Region() != 0 || types.IsCoinBaseTx(tx) || types.IsConversionTx(tx) {
					committedRollup = append(committedRollup, tx)
				}
			}
		}
		if !(&types.PendingEtxsRollup{Header: w.ConvertToPEtxView(), EtxsRollup: committedRollup}).IsValid(trie.NewStackTrie(nil)) {
			panic("activation rollup commitment mismatch")
		}
		derived, e := (&Slice{hc: hc, sliceDb: hc.headerDb, logger: log.Global}).CollectNewlyConfirmedEtxs(w, 2)
		actMust(e)
		if len(derived) != 0 {
			panic("local block invented newly confirmed ETXs")
		}

		actMust(validator.ValidateState(w, db, receipts, emitted, nextSet, usedGas, usedState))

		for _, kind := range []string{"gas", "state", "evm", "etx", "utxo", "receipt", "rollup"} {
			bad := types.CopyWorkObject(w)
			switch kind {
			case "gas":
				bad.Header().SetGasUsed(usedGas + 1)
			case "state":
				bad.Header().SetStateUsed(usedState + 1)
			case "evm":
				bad.Header().SetEVMRoot(common.Hash{31: 123})
			case "etx":
				bad.Header().SetEtxSetRoot(common.Hash{31: 123})
			case "utxo":
				bad.Header().SetUTXORoot(common.Hash{31: 123})
			case "receipt":
				bad.Header().SetReceiptHash(common.Hash{31: 123})
			case "rollup":
				bad.Header().SetEtxRollupHash(common.Hash{31: 123})
			}
			badPow := actSeal(hc, bad)
			actMust(hc.AppendHeader(bad))
			actMust(validator.ValidateBody(bad))
			err := validator.ValidateState(bad, db, receipts, emitted, nextSet, usedGas, usedState)
			stage := "state-commitment"
			if kind == "rollup" {
				pe := types.PendingEtxsRollup{Header: bad.ConvertToPEtxView(), EtxsRollup: committedRollup}
				if pe.IsValid(trie.NewStackTrie(nil)) {
					panic("forged activation rollup accepted")
				}
				err = fmt.Errorf("actual PendingEtxsRollup.IsValid rejected forged commitment")
				stage = "rollup"
			}

			if err == nil {
				panic("forged activation commitment accepted: " + kind)
			}
			negatives = append(negatives, map[string]interface{}{"id": fmt.Sprintf("%s-%d", kind, n), "parent_height": parent.NumberU64(2), "work_object": actWire(bad), "work_hash": badPow.Hex(), "header_body_accepted": true, "accepted": false, "rejection_stage": stage, "error": err.Error()})
		}
		sl := &Slice{hc: hc, sliceDb: hc.headerDb, logger: log.Global}
		b := hc.headerDb.NewBatch()
		ret, terms, e := sl.pcrc(b, w, common.Hash{}, false)
		actMust(e)
		actMust(b.Write())
		actStore(hc, w)
		hc.CalculateManifest(w)
		manifest := rawdb.ReadManifest(hc.headerDb, w.Hash())
		rawdb.WriteMultiSet(hc.headerDb, w.Hash(), nextSet)
		rawdb.WriteUTXOSetSize(hc.headerDb, w.Hash(), utxoSize)
		rr := []string{}
		for j := range receipts {
			var b bytes.Buffer
			receipts.EncodeIndex(j, &b)
			rr = append(rr, actHex(b.Bytes()))
		}
		rows = append(rows, map[string]interface{}{"id": fmt.Sprintf("activation-%d", n), "height": n, "work_object": actWire(w), "block_hash": w.Hash().Hex(), "work_hash": pow.Hex(), "gas_used": usedGas, "state_used": usedState, "receipt_rlp": rr, "evm_root": root.Hex(), "etx_root": etxRoot.Hex(), "utxo_root": nextSet.Hash().Hex(), "utxo_set_size": utxoSize, "quai_state_size": db.GetQuaiTrieSize().Uint64(), "outbound_count": len(emitted), "cross_rollup": func() []string {
			out := []string{}
			for _, tx := range committedRollup {
				out = append(out, actTx(tx))
			}
			return out
		}(), "confirmed": []string{}, "unlock_count": len(unlocks), "manifest": manifest, "returned_dominant": ret.Hex(), "termini": map[string]interface{}{"dom": terms.DomTermini(), "sub": terms.SubTermini()}})
		parent = w
	}
	return map[string]interface{}{"scope": "Actual native local-zone AppendHeader, ValidateBody, StateProcessor.Process, ValidateState and PCRC on four signed post-activation blocks, starting from an explicit synthetic checkpoint and explicit empty historical slices. Real ProgPoW, Quai CREATE/SSTORE, Qi ownership/spend and standard ETX execution. Supplied checkpoint ancestry/state/historical slices are trusted inputs, not validated genesis-to-checkpoint history or canonical authority.", "genesis": actWire(genesis), "checkpoint_height": params.TimeToStartTx, "checkpoint_records": seed, "checkpoint_snapshots": snapshots, "initial_world": []interface{}{map[string]interface{}{"address": actHex(sender.Bytes()), "balance": actHex(balance.Bytes()), "nonce": 0, "code": "0x", "storage_size": "0x", "storage": []interface{}{}}, map[string]interface{}{"address": actHex(lockAddr.Bytes()), "balance": "0x", "nonce": 1, "code": "0x", "storage_size": "0x", "storage": []interface{}{}}}, "initial_etxs": []string{actTx(queued)}, "initial_utxos": []interface{}{map[string]interface{}{"hash": point.TxHash.Hex(), "index": point.Index, "denomination": entry.Denomination, "address": actHex(owner.Bytes()), "lock": "0x"}}, "historical_slices": historicalRows, "vectors": rows, "negatives": negatives, "now": 1769113000, "validated_from_genesis": false, "canonical_authority": false, "public_network": false}
}
