// Offline benchmark adapter for the quai-rust replay comparison (ROADMAP
// Phase 4). `go build -overlay` adds this file to package progpow of the
// pinned go-quai v0.57.0 checkout; the checkout itself is not modified.
//
// NativeBenchEpoch is one epoch's verification cache, generated as
// cache.generate does without a cache directory, and the light kernel
// (progpowLight) on it, without the header and hash caches: the cost of one
// light verification, and its result for the verdict comparison with Rust.
package progpow

import (
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/log"
)

// NativeBenchEpochLength is C_epochLength, in prime terminus numbers.
func NativeBenchEpochLength() uint64 { return C_epochLength }

// NativeBenchEpoch holds the verification cache of one epoch.
type NativeBenchEpoch struct {
	Epoch uint64
	cache []uint32
	cDag  []uint32
}

// NativeBenchNewEpoch generates the cache of the epoch of block number
// `number` (a prime terminus number) as cache.generate does with no cache
// directory.
func NativeBenchNewEpoch(number uint64) *NativeBenchEpoch {
	epoch := number / C_epochLength
	size := cacheSize(epoch*C_epochLength + 1)
	seed := seedHash(epoch*C_epochLength + 1)
	e := &NativeBenchEpoch{Epoch: epoch, cache: make([]uint32, size/4), cDag: make([]uint32, progpowCacheWords)}
	generateCache(e.cache, epoch, seed, log.Global)
	generateCDag(e.cDag, e.cache, epoch, log.Global)
	return e
}

// Light runs the light kernel for one input: the mix digest and the
// proof-of-work hash.
func (e *NativeBenchEpoch) Light(seal common.Hash, nonce, number uint64) (common.Hash, common.Hash) {
	digest, result := progpowLight(datasetSize(number), e.cache, seal.Bytes(), nonce, number, e.cDag)
	return common.BytesToHash(digest), common.BytesToHash(result)
}
