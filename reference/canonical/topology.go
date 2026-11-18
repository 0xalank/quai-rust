// Actual canonical topology transitions; execution roots come from the separately
// qualified native StateProcessor corpus. State application is disabled here.
package core

import (
	"encoding/hex"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	lru "github.com/hashicorp/golang-lru/v2"
	"google.golang.org/protobuf/proto"
)

func ctDecode(s string) *types.WorkObject {
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
func NativeCanonicalTopology(v map[string]interface{}) {
	db := rawdb.NewMemoryDatabase(log.Global)
	defer db.Close()
	config := *params.TestChainConfig
	config.Location = common.Location{0, 0}
	bc := NewTestBodyDb(db)
	bc.chainConfig = &config
	bc.logger = log.Global
	cache, _ := lru.New[common.Hash, types.WorkObject](128)
	nums, _ := lru.New[common.Hash, uint64](128)
	g := ctDecode(v["genesis"].(string))
	hc := &HeaderChain{config: &config, bc: bc, headerDb: db, headerCache: cache, numberCache: nums, genesisHeader: g, logger: log.Global}
	hc.currentHeader.Store(g)
	rawdb.WriteGenesisHashes(db, common.Hashes{g.Hash()})
	store := func(w *types.WorkObject) {
		cache.Add(w.Hash(), *w)
		bc.WriteBlock(w, 2)
		rawdb.WriteTermini(db, w.Hash(), types.EmptyTermini())
	}
	store(g)
	rawdb.WriteCanonicalHash(db, g.Hash(), 0)
	rawdb.WriteHeadBlockHash(db, g.Hash())
	blocks := map[common.Hash]*types.WorkObject{g.Hash(): g}
	for _, br := range v["branches"].([]interface{}) {
		for _, rr := range br.(map[string]interface{})["vectors"].([]interface{}) {
			w := ctDecode(rr.(map[string]interface{})["work_object"].(string))
			blocks[w.Hash()] = w
			store(w)
		}
	}
	for _, ev := range v["canonical_events"].([]interface{}) {
		e := ev.(map[string]interface{})
		head := blocks[common.HexToHash(e["head"].(string))]
		if err := hc.SetCurrentHeader(head); err != nil {
			panic(err)
		}
		hashes := []string{}
		for n := uint64(0); n <= head.NumberU64(2); n++ {
			hashes = append(hashes, rawdb.ReadCanonicalHash(db, n).Hex())
		}
		e["canonical_hashes"] = hashes
		e["head_number"] = head.NumberU64(2)
	}
}
