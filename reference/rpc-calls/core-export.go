// Observation-only adapter: a HeaderChain with only the database, number
// cache and body database that HeaderChain.CalcBaseFee reads, so the oracle
// runs the production CalcBaseFee on controlled blocks.
package core

import (
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/ethdb"
	lru "github.com/hashicorp/golang-lru/v2"
)

func NativeRPCCallsHeaderChain(db ethdb.Database) *HeaderChain {
	hc := &HeaderChain{headerDb: db, bc: NewTestBodyDb(db)}
	numberCache, _ := lru.New[common.Hash, uint64](numberCacheLimit)
	hc.numberCache = numberCache
	return hc
}
