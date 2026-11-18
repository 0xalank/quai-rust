// Offline-only projections through actual core methods. Not a node or proof oracle.
package core

import (
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	lru "github.com/hashicorp/golang-lru/v2"
	"math/big"
)

func nativeAdmissionHC(context int, engine consensus.Engine) *HeaderChain {
	location := common.Location{}
	if context == 1 {
		location = common.Location{0}
	}
	if context == 2 {
		location = common.Location{0, 0}
	}
	config := *params.TestChainConfig
	config.Location = location
	db := rawdb.NewMemoryDatabase(log.Global)
	cache, _ := lru.New[common.Hash, types.WorkObject](8)
	orders, _ := lru.New[common.Hash, calcOrderResponse](8)
	return &HeaderChain{config: &config, powConfig: params.PowConfig{NodeLocation: location, DurationLimit: big.NewInt(5), MinDifficulty: big.NewInt(2), GasCeil: 30000000}, bc: &BodyDb{chainConfig: &config, db: db}, headerDb: db, headerCache: cache, calcOrderCache: orders, engine: []consensus.Engine{engine, engine}, logger: log.Global}
}
