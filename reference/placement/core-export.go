// Actual offline Core/HeaderChain/Slice methods on disposable memory databases.
package core

import (
	"encoding/binary"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/consensus/kawpow"
	"github.com/dominant-strategies/go-quai/consensus/progpow"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/dominant-strategies/go-quai/trie"
	lru "github.com/hashicorp/golang-lru/v2"
	"google.golang.org/protobuf/proto"
	"math/big"
)

func plHC(ctx int) *HeaderChain {
	hc := nativeHC(ctx)
	hc.bc = NewTestBodyDb(hc.headerDb)
	hc.bc.chainConfig = hc.config
	hc.numberCache, _ = lru.New[common.Hash, uint64](128)
	hc.fetchPrimeBlock = hc.GetBlockByHash
	hc.powConfig.GasCeil = 50000000
	hc.powConfig.WorkShareThreshold = 7
	return hc
}
func plCore(hc *HeaderChain) *Core {
	q, _ := lru.New[common.Hash, blockNumberAndRetryCounter](8)
	return &Core{sl: &Slice{hc: hc, logger: log.Global}, appendQueue: q, logger: log.Global}
}
func plTermini(seed int) types.Termini {
	t := types.EmptyTermini()
	for i := 0; i < 16; i++ {
		t.SetDomTerminiAtIndex(common.BigToHash(nh(uint64(seed*100+i+1))), i)
		t.SetSubTerminiAtIndex(common.BigToHash(nh(uint64(seed*100+i+51))), i)
	}
	return t
}
func plTerm(t types.Termini) string {
	b, e := proto.Marshal(t.ProtoEncode())
	if e != nil {
		panic(e)
	}
	return hh(b)
}
func plHashes(v common.Hashes) []string {
	out := []string{}
	for _, h := range v {
		out = append(out, h.Hex())
	}
	return out
}
func plBlock(ctx, entropy int, exp uint8, index int) *types.WorkObject {
	w := hnParent(0, 10, nh(2))
	for i := 0; i < 3; i++ {
		w.SetNumber(nh(10), i)
		w.SetParentHash(common.HexToHash("0xaa"), i)
	}
	w.Header().SetExpansionNumber(exp)
	w.Header().SetParentDeltaEntropy(new(big.Int).Lsh(nh(1000000000000), 64), 1)
	w.Header().SetParentDeltaEntropy(new(big.Int).Lsh(nh(1000000000000), 64), 2)
	pow := new(big.Int).Lsh(nh(1), uint(256-entropy))
	w.WorkObjectHeader().SetMixHash(common.BigToHash(pow))
	w.WorkObjectHeader().SetTime(uint64(1769112000 + index))
	hnSync(w)
	return hnRound(w)
}
func plRoute(ctx, order int) string {
	if order == ctx {
		return "local"
	}
	if order < ctx {
		return "dominant"
	}
	return "other"
}
func NativePlacementVectors() map[string]interface{} {
	out := map[string]interface{}{}
	orders := []interface{}{}
	updates := []interface{}{}
	for ctx := 0; ctx < 3; ctx++ {
		for exp := 0; exp < 8; exp++ {
			for _, entropy := range []int{1, 2, 3, 4, 5, 6, 7, 8, 10, 14, 22, 40, 80, 200} {
				hc := plHC(ctx)
				w := plBlock(ctx, entropy, uint8(exp), ctx*1000+exp*100+entropy)
				_, order, e := hc.CalcOrder(w)
				if e != nil {
					panic(e)
				}
				rank := interface{}(nil)
				if order == 0 {
					r, e := hc.CalcRank(w)
					if e != nil {
						panic(e)
					}
					rank = r
				}
				c := plCore(hc)
				if e = c.addToAppendQueue(w); e != nil {
					panic(e)
				}
				orders = append(orders, map[string]interface{}{"context": ctx, "block": hp(w), "pow_hash": w.MixHash().Hex(), "order": order, "rank": rank, "queued": c.appendQueue.Contains(w.Hash()), "route": plRoute(ctx, order)})
				parent := types.BlockManifest{common.HexToHash("0x11"), common.HexToHash("0x22")}
				links := common.Hashes{common.HexToHash("0xa0"), common.HexToHash("0xa1"), common.HexToHash("0xa2"), common.HexToHash("0xa3")}
				rawdb.WriteManifest(hc.headerDb, w.ParentHash(ctx), parent)
				rawdb.WriteInterlinkHashes(hc.headerDb, w.ParentHash(0), links)
				returnedManifest := hc.CalculateManifest(w)
				manifest := rawdb.ReadManifest(hc.headerDb, w.Hash())
				v := map[string]interface{}{"context": ctx, "block": hp(w), "order": order, "rank": rank, "parent_manifest": plHashes(common.Hashes(parent)), "parent_interlinks": plHashes(links), "manifest": plHashes(common.Hashes(manifest)), "returned_manifest": plHashes(common.Hashes(returnedManifest))}
				if ctx == 0 && order == 0 {
					next, e := hc.CalculateInterlink(w)
					if e != nil {
						panic(e)
					}
					v["interlinks"] = plHashes(next)
				}
				updates = append(updates, v)
				hc.headerDb.Close()
			}
		}
	}
	out["orders"] = orders
	out["updates"] = updates
	appendCases := []interface{}{}
	for ctx := 0; ctx < 3; ctx++ {
		for _, length := range []int{0, 1, 2, 15, 16, 127} {
			for variant := 0; variant < 4; variant++ {
				hc := plHC(ctx)
				w := plBlock(ctx, 4, 0, length+variant)
				parent := types.BlockManifest{}
				for i := 0; i < length; i++ {
					parent = append(parent, common.BigToHash(nh(uint64(i+1))))
				}
				links := common.Hashes{common.HexToHash("0xa0"), common.HexToHash("0xa1"), common.HexToHash("0xa2"), common.HexToHash("0xa3")}
				if ctx > 0 {
					w.Header().SetManifestHash(types.DeriveSha(parent, trie.NewStackTrie(nil)), ctx)
				} else {
					w.Header().SetInterlinkRootHash(types.DeriveSha(links, trie.NewStackTrie(nil)))
				}
				if variant == 1 {
					if ctx > 0 {
						w.Header().SetManifestHash(common.HexToHash("0xbb"), ctx)
					} else {
						w.Header().SetInterlinkRootHash(common.HexToHash("0xbb"))
					}
				}
				hnSync(w)
				rawdb.WriteTermini(hc.headerDb, w.Hash(), types.EmptyTermini())
				hc.headerCache.Add(w.Hash(), *w) // Isolate ancestry checks after the actual known-header shortcut.
				if variant != 2 {
					rawdb.WriteManifest(hc.headerDb, w.ParentHash(ctx), parent)
					rawdb.WriteInterlinkHashes(hc.headerDb, w.ParentHash(ctx), links)
				}
				if variant == 3 {
					if ctx > 0 {
						rawdb.WriteManifest(hc.headerDb, w.ParentHash(ctx), types.BlockManifest{common.HexToHash("0xcc")})
					} else {
						rawdb.WriteInterlinkHashes(hc.headerDb, w.ParentHash(ctx), common.Hashes{common.HexToHash("0xcc"), links[1], links[2], links[3]})
					}
				}
				err := hc.AppendHeader(w)
				actualParent := parent
				actualLinks := links
				if variant == 3 {
					if ctx > 0 {
						actualParent = types.BlockManifest{common.HexToHash("0xcc")}
					} else {
						actualLinks = common.Hashes{common.HexToHash("0xcc"), links[1], links[2], links[3]}
					}
				}
				appendCases = append(appendCases, map[string]interface{}{"context": ctx, "block": hp(w), "parent_manifest": plHashes(common.Hashes(actualParent)), "parent_interlinks": plHashes(actualLinks), "parent_present": variant != 2, "valid": err == nil, "error": fmt.Sprint(err)})
				hc.headerDb.Close()
			}
		}
	}
	out["append_commitments"] = appendCases
	codecs := []interface{}{}
	for i := 0; i < 48; i++ {
		p := &types.ProtoTermini{}
		count := 16
		if i%8 == 0 {
			count = 15
		}
		if i%8 == 1 {
			count = 17
		}
		for j := 0; j < count; j++ {
			size := []int{0, 1, 31, 32}[i%4]
			b := make([]byte, size)
			for k := range b {
				b[k] = byte(i + j + k)
			}
			p.DomTermini = append(p.DomTermini, &common.ProtoHash{Value: b})
			p.SubTermini = append(p.SubTermini, &common.ProtoHash{Value: append([]byte{}, b...)})
		}
		b, e := proto.Marshal(p)
		if e != nil {
			panic(e)
		}
		t := new(types.Termini)
		e = t.ProtoDecode(p)
		valid := e == nil && t.IsValid()
		v := map[string]interface{}{"input": hh(b), "valid": valid}
		if valid {
			v["canonical"] = plTerm(*t)
		}
		codecs = append(codecs, v)
	}
	out["termini_codecs"] = codecs
	pcrcs := []interface{}{}
	nodes := []common.Location{{}, {0}, {3}, {15}, {0, 0}, {1, 2}, {15, 15}}
	locations := []common.Location{}
	for i := 0; i < 16; i++ {
		locations = append(locations, common.Location{byte(i), byte(i)})
	}
	locations = append(locations, common.Location{2, 15}, common.Location{15, 2})
	for ni, node := range nodes {
		for li, location := range locations {
			for _, origin := range []bool{false, true} {
				for mode := 0; mode < 3; mode++ {
					hc := plHC(len(node))
					hc.config.Location = node
					hc.powConfig.NodeLocation = node
					w := plBlock(len(node), 4, 0, ni*1000+li*10+mode)
					w.WorkObjectHeader().SetLocation(location)
					hnSync(w)
					t := plTermini(ni + 1)
					rawdb.WriteTermini(hc.headerDb, w.ParentHash(len(node)), t)
					old := t.DomTerminus(node)
					claimed := old
					if mode == 1 {
						claimed = common.HexToHash("0xdead")
					}
					genesis := []string{}
					if mode == 2 {
						hc.AddGenesisHash(old)
						genesis = append(genesis, old.Hex())
						claimed = common.HexToHash("0xdead")
					}
					sl := &Slice{hc: hc, sliceDb: hc.headerDb, logger: log.Global}
					batch := hc.headerDb.NewBatch()
					returned, next, err := sl.pcrc(batch, w, claimed, origin)
					loc := []int{}
					for _, b := range node {
						loc = append(loc, int(b))
					}
					v := map[string]interface{}{"location": loc, "header_location": []int{int(location[0]), int(location[1])}, "id": w.Hash().Hex(), "parent": plTerm(t), "dominant_origin": origin, "claimed_dominant": claimed.Hex(), "genesis": genesis, "valid": err == nil, "error": fmt.Sprint(err), "input_unchanged": plTerm(t) == plTerm(*rawdb.ReadTermini(hc.headerDb, w.ParentHash(len(node))))}
					if err == nil {
						if e := batch.Write(); e != nil {
							panic(e)
						}
						v["returned_dominant"] = returned.Hex()
						v["next"] = plTerm(next)
						v["stored"] = plTerm(*rawdb.ReadTermini(hc.headerDb, w.Hash()))
					}
					pcrcs = append(pcrcs, v)
					hc.headerDb.Close()
				}
			}
		}
	}
	out["pcrc"] = pcrcs
	out["native"] = plNative()
	return out
}
func plStore(hc *HeaderChain, w *types.WorkObject) {
	hstore(hc, w)
	hc.numberCache.Add(w.Hash(), w.NumberU64(hc.NodeCtx()))
}
func plSnapshot(manifest types.BlockManifest, links common.Hashes, t types.Termini) map[string]interface{} {
	v := map[string]interface{}{"manifest": plHashes(common.Hashes(manifest)), "termini": plTerm(t)}
	if links != nil {
		v["interlinks"] = plHashes(links)
	}
	return v
}
func plNative() []interface{} {
	out := []interface{}{}
	engines := []consensus.Engine{progpow.New(params.PowConfig{}, nil, false, log.Global), kawpow.New(params.PowConfig{}, nil, false, log.Global)}
	for ctx := 0; ctx < 3; ctx++ {
		heights := []uint64{0, 1171500}
		if ctx == 2 {
			heights = []uint64{0}
		}
		for _, prime := range heights {
			hc := plHC(ctx)
			hc.engine = engines
			root := hnParent(prime, 0, nh(2))
			root.SetNumber(nh(0), ctx)
			if ctx == 2 {
				root.WorkObjectHeader().SetLocation(common.Location{})
			}
			hnSync(root)
			hnBind(root)
			root = hnRound(root)
			plStore(hc, root)
			hc.AddGenesisHash(root.Hash())
			var parent *types.WorkObject
			if ctx == 2 {
				parent = hnChild(hc, root)
			} else {
				parent = hrChild(hc, root)
				parent.Header().SetParentDeltaEntropy(new(big.Int).Lsh(nh(1000), 64), 1)
				parent.Header().SetParentDeltaEntropy(new(big.Int).Lsh(nh(1000), 64), 2)
				hnSync(parent)
				hnBind(parent)
			}
			plMine(hc, parent, ctx)
			plStore(hc, parent)
			for _, domOrigin := range []bool{false, true} {
				if ctx == 0 && domOrigin {
					continue
				}
				var base *types.WorkObject
				if ctx == 2 {
					base = hnChild(hc, parent)
				} else {
					base = hrChild(hc, parent)
				}
				manifest := types.BlockManifest{parent.Hash()}
				links := common.Hashes{root.Hash(), root.Hash(), root.Hash(), root.Hash()}
				termini := types.Termini(types.EmptyTermini())
				for i := 0; i < 16; i++ {
					termini.SetDomTerminiAtIndex(root.Hash(), i)
					termini.SetSubTerminiAtIndex(root.Hash(), i)
				}
				if ctx > 0 {
					base.Header().SetManifestHash(types.DeriveSha(manifest, trie.NewStackTrie(nil)), ctx)
				} else {
					base.Body().SetInterlinkHashes(links)
					base.Header().SetInterlinkRootHash(types.DeriveSha(links, trie.NewStackTrie(nil)))
				}
				hnSync(base)
				hnBind(base)
				wanted := ctx
				if domOrigin {
					wanted = ctx - 1
				}
				plMine(hc, base, wanted)
				for variant := 0; variant < 7; variant++ {
					w := hnRound(base)
					m := append(types.BlockManifest{}, manifest...)
					l := append(common.Hashes{}, links...)
					t := types.CopyTermini(termini)
					present := true
					claimed := root.Hash()
					switch variant {
					case 1:
						if w.AuxPow() == nil {
							mix := w.MixHash()
							mix[0] ^= 1
							w.WorkObjectHeader().SetMixHash(mix)
						} else {
							mix := w.AuxPow().Header().MixHash()
							mix[0] ^= 1
							w.AuxPow().Header().SetMixHash(mix)
						}
					case 2:
						if ctx == 0 {
							l[0] = common.HexToHash("0xdeaf")
						} else {
							m[0] = common.HexToHash("0xdeaf")
						}
					case 3:
						present = false
					case 4:
						for i := 0; i < 16; i++ {
							t.SetDomTerminiAtIndex(common.HexToHash("0xdead"), i)
						}
						claimed = common.HexToHash("0xbeef")
					case 5:
						w.WorkObjectHeader().SetTime(1)
					case 6:
						if ctx == 0 {
							w.Header().SetPrimeStateRoot(common.HexToHash("0x01"))
							hnSync(w)
						} else if ctx == 1 {
							w.Header().SetRegionStateRoot(common.HexToHash("0x01"))
							hnSync(w)
						} else {
							w.Header().SetBaseFee(nh(1))
							hnSync(w)
						}
					}
					// Recompute proof after policy mutations so a stale mix does not
					// mask timestamp/state policy failures in these negative cases.
					if variant == 5 || variant == 6 {
						hnBind(w)
						plMine(hc, w, wanted)
					}
					rawdb.WriteTermini(hc.headerDb, parent.Hash(), t)
					if present {
						rawdb.WriteManifest(hc.headerDb, parent.Hash(), m)
						rawdb.WriteInterlinkHashes(hc.headerDb, parent.Hash(), l)
					} else {
						rawdb.DeleteManifest(hc.headerDb, parent.Hash())
						rawdb.DeleteInterlinkHashes(hc.headerDb, parent.Hash())
					}
					err := error(nil)
					_, order, e := hc.CalcOrder(w)
					rank := interface{}(nil)
					if e != nil {
						err = e
					} else {
						if order == 0 {
							r, e := hc.CalcRank(w)
							if e != nil {
								err = e
							} else {
								rank = r
							}
						}
						if err == nil && ((!domOrigin && order != ctx) || (domOrigin && order >= ctx)) {
							err = fmt.Errorf("append path mismatch")
						}
					}
					if err == nil {
						err = hc.AppendHeader(w)
					}
					if err == nil {
						err = NewBlockValidator(hc.config, hc, hc.engine).ValidateBody(w)
					}
					returned := common.Hash{}
					nextT := types.EmptyTermini()
					if err == nil {
						sl := &Slice{hc: hc, sliceDb: hc.headerDb, logger: log.Global}
						batch := hc.headerDb.NewBatch()
						returned, nextT, err = sl.pcrc(batch, w, claimed, domOrigin)
					}
					loc := []int{}
					for _, b := range hc.NodeLocation() {
						loc = append(loc, int(b))
					}
					v := map[string]interface{}{"id": fmt.Sprintf("native/%d/%d/%v/%d", ctx, prime, domOrigin, variant), "location": loc, "genesis": root.Hash().Hex(), "records": []string{hp(root), hp(parent)}, "block": hp(w), "parent_snapshot": plSnapshot(m, func() common.Hashes {
						if ctx == 0 {
							return l
						}
						return nil
					}(), t), "parent_present": present, "dominant_origin": domOrigin, "claimed_dominant": claimed.Hex(), "now": 1769112040, "valid": err == nil, "error": fmt.Sprint(err)}
					if !present {
						delete(v["parent_snapshot"].(map[string]interface{}), "manifest")
						delete(v["parent_snapshot"].(map[string]interface{}), "interlinks")
					}
					if err == nil {
						rawdb.WriteTermini(hc.headerDb, w.Hash(), nextT)
						returnedManifest := hc.CalculateManifest(w)
						nextM := rawdb.ReadManifest(hc.headerDb, w.Hash())
						v["returned_manifest"] = plHashes(common.Hashes(returnedManifest))
						var nextL common.Hashes
						if ctx == 0 {
							nextL, err = hc.CalculateInterlink(w)
							if err != nil {
								panic(err)
							}
						}
						v["next"] = plSnapshot(nextM, nextL, nextT)
						v["returned_dominant"] = returned.Hex()
						v["order"] = order
						v["rank"] = rank
					}
					out = append(out, v)
				}
			}
			hc.headerDb.Close()
		}
	}
	return out
}

