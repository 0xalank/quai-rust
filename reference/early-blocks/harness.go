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

// The arithmetic oracle supplies a hash. Real kernel proofs use separate vectors.
// This engine is never a Rust runtime mode and does not qualify a valid block.
type nativeProjectionEngine struct{}

func (nativeProjectionEngine) Seal(*types.WorkObject, chan<- *types.WorkObject, <-chan struct{}) error {
	return nil
}
func (nativeProjectionEngine) SetThreads(int) {}
func (nativeProjectionEngine) ComputePowHash(h *types.WorkObjectHeader) (common.Hash, error) {
	return h.MixHash(), nil
}
func (nativeProjectionEngine) ComputePowLight(h *types.WorkObjectHeader) (common.Hash, common.Hash) {
	return h.MixHash(), h.MixHash()
}
func nativeHC(context int) *HeaderChain {
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
	return &HeaderChain{config: &config, powConfig: params.PowConfig{NodeLocation: location, DurationLimit: big.NewInt(5), MinDifficulty: big.NewInt(1)}, bc: &BodyDb{chainConfig: &config, db: db}, headerDb: db, headerCache: cache, calcOrderCache: orders, engine: []consensus.Engine{nativeProjectionEngine{}, nativeProjectionEngine{}}, logger: log.Global}
}
