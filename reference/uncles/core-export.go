// Explicit native hierarchy/rollup/execution oracle on disposable private chains.
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
func ebMust(e error) {
	if e != nil {
		panic(e)
	}
}
func ebHex(b []byte) string { return "0x" + hex.EncodeToString(b) }
func ebWire(w *types.WorkObject) string {
	p, e := w.ProtoEncode(types.BlockObject)
	ebMust(e)
	b, e := proto.Marshal(p)
	ebMust(e)
	return ebHex(b)
}
func ebTxs(txs types.Transactions) []string {
	out := []string{}
	for _, tx := range txs {
		p, e := tx.ProtoEncode()
		ebMust(e)
		b, e := proto.Marshal(p)
		ebMust(e)
		out = append(out, ebHex(b))
	}
	return out
}

type confDatabase struct{ ethdb.Database }

func (db confDatabase) Location() common.Location { return common.Location{0, 0} }
func confStore(h *HeaderChain, w *types.WorkObject) {
	h.headerCache.Add(w.Hash(), *w)
	h.bc.WriteBlock(w, h.NodeCtx())
}
func confView(w *types.WorkObject, ctx int, manifest types.BlockManifest, links common.Hashes) *types.WorkObject {
	v := types.CopyWorkObject(w)
	if ctx < 2 {
		v.Body().SetTransactions(types.Transactions{})
		v.Body().SetOutboundEtxs(types.Transactions{})
		v.Body().SetUncles([]*types.WorkObjectHeader{})
		v.Body().SetManifest(manifest)
	}
	if ctx == 0 {
		v.Body().SetInterlinkHashes(links)
	} else {
		v.Body().SetInterlinkHashes(common.Hashes{})
	}
	if ctx == 2 {
		v.Body().SetManifest(types.BlockManifest{})
	}
	return v
}
func confCross(txs types.Transactions) types.Transactions {
	out := types.Transactions{}
	for _, tx := range txs {
		if tx.To().Location().Region() != 0 || types.IsCoinBaseTx(tx) || types.IsConversionTx(tx) {
			out = append(out, tx)
		}
	}
	return out
}

// Each uncle has its own real full-target native ProgPoW, with a recent
// canonical parent. No discarded-branch state is used as execution input.
func confStrongUncles(engine *progpow.Progpow, number uint64, blocks []*types.WorkObject) []*types.WorkObjectHeader {
	plan := map[uint64][]uint64{5: {1}, 7: {1, 2}, 9: {2}, 12: {1}, 14: {1, 2}, 16: {2}}
	out := []*types.WorkObjectHeader{}
	for j, delay := range plan[number] {
		h := types.CopyWorkObject(blocks[number-delay]).WorkObjectHeader()
		found := false
		for n := uint64(1000) + number*100 + uint64(j)*20000; n < 60000; n++ {
			h.SetNonce(types.EncodeNonce(n))
			mix, work := engine.ComputePowLight(h)
			h.SetMixHash(mix)
			if work.Big().Cmp(new(big.Int).Div(common.Big2e256, h.Difficulty())) <= 0 {
				found = true
				break
			}
		}
		if !found {
			panic("strong uncle nonce search exhausted")
		}
		out = append(out, h)
	}
	return out
}

