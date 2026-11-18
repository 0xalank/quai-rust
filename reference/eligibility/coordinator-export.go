// Actual coordinator eligibility methods over explicit disposable backend views.
package utils

import (
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/internal/quaiapi"
	"github.com/dominant-strategies/go-quai/quai"
	"google.golang.org/protobuf/proto"
	"math/big"
)

type ecBackend struct {
	quaiapi.Backend
	headers map[common.Hash]*types.WorkObject
	terms   map[common.Hash]*types.Termini
	genesis map[common.Hash]bool
	orders  map[common.Hash]int
}

func (b *ecBackend) GetHeaderByHash(h common.Hash) *types.WorkObject { return b.headers[h] }
func (b *ecBackend) GetTerminiByHash(h common.Hash) *types.Termini   { return b.terms[h] }
func (b *ecBackend) IsGenesisHash(h common.Hash) bool                { return b.genesis[h] }
func (b *ecBackend) CalcOrder(w *types.WorkObject) (*big.Int, int, error) {
	if w == nil {
		return nil, 0, fmt.Errorf("missing order header")
	}
	o, ok := b.orders[w.Hash()]
	if !ok {
		return nil, 0, fmt.Errorf("missing supplied order")
	}
	return new(big.Int), o, nil
}

type ecConsensus struct {
	quai.ConsensusAPI
	views map[string]quaiapi.Backend
}

