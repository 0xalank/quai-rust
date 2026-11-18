// Time adapter for the transaction pool oracle (reference/txpool): the pool
// expires queued and pending transactions by their heartbeat and time, and
// Qi transactions by their receive time. The oracle ages them instead of
// waiting for the production lifetimes.
package types

import "time"

// OracleSetTxTime sets the time the transaction was first seen.
func OracleSetTxTime(tx *Transaction, t time.Time) { tx.time = t }

// OracleSetReceived sets the time a Qi pool entry was received.
func OracleSetReceived(tx *TxWithMinerFee, t time.Time) { tx.received = t }
