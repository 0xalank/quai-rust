// Explicit offline auxiliary oracle. Deterministic public test keys are lab-only.
package main

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"github.com/btcsuite/btcd/btcec/v2"
	"github.com/btcsuite/btcd/btcec/v2/schnorr"
	"github.com/btcsuite/btcd/btcec/v2/schnorr/musig2"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus/kawpow"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"google.golang.org/protobuf/proto"
	"math/big"
	"os"
)

var referenceRevision string

func must(e error) {
	if e != nil {
		panic(e)
	}
}
func hx(b []byte) string           { return "0x" + hex.EncodeToString(b) }
func marsh(m proto.Message) []byte { b, e := proto.Marshal(m); must(e); return b }
func tag(t string, b []byte) []byte {
	a := sha256.Sum256([]byte(t))
	x := append(append(append([]byte{}, a[:]...), a[:]...), b...)
	h := sha256.Sum256(x)
	return h[:]
}

var pairs = [][2]int{{0, 1}, {1, 0}, {0, 2}, {2, 0}, {1, 2}, {2, 1}}
var labPrivate []*btcec.PrivateKey
var labKeys []string

func sign(t *types.AuxTemplate, pair [2]int) {
	a, b := labPrivate[pair[0]], labPrivate[pair[1]]
	keys := [][]byte{a.PubKey().SerializeCompressed(), b.PubKey().SerializeCompressed()}
	fingerprint := tag("KeyAgg list", append(append([]byte{}, keys[0]...), keys[1]...))
	sum := new(big.Int)
	for i, k := range []*btcec.PrivateKey{a, b} {
		coefficient := big.NewInt(1)
		if i == 0 {
			coefficient.SetBytes(tag("KeyAgg coefficient", append(append([]byte{}, fingerprint...), keys[i]...)))
		}
		term := new(big.Int).Mul(coefficient, new(big.Int).SetBytes(k.Serialize()))
		sum.Add(sum, term)
	}
	sum.Mod(sum, btcec.S256().N)
	private, _ := btcec.PrivKeyFromBytes(sum.FillBytes(make([]byte, 32)))
	message := t.Hash()
	sig, e := schnorr.Sign(private, message[:], schnorr.CustomNonce([32]byte{}))
	must(e)
	t.SetSigs(sig.Serialize())
	if !t.VerifySignature() {
		panic("invalid generated lab signature")
	}
}
func profile(id string, keys []string) map[string]interface{} {
	aggregate := []string{}
	for _, p := range pairs {
		a, e := hex.DecodeString(keys[p[0]])
		must(e)
		b, e := hex.DecodeString(keys[p[1]])
		must(e)
		ka, e := btcec.ParsePubKey(a)
		must(e)
		kb, e := btcec.ParsePubKey(b)
		must(e)
		k, _, _, e := musig2.AggregateKeys([]*btcec.PublicKey{ka, kb}, false)
		must(e)
		aggregate = append(aggregate, hx(k.FinalKey.SerializeCompressed()))
	}
	return map[string]interface{}{"id": id, "keys": keys, "aggregate": aggregate}
}
func templateVector(id, profile string, bytes []byte) map[string]interface{} {
	p := new(types.ProtoAuxTemplate)
	must(proto.Unmarshal(bytes, p))
	t := types.NewAuxTemplate()
	e := t.ProtoDecode(p)
	if e != nil {
		return map[string]interface{}{"id": id, "profile": profile, "input": hx(bytes), "decoded": false}
	}
	canonical := t.ProtoEncode()
	signing := proto.Clone(canonical).(*types.ProtoAuxTemplate)
	signing.Sigs = nil
	if t.PowID() == types.SHA_BTC || t.PowID() == types.SHA_BCH {
		signing.Version = proto.Uint32(signing.GetVersion() & 0xe0000000)
	}
	h := t.Hash()
	return map[string]interface{}{"id": id, "profile": profile, "input": hx(bytes), "decoded": true, "canonical": hx(marsh(canonical)), "signing": hx(marsh(signing)), "hash": hx(h[:]), "signature_valid": t.VerifySignature()}
}
func baseHeader() *types.WorkObject {
	w := types.EmptyWorkObject(common.ZONE_CTX)
	h := w.WorkObjectHeader()
	h.SetLocation(common.Location{0, 0})
	h.SetPrimaryCoinbase(common.BytesToAddress(make([]byte, 20), common.Location{0, 0}))
	h.SetNumber(big.NewInt(1))
	h.SetDifficulty(big.NewInt(2))
	h.SetPrimeTerminusNumber(big.NewInt(1530501))
	h.SetTime(1769112000)
	h.SetData([]byte{0})
	h.SetHeaderHash(w.Header().Hash())
	return w
}
func build(t *types.AuxTemplate) *types.WorkObject {
	w := baseHeader()
	h := w.WorkObjectHeader()
	seal := h.SealHash()
	if t.PowID() == types.Scrypt {
		seal = types.CreateAuxMerkleRoot(common.BytesToHash(t.AuxPow2()), seal)
	}
	tx := types.NewAuxPowCoinbaseTx(t.PowID(), t.Height(), t.CoinbaseOut(), seal, t.SignatureTime())
	root := types.CalculateMerkleRoot(t.PowID(), tx, t.MerkleBranch())
	donor := types.NewBlockHeader(t.PowID(), int32(t.Version()), t.PrevHash(), root, t.SignatureTime()+1, t.Bits(), 0, t.Height())
	aux := types.NewAuxPow(t.PowID(), donor, t.AuxPow2(), t.Sigs(), t.MerkleBranch(), tx)
	h.SetAuxPow(aux)
	return w
}

