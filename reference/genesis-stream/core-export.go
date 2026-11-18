// Explicit native hierarchy/rollup/execution oracle on disposable private chains.
package core

import (
	"bytes"
"crypto/ecdsa"
	"encoding/hex"
	"fmt"
"io"
"os"
"encoding/json"
"strings"
"github.com/dominant-strategies/go-quai/crypto/multiset"
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
func NativeGenesisStream(total uint64, output io.Writer) {
 encoder := json.NewEncoder(output)
 now := uint64(1769112000) + 5*total + 1000
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
 rawdb.WriteCanonicalHash(h.headerDb,genesis.Hash(),0)
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
	var signingKey *ecdsa.PrivateKey
 for n := uint64(1); ; n++ {
		key, e := crypto.ToECDSA(ebU(n).FillBytes(make([]byte, 32)))
		ebMust(e)
		a := crypto.PubkeyToAddress(key.PublicKey, common.Location{0, 0})
		if _, e = a.InternalAndQuaiAddress(); e == nil {
			sender = a
 signingKey = key
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
	ebMust(encoder.Encode(map[string]interface{}{"kind":"genesis","work_object":ebWire(genesis),"hash":genesis.Hash(),"blocks":total,"now":now,"reference_revision":"33db82f2891b267ba898ca38bd2dd8bf75cbb072","checkpoint_imported":false,"initial_allocations":false}))
 signedCount:=uint64(0)
	for number:=uint64(1);number<=total;number++ {
 wanted:=orders[(number-1)%uint64(len(orders))]
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
        txs := append(types.Transactions{},inbound...)
        if number>params.TimeToStartTx {
            // Spend only rewards actually matured by the unmodified processor.
            tx := types.NewTx(&types.QuaiTx{ChainID:ebU(1337),Nonce:signedCount,GasPrice:w.BaseFee(),Gas:100000,To:&sender,Value:ebU(1),Data:[]byte{},AccessList:types.AccessList{}})
            signed,e:=types.SignTx(tx,types.NewSigner(ebU(1337),common.Location{0,0}),signingKey);ebMust(e)
            txs=append(types.Transactions{signed},txs...);signedCount++
        }
        w.Body().SetTransactions(txs)
		w.Header().SetTxHash(types.DeriveSha(w.Transactions(), trie.NewStackTrie(nil)))
		w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
        var receipts types.Receipts
        var etxs []*types.Transaction
        var db *state.StateDB
        var gas, used, size uint64
        var set *multiset.MultiSet
        var batch ethdb.Batch
        var e error
        assembled:=false
        for attempt:=0;attempt<4;attempt++ {
            batch=chains[2].headerDb.NewBatch()
            receipts,etxs,_,db,gas,used,size,set,_,e=processor.Process(w,batch)
            if e==nil {assembled=true;break}
            msg:=e.Error();var remote,local string
            if strings.HasPrefix(msg,"invalid avgTxFees used") {
                _,e=fmt.Sscanf(msg,"invalid avgTxFees used (remote: %s local: %s",&remote,&local);ebMust(e)
                v,ok:=new(big.Int).SetString(strings.TrimSuffix(local,")"),10);if !ok {panic(msg)};w.Header().SetAvgTxFees(v)
            } else if strings.HasPrefix(msg,"invalid totalFees used") {
                _,e=fmt.Sscanf(msg,"invalid totalFees used (remote: %s local: %s",&remote,&local);ebMust(e)
                v,ok:=new(big.Int).SetString(strings.TrimSuffix(local,")"),10);if !ok {panic(msg)};w.Header().SetTotalFees(v)
            } else {panic(fmt.Errorf("genesis stream height %d: %w",number,e))}
            w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
        }
        if !assembled {panic("fee assembly did not converge")}
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
 rawdb.WriteCanonicalHash(h.headerDb,views[c].Hash(),views[c].NumberU64(c))
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
		ebMust(encoder.Encode(map[string]interface{}{"kind":"block","height":number,"gas_used":gas,"state_used":used,"signed_quai_transactions":signedCount,"id": fmt.Sprintf("hierarchy-%d", number), "order": wanted, "work_object": ebWire(confView(w, 2, nil, nil)), "contexts": contexts, "work_hash": work.Hex(), "inbound": ebTxs(inbound), "confirmed": ebTxs(confirmed), "cross_rollup": ebTxs(cross), "evm_root": root.Hex(), "etx_root": eroot.Hex(), "utxo_root": set.Hash().Hex(), "utxo_set_size": size, "quai_state_size": db.GetQuaiTrieSize().Uint64(), "receipt_rlp": rs}))
 if number%1000==0 {fmt.Fprintf(os.Stderr,"GENESIS_ORACLE height=%d head=%s prime=%d signed=%d\n",number,w.Hash(),w.PrimeTerminusNumber().Uint64(),signedCount)}
	}
    ebMust(encoder.Encode(map[string]interface{}{"kind":"complete","blocks":total,"head":parents[2].Hash(),"signed_quai_transactions":signedCount,"validated_from_genesis":true,"checkpoint_imported":false,"public_network":false}))
}
