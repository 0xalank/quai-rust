// Local-only proof-of-work driver. It submits no transactions and holds no keys.
package main

import (
	"context"
	"crypto/ecdsa"
	"encoding/json"
	"flag"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/common/hexutil"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/rpc"
	"google.golang.org/protobuf/proto"
	"lukechampine.com/blake3"
	"math/big"
	"net/url"
	"os"
	"time"
)

// Deliberately public deterministic test identity; never usable with real funds.
func testKey() *ecdsa.PrivateKey {
	for seed := int64(1); ; seed++ {
		key, err := crypto.HexToECDSA(fmt.Sprintf("%064x", seed))
		if err != nil {
			panic(err)
		}
		address := crypto.PubkeyToAddress(key.PublicKey, common.Location{0, 0})
		if address.Bytes()[0] == 0 && address.IsInQuaiLedgerScope() {
			return key
		}
	}
}

func run() error {
	endpoint := flag.String("url", "", "isolated loopback node")
	genesis := flag.String("genesis", "", "expected local genesis hash")
	count := flag.Int("blocks", 1, "number of local blocks (1..16)")
	writeFixture := flag.String("write-fixture", "", "write local test allocations and print public identity")
	send := flag.Bool("send", false, "submit a deterministic local test transfer before mining")
	flag.Parse()
	if *writeFixture != "" {
		address := crypto.PubkeyToAddress(testKey().PublicKey, common.Location{0, 0})
		allocation := []interface{}{map[string]interface{}{"unlockSchedule": 1, "address": address.Hex(), "award": new(big.Int).Exp(big.NewInt(10), big.NewInt(27), nil), "vested": 0, "lumpSumMonth": 0, "balanceSchedule": map[string]interface{}{"0": new(big.Int).Exp(big.NewInt(10), big.NewInt(27), nil)}}}
		data, e := json.Marshal(allocation)
		if e != nil {
			return e
		}
		if e = os.WriteFile(*writeFixture, data, 0600); e != nil {
			return e
		}
		h := blake3.Sum256(data)
		return json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"allocation_blake3": fmt.Sprintf("0x%x", h), "account": address.Hex(), "public_test_identity": true})
	}
	u, err := url.Parse(*endpoint)
	if err != nil || u.Scheme != "http" || (u.Hostname() != "127.0.0.1" && u.Hostname() != "::1") || u.User != nil || *count < 1 || *count > 16 || len(*genesis) != 66 {
		return fmt.Errorf("requires literal loopback HTTP, expected genesis and 1..16 blocks")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()
	client, err := rpc.DialContext(ctx, *endpoint)
	if err != nil {
		return err
	}
	defer client.Close()
	var chain string
	var root map[string]interface{}
	var peers string
	if err = client.CallContext(ctx, &chain, "quai_chainId"); err != nil {
		return err
	}
	if err = client.CallContext(ctx, &peers, "net_peerCount"); err != nil {
		return err
	}
	if err = client.CallContext(ctx, &root, "quai_getBlockByNumber", "0x0", false); err != nil {
		return err
	}
	if chain != "0x539" || peers != "0x0" || root["hash"] != *genesis {
		return fmt.Errorf("refusing non-local network or unexpected genesis/peers")
	}
	var txHash common.Hash
	if *send {
		key := testKey()
		from := crypto.PubkeyToAddress(key.PublicKey, common.Location{0, 0})
		var balance hexutil.Big
		if err = client.CallContext(ctx, &balance, "quai_getBalance", from.Hex(), "latest"); err != nil {
			return err
		}
		if (*big.Int)(&balance).Sign() <= 0 {
			return fmt.Errorf("test identity is not funded on the isolated fixture")
		}
		var nonce hexutil.Uint64
		var price hexutil.Big
		if err = client.CallContext(ctx, &nonce, "quai_getTransactionCount", from.Hex(), "latest"); err != nil {
			return err
		}
		if err = client.CallContext(ctx, &price, "quai_gasPrice"); err != nil {
			return err
		}
		to := common.HexToAddress("0x0010000000000000000000000000000000000002", common.Location{0, 0})
		tx, e := types.SignTx(types.NewTx(&types.QuaiTx{ChainID: big.NewInt(1337), Nonce: uint64(nonce), GasPrice: (*big.Int)(&price), Gas: 100000, To: &to, Value: big.NewInt(123), AccessList: types.AccessList{}}), types.NewSigner(big.NewInt(1337), common.Location{0, 0}), key)
		if e != nil {
			return e
		}
		encoded, e := tx.ProtoEncode()
		if e != nil {
			return e
		}
		raw, e := proto.Marshal(encoded)
		if e != nil {
			return e
		}
		if err = client.CallContext(ctx, &txHash, "quai_sendRawTransaction", hexutil.Bytes(raw)); err != nil {
			return err
		}
		time.Sleep(1500 * time.Millisecond)
	}
	for i := 0; i < *count; i++ {
		var raw hexutil.Bytes
		if err = client.CallContext(ctx, &raw, "quai_getPendingHeader"); err != nil {
			return err
		}
		encoded := new(types.ProtoWorkObject)
		if err = proto.Unmarshal(raw, encoded); err != nil {
			return err
		}
		wo := new(types.WorkObject)
		if err = wo.ProtoDecode(encoded, common.Location{0, 0}, types.PEtxObject); err != nil {
			return err
		}
		target := new(big.Int).Div(new(big.Int).Lsh(big.NewInt(1), 256), wo.Difficulty())
		start := time.Now()
		var nonce uint64
		for ; nonce < 50000000; nonce++ {
			if nonce%4096 == 0 {
				select {
				case <-ctx.Done():
					return ctx.Err()
				default:
				}
			}
			wo.WorkObjectHeader().SetNonce(types.EncodeNonce(nonce))
			h := wo.WorkObjectHeader().Hash()
			if new(big.Int).SetBytes(h.Bytes()).Cmp(target) <= 0 {
				break
			}
		}
		if nonce == 50000000 {
			return fmt.Errorf("local nonce limit reached")
		}
		encoded, err = wo.ProtoEncode(types.PEtxObject)
		if err != nil {
			return err
		}
		raw, err = proto.Marshal(encoded)
		if err != nil {
			return err
		}
		if err = client.CallContext(ctx, nil, "quai_receiveMinedHeader", hexutil.Bytes(raw)); err != nil {
			return fmt.Errorf("submit local mined header: %w", err)
		}
		want := wo.WorkObjectHeader().NumberU64()
		for {
			var height hexutil.Uint64
			if err = client.CallContext(ctx, &height, "quai_blockNumber"); err != nil {
				return err
			}
			if uint64(height) >= want {
				break
			}
			select {
			case <-ctx.Done():
				return ctx.Err()
			case <-time.After(100 * time.Millisecond):
			}
		}
		json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"number": want, "hash": wo.Hash().Hex(), "nonces": nonce + 1, "seconds": time.Since(start).Seconds(), "pow": "blake3", "submitted_to_go_validation": true})
		time.Sleep(1100 * time.Millisecond)
	}
	if *send {
		deadline := time.Now().Add(8 * time.Second)
		for time.Now().Before(deadline) {
			var receipt map[string]interface{}
			if err = client.CallContext(ctx, &receipt, "quai_getTransactionReceipt", txHash); err != nil {
				return err
			}
			if receipt != nil {
				if receipt["status"] != "0x1" {
					return fmt.Errorf("local transfer failed with status %v", receipt["status"])
				}
				return json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"mined_transfer_receipt": receipt, "test_account": crypto.PubkeyToAddress(testKey().PublicKey, common.Location{0, 0}).Hex()})
			}
			time.Sleep(100 * time.Millisecond)
		}
		return fmt.Errorf("local transfer was not mined")
	}
	return nil
}
func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