// Calls the real validation helpers in the same order as core.verifyHeader's
// auxiliary stage. Full header/uncle/state acceptance is deliberately excluded.
func admit(w *types.WorkObject) error {
	h := w.WorkObjectHeader()
	a := h.AuxPow()
	s := types.ExtractScriptSigFromCoinbaseTx(a.Transaction())
	tm, e := types.ExtractSignatureTimeFromCoinbase(s)
	if e != nil {
		return e
	}
	if a.Header().Timestamp() < tm || h.Time() < uint64(tm) {
		return fmt.Errorf("signature time")
	}
	seal, e := types.ExtractSealHashFromCoinbase(s)
	if e != nil {
		return e
	}
	expected := h.SealHash()
	if a.PowID() == types.Scrypt {
		doge := common.BytesToHash(a.AuxPow2())
		if doge == (common.Hash{}) {
			return fmt.Errorf("zero auxpow2")
		}
		expected = types.CreateAuxMerkleRoot(doge, expected)
		size, nonce, e := types.ExtractMerkleSizeAndNonceFromCoinbase(s)
		if e != nil {
			return e
		}
		if size != params.MerkleSize || nonce != params.MerkleNonce {
			return fmt.Errorf("merged mining parameters")
		}
	}
	if expected != seal {
		return fmt.Errorf("seal commitment")
	}
	if types.CalculateMerkleRoot(a.PowID(), a.Transaction(), a.MerkleBranch()) != a.Header().MerkleRoot() {
		return fmt.Errorf("donor merkle root")
	}
	if e = types.ValidatePrevOutPointIndexAndSequenceOfCoinbase(a.Transaction()); e != nil {
		return e
	}
	if !a.ConvertToTemplate().VerifySignature() {
		return fmt.Errorf("quorum signature")
	}
	return nil
}
func auxVector(id, profile string, w *types.WorkObject) map[string]interface{} {
	a := w.AuxPow()
	p := a.ProtoEncode()
	hp, e := w.WorkObjectHeader().ProtoEncode()
	must(e)
	t := a.ConvertToTemplate()
	th := t.Hash()
	err := admit(w)
	var problem interface{}
	if err != nil {
		problem = err.Error()
	}
	full, e := w.ProtoEncode(types.BlockObject)
	must(e)
	merkle := types.CalculateMerkleRoot(a.PowID(), a.Transaction(), a.MerkleBranch())
	return map[string]interface{}{"id": id, "profile": profile, "aux": hx(marsh(p)), "quai_header": hx(marsh(hp)), "work_object": hx(marsh(full)), "canonical_template": hx(marsh(t.ProtoEncode())), "template_hash": hx(th[:]), "quai_seal": w.SealHash().Hex(), "block_hash": w.Hash().Hex(), "quai_time": w.Time(), "valid": err == nil, "reference_error": problem, "signature_valid": t.VerifySignature(), "merkle_root": hx(merkle[:]), "block_role": core.NewTestHeaderChain().CheckPowIdValidity(w.WorkObjectHeader()) == nil, "share_role": core.NewTestHeaderChain().CheckPowIdValidityForWorkshare(w.WorkObjectHeader()) == nil}
}
func codecVector(id string, bytes []byte) map[string]interface{} {
	p := new(types.ProtoAuxPow)
	must(proto.Unmarshal(bytes, p))
	a := new(types.AuxPow)
	e := a.ProtoDecode(p)
	if e != nil {
		return map[string]interface{}{"id": id, "input": hx(bytes), "decoded": false}
	}
	return map[string]interface{}{"id": id, "input": hx(bytes), "decoded": true, "canonical": hx(marsh(a.ProtoEncode()))}
}
func resetMerkle(a *types.AuxPow) {
	old := a.Header()
	root := types.CalculateMerkleRoot(a.PowID(), a.Transaction(), a.MerkleBranch())
	h := types.NewBlockHeader(a.PowID(), old.Version(), old.PrevBlock(), root, old.Timestamp(), old.Bits(), old.Nonce(), old.Height())
	h.SetNonce64(old.Nonce64())
	h.SetMixHash(old.MixHash())
	a.SetHeader(h)
}
func resetDonor(a *types.AuxPow, version, bits, time uint32) {
	old := a.Header()
	h := types.NewBlockHeader(a.PowID(), int32(version), old.PrevBlock(), old.MerkleRoot(), time, bits, old.Nonce(), old.Height())
	h.SetNonce64(old.Nonce64())
	h.SetMixHash(old.MixHash())
	a.SetHeader(h)
}
func main() {
	log.Global.SetOutput(os.Stderr)
	pinned := append([]string{}, params.MuSig2PublicKeys...)
	for i := 1; i <= 3; i++ {
		b := make([]byte, 32)
		b[31] = byte(i)
		key, _ := btcec.PrivKeyFromBytes(b)
		labPrivate = append(labPrivate, key)
		labKeys = append(labKeys, hex.EncodeToString(key.PubKey().SerializeCompressed()))
	}
	profiles := []interface{}{profile("pinned", pinned), profile("lab", labKeys)}
	templates := []interface{}{}
	auxiliaries := []interface{}{}
	golden := []interface{}{}
	codecs := []interface{}{}
	for _, pr := range []string{"pinned", "lab"} {
		if pr == "pinned" {
			params.MuSig2PublicKeys = pinned
		} else {
			params.MuSig2PublicKeys = labKeys
		}
		for _, id := range []types.PowID{types.Kawpow, types.SHA_BTC, types.SHA_BCH, types.Scrypt} {
			orders := 1
			if pr == "lab" {
				orders = 6
			}
			for order := 0; order < orders; order++ {
				var t *types.AuxTemplate
				switch id {
				case types.Kawpow:
					t = types.DefaultKawpowAuxTemplate()
				case types.Scrypt:
					t = types.DefaultScryptAuxTemplate()
				default:
					t = types.DefaultShaBchAuxTemplate()
					t.SetPowID(id)
				}
				if pr == "lab" {
					t.SetHeight(1000)
					t.SetSignatureTime(1769111000)
					sign(t, pairs[order])
				}
				name := fmt.Sprintf("%s-%d-%d", pr, id, order)
				p := t.ProtoEncode()
				templates = append(templates, templateVector(name, pr, marsh(p)))
				for field := 0; field < 15; field++ {
					m := proto.Clone(p).(*types.ProtoAuxTemplate)
					switch field {
					case 0:
						m.PrevHash[0] ^= 1
					case 1:
						m.Version = proto.Uint32(m.GetVersion() ^ 1)
					case 2:
						m.Version = proto.Uint32(m.GetVersion() ^ 0x40000000)
					case 3:
						m.Bits = proto.Uint32(m.GetBits() ^ 1)
					case 4:
						m.SignatureTime = proto.Uint32(m.GetSignatureTime() + 1)
					case 5:
						m.Height = proto.Uint32(m.GetHeight() + 1)
					case 6:
						m.CoinbaseOut = append(m.CoinbaseOut, 0)
					case 7:
						m.MerkleBranch = append(m.MerkleBranch, make([]byte, 32))
					case 8:
						m.Sigs = nil
					case 9:
						m.Sigs = []byte{}
					case 10:
						m.Sigs = m.Sigs[:63]
					case 11:
						m.Sigs[0] ^= 1
					case 12:
						m.PrevHash = m.PrevHash[:31]
					case 13:
						m.AuxPow2 = []byte{}
					case 14:
						m.ChainId = nil
					}
					templates = append(templates, templateVector(fmt.Sprintf("%s-field-%d", name, field), pr, marsh(m)))
				}
				templates = append(templates, templateVector(name+"-unknown", pr, append(marsh(p), 0xb8, 0x3e, 7)))
				w := build(t)

				for mode := 0; mode < 8; mode++ {
					a := proto.Clone(w.AuxPow().ProtoEncode()).(*types.ProtoAuxPow)
					switch mode {
					case 0:
						a.SignatureTime = proto.Uint64(123456789)
					case 1:
						a.Header = append(a.Header, 1, 2, 3)
					case 2:
						a.Header = a.Header[:len(a.Header)-1]
					case 3:
						a.Signature = nil
					case 4:
						a.ChainId = nil
					case 5:
						a.Auxpow2 = []byte{}
					case 6:
						a.MerkleBranch = [][]byte{{7}, make([]byte, 40)}
					case 7:
					}
					b := marsh(a)
					if mode == 7 {
						b = append(b, 0xb8, 0x3e, 7)
					}
					codecs = append(codecs, codecVector(fmt.Sprintf("%s-codec-%d", name, mode), b))
				}
				auxiliaries = append(auxiliaries, auxVector(name, pr, w))
				for field := 0; field < 16; field++ {
					m := types.CopyWorkObject(w)
					a := m.AuxPow()
					switch field {
					case 0:
						s := append([]byte{}, a.Signature()...)
						s[0] ^= 1
						a.SetSignature(s)
					case 1:
						a.SetSignature(nil)
					case 2:
						a.SetMerkleBranch(append(a.MerkleBranch(), make([]byte, 32)))
					case 3:
						a.Header().SetNonce(17)
					case 4:
						a.Header().SetHeight(t.Height() + 1)
					case 5:
						m.WorkObjectHeader().SetTime(uint64(t.SignatureTime() - 1))
					case 6:
						resetDonor(a, uint32(a.Header().Version()), a.Header().Bits(), t.SignatureTime()-1)
					case 7:
						tx := append([]byte{}, a.Transaction()...)
						tx[5] = 1
						a.SetTransaction(tx)
					case 8:
						tx := append([]byte{}, a.Transaction()...)
						tx[37] = 0
						a.SetTransaction(tx)
					case 9:
						tx := append([]byte{}, a.Transaction()...)
						script := types.ExtractScriptSigFromCoinbaseTx(tx)
						scriptStart := 42
						_ = script
						tx[scriptStart+1] ^= 1
						a.SetTransaction(tx)
					case 10:
						a.SetAuxPow2(make([]byte, 32))
					case 11:
						m.WorkObjectHeader().SetData([]byte{0, 1})
					case 12:
						resetDonor(a, t.Version()^1, a.Header().Bits(), a.Header().Timestamp())
					case 13:
						resetDonor(a, t.Version()^0x40000000, a.Header().Bits(), a.Header().Timestamp())
					case 14:
						resetDonor(a, uint32(a.Header().Version()), t.Bits()^1, a.Header().Timestamp())
					case 15:
						a.SetTransaction(a.Transaction()[:10])
					}
					auxiliaries = append(auxiliaries, auxVector(fmt.Sprintf("%s-mutation-%d", name, field), pr, m))
				}

				for mode := 0; mode < 3; mode++ {
					m := types.CopyWorkObject(w)
					a := m.AuxPow()
					tx := append([]byte{}, a.Transaction()...)
					switch mode {
					case 0:
						script := types.ExtractScriptSigFromCoinbaseTx(tx)
						pos := 1 + int(script[0]) + 45 + 1
						tx[42+pos] ^= 1
					case 1:
						tx[0] ^= 4
					case 2:
						script := types.ExtractScriptSigFromCoinbaseTx(tx)
						pos := 1 + int(script[0]) + 45
						next := pos + 1 + int(script[pos])
						updated := append(append(append([]byte{}, script[:pos]...), 1, 42), script[next:]...)
						tx = append(append(append([]byte{}, tx[:41]...), byte(len(updated))), append(updated, tx[42+len(script):]...)...)
					}
					a.SetTransaction(tx)
					resetMerkle(a)
					auxiliaries = append(auxiliaries, auxVector(fmt.Sprintf("%s-flex-%d", name, mode), pr, m))
				}
				if id == types.Kawpow && order == 0 {
					engine := kawpow.New(params.PowConfig{}, nil, false, log.Global)
					for nonce := uint64(0); ; nonce++ {
						w.AuxPow().Header().SetNonce64(nonce)
						mix, pow := engine.ComputePowLight(w.WorkObjectHeader())
						if new(big.Int).SetBytes(pow.Bytes()).Cmp(new(big.Int).Lsh(big.NewInt(1), 255)) <= 0 {
							w.AuxPow().Header().SetMixHash(mix)
							g := auxVector(name+"-pow", pr, w)
							g["pow_hash"] = pow.Hex()
							golden = append(golden, g)
							break
						}
					}
				}
			}
		}
	}
	params.MuSig2PublicKeys = pinned
	result := map[string]interface{}{"schema": 1, "kind": "auxiliary-admission", "reference_revision": referenceRevision, "profiles": profiles, "templates": templates, "auxiliaries": auxiliaries, "golden": golden, "codecs": codecs, "scope": "Signed auxiliary template and coinbase/Merkle/Quai-seal/time bindings plus supported KAWPOW proof integration. No full header/uncle/economic state validity or canonical authority."}
	e := json.NewEncoder(os.Stdout)
	e.SetIndent("", "  ")
	must(e.Encode(result))
}