// Re-decode after each attempt: Go's ComputePowLight cache-hit path does not
// refresh per-object atomic work fields. Fresh objects avoid stale test inputs.
func plMine(hc *HeaderChain, w *types.WorkObject, want int) {
	target := new(big.Int).Div(new(big.Int).Set(common.Big2e256), w.Difficulty())
	for nonce := uint64(0); nonce < 10000; nonce++ {
		*w = *hnRound(w)
		h := w.WorkObjectHeader()
		var mix, pow common.Hash
		if h.AuxPow() == nil {
			var b [8]byte
			binary.BigEndian.PutUint64(b[:], nonce)
			h.SetNonce(types.BlockNonce(b))
			mix, pow = hc.GetEngineForHeader(h).ComputePowLight(h)
			h.SetMixHash(mix)
		} else {
			h.AuxPow().Header().SetNonce64(nonce)
			mix, pow = hc.GetEngineForHeader(h).ComputePowLight(h)
			h.AuxPow().Header().SetMixHash(mix)
		}
		*w = *hnRound(w)
		if new(big.Int).SetBytes(pow[:]).Cmp(target) > 0 {
			continue
		}
		_, order, e := hc.CalcOrder(w)
		if e != nil {
			panic(fmt.Sprintf("fresh mine ctx=%d prime=%d: %v", hc.NodeCtx(), w.PrimeTerminusNumber().Uint64(), e))
		}
		if order == want {
			return
		}
	}
	panic("placement mining nonce bound")
}
