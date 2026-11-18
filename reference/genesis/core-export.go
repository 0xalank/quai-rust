// Offline genesis and candidate branch evidence on disposable memory databases.
package core

import (
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
	"math/big"
)

type gnDominant struct {
	CoreBackend
	sl *Slice
}

func (g gnDominant) AddPendingEtxsRollup(p types.PendingEtxsRollup) error {
	return g.sl.AddPendingEtxsRollup(p)
}
func gnLocations(v []common.Location) [][]int {
	out := [][]int{}
	for _, l := range v {
		a := []int{}
		for _, b := range l {
			a = append(a, int(b))
		}
		out = append(out, a)
	}
	return out
}
func gnHC(ctx int) *HeaderChain {
	h := plHC(ctx)
	h.pendingEtxs, _ = lru.New[common.Hash, types.PendingEtxs](32)
	h.pendingEtxsRollup, _ = lru.New[common.Hash, types.PendingEtxsRollup](32)
	h.blooms, _ = lru.New[common.Hash, types.Bloom](32)
	return h
}
func gnSeed(hc *HeaderChain, w *types.WorkObject) map[string]interface{} {
	t := types.EmptyTermini()
	for i := 0; i < 16; i++ {
		t.SetDomTerminiAtIndex(w.Hash(), i)
		t.SetSubTerminiAtIndex(w.Hash(), i)
	}
	rawdb.WriteTermini(hc.headerDb, w.Hash(), t)
	var links common.Hashes
	if hc.NodeCtx() == 0 {
		var e error
		links, e = hc.CalculateInterlink(w)
		if e != nil {
			panic(e)
		}
	}
	return plSnapshot(rawdb.ReadManifest(hc.headerDb, w.Hash()), links, t)
}
func gnInit(hc *HeaderChain, w *types.WorkObject) {
	sl := &Slice{hc: hc, sliceDb: hc.headerDb, logger: log.Global}
	if hc.NodeCtx() == 1 {
		dom := gnHC(0)
		defer dom.headerDb.Close()
		dom.AddGenesisHash(w.Hash())
		sl.domInterface = gnDominant{sl: &Slice{hc: dom, sliceDb: dom.headerDb, logger: log.Global}}
	}
	sl.WriteGenesisBlock(w, hc.NodeLocation())
}
func NativeGenesisVectors() map[string]interface{} {
	out := map[string]interface{}{}
	specs := []interface{}{}
	starts := []uint64{0, 1, 2, 29, 30, 31, 255, 256, 257, ^uint64(0)}
	for ctx := 0; ctx < 3; ctx++ {
		for j, start := range starts {
			for mode := 0; mode < 4; mode++ {
				hc := gnHC(ctx)
				extra := make([]byte, []int{0, 1, 32, 64}[mode])
				for i := range extra {
					extra[i] = byte(i + j)
				}
				d := nh(uint64(mode))
				if mode == 3 {
					d = new(big.Int).Add(new(big.Int).Lsh(nh(1), 255), nh(99))
				}
				gas := []uint64{0, 1, 12000000, ^uint64(0)}[mode]
				alloc := common.BigToHash(nh(uint64(100 + j + mode)))
				g := &Genesis{Config: hc.config, Nonce: uint64(j*100 + mode), Timestamp: uint64(1769112000 + j), ExtraData: extra, Difficulty: d, GasLimit: gas, AllocHash: alloc, Mixhash: common.HexToHash("0xdead"), Coinbase: common.Zero, Number: []uint64{99, 99, 99}, GasUsed: 999, BaseFee: nh(999)}
				w := g.ToBlock(start)
				committed, e := g.Commit(hc.headerDb, hc.NodeLocation(), start)
				if e != nil {
					panic(e)
				}
				commitT := rawdb.ReadTermini(hc.headerDb, w.Hash())
				gnInit(hc, w)
				snap := gnSeed(hc, w)
				specs = append(specs, map[string]interface{}{"context": ctx, "nonce": g.Nonce, "timestamp": g.Timestamp, "extra": hh(extra), "allocation_hash": alloc.Hex(), "gas_limit": gas, "difficulty": d.String(), "starting_expansion": start, "block": hp(hnRound(w)), "raw_block": hp(w), "hash": w.Hash().Hex(), "body_hash": w.Header().Hash().Hex(), "commit_hash": committed.Hash().Hex(), "commit_termini": plTerm(*commitT), "seed": snap})
				hc.headerDb.Close()
			}
		}
	}
	out["specs"] = specs
	geometry := []interface{}{}
	for exp := 0; exp < 256; exp++ {
		r, z := common.GetHierarchySizeForExpansionNumber(uint8(exp))
		hc := gnHC(0)
		hc.SetCurrentExpansionNumber(uint8(exp))
		sl := &Slice{hc: hc}
		v := map[string]interface{}{"expansion": exp, "regions": r, "zones": z, "new_chains": gnLocations(common.NewChainsAdded(uint8(exp))), "active_supported": exp <= 30, "new_supported": exp > 0 && exp <= 30}
		if exp <= 30 {
			v["active"] = gnLocations(sl.ActiveSlices())
		}
		geometry = append(geometry, v)
		hc.headerDb.Close()
	}
	out["geometry"] = geometry
	difficulties := []interface{}{}
	for _, exp := range []uint8{0, 1, 2, 30, 31, 255} {
		for loc := 0; loc < 3; loc++ {
			for def := 0; def < 2; def++ {
				for ent := 0; ent < 2; ent++ {
					hc := gnHC(2)
					w := hnParent(0, 0, nh(1000000))
					w.Header().SetExpansionNumber(exp)
					w.Header().SetParentEntropy(nh(uint64(ent)), 0)
					if loc == 0 {
						w.WorkObjectHeader().SetLocation(common.Location{})
					} else if loc == 1 {
						w.WorkObjectHeader().SetLocation(common.Location{0, 0})
					} else {
						w.WorkObjectHeader().SetLocation(common.Location{1, 2})
					}
					hnSync(w)
					w = hnRound(w)
					plStore(hc, w)
					hc.AddGenesisHash(w.Hash())
					defaultHash := common.HexToHash("0xdada")
					if def == 1 {
						defaultHash = w.Hash()
					}
					hc.config.DefaultGenesisHash = defaultHash
					d := hc.CalcDifficulty(w.WorkObjectHeader(), exp)
					value := interface{}(nil)
					if d != nil {
						value = d.String()
					}
					difficulties = append(difficulties, map[string]interface{}{"block": hp(w), "expansion": exp, "default_genesis": defaultHash.Hex(), "difficulty": value})
					hc.headerDb.Close()
				}
			}
		}
	}
	out["difficulties"] = difficulties
	out["branches"] = gnBranches()
	return out
}
func gnBranches() []interface{} {
	out := []interface{}{}
	engines := []consensus.Engine{progpow.New(params.PowConfig{}, nil, false, log.Global), kawpow.New(params.PowConfig{}, nil, false, log.Global)}
	for ctx := 0; ctx < 3; ctx++ {
		for _, prime := range []uint64{0, 1171500} {
			if ctx == 2 && prime > 0 {
				continue
			}
			hc := gnHC(ctx)
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
			gnInit(hc, root)
			seed := gnSeed(hc, root)
			parents := []*types.WorkObject{root, root}
			steps := []interface{}{}
			stored := []*types.WorkObject{root}
			for depth := 1; depth <= 3; depth++ {
				for branch := 0; branch < 2; branch++ {
					parent := parents[branch]
					var base *types.WorkObject
					if ctx == 2 {
						base = hnChild(hc, parent)
					} else {
						base = hrChild(hc, parent)
					}
					// Keep the small-difficulty fixture at five-second spacing. A sibling
					// tag changes the seal without driving difficulty to one.
					base.Header().SetExtra([]byte{byte(branch)})
					if ctx == 0 {
						links := rawdb.ReadInterlinkHashes(hc.headerDb, parent.Hash())
						base.Body().SetInterlinkHashes(links)
						base.Header().SetInterlinkRootHash(types.DeriveSha(links, trie.NewStackTrie(nil)))
					} else {
						base.Header().SetManifestHash(types.DeriveSha(rawdb.ReadManifest(hc.headerDb, parent.Hash()), trie.NewStackTrie(nil)), ctx)
					}
					hnSync(base)
					hnBind(base)
					plMine(hc, base, ctx)
					for variant := 0; variant < 4; variant++ {
						w := hnRound(base)
						if variant == 0 {
							if w.AuxPow() == nil {
								m := w.MixHash()
								m[0] ^= 1
								w.WorkObjectHeader().SetMixHash(m)
							} else {
								m := w.AuxPow().Header().MixHash()
								m[0] ^= 1
								w.AuxPow().Header().SetMixHash(m)
							}
						}
						if variant == 1 {
							w.SetParentHash(common.HexToHash("0xdada"), ctx)
							hnSync(w)
							hnBind(w)
							plMine(hc, w, ctx)
						}
						if variant == 2 {
							if ctx == 0 {
								w.Header().SetInterlinkRootHash(common.HexToHash("0xbaba"))
							} else {
								w.Header().SetManifestHash(common.HexToHash("0xbaba"), ctx)
							}
							hnSync(w)
							hnBind(w)
							plMine(hc, w, ctx)
						}
						_, order, err := hc.CalcOrder(w)
						if err == nil && order != ctx {
							err = fmt.Errorf("route mismatch")
						}
						if err == nil {
							err = hc.AppendHeader(w)
						}
						if err == nil {
							err = NewBlockValidator(hc.config, hc, hc.engine).ValidateBody(w)
						}
						ret := common.Hash{}
						var nextT types.Termini
						if err == nil {
							sl := &Slice{hc: hc, sliceDb: hc.headerDb, logger: log.Global}
							batch := hc.headerDb.NewBatch()
							ret, nextT, err = sl.pcrc(batch, w, root.Hash(), false)
							if err == nil {
								err = batch.Write()
							}
						}
						v := map[string]interface{}{"id": fmt.Sprintf("%d/%d/%d/%d/%d", ctx, prime, depth, branch, variant), "block": hp(w), "valid": err == nil, "error": fmt.Sprint(err), "branch": branch}
						if err == nil {
							plStore(hc, w)
							rawdb.WriteTermini(hc.headerDb, w.Hash(), nextT)
							hc.CalculateManifest(w)
							var l common.Hashes
							if ctx == 0 {
								l, err = hc.CalculateInterlink(w)
								if err != nil {
									panic(err)
								}
							}
							v["next"] = plSnapshot(rawdb.ReadManifest(hc.headerDb, w.Hash()), l, nextT)
							v["returned_dominant"] = ret.Hex()
							parents[branch] = w
							stored = append(stored, w)
						}
						steps = append(steps, v)
					}
				}
			}
			snapshots := []interface{}{}
			for _, w := range stored {
				var l common.Hashes
				if ctx == 0 {
					l = rawdb.ReadInterlinkHashes(hc.headerDb, w.Hash())
				}
				snapshots = append(snapshots, map[string]interface{}{"hash": w.Hash().Hex(), "snapshot": plSnapshot(rawdb.ReadManifest(hc.headerDb, w.Hash()), l, *rawdb.ReadTermini(hc.headerDb, w.Hash()))})
			}
			out = append(out, map[string]interface{}{"location": gnLocations([]common.Location{hc.NodeLocation()})[0], "genesis": root.Hash().Hex(), "root": hp(root), "seed": seed, "steps": steps, "snapshots": snapshots, "now": 1769112040})
			hc.headerDb.Close()
		}
	}
	return out
}
