// Disposable backends, observation hooks and virtual clock for actual coordinator methods.
package utils

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/internal/quaiapi"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/dominant-strategies/go-quai/quai"
	lru "github.com/hashicorp/golang-lru/v2"
	"math/big"
	"time"
)

type csBackend struct {
	*ecBackend
	config *params.ChainConfig
	totals map[common.Hash]*big.Int
}

func (b *csBackend) Config() *params.ChainConfig                    { return b.config }
func (b *csBackend) GetBlockByHash(h common.Hash) *types.WorkObject { return b.headers[h] }
func (b *csBackend) TotalLogEntropy(w *types.WorkObject) *big.Int {
	if w == nil {
		panic("missing total header")
	}
	n, ok := b.totals[w.Hash()]
	if !ok {
		panic("missing total projection")
	}
	return new(big.Int).Set(n)
}

type csTraceRow struct {
	Location []int  `json:"location"`
	Hash     string `json:"hash"`
	First    bool   `json:"first"`
	Accepted bool   `json:"accepted"`
}
type csAttemptRow struct {
	Leaders     []interface{} `json:"leaders"`
	Traces      []csTraceRow  `json:"traces"`
	Constraints []interface{} `json:"constraints"`
	Rejected    interface{}   `json:"rejected"`
}

var csObserved []csAttemptRow
var csSleeps []uint64
var csExpansion uint8
var csDefault common.Hash

func csAttempt() {
	csObserved = append(csObserved, csAttemptRow{Leaders: []interface{}{}, Traces: []csTraceRow{}, Constraints: []interface{}{}})
}
func csLeaders(v []Node) {
	for _, n := range v {
		csObserved[len(csObserved)-1].Leaders = append(csObserved[len(csObserved)-1].Leaders, csNode(n))
	}
}
func csTrace(l common.Location, h common.Hash, first bool, e error) {
	a := &csObserved[len(csObserved)-1]
	a.Traces = append(a.Traces, csTraceRow{fcLoc(l), h.Hex(), first, e == nil})
}
func csReject(h common.Hash) { csObserved[len(csObserved)-1].Rejected = h.Hex() }
func csConstraints(m map[string]common.Hash) {
	v := ecCopy(m)
	for _, l := range fcLocations(csExpansion) {
		if _, ok := v[l.Name()]; !ok {
			v[l.Name()] = csDefault
		}
	}
	csObserved[len(csObserved)-1].Constraints = csMap(v, csExpansion)
}

var csVisits int
var csExtensions int