func confBranch(branch int) map[string]interface{} {
	engine := progpow.New(params.PowConfig{CachesInMem: 2}, nil, false, log.Global)
	chains := []*HeaderChain{nativeAdmissionHC(0, engine), nativeAdmissionHC(1, engine), nativeAdmissionHC(2, engine)}
	for _, h := range chains {
		h.config.ChainID = ebU(1337)
		h.headerDb = confDatabase{h.headerDb}
		h.bc = NewTestBodyDb(h.headerDb)
		h.bc.chainConfig = h.config
		h.numberCache, _ = lru.New[common.Hash, uint64](64)
		senders, _ := lru.New[common.Hash, common.InternalAddress](64)
		h.pool = &TxPool{senders: senders, signer: types.NewSigner(ebU(1337), common.Location{0, 0})}
		h.pendingEtxs, _ = lru.New[common.Hash, types.PendingEtxs](64)
		h.pendingEtxsRollup, _ = lru.New[common.Hash, types.PendingEtxsRollup](64)
		h.subRollupCache, _ = lru.New[common.Hash, types.Transactions](64)
		h.fetchPrimeBlock = func(id common.Hash) *types.WorkObject { return chains[0].GetBlockByHash(id) }
		defer h.headerDb.Close()
	}
	genesis := (&Genesis{Config: chains[2].config, Timestamp: 1769112000, ExtraData: []byte("quai-rust-admission-v1"), GasLimit: 30000000, Difficulty: ebU(2)}).ToBlock(0)
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
			ebMust(e)
		}
	}
	ebMust(chains[1].AddPendingEtxs(types.PendingEtxs{Header: genesis.ConvertToPEtxView(), OutboundEtxs: types.Transactions{}}))
	rawdb.WritePendingEtxsRollup(chains[0].headerDb, types.PendingEtxsRollup{Header: genesis.ConvertToPEtxView(), EtxsRollup: types.Transactions{}})
	var sender common.Address
	for n := uint64(1); ; n++ {
		key, e := crypto.ToECDSA(ebU(n).FillBytes(make([]byte, 32)))
		ebMust(e)
		a := crypto.PubkeyToAddress(key.PublicKey, common.Location{0, 0})
		if _, e = a.InternalAndQuaiAddress(); e == nil {
			sender = a
			break
		}
	}
	vm.PrecompiledContracts = make(map[common.AddressBytes]vm.PrecompiledContract)
	vm.PrecompiledAddresses = make(map[string][]common.Address)
	vm.InitializePrecompiles(common.Location{0, 0})
	cache := state.NewDatabase(chains[2].headerDb)
	processor := &StateProcessor{config: chains[2].config, hc: chains[2], stateCache: cache, etxCache: cache, logger: log.Global}
	parents := []*types.WorkObject{genesis, genesis, genesis}
	orders := []int{2, 2, 1, 2, 0, 2, 2, 1, 2, 0, 2, 1, 2, 2, 0, 2}
	rows := []interface{}{}
	zoneBlocks := []*types.WorkObject{genesis}
	negatives := []interface{}{}
	capabilities := []interface{}{}
	for i, wanted := range orders {
		number := uint64(i + 1)
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
			w.SetNumber(new(big.Int).Add(p.Number(c), ebU(1)), c)
			w.Header().SetParentEntropy(h.TotalLogEntropy(p), c)
			_, po, e := h.CalcOrder(p)
			ebMust(e)
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
			ebMust(e)
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
			ebMust(e)
			rollup = append(rollup, p.OutboundEtxs...)
		}
		cross := confCross(rollup)
		w.Header().SetEtxRollupHash(types.DeriveSha(cross, trie.NewStackTrie(nil)))
		inbound := rawdb.ReadInboundEtxs(chains[2].headerDb, parents[2].Hash())
		w.Body().SetTransactions(inbound)
		w.Header().SetTxHash(types.DeriveSha(w.Transactions(), trie.NewStackTrie(nil)))
		if branch == 1 && number > 8 {
			w.Header().SetExtra([]byte("alternate"))
		}
		w.Body().SetUncles(confStrongUncles(engine, number, zoneBlocks))
		w.Header().SetUncleHash(types.CalcUncleHash(w.Uncles()))
		w.Header().SetUncledEntropy(chains[2].UncledLogEntropy(w))
		w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
		batch := chains[2].headerDb.NewBatch()
		receipts, etxs, _, db, gas, used, size, set, _, e := processor.Process(w, batch)
		ebMust(e)
		ebMust(batch.Write())
		root, e := db.Commit(true)
		ebMust(e)
		ebMust(cache.TrieDB().Commit(root, false, nil))
		eroot, e := db.CommitEtxs()
		ebMust(e)
		ebMust(cache.TrieDB().Commit(eroot, false, nil))
		w.Header().SetEVMRoot(root)
		w.Header().SetEtxSetRoot(eroot)
		w.Header().SetUTXORoot(set.Hash())
		w.Header().SetQuaiStateSize(db.GetQuaiTrieSize())
		w.Header().SetGasUsed(gas)
		w.Header().SetStateUsed(used)
		w.Header().SetReceiptHash(types.DeriveSha(receipts, trie.NewStackTrie(nil)))
		w.Header().SetOutboundEtxHash(types.DeriveSha(types.Transactions(etxs), trie.NewStackTrie(nil)))
		w.Body().SetOutboundEtxs(etxs)
		w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
		var work common.Hash
		found := false
		for nonce := uint64(0); nonce < 65536; nonce++ {
			w.WorkObjectHeader().SetNonce(types.EncodeNonce(nonce))
			mix, pow := engine.ComputePowLight(w.WorkObjectHeader())
			w.WorkObjectHeader().SetMixHash(mix)
			target := new(big.Int).Div(common.Big2e256, w.Difficulty())
			if pow.Big().Cmp(target) > 0 {
				continue
			}
			_, order, e := chains[2].CalcOrder(w)
			ebMust(e)
			if order == wanted {
				work = pow
				found = true
				break
			}
		}
		if !found {
			panic("hierarchy nonce search exhausted")
		}
		views := make([]*types.WorkObject, 3)
		contexts := []interface{}{}
		claim := common.Hash{}
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
			ebMust(h.AppendHeader(views[c]))
			validator := NewBlockValidator(h.config, h, h.engine)
			ebMust(validator.ValidateBody(views[c]))
			if c == 2 {
				ebMust(validator.ValidateState(w, db, receipts, etxs, set, gas, used))
			}
			sl := &Slice{hc: h, sliceDb: h.headerDb, logger: log.Global}
			b := h.headerDb.NewBatch()
			ret, t, e := sl.pcrc(b, w, claim, wanted < c)
			ebMust(e)
			ebMust(b.Write())
			claim = ret
			contexts = append(contexts, map[string]interface{}{"context": c, "work_object": ebWire(views[c]), "returned_dominant": ret.Hex(), "termini": map[string]interface{}{"dom": t.DomTermini(), "sub": t.SubTermini()}})
		}
		if number == 7 && branch == 0 {
			cases := []string{"duplicate", "ancestor", "sibling", "previously-included", "wrong-number", "bad-mix", "uncle-root", "uncled-entropy"}
			for _, name := range cases {
				bad := types.CopyWorkObject(w)
				list := bad.Uncles()
				for j, h := range list {
					list[j] = types.CopyWorkObjectHeader(h)
				}
				switch name {
				case "duplicate":
					list = append(list, list[0])
				case "ancestor":
					list = []*types.WorkObjectHeader{types.CopyWorkObject(parents[2]).WorkObjectHeader()}
				case "sibling":
					h := types.CopyWorkObject(w).WorkObjectHeader()
					h.SetNonce(types.EncodeNonce(23456))
					for n := uint64(23456); ; n++ {
						h.SetNonce(types.EncodeNonce(n))
						m, p := engine.ComputePowLight(h)
						h.SetMixHash(m)
						if p.Big().Cmp(new(big.Int).Div(common.Big2e256, h.Difficulty())) <= 0 {
							break
						}
					}
					list = []*types.WorkObjectHeader{h}
				case "previously-included":
					list = []*types.WorkObjectHeader{zoneBlocks[5].Uncles()[0]}
				case "wrong-number":
					h := list[0]
					h.SetNumber(new(big.Int).Add(h.Number(), ebU(1)))
					for n := uint64(34567); ; n++ {
						h.SetNonce(types.EncodeNonce(n))
						m, p := engine.ComputePowLight(h)
						h.SetMixHash(m)
						if p.Big().Cmp(new(big.Int).Div(common.Big2e256, h.Difficulty())) <= 0 {
							break
						}
					}
				case "bad-mix":
					list[0].SetMixHash(common.Hash{})
				}
				bad.Body().SetUncles(list)
				bad.Header().SetUncleHash(types.CalcUncleHash(list))
				bad.Header().SetUncledEntropy(chains[2].UncledLogEntropy(bad))
				if name == "uncle-root" {
					bad.Header().SetUncleHash(common.HexToHash("0x1234"))
				}
				if name == "uncled-entropy" {
					bad.Header().SetUncledEntropy(ebU(0))
				}
				bad.WorkObjectHeader().SetHeaderHash(bad.Header().Hash())
				var badWork common.Hash
				for n := uint64(100000); ; n++ {
					bad.WorkObjectHeader().SetNonce(types.EncodeNonce(n))
					m, p := engine.ComputePowLight(bad.WorkObjectHeader())
					bad.WorkObjectHeader().SetMixHash(m)
					if p.Big().Cmp(new(big.Int).Div(common.Big2e256, bad.Difficulty())) > 0 {
						continue
					}
					_, o, e := chains[2].CalcOrder(bad)
					ebMust(e)
					if o == wanted {
						badWork = p
						break
					}
				}
				ebMust(chains[2].AppendHeader(bad))
				validator := NewBlockValidator(chains[2].config, chains[2], chains[2].engine)
				err := validator.ValidateBody(bad)
				stage := "body"
				if err == nil {
					err = validator.ValidateState(bad, db, receipts, etxs, set, gas, used)
					stage = "state"
				}
				if err == nil {
					panic("invalid uncle accepted: " + name)
				}
				negatives = append(negatives, map[string]interface{}{"id": name, "at_height": number, "work_object": ebWire(bad), "contexts": []interface{}{map[string]interface{}{"context": 2, "work_object": ebWire(bad)}}, "work_hash": badWork.Hex(), "go_error": err.Error(), "validation_stage": stage, "accepted": false})
			}
		}
		if number == 7 && branch == 0 {
			weak := types.CopyWorkObject(w)
			h := types.CopyWorkObjectHeader(w.Uncles()[0])
			for n := uint64(45678); ; n++ {
				h.SetNonce(types.EncodeNonce(n))
				m, p := engine.ComputePowLight(h)
				h.SetMixHash(m)
				if p.Big().Cmp(new(big.Int).Div(common.Big2e256, h.Difficulty())) > 0 {
					break
				}
			}
			weak.Body().SetUncles([]*types.WorkObjectHeader{h})
			weak.Header().SetUncleHash(types.CalcUncleHash(weak.Uncles()))
			weak.Header().SetUncledEntropy(chains[2].UncledLogEntropy(weak))
			weak.WorkObjectHeader().SetHeaderHash(weak.Header().Hash())
			var proof common.Hash
			for n := uint64(200000); ; n++ {
				weak.WorkObjectHeader().SetNonce(types.EncodeNonce(n))
				m, p := engine.ComputePowLight(weak.WorkObjectHeader())
				weak.WorkObjectHeader().SetMixHash(m)
				if p.Big().Cmp(new(big.Int).Div(common.Big2e256, weak.Difficulty())) > 0 {
					continue
				}
				_, o, e := chains[2].CalcOrder(weak)
				ebMust(e)
				if o == wanted {
					proof = p
					break
				}
			}
			ebMust(chains[2].AppendHeader(weak))
			validator := NewBlockValidator(chains[2].config, chains[2], chains[2].engine)
			ebMust(validator.ValidateBody(weak))
			ebMust(validator.ValidateState(weak, db, receipts, etxs, set, gas, used))
			capabilities = append(capabilities, map[string]interface{}{"id": "weak-pre-fork-entropy", "at_height": number, "work_object": ebWire(weak), "contexts": []interface{}{map[string]interface{}{"context": 2, "work_object": ebWire(weak)}}, "work_hash": proof.Hex(), "accepted_by_go": true, "rust_supported": false})
		}
		if number == 10 && branch == 0 {
			bad := types.CopyWorkObject(w)
			bad.Header().SetEtxRollupHash(common.HexToHash("0x1234"))
			bad.WorkObjectHeader().SetHeaderHash(bad.Header().Hash())
			var badWork common.Hash
			ok := false
			for n := uint64(50000); n < 100000; n++ {
				bad.WorkObjectHeader().SetNonce(types.EncodeNonce(n))
				mix, work := engine.ComputePowLight(bad.WorkObjectHeader())
				bad.WorkObjectHeader().SetMixHash(mix)
				if work.Big().Cmp(new(big.Int).Div(common.Big2e256, bad.Difficulty())) > 0 {
					continue
				}
				_, o, e := chains[2].CalcOrder(bad)
				ebMust(e)
				if o == wanted {
					badWork = work
					ok = true
					break
				}
			}
			if !ok {
				panic("bad rollup nonce search exhausted")
			}
			parts := []interface{}{}
			for c := wanted; c < 3; c++ {
				m := manifests[2]
				if c == 0 {
					m = manifests[1]
				}
				view := confView(bad, c, m, links)
				if c == 2 {
					view = confView(bad, 2, nil, nil)
				}
				ebMust(chains[c].AppendHeader(view))
				ebMust(NewBlockValidator(chains[c].config, chains[c], chains[c].engine).ValidateBody(view))
				parts = append(parts, map[string]interface{}{"context": c, "work_object": ebWire(view)})
			}
			pe := types.PendingEtxsRollup{Header: bad.ConvertToPEtxView(), EtxsRollup: cross}
			if pe.IsValid(trie.NewStackTrie(nil)) {
				panic("forged rollup accepted")
			}
			negatives = append(negatives, map[string]interface{}{"id": "forged-rollup-with-valid-work", "at_height": 10, "work_hash": badWork.Hex(), "work_object": ebWire(confView(bad, 2, nil, nil)), "contexts": parts, "accepted": false})
		}
		confirmed := types.Transactions{}
		if wanted == 0 {
			sl := &Slice{hc: chains[0], sliceDb: chains[0].headerDb, logger: log.Global}
			confirmed, e = sl.CollectNewlyConfirmedEtxs(views[0], 0)
			ebMust(e)
			confirmed = confirmed.FilterToSub(w.Location(), 1, 0)
		} else if wanted == 1 {
			sl := &Slice{hc: chains[1], sliceDb: chains[1].headerDb, logger: log.Global}
			confirmed, e = sl.CollectNewlyConfirmedEtxs(views[1], 1)
			ebMust(e)
		}
		rawdb.WriteInboundEtxs(chains[2].headerDb, w.Hash(), confirmed)
		for c := wanted; c < 3; c++ {
			h := chains[c]
			confStore(h, views[c])
			h.CalculateManifest(views[c])
			if c == 0 {
				_, e = h.CalculateInterlink(views[c])
				ebMust(e)
			}
			parents[c] = views[c]
		}
		ebMust(chains[1].AddPendingEtxs(types.PendingEtxs{Header: w.ConvertToPEtxView(), OutboundEtxs: etxs}))
		if wanted <= 1 {
			pe := types.PendingEtxsRollup{Header: w.ConvertToPEtxView(), EtxsRollup: cross}
			if !pe.IsValid(trie.NewStackTrie(nil)) {
				panic("rollup not committed")
			}
			rawdb.WritePendingEtxsRollup(chains[0].headerDb, pe)
		}
		rawdb.WriteMultiSet(chains[2].headerDb, w.Hash(), set)
		rawdb.WriteUTXOSetSize(chains[2].headerDb, w.Hash(), size)
		rs := []string{}
		for j := range receipts {
			var b bytes.Buffer
			receipts.EncodeIndex(j, &b)
			rs = append(rs, ebHex(b.Bytes()))
		}
		zoneBlocks = append(zoneBlocks, w)
		rows = append(rows, map[string]interface{}{"id": fmt.Sprintf("hierarchy-%d", number), "order": wanted, "work_object": ebWire(confView(w, 2, nil, nil)), "contexts": contexts, "work_hash": work.Hex(), "uncle_count": len(w.Uncles()), "uncled_entropy": w.UncledEntropy().String(), "workshare_entropy": func() string { e, err := chains[2].WorkShareLogEntropy(w); ebMust(err); return e.String() }(), "inbound": ebTxs(inbound), "confirmed": ebTxs(confirmed), "cross_rollup": ebTxs(cross), "evm_root": root.Hex(), "etx_root": eroot.Hex(), "utxo_root": set.Hash().Hex(), "utxo_set_size": size, "quai_state_size": db.GetQuaiTrieSize().Uint64(), "receipt_rlp": rs})
	}
	return map[string]interface{}{"genesis": ebWire(genesis), "genesis_hash": genesis.Hash(), "genesis_body_hash": genesis.Header().Hash(), "vectors": rows, "negatives": negatives, "capabilities": capabilities}
}
func NativeUnclesPipeline() map[string]interface{} {
	a := confBranch(0)
	b := confBranch(1)
	return map[string]interface{}{"genesis": a["genesis"], "genesis_hash": a["genesis_hash"], "genesis_body_hash": a["genesis_body_hash"], "branches": []interface{}{a, b}, "now": 1769113000, "negatives": a["negatives"], "capabilities": a["capabilities"], "filters": confFilters(), "scope": "Actual native mixed zone/region/prime header/manifest/body/PCRC validation, committed ETX rollup collection/filtering and zone StateProcessor/ValidateState on two early private branches. Real full-target uncles at distances one and two; no fake proof/order. Weak pre-fork shares and full fork/economic controllers, PoEM canonical publication and public sync remain unqualified."}
}

