// Offline-only deterministic transaction oracle. Test keys carry no funds.
package main

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math/big"
	"math/rand"
	"os"
	"reflect"

	"github.com/btcsuite/btcd/btcec/v2"
	"github.com/btcsuite/btcd/btcec/v2/schnorr"
	"github.com/btcsuite/btcd/btcec/v2/schnorr/musig2"
	"github.com/btcsuite/btcd/chaincfg/chainhash"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/crypto"
	"google.golang.org/protobuf/proto"
)

var referenceRevision string

func hx(b []byte) string { return "0x" + hex.EncodeToString(b) }
func must(e error) {
	if e != nil {
		panic(e)
	}
}
func encoded(p *types.ProtoTransaction) []byte { b, e := proto.Marshal(p); must(e); return b }
func aggregatePrivate(keys []*btcec.PrivateKey) *btcec.PrivateKey {
	if len(keys) == 1 {
		return keys[0]
	}
	pubs := make([]*btcec.PublicKey, len(keys))
	var joined []byte
	for i, k := range keys {
		pubs[i] = k.PubKey()
		joined = append(joined, pubs[i].SerializeCompressed()...)
	}
	fingerprint := chainhash.TaggedHash(musig2.KeyAggTagList, joined)
	var second []byte
	for _, p := range pubs {
		if !bytes.Equal(p.SerializeCompressed(), pubs[0].SerializeCompressed()) {
			second = p.SerializeCompressed()
			break
		}
	}
	n, _ := new(big.Int).SetString("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141", 16)
	sum := new(big.Int)
	for i, k := range keys {
		coefficient := big.NewInt(1)
		if second == nil || !bytes.Equal(second, pubs[i].SerializeCompressed()) {
			digest := chainhash.TaggedHash(musig2.KeyAggTagCoeff, fingerprint[:], pubs[i].SerializeCompressed())
			coefficient.SetBytes(digest[:])
			coefficient.Mod(coefficient, n)
		}
		term := new(big.Int).Mul(coefficient, new(big.Int).SetBytes(k.Serialize()))
		sum.Add(sum, term)
		sum.Mod(sum, n)
	}
	key, _ := btcec.PrivKeyFromBytes(sum.Bytes())
	agg, _, _, e := musig2.AggregateKeys(pubs, false)
	must(e)
	if !bytes.Equal(key.PubKey().SerializeCompressed(), agg.FinalKey.SerializeCompressed()) {
		panic("aggregate test key mismatch")
	}
	return key
}
func main() {
	rng := rand.New(rand.NewSource(20261105))
	random := func(n int) []byte { b := make([]byte, n); rng.Read(b); return b }
	vectors := []interface{}{}
	rejected := []interface{}{}
	bases := map[int]*types.ProtoTransaction{}
	scalarOrder, _ := hex.DecodeString("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141")
	for kind := 0; kind < 3; kind++ {
		for i := 0; i < 96; i++ {
			loc := common.Location{byte(i % 16), byte((i / 16) % 16)}
			to := common.BytesToAddress(random(20), common.Location{0, 0})
			chain := new(big.Int).SetBytes(random(i % 33))
			value := new(big.Int).SetBytes(random((i + 1) % 33))
			gas := rng.Uint64()
			nonce := rng.Uint64()
			if i%11 == 0 {
				gas = 0
				nonce = 0
			}
			if i%13 == 0 {
				gas = ^uint64(0)
				nonce = ^uint64(0)
			}
			parent := common.BytesToHash(random(32))
			mix := common.BytesToHash(random(32))
			workNonce := types.EncodeNonce(rng.Uint64())
			var tx *types.Transaction
			switch kind {
			case 0:
				inner := &types.QuaiTx{ChainID: chain, Nonce: nonce, GasPrice: new(big.Int).SetBytes(random(i % 33)), Gas: gas, To: &to, Value: value, Data: random(i % 70), AccessList: types.AccessList{}, V: new(big.Int), R: new(big.Int), S: new(big.Int)}
				if i%5 == 0 {
					inner.To = nil
				}
				if i%2 == 0 {
					inner.AccessList = types.AccessList{{Address: to, StorageKeys: []common.Hash{parent, mix, parent}}}
				}
				if i%3 == 0 {
					inner.ParentHash = &parent
				}
				if i%4 == 0 {
					inner.MixHash = &mix
				}
				if i%5 == 0 {
					inner.WorkNonce = &workNonce
				}
				tx = types.NewTx(inner)
				if i%7 != 0 {
					key, e := crypto.ToECDSA(common.LeftPadBytes(big.NewInt(int64(i+1)).Bytes(), 32))
					must(e)
					tx, e = types.SignTx(tx, types.NewSigner(chain, common.Location{0, 0}), key)
					must(e)
				}
			case 1:
				sender := common.BytesToAddress(random(20), common.Location{0, 0})
				sb := sender.Bytes()
				sb[1] = (sb[1] & 0x7f) | byte((i%2)*128)
				sender = common.BytesToAddress(sb, common.Location{0, 0})
				tb := to.Bytes()
				tb[1] = (tb[1] & 0x7f) | byte(((i/2)%2)*128)
				to = common.BytesToAddress(tb, common.Location{0, 0})
				inner := &types.ExternalTx{OriginatingTxHash: parent, ETXIndex: uint16(i * 701), Gas: gas, To: &to, Value: value, Data: random(i % 70), AccessList: types.AccessList{}, Sender: sender, EtxType: uint64(i % 8)}
				if i%2 == 0 {
					inner.AccessList = types.AccessList{{Address: to, StorageKeys: []common.Hash{mix, parent, mix}}}
				}
				tx = types.NewTx(inner)
			case 2:
				inner := &types.QiTx{ChainID: chain, Data: random(i % 70), Signature: new(schnorr.Signature)}
				keys := []*btcec.PrivateKey{}
				for j := 0; j < 1+i%4; j++ {
					index := int64(1 + (i+j)%7)
					if i%3 == 0 {
						index = 3
					}
					key, _ := btcec.PrivKeyFromBytes(common.LeftPadBytes(big.NewInt(index).Bytes(), 32))
					keys = append(keys, key)
					inner.TxIn = append(inner.TxIn, types.TxIn{PreviousOutPoint: types.OutPoint{TxHash: common.BytesToHash(random(32)), Index: uint16(i*701 + j)}, PubKey: key.PubKey().SerializeUncompressed()})
				}
				if i%19 != 0 {
					for j := 0; j < 1+i%3; j++ {
						inner.TxOut = append(inner.TxOut, types.TxOut{Denomination: uint8((i + j) % 256), Address: random(20), Lock: new(big.Int).SetBytes(random(i % 33))})
					}
				}
				if i%3 == 0 {
					inner.ParentHash = &parent
				}
				if i%4 == 0 {
					inner.MixHash = &mix
				}
				if i%5 == 0 {
					inner.WorkNonce = &workNonce
				}
				tx = types.NewTx(inner)
				digest, e := types.NewSigner(chain, common.Location{0, 0}).Hash(tx)
				must(e)
				inner.Signature, e = schnorr.Sign(aggregatePrivate(keys), digest[:])
				must(e)
				if i%7 == 0 {
					inner.Signature = new(schnorr.Signature)
				}
				tx = types.NewTx(inner)
			}
			p, e := tx.ProtoEncode()
			must(e)
			if i%9 == 0 {
				for _, name := range []string{"ChainId", "Value", "GasPrice", "V", "R", "S"} {
					f := reflect.ValueOf(p).Elem().FieldByName(name)
					if !f.IsNil() {
						f.SetBytes(append([]byte{0, 0}, f.Bytes()...))
					}
				}
			}
			if kind == 1 && i%17 == 0 {
				p.EtxSender = nil
			}
			if kind == 1 && i%13 == 0 {
				*p.EtxIndex += 65536
			}
			if kind == 2 {
				if i%2 == 0 {
					for j, v := range p.TxIns.TxIns {
						v.PubKey = tx.TxIn()[j].PubKey
					}
				}
				if i%13 == 0 {
					for _, v := range p.TxIns.TxIns {
						*v.PreviousOutPoint.Index += 65536
					}
				}
				if i%29 == 0 && len(p.TxOuts.TxOuts) > 0 {
					p.TxOuts.TxOuts[0].Lock = nil
					p.TxOuts.TxOuts[0].Address = nil
				}
			}
			raw := encoded(p)
			if i%5 == 0 {
				raw = append(raw, 0xa0, 0x06, 0x01)
			}
			parsed := new(types.ProtoTransaction)
			must(proto.Unmarshal(raw, parsed))
			decoded := new(types.Transaction)
			must(decoded.ProtoDecode(parsed, common.Location{0, 0}))
			canonical, e := decoded.ProtoEncode()
			must(e)
			sign, e := decoded.ProtoEncodeTxSigningData()
			must(e)
			signerChain := new(big.Int)
			if kind != 1 {
				signerChain = decoded.ChainId()
			}
			signer := types.NewSigner(signerChain, common.Location{0, 0})
			digest, e := signer.Hash(decoded)
			must(e)
			binary, e := decoded.MarshalBinary()
			must(e)
			vector := map[string]interface{}{"id": fmt.Sprintf("type-%d-%03d", kind, i), "type": kind, "input": hx(raw), "canonical": hx(encoded(canonical)), "signing_bytes": hx(encoded(sign)), "signing_hash": digest.Hex(), "typed_rlp": hx(binary)}
			if kind == 0 {
				vector["origin"] = loc
				id, e := decoded.HashWithError(loc...)
				must(e)
				vector["id_explicit"] = id.Hex()
			}
			// Re-decode before hashing without explicit location: Go caches the first routed ID.
			again := new(types.Transaction)
			must(again.ProtoDecode(parsed, common.Location{0, 0}))
			id, e := again.HashWithError()
			if e == nil {
				vector["id_automatic"] = id.Hex()
			} else {
				vector["id_automatic"] = nil
			}
			sender, e := types.Sender(signer, decoded)
			if e == nil {
				vector["sender"] = hx(sender.Bytes())
			} else {
				vector["sender"] = nil
			}
			if kind == 2 {
				pubs := []*btcec.PublicKey{}
				for _, v := range decoded.TxIn() {
					k, e := btcec.ParsePubKey(v.PubKey)
					must(e)
					pubs = append(pubs, k)
				}
				final := pubs[0]
				if len(pubs) > 1 {
					agg, _, _, e := musig2.AggregateKeys(pubs, false)
					must(e)
					final = agg.FinalKey
				}
				vector["aggregate_key"] = hx(final.SerializeCompressed())
				vector["signature_valid"] = decoded.GetSchnorrSignature().Verify(digest[:], final)
			}
			vectors = append(vectors, vector)
			if i == 1 {
				bases[kind] = canonical
			}
		}
	}
	reject := func(name string, p *types.ProtoTransaction) {
		raw := encoded(p)
		parsed := new(types.ProtoTransaction)
		e := proto.Unmarshal(raw, parsed)
		if e == nil {
			e = new(types.Transaction).ProtoDecode(parsed, common.Location{0, 0})
		}
		if e == nil {
			panic("oracle unexpectedly accepted " + name)
		}
		rejected = append(rejected, map[string]interface{}{"id": name, "input": hx(raw), "reference_error": e.Error()})
	}
	fields := map[int][]string{0: {"Type", "Nonce", "Gas", "AccessList", "Value", "GasPrice", "Data", "ChainId", "V", "R", "S"}, 1: {"Type", "Gas", "AccessList", "Value", "Data", "To", "OriginatingTxHash", "EtxIndex", "EtxType"}, 2: {"Type", "TxIns", "TxOuts", "Signature", "ChainId", "Data"}}
	for kind := 0; kind < 3; kind++ {
		for _, name := range fields[kind] {
			p := proto.Clone(bases[kind]).(*types.ProtoTransaction)
			f := reflect.ValueOf(p).Elem().FieldByName(name)
			f.Set(reflect.Zero(f.Type()))
			reject(fmt.Sprintf("missing-%d-%s", kind, name), p)
		}
	}
	mutate := func(kind int, name string, fn func(*types.ProtoTransaction)) {
		p := proto.Clone(bases[kind]).(*types.ProtoTransaction)
		fn(p)
		reject(name, p)
	}
	mutate(0, "unknown-type", func(p *types.ProtoTransaction) { v := uint64(3); p.Type = &v })
	mutate(0, "ecdsa-zero-r", func(p *types.ProtoTransaction) { p.R = []byte{} })
	mutate(0, "ecdsa-high-s", func(p *types.ProtoTransaction) { p.S = scalarOrder })
	mutate(0, "ecdsa-bad-v", func(p *types.ProtoTransaction) { p.V = []byte{2} })
	mutate(2, "qi-empty-inputs", func(p *types.ProtoTransaction) { p.TxIns.TxIns = nil })
	mutate(2, "qi-missing-outpoint", func(p *types.ProtoTransaction) { p.TxIns.TxIns[0].PreviousOutPoint = nil })
	mutate(2, "qi-missing-outpoint-hash", func(p *types.ProtoTransaction) { p.TxIns.TxIns[0].PreviousOutPoint.Hash = nil })
	mutate(2, "qi-missing-outpoint-index", func(p *types.ProtoTransaction) { p.TxIns.TxIns[0].PreviousOutPoint.Index = nil })
	mutate(2, "qi-invalid-key", func(p *types.ProtoTransaction) { p.TxIns.TxIns[0].PubKey = make([]byte, 33) })
	mutate(2, "qi-large-s", func(p *types.ProtoTransaction) { copy(p.Signature[32:], scalarOrder) })
	mutate(2, "qi-short-signature", func(p *types.ProtoTransaction) { p.Signature = p.Signature[:63] })
	mutate(2, "qi-large-r", func(p *types.ProtoTransaction) {
		for i := 0; i < 32; i++ {
			p.Signature[i] = 255
		}
	})
	mutate(2, "qi-large-denomination", func(p *types.ProtoTransaction) { v := uint32(256); p.TxOuts.TxOuts[0].Denomination = &v })
	mutate(2, "qi-missing-denomination", func(p *types.ProtoTransaction) { p.TxOuts.TxOuts[0].Denomination = nil })
	result := map[string]interface{}{"schema": 1, "reference_revision": referenceRevision, "transactions": vectors, "rejected": rejected, "scope": "Bounded native transaction protobuf reconstruction, typed-RLP encoding, signing messages, routing IDs, ECDSA recovery and untweaked MuSig2/BIP340 signature mathematics. Not typed-RLP decoding, UTXO ownership, state validation, gas/fees, block commitments, fork choice, mining or sync."}
	must(json.NewEncoder(os.Stdout).Encode(result))
}
