// Native multi-context component replay on disposable memory databases.
package core

import (
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/consensus/kawpow"
	"github.com/dominant-strategies/go-quai/consensus/misc"
	"github.com/dominant-strategies/go-quai/consensus/progpow"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/dominant-strategies/go-quai/trie"
	"math/big"
)

func newBigAdd(n *big.Int, x int64) *big.Int { return new(big.Int).Add(n, big.NewInt(x)) }
func NativeEligibilityComponents() map[string]interface{} {
	engines := []consensus.Engine{progpow.New(params.PowConfig{}, nil, false, log.Global), kawpow.New(params.PowConfig{}, nil, false, log.Global)}
	chains := []*HeaderChain{gnHC(0), gnHC(1), gnHC(2)}
	for _, h := range chains {
		h.engine = engines
		defer h.headerDb.Close()
	}
	root := hnParent(0, 0, nh(2))
	root.WorkObjectHeader().SetLocation(common.Location{})
	for c := 0; c < 3; c++ {
		root.SetNumber(nh(0), c)
	}
	hnSync(root)
	hnBind(root)
	root = hnRound(root)
	seeds := []interface{}{}
	for _, h := range chains {
		h.config.DefaultGenesisHash = root.Hash()
		plStore(h, root)
		h.AddGenesisHash(root.Hash())
		gnInit(h, root)
		seeds = append(seeds, map[string]interface{}{"location": gnLocations([]common.Location{h.NodeLocation()})[0], "snapshot": gnSeed(h, root)})
	}
	parents := []*types.WorkObject{root, root}
	steps := []interface{}{}
	for depth := 1; depth <= 3; depth++ {
		for branch := 0; branch < 2; branch++ {
			p := parents[branch]
			w := hrChild(chains[0], p)
			for c, h := range chains {
				w.SetParentHash(p.Hash(), c)
				w.SetNumber(newBigAdd(p.Number(c), 1), c)
				w.Header().SetParentEntropy(h.TotalLogEntropy(p), c)
			}
			w.WorkObjectHeader().SetLocation(common.Location{0, 0})
			w.Header().SetExtra([]byte{byte(branch)})
			w.Header().SetPrimeTerminusHash(p.Hash())
			w.WorkObjectHeader().SetPrimeTerminusNumber(p.Number(0))
			w.WorkObjectHeader().SetDifficulty(chains[2].CalcDifficulty(p.WorkObjectHeader(), p.ExpansionNumber()))
			w.Header().SetGasLimit(CalcGasLimit(p, 50000000))
			w.Header().SetStateLimit(misc.CalcStateLimit(p, params.StateCeil))
			w.Header().SetBaseFee(chains[2].CalcBaseFee(p))
			m := rawdb.ReadManifest(chains[1].headerDb, p.Hash())
			w.Body().SetManifest(m)
			for c := 1; c < 3; c++ {
				w.Header().SetManifestHash(types.DeriveSha(rawdb.ReadManifest(chains[c].headerDb, p.Hash()), trie.NewStackTrie(nil)), c)
			}
			links := rawdb.ReadInterlinkHashes(chains[0].headerDb, p.Hash())
			w.Body().SetInterlinkHashes(links)
			w.Header().SetInterlinkRootHash(types.DeriveSha(links, trie.NewStackTrie(nil)))
			hnSync(w)
			hnBind(w)
			plMine(chains[0], w, 0)
			nexts := []types.Termini{}
			views := []*types.WorkObject{}
			contexts := []interface{}{}
			for c, h := range chains {
				view := hnRound(w)
				if c == 2 {
					view.Body().SetManifest(types.BlockManifest{})
					view.Body().SetInterlinkHashes(common.Hashes{})
				}
				views = append(views, view)
				if e := NewBlockValidator(h.config, h, h.engine).SanityCheckWorkObjectBlockViewBody(view); e != nil {
					panic(fmt.Sprintf("global sanity %d: %v", c, e))
				}
				_, order, e := h.CalcOrder(w)
				if e != nil || order != 0 {
					panic(fmt.Sprintf("global own proof %d: %v / %d", c, e, order))
				}
				if e = h.AppendHeader(w); e != nil {
					panic(fmt.Sprintf("global header %d: %v", c, e))
				}
				if e = NewBlockValidator(h.config, h, h.engine).ValidateBody(view); e != nil {
					panic(fmt.Sprintf("global body %d: %v", c, e))
				}
				sl := &Slice{hc: h, sliceDb: h.headerDb, logger: log.Global}
				batch := h.headerDb.NewBatch()
				ret, next, e := sl.pcrc(batch, w, p.Hash(), c > 0)
				if e != nil {
					panic(e)
				}
				nexts = append(nexts, next)
				contexts = append(contexts, map[string]interface{}{"location": gnLocations([]common.Location{h.NodeLocation()})[0], "returned_dominant": ret.Hex(), "block": hp(view)})
			}
			for c, h := range chains {
				plStore(h, w)
				rawdb.WriteTermini(h.headerDb, w.Hash(), nexts[c])
				h.CalculateManifest(w)
				var l common.Hashes
				if c == 0 {
					var e error
					l, e = h.CalculateInterlink(w)
					if e != nil {
						panic(e)
					}
				}
				contexts[c].(map[string]interface{})["snapshot"] = plSnapshot(rawdb.ReadManifest(h.headerDb, w.Hash()), l, nexts[c])
			}
			steps = append(steps, map[string]interface{}{"id": fmt.Sprintf("native/%d/%d", depth, branch), "block": hp(w), "branch": branch, "contexts": contexts, "metrics": frMetrics(chains[0], w)})
			parents[branch] = w
		}
	}
	return map[string]interface{}{"root": hp(root), "genesis": root.Hash().Hex(), "seeds": seeds, "steps": steps, "now": 1769112040}
}
