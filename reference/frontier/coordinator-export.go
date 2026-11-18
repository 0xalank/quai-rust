// Offline projections through actual coordinator methods, without backends.
package utils

import (
	"encoding/hex"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/types"
	"google.golang.org/protobuf/proto"
	"math/big"
)

func fcLoc(v common.Location) []int {
	out := []int{}
	for _, b := range v {
		out = append(out, int(b))
	}
	return out
}
func fcRound(w *types.WorkObject) *types.WorkObject {
	p, e := w.ProtoEncode(types.BlockObject)
	if e != nil {
		panic(e)
	}
	n := new(types.WorkObject)
	if e = n.ProtoDecode(p, common.Location{0, 0}, types.BlockObject); e != nil {
		panic(e)
	}
	return n
}
func fcDecode(s string) *types.WorkObject {
	b, e := hex.DecodeString(s[2:])
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
func fcWire(w *types.WorkObject) string {
	p, e := w.ProtoEncode(types.BlockObject)
	if e != nil {
		panic(e)
	}
	b, e := proto.Marshal(p)
	if e != nil {
		panic(e)
	}
	return "0x" + hex.EncodeToString(b)
}
func fcNumber(n uint64) *big.Int { return new(big.Int).SetUint64(n) }
func fcLocations(exp uint8) []common.Location {
	r, z := common.GetHierarchySizeForExpansionNumber(exp)
	out := []common.Location{{}}
	for i := 0; i < int(r); i++ {
		out = append(out, common.Location{byte(i)})
		for j := 0; j < int(z); j++ {
			out = append(out, common.Location{byte(i), byte(j)})
		}
	}
	return out
}
func fcSnapshot(ns NodeSet, exp uint8) []interface{} {
	out := []interface{}{}
	for _, l := range fcLocations(exp) {
		n := ns.nodes[l.Name()]
		nums := []uint64{}
		for _, v := range n.number {
			nums = append(nums, v.Uint64())
		}
		out = append(out, map[string]interface{}{"location": fcLoc(l), "hash": n.hash.Hex(), "numbers": nums, "entropy": n.entropy.String()})
	}
	return out
}
func fcSet(exp uint8, seed int) NodeSet {
	ns := NodeSet{nodes: map[string]Node{}}
	for i, l := range fcLocations(exp) {
		n := fcNumber(uint64(seed*1000 + i + 1))
		if seed%2 == 1 {
			n.Sub(new(big.Int).Lsh(fcNumber(1), 256), fcNumber(uint64(i+1)))
		}
		ns.nodes[l.Name()] = Node{hash: common.BigToHash(fcNumber(uint64(i + seed*1000 + 1))), number: []*big.Int{fcNumber(uint64(i)), fcNumber(uint64(i + 1)), fcNumber(uint64(i + 2))}, location: l, entropy: n}
	}
	return ns
}
func NativeCoordinatorVectors() map[string]interface{} {
	updates := []interface{}{}
	for _, exp := range []uint8{0, 1, 2, 3, 4, 6, 10, 30} {
		r, z := common.GetHierarchySizeForExpansionNumber(exp)
		for order := 0; order < 3; order++ {
			for endpoint := 0; endpoint < 2; endpoint++ {
				loc := common.Location{0, 0}
				if endpoint == 1 {
					loc = common.Location{byte(r - 1), byte(z - 1)}
				}
				for bad := 0; bad < 4; bad++ {
					seed := endpoint + order
					ns := fcSet(exp, seed)
					w := types.EmptyWorkObject(2)
					w.WorkObjectHeader().SetDifficulty(fcNumber(2))
					w.WorkObjectHeader().SetLocation(loc)
					w.Header().SetExpansionNumber(exp)
					for i := 0; i < 3; i++ {
						w.SetNumber(fcNumber(uint64(100+i)), i)
						var l common.Location
						if i == 0 {
							l = common.Location{}
						} else if i == 1 {
							l = loc[:1]
						} else {
							l = loc
						}
						w.SetParentHash(ns.nodes[l.Name()].hash, i)
					}
					if bad > 0 {
						w.SetParentHash(common.HexToHash("0xbad"), bad-1)
					}
					w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
					w = fcRound(w)
					before := fcSnapshot(ns, exp)
					ok := ns.Extendable(w, order)
					v := map[string]interface{}{"expansion": exp, "order": order, "block": fcWire(w), "before": before, "before_total": ns.Entropy(int(r), int(z)).String(), "extendable": ok}
					if ok {
						next := ns.Copy()
						entropy := fcNumber(uint64(777 + bad + order))
						next.Update(w, entropy, order)
						v["score"] = entropy.String()
						v["after"] = fcSnapshot(next, exp)
						v["after_total"] = next.Entropy(int(r), int(z)).String()
					}
					v["original_after"] = fcSnapshot(ns, exp)
					updates = append(updates, v)
				}
			}
		}
	}
	pool := &HierarchicalCoordinator{bestEntropy: new(big.Int)}
	pending := NewPendingHeaders()
	pool.pendingHeaders = pending
	offers := []interface{}{}
	// Each key is a real sum over a complete three-node frontier. The first
	// offer is deliberately the highest and will be evicted later.
	for i := 0; i < 2005; i++ {
		ns := fcSet(0, 0)
		value := uint64(i + 1)
		if i == 0 {
			value = 100000
		}
		if i == 2 {
			value = 2
		}
		if i == 2002 {
			value = 2
		}
		for _, l := range fcLocations(0) {
			n := ns.nodes[l.Name()]
			n.entropy = fcNumber(value)
			n.hash = common.BigToHash(fcNumber(uint64(i + 999)))
			ns.nodes[l.Name()] = n
		}
		key := ns.Entropy(1, 1)
		_, had := pool.Get(key)
		pool.Add(key, ns, pending)
		stored, _ := pool.Get(key)
		offers = append(offers, map[string]interface{}{"tips": fcSnapshot(ns, 0), "inserted": !had, "len": pending.collection.Len(), "key": key.String(), "best_ever": pool.bestEntropy.String(), "stored_prime": stored.nodes[common.Location{}.Name()].hash.Hex()})
	}
	keys := []string{}
	for _, key := range pending.collection.Keys() {
		keys = append(keys, key)
	}
	return map[string]interface{}{"updates": updates, "offers": offers, "retained_keys": keys, "best_ever": pool.bestEntropy.String(), "best_retained": pending.collection.Contains(pool.bestEntropy.String())}
}
func NativeCoordinatorReplay(profiles []interface{}) []interface{} {
	out := []interface{}{}
	for _, r := range profiles {
		row := r.(map[string]interface{})
		root := fcDecode(row["root"].(string))
		exp := root.ExpansionNumber()
		initial := NodeSet{nodes: map[string]Node{}}
		for _, l := range fcLocations(exp) {
			initial.nodes[l.Name()] = Node{hash: root.Hash(), number: root.NumberArray(), location: l, entropy: new(big.Int)}
		}
		fronts := []NodeSet{initial.Copy(), initial.Copy()}
		steps := []interface{}{}
		for _, s := range row["steps"].([]interface{}) {
			step := s.(map[string]interface{})
			if !step["valid"].(bool) {
				continue
			}
			w := fcDecode(step["block"].(string))
			m := step["metrics"].(map[string]interface{})
			order := m["order"].(int)
			branch := step["branch"].(int)
			entropy, ok := new(big.Int).SetString(m["total"].(string), 10)
			if !ok {
				panic("score")
			}
			if !fronts[branch].Extendable(w, order) {
				panic(fmt.Sprintf("native frontier %v not extendable", step["id"]))
			}
			fronts[branch].Update(w, entropy, order)
			r, z := common.GetHierarchySizeForExpansionNumber(exp)
			steps = append(steps, map[string]interface{}{"id": step["id"], "branch": branch, "tips": fcSnapshot(fronts[branch], exp), "total": fronts[branch].Entropy(int(r), int(z)).String()})
		}
		out = append(out, map[string]interface{}{"initial": fcSnapshot(initial, exp), "expansion": exp, "steps": steps})
	}
	return out
}
