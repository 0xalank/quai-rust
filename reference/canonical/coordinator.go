// Offline oracle through the actual pinned NodeSet and BuildPendingHeaders paths.
package utils

import (
	"encoding/hex"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/types"
	"google.golang.org/protobuf/proto"
	"math/big"
)

func ccDecode(s string) *types.WorkObject {
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
func NativeCanonicalReplay(v map[string]interface{}) {
	g := ccDecode(v["genesis"].(string))
	locs := []common.Location{{}, {0}, {0, 0}}
	base := NodeSet{nodes: map[string]Node{}}
	for _, l := range locs {
		base.nodes[l.Name()] = Node{hash: g.Hash(), number: g.NumberArray(), location: l, entropy: new(big.Int)}
	}
	fronts := map[common.Hash]NodeSet{g.Hash(): base}
	branches := v["branches"].([]interface{})
	for _, branch := range branches {
		for _, raw := range branch.(map[string]interface{})["vectors"].([]interface{}) {
			row := raw.(map[string]interface{})
			w := ccDecode(row["work_object"].(string))
			prior := fronts[w.ParentHash(2)]
			ns := prior.Copy()
			order := int(row["order"].(float64))
			entropy, _ := new(big.Int).SetString(row["poem"].([]interface{})[2].(map[string]interface{})["total"].(string), 10)
			if !ns.Extendable(w, order) {
				panic("native frontier not extendable")
			}
			ns.Update(w, entropy, order)
			fronts[w.Hash()] = ns
			row["canonical_score"] = ns.Entropy(1, 1).String()
		}
	}
	first := 0
	a := branches[0].(map[string]interface{})["vectors"].([]interface{})
	b := branches[1].(map[string]interface{})["vectors"].([]interface{})
	sa, _ := new(big.Int).SetString(a[15].(map[string]interface{})["canonical_score"].(string), 10)
	sb, _ := new(big.Int).SetString(b[15].(map[string]interface{})["canonical_score"].(string), 10)
	if sa.Cmp(sb) > 0 {
		first = 1
	}
	if sa.Cmp(sb) == 0 {
		panic("canonical test requires distinct final entropy")
	}
	hc := &HierarchicalCoordinator{pendingHeaders: NewPendingHeaders(), bestEntropy: new(big.Int), currentExpansionNumber: 0, generateHeaderWorkersCount: c_maxHeaderWorkers + 1}
	hc.Add(new(big.Int), base, hc.pendingHeaders)
	events := []interface{}{}
	selected := g.Hash()
	for pass := 0; pass < 2; pass++ {
		which := first
		if pass == 1 {
			which = 1 - first
		}
		rows := branches[which].(map[string]interface{})["vectors"].([]interface{})
		start := 0
		if pass == 1 {
			start = 8
		}
		for i := start; i < len(rows); i++ {
			row := rows[i].(map[string]interface{})
			w := ccDecode(row["work_object"].(string))
			entropy, _ := new(big.Int).SetString(row["poem"].([]interface{})[2].(map[string]interface{})["total"].(string), 10)
			hc.BuildPendingHeaders(w, int(row["order"].(float64)), entropy)
			best, ok := hc.Get(hc.bestEntropy)
			if !ok {
				panic("lost source frontier")
			}
			next := best.nodes[common.Location{0, 0}.Name()].hash
			events = append(events, map[string]interface{}{"branch": which, "index": i, "selected": next != selected, "head": next.Hex(), "best_score": hc.bestEntropy.String()})
			selected = next
		}
	}
	// Reoffering the selected tip must retain the existing entropy-keyed frontier.
	which := 1 - first
	row := branches[which].(map[string]interface{})["vectors"].([]interface{})[15].(map[string]interface{})
	w := ccDecode(row["work_object"].(string))
	entropy, _ := new(big.Int).SetString(row["poem"].([]interface{})[2].(map[string]interface{})["total"].(string), 10)
	hc.BuildPendingHeaders(w, int(row["order"].(float64)), entropy)
	best, _ := hc.Get(hc.bestEntropy)
	events = append(events, map[string]interface{}{"branch": which, "index": 15, "selected": false, "duplicate": true, "head": best.nodes[common.Location{0, 0}.Name()].hash.Hex(), "best_score": hc.bestEntropy.String()})
	v["canonical_events"] = events
	v["first_branch"] = first
	v["winning_branch"] = 1 - first
}