func csApplyStart()           { csVisits = 0; csExtensions = 0 }
func csSleep(d time.Duration) { csSleeps = append(csSleeps, uint64(d/time.Second)) }
func csMap(m map[string]common.Hash, e uint8) []interface{} {
	v := []interface{}{}
	seen := map[string]bool{}
	for _, l := range fcLocations(e) {
		if seen[l.Name()] {
			continue
		}
		seen[l.Name()] = true
		if h, ok := m[l.Name()]; ok {
			v = append(v, map[string]interface{}{"location": fcLoc(ecKey(l)), "hash": h.Hex()})
		}
	}
	return v
}
func csNode(n Node) map[string]interface{} {
	nums := []uint64{}
	for _, x := range n.number {
		nums = append(nums, x.Uint64())
	}
	entropy := interface{}(nil)
	if n.entropy != nil {
		entropy = n.entropy.String()
	}
	return map[string]interface{}{"location": fcLoc(n.location), "hash": n.hash.Hex(), "numbers": nums, "entropy": entropy}
}
func csNodes(ns []Node) []interface{} {
	r := []interface{}{}
	for _, n := range ns {
		r = append(r, csNode(n))
	}
	return r
}
func csLRU(capacity int) *lru.Cache[common.Hash, Node] {
	c, e := lru.New[common.Hash, Node](capacity)
	if e != nil {
		panic(e)
	}
	return c
}
func csCache(c *lru.Cache[common.Hash, Node]) []interface{} {
	v := []Node{}
	for _, h := range c.Keys() {
		n, _ := c.Peek(h)
		v = append(v, n)
	}
	return csNodes(v)
}
func csPending(capacity int) *PendingHeaders {
	p := &PendingHeaders{order: []*big.Int{}}
	var e error
	p.collection, e = lru.NewWithEvict[string, NodeSet](capacity, func(k string, _ NodeSet) { removeFromSlice(k, p) })
	if e != nil {
		panic(e)
	}
	return p
}
func csPendingState(hc *HierarchicalCoordinator) map[string]interface{} {
	keys := []string{}
	sets := []interface{}{}
	for _, k := range hc.pendingHeaders.order {
		keys = append(keys, k.String())
		n, ok := hc.pendingHeaders.collection.Peek(k.String())
		if !ok {
			panic("stale pending order")
		}
		sets = append(sets, map[string]interface{}{"key": k.String(), "digest": csSetDigest(n, hc.currentExpansionNumber)})
	}
	_, ready := hc.pendingHeaders.collection.Peek(hc.bestEntropy.String())
	return map[string]interface{}{"order": keys, "sets": sets, "best_ever": hc.bestEntropy.String(), "generation_ready": ready}
}
func csSetDigest(ns NodeSet, exp uint8) string {
	h := sha256.New()
	for _, l := range fcLocations(exp) {
		h.Write([]byte{byte(len(l))})
		h.Write(l)
		n := ns.nodes[l.Name()]
		h.Write(n.hash[:])
		for _, x := range n.number {
			var b [8]byte
			binary.BigEndian.PutUint64(b[:], x.Uint64())
			h.Write(b[:])
		}
		b := make([]byte, 32)
		n.entropy.FillBytes(b)
		h.Write(b)
	}
	return hex.EncodeToString(h.Sum(nil))
}
func csSearch(hc *HierarchicalCoordinator, root common.Hash) map[string]interface{} {
	csObserved = []csAttemptRow{}
	csExpansion = hc.currentExpansionNumber
	csDefault = root
	hc.PendingHeadersMap()
	sets := csPendingState(hc)
	return map[string]interface{}{"attempts": csObserved, "pending": sets, "accepted": len(hc.pendingHeaders.order) > 0}
}
func csRecent(hc *HierarchicalCoordinator, e uint8) []interface{} {
	out := []interface{}{}
	seen := map[string]bool{}
	for _, l := range fcLocations(e) {
		if len(l) != 2 || seen[l.Name()] {
			continue
		}
		seen[l.Name()] = true
		if c, ok := hc.recentBlocks[l.Name()]; ok {
			out = append(out, map[string]interface{}{"location": fcLoc(ecKey(l)), "nodes": csCache(c)})
		}
	}
	return out
}
func csPrepare(exp uint8, root *types.WorkObject, records []*types.WorkObject, orders map[common.Hash]int, terms func(common.Location, *types.WorkObject) *types.Termini, totals map[common.Hash]*big.Int) *HierarchicalCoordinator {
	con := &ecConsensus{views: map[string]quaiapi.Backend{}}
	for _, l := range fcLocations(exp) {
		if _, ok := con.views[l.Name()]; ok {
			continue
		}
		b := &csBackend{ecBackend: &ecBackend{headers: map[common.Hash]*types.WorkObject{}, terms: map[common.Hash]*types.Termini{}, genesis: map[common.Hash]bool{root.Hash(): true}, orders: orders}, config: &params.ChainConfig{DefaultGenesisHash: root.Hash()}, totals: totals}
		for _, w := range records {
			b.headers[w.Hash()] = w
			b.terms[w.Hash()] = terms(l, w)
		}
		con.views[l.Name()] = b
	}
	return &HierarchicalCoordinator{consensus: con, currentExpansionNumber: exp, recentBlocks: map[string]*lru.Cache[common.Hash, Node]{}, pendingHeaders: NewPendingHeaders(), bestEntropy: new(big.Int), generateHeaderWorkersCount: 2}
}
func csDataset(exp uint8, mode, fault int) (map[string]interface{}, *HierarchicalCoordinator, []*types.WorkObject) {
	root := types.EmptyWorkObject(2)
	root.WorkObjectHeader().SetDifficulty(fcNumber(2))
	root.Header().SetExtra([]byte{exp, byte(mode), byte(fault)})
	root.WorkObjectHeader().SetHeaderHash(root.Header().Hash())
	root = fcRound(root)
	records := []*types.WorkObject{root}
	orders := map[common.Hash]int{root.Hash(): 0}
	totals := map[common.Hash]*big.Int{root.Hash(): new(big.Int)}
	for _, l := range fcLocations(exp) {
		if len(l) != 2 {
			continue
		}
		for branch := 0; branch < 2; branch++ {
			p := root
			for depth := 1; depth <= 3; depth++ {
				w := types.EmptyWorkObject(2)
				w.WorkObjectHeader().SetDifficulty(fcNumber(2))
				w.WorkObjectHeader().SetLocation(l)
				w.Header().SetExpansionNumber(exp)
				w.Header().SetExtra([]byte{byte(branch), byte(depth)})
				for c := 0; c < 3; c++ {
					w.SetParentHash(p.Hash(), c)
					w.SetNumber(fcNumber(uint64(depth)), c)
				}
				w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
				w = fcRound(w)
				records = append(records, w)
				orders[w.Hash()] = [][]int{{2, 2, 2}, {2, 1, 2}, {0, 2, 1}, {1, 0, 2}}[mode][depth-1]
				totals[w.Hash()] = fcNumber(uint64(depth*100 + branch*10 + int(l[0])*3 + int(l[1])))
				p = w
			}
		}
	}
	hc := csPrepare(exp, root, records, orders, func(l common.Location, w *types.WorkObject) *types.Termini {
		t := types.EmptyTermini()
		for i := 0; i < 16; i++ {
			t.SetDomTerminiAtIndex(root.Hash(), i)
			t.SetSubTerminiAtIndex(root.Hash(), i)
		}
		if fault == 1 && len(l) == 1 {
			t.SetDomTerminiAtIndex(common.HexToHash("0xdead"), int(l[0]))
		}
		if fault == 2 && len(l) == 2 {
			t.SetDomTerminiAtIndex(common.HexToHash("0xbeef"), int(l[1]))
		}
		return &t
	}, totals)
	entries := []interface{}{}
	for _, l := range fcLocations(exp) {
		b := (*hc.consensus.GetBackend(l)).(*csBackend)
		for _, w := range records {
			entries = append(entries, map[string]interface{}{"location": fcLoc(ecKey(l)), "block": fcWire(w), "termini": ecTerm(*b.terms[w.Hash()]), "order": orders[w.Hash()], "entropy": totals[w.Hash()].String()})
		}
	}
	return map[string]interface{}{"expansion": exp, "legacy": false, "genesis": root.Hash().Hex(), "entries": entries}, hc, records
}
func csSeed(hc *HierarchicalCoordinator, records []*types.WorkObject, pattern int) []interface{} {
	inputs := []interface{}{}
	for _, w := range records {
		l := w.Location()
		if len(l) != 2 {
			continue
		}
		n := w.NumberU64(2)
		branch := int(w.Header().Extra()[0])
		take := pattern == 3 || pattern == 4 || pattern == 1 && branch == 0 && n == 3 || pattern == 2 && branch == 1 && n == 3
		if !take {
			continue
		}
		c, ok := hc.recentBlocks[l.Name()]
		if !ok {
			c = csLRU(1000)
			hc.recentBlocks[l.Name()] = c
		}
		b := *hc.consensus.GetBackend(l)
		node := Node{hash: w.Hash(), number: w.NumberArray(), location: l, entropy: b.TotalLogEntropy(w)}
		c.Add(node.hash, node)
		inputs = append(inputs, csNode(node))
	}
	return inputs
}
func NativeSearchVectors() map[string]interface{} {
	datasets := []interface{}{}
	searches := []interface{}{}
	for _, exp := range []uint8{1, 2} {
		for mode := 0; mode < 4; mode++ {
			for fault := 0; fault < 3; fault++ {
				ds, _, _ := csDataset(exp, mode, fault)
				idx := len(datasets)
				datasets = append(datasets, ds)
				for pattern := 0; pattern < 5; pattern++ {
					_, hc, records := csDataset(exp, mode, fault)
					seeds := csSeed(hc, records, pattern)
					r := csSearch(hc, records[0].Hash())
					searches = append(searches, map[string]interface{}{"dataset": idx, "seeds": seeds, "result": r, "recent_after": csRecent(hc, exp)})
				}
			}
		}
	}
	sorts := []interface{}{}
	for _, n := range []int{0, 1, 2, 12, 13, 16, 49, 50, 51, 64, 128, 255, 256, 999, 1000} {
		for pattern := 0; pattern < 9; pattern++ {
			hc := &HierarchicalCoordinator{currentExpansionNumber: 0, recentBlocks: map[string]*lru.Cache[common.Hash, Node]{}}
			cache := csLRU(1000)
			hc.recentBlocks[common.Location{0, 0}.Name()] = cache
			input := []Node{}
			for i := 0; i < n; i++ {
				value := (i*37 + 11) % n
				switch pattern {
				case 1:
					value = i
				case 2:
					value = n - i
				case 3:
					value = i % 3
				case 4:
					value = 0
				case 5:
					value = i / 7
				case 6:
					value = (n - i) / 7
				case 7:
					if i < n/2 {
						value = i
					} else {
						value = n - i
					}
				case 8:
					value = (i*i + 71*i + 113) % 17
				}
				node := Node{hash: common.BigToHash(fcNumber(uint64(i + 1))), number: []*big.Int{fcNumber(0), fcNumber(0), fcNumber(uint64(i))}, location: common.Location{0, 0}, entropy: fcNumber(uint64(value))}
				cache.Add(node.hash, node)
				input = append(input, node)
			}
			sorts = append(sorts, map[string]interface{}{"input": csNodes(input), "output": csNodes(hc.GetNodeListForLocation(common.Location{0, 0}, map[common.Hash]bool{}))})
		}
	}
	events := csEventVectors()
	replays := csReapplyVectors()
	hc := &HierarchicalCoordinator{pendingHeaders: NewPendingHeaders(), bestEntropy: new(big.Int), generateHeaderWorkersCount: 2, quitCh: make(chan struct{})}
	csSleeps = []uint64{}
	hc.ReapplicationLoop(core.ChainEvent{})
	return map[string]interface{}{"datasets": datasets, "searches": searches, "sorts": sorts, "events": events, "reapplications": replays, "sleeps": csSleeps, "leader_boundaries": csLeaderBoundaries()}
}
func csEventVectors() []interface{} {
	out := []interface{}{}
	for _, capacity := range []int{3, 1000} {
		_, hc, _ := csDataset(1, 0, 0)
		l := common.Location{0, 0}
		b := (*hc.consensus.GetBackend(l)).(*csBackend)
		root := b.headers[b.config.DefaultGenesisHash]
		c := csLRU(capacity)
		c.Add(root.Hash(), Node{hash: root.Hash(), number: root.NumberArray(), location: l, entropy: new(big.Int)})
		hc.recentBlocks[l.Name()] = c
		seed := csCache(c)
		steps := []interface{}{}
		records := []*types.WorkObject{}
		for i := 0; i < 1018; i++ {
			w := types.EmptyWorkObject(2)
			w.WorkObjectHeader().SetLocation(l)
			w.WorkObjectHeader().SetDifficulty(fcNumber(2))
			w.Header().SetExtra([]byte{byte(i), byte(i >> 8)})
			w.SetNumber(fcNumber(uint64(i+1)), 2)
			w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
			w = fcRound(w)
			records = append(records, w)
			value := uint64(i + 1)
			if i%113 == 0 {
				value = 0
			}
			b.headers[w.Hash()] = w
			b.totals[w.Hash()] = fcNumber(value)
			before := csCache(c)
			hc.ComputeMapPending(core.ChainEvent{Block: w})
			_, accepted := c.Peek(w.Hash())
			steps = append(steps, map[string]interface{}{"node": csNode(Node{hash: w.Hash(), number: w.NumberArray(), location: l, entropy: fcNumber(value)}), "accepted": accepted, "keys_before": csKeyState(before), "keys_after": csKeyState(csCache(c))})
		}
		for _, i := range []int{1005, 1003, 2, 1005} {
			w := records[i]
			before := csCache(c)
			hc.ComputeMapPending(core.ChainEvent{Block: w})
			steps = append(steps, map[string]interface{}{"node": csNode(Node{hash: w.Hash(), number: w.NumberArray(), location: l, entropy: b.totals[w.Hash()]}), "keys_before": csKeyState(before), "keys_after": csKeyState(csCache(c))})
		}
		out = append(out, map[string]interface{}{"capacity": capacity, "seed": seed, "steps": steps})
	}
	return out
}
func csKeyState(nodes []interface{}) map[string]interface{} {
	h := sha256.New()
	oldest := interface{}(nil)
	newest := interface{}(nil)
	for i, v := range nodes {
		node := v.(map[string]interface{})
		id := node["hash"].(string)
		b, e := hex.DecodeString(id[2:])
		if e != nil {
			panic(e)
		}
		h.Write(b)
		if i == 0 {
			oldest = id
		}
		newest = id
	}
	return map[string]interface{}{"sha256": hex.EncodeToString(h.Sum(nil)), "length": len(nodes), "oldest": oldest, "newest": newest}
}
func csReapplyVectors() []interface{} {
	out := []interface{}{}
	for _, capacity := range []int{4, 2000} {
		for _, exp := range []uint8{0, 1, 2, 4, 30} {
			for order := 0; order < 3; order++ {
				hc := &HierarchicalCoordinator{currentExpansionNumber: exp, pendingHeaders: csPending(capacity), bestEntropy: new(big.Int), generateHeaderWorkersCount: 2}
				for i := 0; i < 60; i++ {
					ns := fcSet(exp, i+1)
					hc.Add(ns.Entropy(int(csRegions(exp)), int(csZones(exp))), ns, hc.pendingHeaders)
				}
				before := csPendingState(hc)
				w := types.EmptyWorkObject(2)
				w.WorkObjectHeader().SetLocation(common.Location{0, 0})
				w.WorkObjectHeader().SetDifficulty(fcNumber(2))
				w.Header().SetExpansionNumber(exp)
				last, _ := hc.pendingHeaders.collection.Peek(hc.pendingHeaders.order[len(hc.pendingHeaders.order)-1].String())
				for c := 0; c < 3; c++ {
					var l common.Location
					if c == 1 {
						l = common.Location{0}
					} else if c == 2 {
						l = common.Location{0, 0}
					}
					w.SetParentHash(last.nodes[l.Name()].hash, c)
					w.SetNumber(fcNumber(uint64(999+c)), c)
				}
				w.WorkObjectHeader().SetHeaderHash(w.Header().Hash())
				w = fcRound(w)
				steps := []interface{}{}
				for _, entropy := range []uint64{1111, 1111, 2222} {
					hc.BuildPendingHeaders(w, order, fcNumber(entropy))
					steps = append(steps, map[string]interface{}{"entropy": fmt.Sprint(entropy), "after": csPendingState(hc), "visited": csVisits, "extended": csExtensions})
				}
				out = append(out, map[string]interface{}{"expansion": exp, "legacy": exp > 6, "capacity": capacity, "initial_seeds": csSeedNumbers(60), "before": before, "block": fcWire(w), "order": order, "steps": steps})
			}
		}
	}
	return out
}
func csRegions(e uint8) uint64 { r, _ := common.GetHierarchySizeForExpansionNumber(e); return r }
func csZones(e uint8) uint64   { _, z := common.GetHierarchySizeForExpansionNumber(e); return z }
func NativeSearchReplay(v map[string]interface{}) map[string]interface{} {
	root := fcDecode(v["root"].(string))
	records := []*types.WorkObject{root}
	orders := map[common.Hash]int{root.Hash(): 0}
	totals := map[common.Hash]*big.Int{root.Hash(): new(big.Int)}
	for _, s := range v["steps"].([]interface{}) {
		step := s.(map[string]interface{})
		w := fcDecode(step["block"].(string))
		records = append(records, w)
		orders[w.Hash()] = 0
		n, ok := new(big.Int).SetString(step["metrics"].(map[string]interface{})["total"].(string), 10)
		if !ok {
			panic("native total")
		}
		totals[w.Hash()] = n
	}
	terms := map[string]map[common.Hash]*types.Termini{}
	for i, l := range []common.Location{{}, {0}, {0, 0}} {
		m := map[common.Hash]*types.Termini{}
		m[root.Hash()] = ecTermDecode(v["seeds"].([]interface{})[i].(map[string]interface{})["snapshot"].(map[string]interface{})["termini"].(string))
		for _, s := range v["steps"].([]interface{}) {
			step := s.(map[string]interface{})
			w := fcDecode(step["block"].(string))
			m[w.Hash()] = ecTermDecode(step["contexts"].([]interface{})[i].(map[string]interface{})["snapshot"].(map[string]interface{})["termini"].(string))
		}
		terms[l.Name()] = m
	}
	searches := []interface{}{}
	for pattern := 0; pattern < 16; pattern++ {
		hc := csPrepare(0, root, records, orders, func(l common.Location, w *types.WorkObject) *types.Termini { return terms[l.Name()][w.Hash()] }, totals)
		seeds := []interface{}{}
		for i, w := range records[1:] {
			if pattern != 15 && pattern&(1<<uint(i%4)) == 0 {
				continue
			}
			l := common.Location{0, 0}
			c, ok := hc.recentBlocks[l.Name()]
			if !ok {
				c = csLRU(1000)
				hc.recentBlocks[l.Name()] = c
			}
			node := Node{hash: w.Hash(), number: w.NumberArray(), location: l, entropy: totals[w.Hash()]}
			c.Add(node.hash, node)
			seeds = append(seeds, csNode(node))
		}
		searches = append(searches, map[string]interface{}{"seeds": seeds, "result": csSearch(hc, root.Hash()), "recent_after": csRecent(hc, 0)})
	}
	reapply := []interface{}{}
	for _, capacity := range []int{2, 2000} {
		hc := &HierarchicalCoordinator{currentExpansionNumber: 0, pendingHeaders: csPending(capacity), bestEntropy: new(big.Int), generateHeaderWorkersCount: 2}
		ns := NodeSet{nodes: map[string]Node{}}
		for _, l := range []common.Location{{}, {0}, {0, 0}} {
			ns.nodes[l.Name()] = Node{hash: root.Hash(), number: root.NumberArray(), location: l, entropy: new(big.Int)}
		}
		hc.Add(new(big.Int), ns, hc.pendingHeaders)
		before := csPendingState(hc)
		steps := []interface{}{}
		for i, w := range records[1:] {
			hc.BuildPendingHeaders(w, 0, totals[w.Hash()])
			steps = append(steps, map[string]interface{}{"index": i, "after": csPendingState(hc), "visited": csVisits, "extended": csExtensions})
		}
		reapply = append(reapply, map[string]interface{}{"capacity": capacity, "before": before, "steps": steps})
	}
	hc := csPrepare(0, root, records, orders, func(l common.Location, w *types.WorkObject) *types.Termini { return terms[l.Name()][w.Hash()] }, totals)
	l := common.Location{0, 0}
	cache := csLRU(1000)
	seed := Node{hash: root.Hash(), number: root.NumberArray(), location: l, entropy: new(big.Int)}
	cache.Add(seed.hash, seed)
	hc.recentBlocks[l.Name()] = cache
	events := []interface{}{}
	for _, i := range []int{0, 1, 2, 3, 4, 5, 3, 0, 5} {
		w := records[i+1]
		hc.ComputeMapPending(core.ChainEvent{Block: w})
		events = append(events, map[string]interface{}{"index": i, "node": csNode(Node{hash: w.Hash(), number: w.NumberArray(), location: l, entropy: totals[w.Hash()]}), "accepted": true, "after": csKeyState(csCache(cache))})
	}
	return map[string]interface{}{"input": v, "searches": searches, "reapplications": reapply, "events": events, "event_seed": csNode(seed)}
}

