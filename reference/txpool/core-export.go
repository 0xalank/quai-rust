// Step-control and observation adapter for the transaction pool oracle
// (reference/txpool). It adds no pool logic: it runs the production
// TxPool.runReorg (reset, promoteExecutables, demoteUnexecutables,
// truncatePending, truncateQueue) at the oracle's explicit steps instead of
// at the scheduler's 1-second ticker, shortens the eviction and Qi expiry
// tickers of the production loops, ages heartbeats, transaction times and
// Qi receive times, and reads pool fields that have no exported getter.
package core

import (
	"bytes"
	"sort"
	"time"

	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/ethdb"
)

// OracleTxPoolTimers sets the production loops' eviction and Qi expiry
// intervals. Call it before NewTxPool.
func OracleTxPoolTimers(eviction, qiExpiration time.Duration) {
	evictionInterval = eviction
	qiExpirationCheckInterval = qiExpiration
}

// OracleHeaderChain is a HeaderChain with only the database that
// CheckIfEtxIsEligible and IsGenesisHash read.
func OracleHeaderChain(db ethdb.Database) *HeaderChain {
	return &HeaderChain{headerDb: db}
}

// OracleRunReorg runs one production runReorg as scheduleReorgLoop launches
// it: a reset from oldHead to newHead when newHead is not nil, and the dirty
// accounts the adds since the last run reported.
func OracleRunReorg(pool *TxPool, oldHead, newHead *types.WorkObject, dirty []common.InternalAddress) {
	var reset *txpoolResetRequest
	if newHead != nil {
		reset = &txpoolResetRequest{oldHead, newHead}
	}
	var set *accountSet
	if dirty != nil {
		set = newAccountSet(pool.signer, dirty...)
	}
	done := make(chan struct{})
	pool.runReorg(done, make(chan struct{}), reset, set, map[common.InternalAddress]*txSortedMap{}, nil)
	<-done
}

// OracleHas reports whether the pool holds a transaction of addr with this
// nonce in its pending or queued list (what add's replaced flag tests).
func OracleHas(pool *TxPool, addr common.InternalAddress, nonce uint64) bool {
	pool.mu.RLock()
	defer pool.mu.RUnlock()
	if l := pool.pending[addr]; l != nil && l.txs.Get(nonce) != nil {
		return true
	}
	if l := pool.queue[addr]; l != nil && l.txs.Get(nonce) != nil {
		return true
	}
	return false
}

// OracleAgeBeat moves the heartbeat of addr back by ago.
func OracleAgeBeat(pool *TxPool, addr common.InternalAddress, ago time.Duration) bool {
	pool.mu.Lock()
	defer pool.mu.Unlock()
	if _, ok := pool.beats[addr]; !ok {
		return false
	}
	pool.beats[addr] = time.Now().Add(-ago)
	return true
}

// OracleAgeTx moves the time of a pooled Quai transaction back by ago.
func OracleAgeTx(pool *TxPool, hash common.Hash, ago time.Duration) bool {
	pool.mu.Lock()
	defer pool.mu.Unlock()
	tx := pool.all.Get(hash)
	if tx == nil {
		return false
	}
	types.OracleSetTxTime(tx, time.Now().Add(-ago))
	return true
}

// OracleAgeQi moves the receive time of a Qi pool entry back by ago.
func OracleAgeQi(pool *TxPool, hash common.Hash, ago time.Duration) bool {
	entry, ok := pool.qiPool.Peek(hash)
	if !ok {
		return false
	}
	types.OracleSetReceived(entry, time.Now().Add(-ago))
	return true
}

// OracleSharingReady reports whether every sharing client is connected.
func OracleSharingReady(pool *TxPool) bool {
	pool.sharingClientMu.RLock()
	defer pool.sharingClientMu.RUnlock()
	for _, c := range pool.poolSharingClients {
		if c == nil {
			return false
		}
	}
	return true
}

// OraclePoolState is the pool content the oracle records after each step.
type OraclePoolState struct {
	Pending     map[common.InternalAddress][]common.Hash
	Queued      map[common.InternalAddress][]common.Hash
	Qi          []common.Hash
	Locals      []common.InternalAddress
	LocalCount  int
	RemoteCount int
	Slots       int
	Beats       []common.InternalAddress
}

// OracleSnapshot reads the pool under its lock.
func OracleSnapshot(pool *TxPool) OraclePoolState {
	pool.mu.RLock()
	defer pool.mu.RUnlock()
	s := OraclePoolState{
		Pending: map[common.InternalAddress][]common.Hash{},
		Queued:  map[common.InternalAddress][]common.Hash{},
	}
	for addr, list := range pool.pending {
		for _, tx := range list.Flatten() {
			s.Pending[addr] = append(s.Pending[addr], tx.Hash())
		}
		if list.Empty() {
			s.Pending[addr] = []common.Hash{}
		}
	}
	for addr, list := range pool.queue {
		for _, tx := range list.Flatten() {
			s.Queued[addr] = append(s.Queued[addr], tx.Hash())
		}
		if list.Empty() {
			s.Queued[addr] = []common.Hash{}
		}
	}
	for _, entry := range pool.qiPool.Values() {
		s.Qi = append(s.Qi, entry.Tx().Hash())
	}
	s.Locals = pool.locals.flatten()
	sort.Slice(s.Locals, func(i, j int) bool { return bytes.Compare(s.Locals[i][:], s.Locals[j][:]) < 0 })
	s.LocalCount = pool.all.LocalCount()
	s.RemoteCount = pool.all.RemoteCount()
	s.Slots = pool.all.Slots()
	for addr := range pool.beats {
		s.Beats = append(s.Beats, addr)
	}
	sort.Slice(s.Beats, func(i, j int) bool { return bytes.Compare(s.Beats[i][:], s.Beats[j][:]) < 0 })
	return s
}
