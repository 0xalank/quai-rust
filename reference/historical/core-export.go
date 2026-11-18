// Offline observation adapter for official allocation, redemption and trim code.
package core

import (
	"bytes"
	"encoding/json"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/crypto/multiset"
	"github.com/dominant-strategies/go-quai/params"
	"os"
	"path/filepath"
	"sync"
)

func NativeHistoricalVectors(source string) map[string]interface{} {
	allocation := []interface{}{}
	var allocs []params.GenesisAccount
	b, e := os.ReadFile(filepath.Join(source, "params/genesis_alloc.json"))
	niMust(e)
	niMust(json.Unmarshal(b, &allocs))
	forfeiture, e := params.LoadForfeitureAddresses(filepath.Join(source, "params/forfeiture_addresses.json"))
	niMust(e)
	founder, e := params.LoadForfeitureAddresses(filepath.Join(source, "params/founder_forfeiture_addresses.json"))
	niMust(e)
	for _, height := range []uint64{1, 2, 518400, 6220800, 6739200, 12960000, 33696000} {
		for _, prime := range []uint64{1530499, 1530500, 2444573, 2444574} {
			hc := nativeHC(2)
			cache := state.NewDatabase(hc.headerDb)
			s, e := state.New(types.EmptyRootHash, types.EmptyRootHash, niU(0), cache, cache, nil, common.Location{0, 0}, hc.logger)
			niMust(e)
			h := types.EmptyWorkObject(2)
			h.SetNumber(niU(height), 2)
			h.WorkObjectHeader().SetPrimeTerminusNumber(niU(prime))
			niMust(s.AddLockedBalances(h, allocs, forfeiture, founder, hc.logger))
			root := s.IntermediateRoot(true)
			allocation = append(allocation, map[string]interface{}{"height": height, "prime": prime, "root": root.Hex(), "accounts": s.GetQuaiTrieSize().Uint64()})
			hc.headerDb.Close()
		}
	}
	redemption := []interface{}{}
	for _, height := range []uint64{241920, 241921, 1036800, 4000000, 6307201, 9000000} {
		for _, exists := range []bool{false, true} {
			for _, small := range []bool{false, true} {
				hc := nativeHC(2)
				hc.bc = NewTestBodyDb(hc.headerDb)
				hc.bc.chainConfig = hc.config
				cache := state.NewDatabase(hc.headerDb)
				s, e := state.New(types.EmptyRootHash, types.EmptyRootHash, niU(0), cache, cache, nil, common.Location{0, 0}, hc.logger)
				niMust(e)
				to := niAddress(common.Location{0, 0}, 1, false)
				if exists {
					a, _ := to.InternalAddress()
					s.SetNonce(a, 1)
					s.SetBalance(a, niU(100))
				}
				before := s.IntermediateRoot(false)
				sources := []interface{}{}
				value := uint64(1000000)
				if small {
					value = 1
				}
				for lock, depth := range []uint64{241920, 1555200, 3110400, 6307200} {
					if height <= depth {
						continue
					}
					txs := types.Transactions{}
					for _, kind := range []uint64{1, 2} {
						data := append([]byte{byte(lock)}, make([]byte, 32)...)
						txs = append(txs, types.NewTx(&types.ExternalTx{To: &to, Gas: 200000, Value: niU(value), Data: data, Sender: to, EtxType: kind, OriginatingTxHash: common.Hash{31: byte(lock + 1)}}))
					}
					target := types.EmptyWorkObject(2)
					target.WorkObjectHeader().SetLocation(common.Location{0, 0})
					target.SetNumber(niU(height-depth), 2)
					target.Body().SetTransactions(txs)
					hc.bc.WriteBlock(target, 2)
					rawdb.WriteTermini(hc.headerDb, target.Hash(), types.EmptyTermini())
					rawdb.WriteCanonicalHash(hc.headerDb, target.Hash(), height-depth)
					wires := []string{}
					for _, tx := range txs {
						p, e := tx.ProtoEncode()
						niMust(e)
						var b bytes.Buffer
						_ = p
						_ = b
						raw, e := niMarshal(tx)
						niMust(e)
						wires = append(wires, niHex(raw))
					}
					sources = append(sources, map[string]interface{}{"height": height - depth, "transactions": wires})
				}
				h := types.EmptyWorkObject(2)
				h.SetNumber(niU(height), 2)
				parent := types.EmptyWorkObject(2)
				parent.Header().SetQuaiStateSize(niU(1 << 20))
				unlocks, e := RedeemLockedQuai(hc, h, parent, s, nil)
				niMust(e)
				items := []interface{}{}
				for _, v := range unlocks {
					items = append(items, map[string]interface{}{"address": niHex(v.Addr[:]), "amount": niHex(v.Amt.Bytes())})
				}
				redemption = append(redemption, map[string]interface{}{"id": fmt.Sprintf("redeem-%d-%t-%t", height, exists, small), "height": height, "exists": exists, "small": small, "before_root": before.Hex(), "root": s.IntermediateRoot(true).Hex(), "sources": sources, "unlocks": items})
				hc.headerDb.Close()
			}
		}
	}
	trims := []interface{}{}
	for _, prime := range []uint64{2236999, 2237000} {
		for _, spent := range []bool{false, true} {
			for _, duplicate := range []bool{false, true} {
				hc := nativeHC(2)
				batch := hc.headerDb.NewBatch()
				batch.SetPending(true)
				point := common.Hash{31: 1}
				to := niAddress(common.Location{0, 0}, 1, true)
				utxo := types.NewUtxoEntry(types.NewTxOut(0, to.Bytes(), niU(0)))
				niMust(rawdb.CreateUTXO(hc.headerDb, point, 0, utxo))
				locked := types.NewUtxoEntry(types.NewTxOut(0, to.Bytes(), niU(1)))
				niMust(rawdb.CreateUTXO(hc.headerDb, point, 1, locked))
				blockHash := common.Hash{31: 2}
				keys := [][]byte{rawdb.UtxoKeyWithDenomination(point, 0, 0), rawdb.UtxoKeyWithDenomination(point, 1, 0)}
				if duplicate {
					keys = append(keys, keys[0])
				}
				niMust(rawdb.WriteCreatedUTXOKeys(hc.headerDb, blockHash, keys))
				hash := types.UTXOHash(point, 0, utxo)
				deleted := []common.Hash{}
				set := multiset.New()
				set.Add(hash[:])
				lockedHash := types.UTXOHash(point, 1, locked)
				set.Add(lockedHash[:])
				size := uint64(2)
				if spent {
					deleted = append(deleted, hash)
					size--
					batch.Delete(rawdb.UtxoKey(point, 0))
				}
				trimmed := []*types.SpentUtxoEntry{}
				removed := niU(0)
				hc.trimBlock(batch, 0, 1, blockHash, &deleted, newTrimDeleteSet(prime, deleted), &trimmed, removed, &size, true, new(sync.Mutex), hc.logger)
				for _, h := range deleted {
					set.Remove(h[:])
				}
				trims = append(trims, map[string]interface{}{"id": fmt.Sprintf("trim-%d-%t-%t", prime, spent, duplicate), "prime": prime, "spent": spent, "duplicate": duplicate, "root": set.Hash().Hex(), "size": size, "trimmed": len(trimmed), "removed": niHex(removed.Bytes())})
				hc.headerDb.Close()
			}
		}
	}
	return map[string]interface{}{"allocations": allocation, "redemptions": redemption, "trims": trims}
}