func (c *ecConsensus) GetBackend(l common.Location) *quaiapi.Backend {
	b, ok := c.views[l.Name()]
	if !ok {
		panic("missing oracle backend " + l.Name())
	}
	return &b
}
func ecKey(l common.Location) common.Location {
	k := append(common.Location{}, l...)
	if len(k) > 0 && k[0] >= 3 {
		k[0] = 3
	}
	return k
}
func ecTerm(t types.Termini) string {
	b, e := proto.Marshal(t.ProtoEncode())
	if e != nil {
		panic(e)
	}
	return "0x" + fmt.Sprintf("%x", b)
}
func ecTermDecode(s string) *types.Termini {
	var p types.ProtoTermini
	b := common.FromHex(s)
	if e := proto.Unmarshal(b, &p); e != nil {
		panic(e)
	}
	t := types.EmptyTermini()
	if e := t.ProtoDecode(&p); e != nil {
		panic(e)
	}
	return &t
}
func ecNew(l common.Location, records []*types.WorkObject, orders map[common.Hash]int, terms map[common.Hash]*types.Termini, root common.Hash) (*HierarchicalCoordinator, *ecConsensus, []common.Location) {
	views := []common.Location{{}, {l[0]}, l}
	c := &ecConsensus{views: map[string]quaiapi.Backend{}}
	for _, loc := range views {
		h := map[common.Hash]*types.WorkObject{}
		t := map[common.Hash]*types.Termini{}
		for _, w := range records {
			h[w.Hash()] = w
			t[w.Hash()] = terms[w.Hash()]
		}
		c.views[loc.Name()] = &ecBackend{headers: h, terms: t, genesis: map[common.Hash]bool{root: true}, orders: orders}
	}
	return &HierarchicalCoordinator{consensus: c}, c, views
}
func ecMap(m map[string]common.Hash, l common.Location) []interface{} {
	out := []interface{}{}
	for _, loc := range []common.Location{{}, {l[0]}, l} {
		if h, ok := m[loc.Name()]; ok {
			out = append(out, map[string]interface{}{"location": fcLoc(ecKey(loc)), "hash": h.Hex()})
		}
	}
	return out
}
func ecCopy(m map[string]common.Hash) map[string]common.Hash { return CopyConstraintMap(m) }
func ecData(loc common.Location, mode int) (map[string]interface{}, *HierarchicalCoordinator, *ecConsensus, []*types.WorkObject) {
	root := types.EmptyWorkObject(2)
	root.WorkObjectHeader().SetDifficulty(fcNumber(2))
	root.Header().SetExtra([]byte{byte(mode), loc[0], loc[1]})
	root.WorkObjectHeader().SetHeaderHash(root.Header().Hash())
	root = fcRound(root)
	records := []*types.WorkObject{root}
	orders := map[common.Hash]int{root.Hash(): 0}
	seq := [][]int{{2, 2, 2, 2}, {2, 1, 2, 2}, {0, 2, 1, 2}, {0, 1, 0, 2}}[mode]
	for branch := 0; branch < 2; branch++ {
		p := root
		for depth := 1; depth <= 4; depth++ {
			w := types.EmptyWorkObject(2)
			w.WorkObjectHeader().SetDifficulty(fcNumber(2))
			w.WorkObjectHeader().SetLocation(loc)
			w.Header().SetExtra([]byte{byte(mode), byte(branch), byte(depth)})
			for c := 0; c < 3; c++ {
				w.SetParentHash(p.Hash(), c)
				w.SetNumber(fcNumber(uint64(depth)), c)
			}
			w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
			w = fcRound(w)
			records = append(records, w)
			orders[w.Hash()] = seq[depth-1]
			p = w
		}
	}
	terms := map[common.Hash]*types.Termini{}
	for k, w := range records {
		t := types.EmptyTermini()
		for i := 0; i < 16; i++ {
			t.SetDomTerminiAtIndex(common.BigToHash(fcNumber(uint64(1000+k*100+i))), i)
			t.SetSubTerminiAtIndex(common.BigToHash(fcNumber(uint64(5000+k*100+i))), i)
		}
		terms[w.Hash()] = &t
	}
	hc, c, views := ecNew(loc, records, orders, terms, root.Hash())
	entries := []interface{}{}
	for _, v := range views {
		for _, w := range records {
			entries = append(entries, map[string]interface{}{"location": fcLoc(ecKey(v)), "block": fcWire(w), "termini": ecTerm(*terms[w.Hash()]), "order": orders[w.Hash()]})
		}
	}
	return map[string]interface{}{"location": fcLoc(loc), "legacy": loc[0] >= 3, "genesis": root.Hash().Hex(), "views": [][]int{fcLoc(common.Location{}), fcLoc(ecKey(common.Location{loc[0]})), fcLoc(ecKey(loc))}, "entries": entries}, hc, c, records
}
func NativeEligibilityVectors() map[string]interface{} {
	datasets := []interface{}{}
	ancestors := []interface{}{}
	pcrcs := []interface{}{}
	traces := []interface{}{}
	boundaries := []interface{}{}
	for _, loc := range []common.Location{{0, 0}, {1, 2}, {15, 15}} {
		for mode := 0; mode < 4; mode++ {
			ds, hc, c, records := ecData(loc, mode)
			index := len(datasets)
			datasets = append(datasets, ds)
			hs := []common.Hash{}
			for _, w := range records {
				hs = append(hs, w.Hash())
			}
			as := append(append([]common.Hash{}, hs...), common.Hash{}, common.HexToHash("0xbad"))
			headers := append(append([]common.Hash{}, hs...), common.HexToHash("0xbad"))
			for order := 0; order < 3; order++ {
				for _, a := range as {
					for _, h := range headers {
						ok := hc.IsAncestor(a, h, loc, order)
						ancestors = append(ancestors, map[string]interface{}{"dataset": index, "ancestor": a.Hex(), "header": h.Hex(), "order": order, "value": ok, "available": h != common.HexToHash("0xbad") || a == h})
					}
				}
			}
			for _, sl := range []common.Location{{loc[0]}, loc} {
				b := c.views[sl.Name()].(*ecBackend)
				for _, w := range records {
					expected := b.terms[w.Hash()].DomTerminus(sl)
					for variant := 0; variant < 3; variant++ {
						claim := expected
						if variant == 1 {
							claim = common.HexToHash("0xdead")
						}
						old := b.terms[w.Hash()]
						if variant == 2 {
							b.terms[w.Hash()] = nil
						}
						ok := hc.pcrc(w.Hash(), claim, sl, sl.Context())
						b.terms[w.Hash()] = old
						pcrcs = append(pcrcs, map[string]interface{}{"dataset": index, "location": fcLoc(sl), "header": w.Hash().Hex(), "dominant": claim.Hex(), "missing": variant == 2, "value": ok})
					}
				}
			}
			maps := []map[string]common.Hash{{}, {common.Location{}.Name(): hs[0], common.Location{loc[0]}.Name(): hs[0], loc.Name(): hs[0]}, {common.Location{}.Name(): hs[1]}, {common.Location{}.Name(): hs[4]}, {common.Location{loc[0]}.Name(): hs[6]}, {common.Location{}.Name(): hs[1], common.Location{loc[0]}.Name(): hs[6]}, {loc.Name(): hs[8]}}
			for _, leader := range records {
				for _, first := range []bool{false, true} {
					for _, input := range maps {
						original := ecCopy(input)
						passed := ecCopy(input)
						result, e := hc.calculateFrontierPoints(passed, leader, first)
						traces = append(traces, map[string]interface{}{"dataset": index, "leader": leader.Hash().Hex(), "leader_location": fcLoc(loc), "first": first, "constraints": ecMap(original, loc), "valid": e == nil, "result": ecMap(result, loc), "error": fmt.Sprint(e), "caller_after": ecMap(passed, loc)})
					}
				}
			}
			// Source boundary: an existing prime constraint and missing same-hash
			// prime header can return the starting map with a nil error.
			if mode == 3 {
				leader := records[3]
				b := c.views[common.Location{}.Name()].(*ecBackend)
				saved := b.headers[leader.Hash()]
				delete(b.headers, leader.Hash())
				input := map[string]common.Hash{common.Location{}.Name(): rootHash(records)}
				r, e := hc.calculateFrontierPoints(ecCopy(input), leader, false)
				b.headers[leader.Hash()] = saved
				boundaries = append(boundaries, map[string]interface{}{"dataset": index, "leader": leader.Hash().Hex(), "leader_location": fcLoc(loc), "constraints": ecMap(input, loc), "missing_location": []int{}, "missing_header": leader.Hash().Hex(), "go_success": e == nil, "go_result": ecMap(r, loc), "strict_reject": true})
			}
		}
	}
	return map[string]interface{}{"datasets": datasets, "ancestors": ancestors, "pcrc": pcrcs, "traces": traces, "boundaries": boundaries}
}
func rootHash(w []*types.WorkObject) common.Hash { return w[0].Hash() }
func NativeEligibilityReplay(v map[string]interface{}) map[string]interface{} {
	root := fcDecode(v["root"].(string))
	records := []*types.WorkObject{root}
	orders := map[common.Hash]int{root.Hash(): 0}
	for _, s := range v["steps"].([]interface{}) {
		step := s.(map[string]interface{})
		w := fcDecode(step["block"].(string))
		records = append(records, w)
		orders[w.Hash()] = 0
	}
	terms := map[common.Hash]*types.Termini{}
	for _, s := range v["seeds"].([]interface{}) {
		_ = s
	}
	hc, c, views := ecNew(common.Location{0, 0}, records, orders, terms, root.Hash())
	entries := []interface{}{}
	for i, sl := range views {
		b := c.views[sl.Name()].(*ecBackend)
		seed := v["seeds"].([]interface{})[i].(map[string]interface{})
		snap := seed["snapshot"].(map[string]interface{})
		b.terms[root.Hash()] = ecTermDecode(snap["termini"].(string))
		entries = append(entries, map[string]interface{}{"location": fcLoc(sl), "block": v["root"], "termini": snap["termini"], "order": 0})
		for _, s := range v["steps"].([]interface{}) {
			step := s.(map[string]interface{})
			w := fcDecode(step["block"].(string))
			snap := step["contexts"].([]interface{})[i].(map[string]interface{})["snapshot"].(map[string]interface{})
			b.terms[w.Hash()] = ecTermDecode(snap["termini"].(string))
			entries = append(entries, map[string]interface{}{"location": fcLoc(sl), "block": step["contexts"].([]interface{})[i].(map[string]interface{})["block"], "termini": snap["termini"], "order": 0})
		}
	}
	matrix := []interface{}{}
	for _, p := range records {
		for _, r := range records {
			for _, z := range records {
				pt := c.views[common.Location{}.Name()].(*ecBackend).terms[p.Hash()]
				rt := c.views[common.Location{0}.Name()].(*ecBackend).terms[r.Hash()]
				a := hc.pcrc(r.Hash(), pt.SubTerminiAtIndex(0), common.Location{0}, 1)
				b := hc.pcrc(z.Hash(), rt.SubTerminiAtIndex(0), common.Location{0, 0}, 2)
				matrix = append(matrix, map[string]interface{}{"prime": p.Hash().Hex(), "region": r.Hash().Hex(), "zone": z.Hash().Hex(), "valid": a && b, "edges": []bool{a, b}})
			}
		}
	}
	traced := []interface{}{}
	for _, w := range records {
		for _, first := range []bool{false, true} {
			for _, p := range records {
				input := map[string]common.Hash{common.Location{}.Name(): p.Hash(), common.Location{0}.Name(): p.Hash(), common.Location{0, 0}.Name(): p.Hash()}
				r, e := hc.calculateFrontierPoints(ecCopy(input), w, first)
				traced = append(traced, map[string]interface{}{"leader": w.Hash().Hex(), "first": first, "constraints": ecMap(input, common.Location{0, 0}), "valid": e == nil, "result": ecMap(r, common.Location{0, 0}), "error": fmt.Sprint(e)})
			}
		}
	}
	v["entries"] = entries
	v["matrix"] = matrix
	v["traces"] = traced
	return v
}