var _ quai.ConsensusAPI

func csSeedNumbers(n int) []int {
	v := []int{}
	for i := 1; i <= n; i++ {
		v = append(v, i)
	}
	return v
}

func csTryLeaders(hc *HierarchicalCoordinator, bad map[common.Hash]bool) (nodes []Node, panicked bool) {
	defer func() {
		if recover() != nil {
			panicked = true
		}
	}()
	nodes = hc.CalculateLeaders(bad)
	return
}
func csLeaderBoundaries() []interface{} {
	out := []interface{}{}
	for _, exp := range []uint8{0, 1} {
		hc := &HierarchicalCoordinator{currentExpansionNumber: exp, recentBlocks: map[string]*lru.Cache[common.Hash, Node]{}}
		seeds := []interface{}{}
		bad := map[common.Hash]bool{}
		hs := []string{}
		for _, l := range fcLocations(exp) {
			if len(l) != 2 {
				continue
			}
			h := common.BigToHash(fcNumber(uint64(len(seeds) + 1)))
			n := Node{hash: h, number: []*big.Int{fcNumber(0), fcNumber(0), fcNumber(1)}, location: l, entropy: fcNumber(1)}
			c := csLRU(1000)
			c.Add(h, n)
			hc.recentBlocks[l.Name()] = c
			seeds = append(seeds, csNode(n))
			bad[h] = true
			hs = append(hs, h.Hex())
		}
		nodes, panicked := csTryLeaders(hc, bad)
		empty := false
		for _, n := range nodes {
			empty = empty || n.Empty()
		}
		out = append(out, map[string]interface{}{"expansion": exp, "seeds": seeds, "bad": hs, "source_panic": panicked, "source_empty": empty, "strict_reject": true})
	}
	return out
}
