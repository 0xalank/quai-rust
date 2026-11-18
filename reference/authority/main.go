// Offline transition-boundary oracle. NewMessage supplies an authenticated
// sender; no key or signature-recovery bypass exists in the Rust public API.
package main

import (
	"encoding/hex"
	"encoding/json"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/core/vm"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"google.golang.org/protobuf/proto"
	"math/big"
	"os"
)

var referenceRevision string

func u(n uint64) *big.Int { return new(big.Int).SetUint64(n) }
func hx(b []byte) string  { return "0x" + hex.EncodeToString(b) }
func must(e error) {
	if e != nil {
		panic(e)
	}
}
func main() {
	rows := []interface{}{}
	loc := common.Location{0, 0}
	sender := common.HexToAddress("0x00640d82ef6552085e494df2a2eaec18d8215913", loc)
	target := common.HexToAddress("0x0000000000000000000000000000000000001101", loc)
	for _, height := range []uint64{6307199, 6307200, 6307201} {
		for _, kind := range []string{"update", "zero", "leading-zero", "freeze", "unfreeze", "bad", "creation"} {
			disk := rawdb.NewMemoryDatabase(log.Global)
			cache := state.NewDatabase(disk)
			s, e := state.New(types.EmptyRootHash, types.EmptyRootHash, u(0), cache, cache, nil, loc, log.Global)
			must(e)
			ia, _ := sender.InternalAddress()
			it, _ := target.InternalAddress()
			s.SetNonce(ia, 7)
			s.SetBalance(ia, new(big.Int).Lsh(u(1), 128))
			s.SetNonce(it, 1)
			s.SetBalance(it, u(100))
			before, e := s.Commit(false)
			must(e)
			must(cache.TrieDB().Commit(before, false, nil))
			s, e = state.New(before, types.EmptyRootHash, u(0), cache, cache, nil, loc, log.Global)
			must(e)
			data := []byte(kind)
			switch kind {
			case "update":
				data = append([]byte("update"), []byte{1, 2, 3}...)
			case "zero":
				data = append([]byte("update"), 0)
			case "leading-zero":
				data = append([]byte("update"), []byte{0, 0, 1}...)
			case "bad":
				data = []byte("update")
			}
			to := &target
			if kind == "creation" {
				to = nil
				data = []byte("freeze")
			}
			tx := types.NewTx(&types.QuaiTx{ChainID: u(1337), Nonce: 7, GasPrice: u(17), Gas: 100000, To: to, Value: u(13), Data: data, V: u(0), R: u(0), S: u(0)})
			p, e := tx.ProtoEncode()
			must(e)
			wire, e := proto.Marshal(p)
			must(e)
			msg := types.NewMessage(sender, to, 7, u(13), 100000, u(17), data, types.AccessList{}, false)
			ctx := vm.BlockContext{CanTransfer: core.CanTransfer, Transfer: core.Transfer, BlockNumber: u(height), Time: u(1769112000), Difficulty: u(1), BaseFee: u(13), QuaiStateSize: u(1 << 20), GasLimit: 30000000, PrimeTerminusNumber: 2444574}
			config := *params.TestChainConfig
			config.Location = loc
			config.ChainID = u(1337)
			batch := disk.NewBatch()
			batch.SetPending(true)
			env := vm.NewEVM(ctx, vm.TxContext{Origin: sender, GasPrice: u(17), Hash: tx.Hash()}, s, &config, vm.Config{}, batch)
			gp := new(types.GasPool).AddGas(20000000)
			s.Prepare(tx.Hash(), 0)
			r, e := core.ApplyMessage(env, msg, gp)
			must(e)
			s.Finalize(true)
			rows = append(rows, map[string]interface{}{"id": fmt.Sprintf("authority-%d-%s", height, kind), "height": height, "sender": hx(sender.Bytes()), "transaction": hx(wire), "before_root": before.Hex(), "root": s.IntermediateRoot(true).Hex(), "etx_root": s.ETXRoot().Hex(), "gas_used": r.UsedGas, "state_used": r.UsedState, "fee": hx(r.QuaiFees.Bytes()), "gas_remaining": gp.Gas(), "status": r.Err == nil})
			disk.Close()
		}
	}
	json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"reference_revision": referenceRevision, "vectors": rows, "scope": "Authenticated-message authority transition, not signature recovery. Rust production entrypoint still requires signature recovery."})
}
