// Offline native coincident admission from explicit synthetic checkpoint inputs.
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
	"math/big"
	"strings"
)

func coinSeal(h *HeaderChain, w *types.WorkObject, wanted int) common.Hash {
	w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
	for n := uint64(0); n < 1000000; n++ {
		w.WorkObjectHeader().SetNonce(types.EncodeNonce(n))
		mix, pow := h.Engine(w.WorkObjectHeader()).ComputePowLight(w.WorkObjectHeader())
		w.WorkObjectHeader().SetMixHash(mix)
		if pow.Big().Cmp(new(big.Int).Div(common.Big2e256, w.Difficulty())) > 0 {
			continue
		}
		_, order, e := h.CalcOrder(w)
		actMust(e)
		if order == wanted {
			return pow
		}
	}
	panic("coincident nonce search exhausted")
}

func coinCheckViews(chains []*HeaderChain, w *types.WorkObject, wanted int, manifests []types.BlockManifest, links common.Hashes) []interface{} {
	out := []interface{}{}
	for c := wanted; c < 3; c++ {
		m := manifests[2]
		if c == 0 {
			m = manifests[1]
		}
		view := confView(w, c, m, links)
		if c == 2 {
			view = confView(w, 2, nil, nil)
		}
		actMust(chains[c].AppendHeader(view))
		actMust(NewBlockValidator(chains[c].config, chains[c], chains[c].engine).ValidateBody(view))
		out = append(out, map[string]interface{}{"context": c, "work_object": actWire(view)})
	}
	return out
}
func etxBranch(branch int) map[string]interface{} {
	engine := progpow.New(params.PowConfig{CachesInMem: 2}, nil, false, log.Global)
	chains := []*HeaderChain{nativeAdmissionHC(0, engine), nativeAdmissionHC(1, engine), nativeAdmissionHC(2, engine)}
	for _, h := range chains {
		h.config.ChainID = actU(1337)
		h.headerDb = confDatabase{h.headerDb}
		h.bc = NewTestBodyDb(h.headerDb)
		h.bc.chainConfig = h.config
		h.numberCache, _ = lru.New[common.Hash, uint64](64)
		senders, _ := lru.New[common.Hash, common.InternalAddress](64)
		h.pool = &TxPool{senders: senders, signer: types.NewSigner(actU(1337), common.Location{0, 0})}
		h.pendingEtxs, _ = lru.New[common.Hash, types.PendingEtxs](64)
		h.pendingEtxsRollup, _ = lru.New[common.Hash, types.PendingEtxsRollup](64)
		h.subRollupCache, _ = lru.New[common.Hash, types.Transactions](64)
		h.fetchPrimeBlock = func(id common.Hash) *types.WorkObject { return chains[0].GetBlockByHash(id) }
		defer h.headerDb.Close()
	}
	genesis := (&Genesis{Config: chains[2].config, Timestamp: 1769112000, ExtraData: []byte("quai-rust-admission-v1"), GasLimit: 30000000, Difficulty: actU(2)}).ToBlock(0)
	for c, h := range chains {
		h.AddGenesisHash(genesis.Hash())
		confStore(h, genesis)
		t := types.EmptyTermini()
		for i := 0; i < 16; i++ {
			t.SetDomTerminiAtIndex(genesis.Hash(), i)
			t.SetSubTerminiAtIndex(genesis.Hash(), i)
		}
		rawdb.WriteTermini(h.headerDb, genesis.Hash(), t)
		h.CalculateManifest(genesis)
		if c == 0 {
			_, e := h.CalculateInterlink(genesis)
			actMust(e)
		}
	}
	actMust(chains[1].AddPendingEtxs(types.PendingEtxs{Header: genesis.ConvertToPEtxView(), OutboundEtxs: types.Transactions{}}))
	rawdb.WritePendingEtxsRollup(chains[0].headerDb, types.PendingEtxsRollup{Header: genesis.ConvertToPEtxView(), EtxsRollup: types.Transactions{}})
	hc := chains[2]
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
	initialQueue := types.Transactions{queued}
    for _, kind := range []uint64{types.ConversionType, types.CoinbaseLockupType, types.WrappingQiType, types.ConversionRevertType, types.UnwrapQiType} {
        recipient := owner
        from := sender
        data := []byte{}
        if kind == types.ConversionType { recipient = sender; from = owner }
        if kind == types.WrappingQiType { recipient = sender; data = sender.Bytes() }
        next := types.NewTx(&types.ExternalTx{OriginatingTxHash: common.Hash{30: byte(kind), 31: 88}, ETXIndex: uint16(kind), Gas: 500000, To: &recipient, Value: actU(100006), Data: data, AccessList: types.AccessList{}, Sender: from, EtxType: kind})
        initialQueue = append(initialQueue, next)
    }
    actMust(initial.PushETXs(initialQueue))
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
		actMust(chains[1].AddPendingEtxs(types.PendingEtxs{Header: w.ConvertToPEtxView(), OutboundEtxs: types.Transactions{}}))
		parent = w
	}
	processor := &StateProcessor{config: hc.config, hc: hc, stateCache: cache, etxCache: cache, logger: log.Global}

	parents := []*types.WorkObject{genesis, genesis, parent}
	orders := []int{2, 2, 1, 2, 0, 2, 2, 1, 2, 0, 2, 1, 2, 2, 0, 2}
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
	for pos, wanted := range orders {
		i := uint64(pos)
		n := params.TimeToStartTx + 1 + i
		parent = parents[2]
		for _, depth := range params.LockupByteToBlockDepth {
			if n <= depth {
				continue
			}
			height := n - depth
			if !knownHistory[height] {
				old := types.CopyWorkObject(genesis)
				old.SetNumber(actU(height), 2)
				old.Header().SetExtra([]byte(fmt.Sprintf("coincident-empty-history-%d", height)))
				old.WorkObjectHeader().SetHeaderHash(old.Header().Hash())
				actStore(hc, old)
				knownHistory[height] = true
				historicalRows = append(historicalRows, map[string]interface{}{"height": height, "work_object": actWire(old), "incoming": []string{}, "created": []interface{}{}})
			}
		}
		w := types.EmptyWorkObject(2)
		w.WorkObjectHeader().SetLocation(common.Location{0, 0})
		w.WorkObjectHeader().SetTime(parents[2].Time() + 5)
		w.WorkObjectHeader().SetDifficulty(chains[2].CalcDifficulty(parents[2].WorkObjectHeader(), 0))
		w.WorkObjectHeader().SetData([]byte{0})
		w.WorkObjectHeader().SetPrimaryCoinbase(sender)
		w.WorkObjectHeader().SetShaDiffAndCount(types.NewPowShareDiffAndCount(nil, nil, nil))
		w.WorkObjectHeader().SetScryptDiffAndCount(types.NewPowShareDiffAndCount(nil, nil, nil))
		w.WorkObjectHeader().SetShaShareTarget(nil)
		w.WorkObjectHeader().SetScryptShareTarget(nil)
		w.WorkObjectHeader().SetKawpowDifficulty(nil)
		for c, h := range chains {
			p := parents[c]
			w.SetParentHash(p.Hash(), c)
			w.SetNumber(new(big.Int).Add(p.Number(c), actU(1)), c)
			w.Header().SetParentEntropy(h.TotalLogEntropy(p), c)
			_, po, e := h.CalcOrder(p)
			actMust(e)
			if c > 0 && po == c {
				w.Header().SetParentDeltaEntropy(h.DeltaLogEntropy(p), c)
				w.Header().SetParentUncledDeltaEntropy(h.UncledDeltaLogEntropy(p), c)
			}
		}
		w.Header().SetPrimeTerminusHash(parents[0].Hash())
		w.WorkObjectHeader().SetPrimeTerminusNumber(parents[0].Number(0))
		w.Header().SetGasLimit(CalcGasLimit(parents[2], 30000000))
		w.Header().SetStateLimit(misc.CalcStateLimit(parents[2], params.StateCeil))
		w.Header().SetBaseFee(chains[2].CalcBaseFee(parents[2]))
		w.Header().SetPrimeStateRoot(types.EmptyRootHash)
		w.Header().SetRegionStateRoot(types.EmptyRootHash)
		score := uint16(0)
		if parents[0].NumberU64(0) > 0 {
			var e error
			score, e = chains[0].ComputeEfficiencyScore(parents[0])
			actMust(e)
		}
		w.Header().SetEfficiencyScore(score)
		w.Header().SetThresholdCount(0)
		bits := parents[0].EtxEligibleSlices()
		if !chains[0].IsGenesisHash(parents[0].Hash()) {
			bits = chains[0].UpdateEtxEligibleSlices(parents[0], parents[0].Location())
		}
		w.Header().SetEtxEligibleSlices(bits)
		w.Header().SetMinerDifficulty(chains[0].ComputeMinerDifficulty(parents[0]))
		w.Header().SetExchangeRate(params.ExchangeRate)
		w.Header().SetKQuaiDiscount(params.StartingKQuaiDiscount)
		w.Header().SetConversionFlowAmount(params.StartingConversionFlowAmount)
		manifests := []types.BlockManifest{nil, chains[1].CalculateManifest(parents[1]), chains[2].CalculateManifest(parents[2])}
		for c := 1; c < 3; c++ {
			w.Header().SetManifestHash(types.DeriveSha(manifests[c], trie.NewStackTrie(nil)), c)
		}
		links := rawdb.ReadInterlinkHashes(chains[0].headerDb, parents[0].Hash())
		w.Header().SetInterlinkRootHash(types.DeriveSha(links, trie.NewStackTrie(nil)))
		rollup := types.Transactions{}
		for _, id := range manifests[2] {
			p, e := chains[1].GetPendingEtxs(id)
			actMust(e)
			rollup = append(rollup, p.OutboundEtxs...)
		}
		cross := confCross(rollup)
		w.Header().SetEtxRollupHash(types.DeriveSha(cross, trie.NewStackTrie(nil)))
		inbound := rawdb.ReadInboundEtxs(chains[2].headerDb, parents[2].Hash())
		value := i
		if branch == 1 && i >= 8 {
			value += 100
		}
		data := actU(value).FillBytes(make([]byte, 32))
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
			txs = append(txs, initialQueue...)
		}
		txs = append(txs, inbound...)
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
				pow := coinSeal(hc, bad, wanted)
				parts := coinCheckViews(chains, bad, wanted, manifests, links)
				_, _, _, _, _, _, _, _, _, err := processor.Process(bad, hc.headerDb.NewBatch())
				if err == nil {
					panic("bad activation transaction accepted: " + variant.name)
				}
				negatives = append(negatives, map[string]interface{}{"id": variant.name, "parent_height": parent.NumberU64(2), "work_object": actWire(confView(bad, 2, nil, nil)), "contexts": parts, "order": wanted, "work_hash": pow.Hex(), "header_body_accepted": true, "accepted": false, "rejection_stage": "execution", "error": err.Error()})
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
		pow := coinSeal(hc, w, wanted)
		views := make([]*types.WorkObject, 3)
		contexts := []interface{}{}
		claim := common.Hash{}
		var zoneTerms types.Termini
		for c := wanted; c < 3; c++ {
			m := manifests[2]
			if c == 0 {
				m = manifests[1]
			}
			views[c] = confView(w, c, m, links)
			if c == 2 {
				views[c] = confView(w, 2, nil, nil)
			}
			h := chains[c]
			actMust(h.AppendHeader(views[c]))
			validator := NewBlockValidator(h.config, h, h.engine)
			actMust(validator.ValidateBody(views[c]))
			if c == 2 {
				actMust(validator.ValidateState(w, db, receipts, emitted, nextSet, usedGas, usedState))
			}
			sl := &Slice{hc: h, sliceDb: h.headerDb, logger: log.Global}
			b := h.headerDb.NewBatch()
			ret, t, e := sl.pcrc(b, w, claim, wanted < c)
			actMust(e)
			actMust(b.Write())
			if c == 2 {
				zoneTerms = t
			}
			claim = ret
			contexts = append(contexts, map[string]interface{}{"context": c, "work_object": actWire(views[c]), "returned_dominant": ret.Hex(), "termini": map[string]interface{}{"dom": t.DomTermini(), "sub": t.SubTermini()}})
		}
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
			badPow := coinSeal(hc, bad, wanted)
			parts := coinCheckViews(chains, bad, wanted, manifests, links)
			err := NewBlockValidator(hc.config, hc, hc.engine).ValidateState(bad, db, receipts, emitted, nextSet, usedGas, usedState)
			stage := "state-commitment"
			if kind == "rollup" {
				pe := types.PendingEtxsRollup{Header: bad.ConvertToPEtxView(), EtxsRollup: cross}
				if pe.IsValid(trie.NewStackTrie(nil)) {
					panic("forged activation rollup accepted")
				}
				err = fmt.Errorf("actual PendingEtxsRollup.IsValid rejected forged commitment")
				stage = "rollup"
			}

			if err == nil {
				panic("forged activation commitment accepted: " + kind)
			}
			negatives = append(negatives, map[string]interface{}{"id": fmt.Sprintf("%s-%d", kind, n), "parent_height": parent.NumberU64(2), "work_object": actWire(confView(bad, 2, nil, nil)), "contexts": parts, "order": wanted, "work_hash": badPow.Hex(), "header_body_accepted": true, "accepted": false, "rejection_stage": stage, "error": err.Error()})
		}

		if wanted < 2 {
			parts := []interface{}{}
			rejected := false
			for c := wanted; c < 3; c++ {
				m := manifests[2]
				if c == 0 {
					m = manifests[1]
				}
				view := confView(w, c, m, links)
				if c == 2 {
					view = confView(w, 2, nil, nil)
				}
				if c == wanted {
					view.Body().SetManifest(types.BlockManifest{common.Hash{31: 123}})
				}
				actMust(chains[c].AppendHeader(view))
				err := NewBlockValidator(chains[c].config, chains[c], chains[c].engine).ValidateBody(view)
				if c == wanted {
					if err == nil {
						panic("forged context manifest accepted")
					}
					rejected = true
				} else {
					actMust(err)
				}
				parts = append(parts, map[string]interface{}{"context": c, "work_object": actWire(view)})
			}
			if !rejected {
				panic("context mutation did not reject")
			}
			negatives = append(negatives, map[string]interface{}{"id": fmt.Sprintf("context-manifest-%d", n), "parent_height": parent.NumberU64(2), "work_object": actWire(confView(w, 2, nil, nil)), "work_hash": pow.Hex(), "contexts": parts, "order": wanted, "header_body_accepted": false, "accepted": false, "rejection_stage": "context-body", "error": "actual ValidateBody rejected context manifest"})
		}
		confirmed := types.Transactions{}
		if wanted == 0 {
			sl := &Slice{hc: chains[0], sliceDb: chains[0].headerDb, logger: log.Global}
			confirmed, e = sl.CollectNewlyConfirmedEtxs(views[0], 0)
			actMust(e)
			confirmed = confirmed.FilterToSub(w.Location(), 1, 0)
		} else if wanted == 1 {
			sl := &Slice{hc: chains[1], sliceDb: chains[1].headerDb, logger: log.Global}
			confirmed, e = sl.CollectNewlyConfirmedEtxs(views[1], 1)
			actMust(e)
		}
		rawdb.WriteInboundEtxs(chains[2].headerDb, w.Hash(), confirmed)
		for c := wanted; c < 3; c++ {
			h := chains[c]
			confStore(h, views[c])
			h.CalculateManifest(views[c])
			if c == 0 {
				_, e = h.CalculateInterlink(views[c])
				actMust(e)
			}
			parents[c] = views[c]
		}
		actMust(chains[1].AddPendingEtxs(types.PendingEtxs{Header: w.ConvertToPEtxView(), OutboundEtxs: emitted}))
		if wanted <= 1 {
			pe := types.PendingEtxsRollup{Header: w.ConvertToPEtxView(), EtxsRollup: cross}
			if !pe.IsValid(trie.NewStackTrie(nil)) {
				panic("rollup not committed")
			}
			rawdb.WritePendingEtxsRollup(chains[0].headerDb, pe)
		}
		rawdb.WriteMultiSet(chains[2].headerDb, w.Hash(), nextSet)
		rawdb.WriteUTXOSetSize(chains[2].headerDb, w.Hash(), utxoSize)

		rr := []string{}
		for j := range receipts {
			var b bytes.Buffer
			receipts.EncodeIndex(j, &b)
			rr = append(rr, actHex(b.Bytes()))
		}
		rows = append(rows, map[string]interface{}{"id": fmt.Sprintf("etx-family-%d-%d", branch, n), "height": n, "order": wanted, "work_object": actWire(confView(w, 2, nil, nil)), "contexts": contexts, "block_hash": w.Hash().Hex(), "work_hash": pow.Hex(), "gas_used": usedGas, "state_used": usedState, "receipt_rlp": rr, "evm_root": root.Hex(), "etx_root": etxRoot.Hex(), "utxo_root": nextSet.Hash().Hex(), "utxo_set_size": utxoSize, "quai_state_size": db.GetQuaiTrieSize().Uint64(), "outbound_count": len(emitted), "cross_rollup": ebTxs(cross), "inbound": ebTxs(inbound), "confirmed": ebTxs(confirmed), "unlock_count": len(unlocks), "manifest": rawdb.ReadManifest(hc.headerDb, w.Hash()), "returned_dominant": claim.Hex(), "termini": map[string]interface{}{"dom": zoneTerms.DomTermini(), "sub": zoneTerms.SubTermini()}})
	}
	return map[string]interface{}{"scope": "Native coincident zone/region/prime AppendHeader, ValidateBody, PCRC and committed rollup collection, plus signed Quai/Qi plus standard, conversion, coinbase-lockup, wrapping, conversion-revert, unwrapping and derived coinbase ETX Process/ValidateState, on a conditional post-activation branch. Synthetic checkpoint and explicit historical slices remain trusted unadmitted inputs; no genesis-to-checkpoint or canonical authority.", "genesis": actWire(genesis), "checkpoint_height": params.TimeToStartTx, "checkpoint_records": seed, "checkpoint_snapshots": snapshots, "initial_world": []interface{}{map[string]interface{}{"address": actHex(sender.Bytes()), "balance": actHex(balance.Bytes()), "nonce": 0, "code": "0x", "storage_size": "0x", "storage": []interface{}{}}, map[string]interface{}{"address": actHex(lockAddr.Bytes()), "balance": "0x", "nonce": 1, "code": "0x", "storage_size": "0x", "storage": []interface{}{}}}, "initial_etxs": ebTxs(initialQueue), "initial_utxos": []interface{}{map[string]interface{}{"hash": point.TxHash.Hex(), "index": point.Index, "denomination": entry.Denomination, "address": actHex(owner.Bytes()), "lock": "0x"}}, "historical_slices": historicalRows, "vectors": rows, "branch": branch, "negatives": negatives, "now": 1769113000, "validated_from_genesis": false, "canonical_authority": false, "public_network": false}
}

func NativeETXFamilyActivation() map[string]interface{} {
	a := etxBranch(0)
	b := etxBranch(1)
	return map[string]interface{}{"branches": []interface{}{a, b}, "scope": a["scope"], "now": 1769113000, "checkpoint_height": params.TimeToStartTx, "validated_from_genesis": false, "canonical_authority": false, "public_network": false}
}
