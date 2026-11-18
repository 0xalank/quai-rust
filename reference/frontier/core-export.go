// Actual offline startup, PoEM score and candidate branch methods.
package core

import (
	"encoding/hex"
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
	"google.golang.org/protobuf/proto"
	"math/big"
)

func frDecode(v string) *types.WorkObject {
	b, e := hex.DecodeString(v[2:])
	if e != nil {
		panic(e)
	}
	p := &types.ProtoWorkObject{}
	if e = proto.Unmarshal(b, p); e != nil {
		panic(e)
	}
	w := new(types.WorkObject)
	if e = w.ProtoDecode(p, common.Location{0, 0}, types.BlockObject); e != nil {
		panic(e)
	}
	return w
}
func frMetrics(hc *HeaderChain, w *types.WorkObject) map[string]interface{} {
	s, o, e := hc.CalcOrder(w)
	if e != nil {
		panic(e)
	}
	r := interface{}(nil)
	if o == 0 {
		rank, e := hc.CalcRank(w)
		if e != nil {
			panic(e)
		}
		r = rank
	}
	ws := nh(0)
	if hc.NodeCtx() == 2 {
		ws, e = hc.WorkShareLogEntropy(w)
		if e != nil {
			panic(e)
		}
	}
	return map[string]interface{}{"intrinsic": s.String(), "order": o, "rank": r, "total": hc.TotalLogEntropy(w).String(), "delta": hc.DeltaLogEntropy(w).String(), "uncled_delta": hc.UncledDeltaLogEntropy(w).String(), "workshares": ws.String()}
}
func NativeFrontierCoreVectors() map[string]interface{} {
	profiles := append(frCoincidentBranches(), frStartups()...)
	engines := []consensus.Engine{progpow.New(params.PowConfig{}, nil, false, log.Global), kawpow.New(params.PowConfig{}, nil, false, log.Global)}
	for _, value := range profiles {
		row := value.(map[string]interface{})
		ctx := len(row["location"].([]int))
		hc := gnHC(ctx)
		hc.engine = engines
		root := frDecode(row["root"].(string))
		plStore(hc, root)
		hc.AddGenesisHash(root.Hash())
		if d, ok := row["default_genesis"]; ok {
			hc.config.DefaultGenesisHash = common.HexToHash(d.(string))
		}
		totals := []*types.WorkObject{root, root}
		for _, s := range row["steps"].([]interface{}) {
			step := s.(map[string]interface{})
			if !step["valid"].(bool) {
				continue
			}
			w := frDecode(step["block"].(string))
			step["metrics"] = frMetrics(hc, w)
			totals[step["branch"].(int)] = w
			step["comparison"] = hc.TotalLogEntropy(totals[0]).Cmp(hc.TotalLogEntropy(totals[1]))
			plStore(hc, w)
		}
		hc.headerDb.Close()
	}
	arithmetic := []interface{}{}
	for ctx := 0; ctx < 3; ctx++ {
		for _, prime := range []uint64{0, 1171500} {
			for _, ent := range []int{1, 2, 4, 6, 8, 12, 40, 200} {
				for _, count := range []int{0, 1, 2, 3, 16, 32} {
					if prime == 0 && count > 0 {
						continue
					}
					hc := gnHC(ctx)
					w := plBlock(ctx, ent, 0, ctx+ent+count)
					w.WorkObjectHeader().SetPrimeTerminusNumber(nh(prime))
					if prime >= 1171500 {
						h := w.WorkObjectHeader()
						h.SetShaDiffAndCount(types.NewPowShareDiffAndCount(nh(0), nh(0), nh(0)))
						h.SetScryptDiffAndCount(types.NewPowShareDiffAndCount(nh(0), nh(0), nh(0)))
						h.SetShaShareTarget(nh(0))
						h.SetScryptShareTarget(nh(0))
						h.SetKawpowDifficulty(nh(0))
					}

					w.Header().SetParentEntropy(nh(11), 0)
					w.Header().SetParentEntropy(nh(13), 1)
					w.Header().SetParentEntropy(nh(17), 2)
					w.Header().SetParentUncledDeltaEntropy(nh(19), 1)
					w.Header().SetParentUncledDeltaEntropy(nh(23), 2)
					w.Header().SetUncledEntropy(nh(29))
					uncles := []*types.WorkObjectHeader{}
					for n := 0; n < count; n++ {
						h := hnParent(prime, uint64(n+1), nh(2))
						uncles = append(uncles, h.WorkObjectHeader())
					}
					w.Body().SetUncles(uncles)
					w.Header().SetUncleHash(types.CalcUncleHash(uncles))
					hnSync(w)
					hnBind(w)
					w = hnRound(w)
					m := frMetrics(hc, w)
					arithmetic = append(arithmetic, map[string]interface{}{"context": ctx, "block": hp(w), "pow_hash": w.MixHash().Hex(), "metrics": m})
					hc.headerDb.Close()
				}
			}
		}
	}
	return map[string]interface{}{"profiles": profiles, "scores": arithmetic}
}
func frStartups() []interface{} {
	out := []interface{}{}
	engines := []consensus.Engine{progpow.New(params.PowConfig{}, nil, false, log.Global), kawpow.New(params.PowConfig{}, nil, false, log.Global)}
	for _, expansion := range []uint64{1, 2, 4, 30} {
		ctx := 2
		prime := uint64(0)
		{
			hc := gnHC(ctx)
			hc.engine = engines

			g := &Genesis{Config: hc.config, Nonce: 100 + expansion, Timestamp: 1769112000, ExtraData: []byte{1, 2, 3}, Difficulty: nh(2), GasLimit: 12000000, AllocHash: common.HexToHash("0xbeef")}
			root, err := g.Commit(hc.headerDb, hc.NodeLocation(), expansion)
			if err != nil {
				panic(err)
			}
			root = hnRound(root)
			hc.config.DefaultGenesisHash = root.Hash()
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
					for variant := 0; variant < 5; variant++ {
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

						if variant == 3 && depth > 1 {
							w.SetNumber(new(big.Int).Add(w.Number(ctx), nh(5)), ctx)
							hnSync(w)
							hnBind(w)
							plMine(hc, w, ctx)
						}
						correctDefault := hc.config.DefaultGenesisHash
						if variant == 3 && depth == 1 {
							hc.config.DefaultGenesisHash = common.HexToHash("0xdeaddead")
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
						hc.config.DefaultGenesisHash = correctDefault
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
						v := map[string]interface{}{"id": fmt.Sprintf("%d/%d/%d/%d/%d", ctx, prime, depth, branch, variant), "block": hp(w), "valid": err == nil, "error": fmt.Sprint(err), "branch": branch, "wrong_default": variant == 3 && depth == 1}
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
			out = append(out, map[string]interface{}{"location": gnLocations([]common.Location{hc.NodeLocation()})[0], "genesis": root.Hash().Hex(), "root": hp(root), "seed": seed, "steps": steps, "snapshots": snapshots, "now": 1769112040, "default_genesis": root.Hash().Hex(), "starting_expansion": expansion, "spec": map[string]interface{}{"context": ctx, "nonce": g.Nonce, "timestamp": g.Timestamp, "extra": hh(g.ExtraData), "allocation_hash": g.AllocHash.Hex(), "gas_limit": g.GasLimit, "difficulty": g.Difficulty.String(), "starting_expansion": expansion}})
			hc.headerDb.Close()
		}
	}
	return out
}

func frCoincidentBranches() []interface{} {
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

					// Couple the coordinator's coincident parent slots. Local
					// header validity alone does not establish this condition.
					for c := 0; c < 3; c++ {
						base.SetParentHash(parent.Hash(), c)
						base.SetNumber(new(big.Int).Add(parent.Number(c), nh(1)), c)
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