func confFilters() map[string]interface{} {
	input := types.Transactions{}
	for ty := uint64(0); ty < 7; ty++ {
		for _, prefix := range []byte{0, 1, 16} {
			addr := make([]byte, 20)
			addr[0] = prefix
			addr[19] = byte(ty + 1)
			to := common.BytesToAddress(addr, common.Location{0, 0})
			input = append(input, types.NewTx(&types.ExternalTx{OriginatingTxHash: common.BigToHash(ebU(ty + 100)), ETXIndex: uint16(prefix), Gas: 21000, To: &to, Value: ebU(42), Data: []byte{}, AccessList: types.AccessList{}, Sender: common.Zero, EtxType: ty}))
		}
	}
	input = append(input, input[0])
	rows := []interface{}{}
	for _, location := range []common.Location{{0, 0}, {0, 1}, {1, 0}} {
		for ctx := 0; ctx < 3; ctx++ {
			for order := 0; order < 3; order++ {
				rows = append(rows, map[string]interface{}{"location": []int{int(location[0]), int(location[1])}, "context": ctx, "order": order, "expected": ebTxs(input.FilterToSub(location, ctx, order))})
			}
		}
	}
	cross := []interface{}{}
	for region := 0; region < 2; region++ {
		part := types.Transactions{}
		for _, tx := range input {
			if tx.To().Location().Region() != region || types.IsCoinBaseTx(tx) || types.IsConversionTx(tx) {
				part = append(part, tx)
			}
		}
		cross = append(cross, map[string]interface{}{"region": region, "expected": ebTxs(part)})
	}
	return map[string]interface{}{"input": ebTxs(input), "rows": rows, "cross": cross}
}
