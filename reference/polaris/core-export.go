// Binding, zero-height order and PoW-identity observations; no admission bypass.
package core

import (
	"errors"
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/consensus"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/params"
)

func NativePolarisVectors() map[string]interface{} {
	bindings := []interface{}{}
	identities := []interface{}{}
	for _, prime := range []uint64{params.KawPowForkBlock - 1, params.KawPowForkBlock, params.PolarisForkBlock - 1, params.PolarisForkBlock, params.PolarisForkBlock + 1} {
		for _, zero := range []bool{false, true} {
			number := uint64(1)
			if zero {
				number = 0
			}
			base := hnParent(prime, number, nh(2))
			for variant := 0; variant < 5; variant++ {
				w := hnRound(base)
				switch variant {
				case 1:
					w.Header().SetParentDeltaEntropy(nh(999), 1)
				case 2:
					w.WorkObjectHeader().SetHeaderHash(common.HexToHash("0x1234"))
				case 3:
					if w.AuxPow() != nil {
						sig := append([]byte{}, w.AuxPow().Signature()...)
						if len(sig) > 0 {
							sig[0] ^= 1
						}
						w.AuxPow().SetSignature(sig)
					}
				case 4:
					if w.AuxPow() != nil {
						w.AuxPow().Header().SetNonce64(7)
					}
				}
				hc := nativeHC(2)
				_, order, e := hc.CalcOrder(w)
				binding := hc.verifyWorkObjectBinding(w)
				hc.headerDb.Close()
				bindings = append(bindings, map[string]interface{}{"id": fmt.Sprintf("binding/%d/%t/%d", prime, zero, variant), "block": hp(w), "pow_hash": w.MixHash().Hex(), "zero": zero, "binding_valid": binding == nil, "order_valid": e == nil, "order": order, "error": fmt.Sprint(e)})
			}
			h := base.WorkObjectHeader()
			identities = append(identities, map[string]interface{}{"header": hwh(h), "identity": h.PowIdentity().Hex(), "hash": h.Hash().Hex()})
		}
	}
	for _, id := range []types.PowID{types.Kawpow, types.SHA_BTC, types.SHA_BCH} {
		base := hnParent(params.PolarisForkBlock, 1, nh(2))
		h := base.WorkObjectHeader()
		if id != types.Kawpow {
			tx := types.NewAuxPowCoinbaseTx(id, 100, []byte{0x51}, h.SealHash(), 123)
			mr := types.CalculateMerkleRoot(id, tx, nil)
			donor := types.NewBitcoinBlockHeader(1, [32]byte{1}, mr, 1000, 0x1d00ffff, 7)
			h.SetAuxPow(types.NewAuxPow(id, types.NewAuxPowHeader(donor), nil, []byte{1, 2}, nil, tx))
		}
		for variant := 0; variant < 6; variant++ {
			w := hnRound(base)
			a := w.AuxPow()
			switch variant {
			case 1:
				a.SetSignature([]byte{0xde, 0xad})
			case 2:
				a.SetAuxPow2(nil)
			case 3:
				a.SetAuxPow2([]byte{})
			case 4:
				a.Header().SetNonce64(8)
			case 5:
				if id == types.SHA_BTC {
					a.SetPowID(types.SHA_BCH)
				}
			}
			h := w.WorkObjectHeader()
			identities = append(identities, map[string]interface{}{"header": hwh(h), "identity": h.PowIdentity().Hex(), "hash": h.Hash().Hex()})
		}
	}
	return map[string]interface{}{"polaris_prime_block": params.PolarisForkBlock, "bindings": bindings, "identities": identities, "inclusions": nativePolarisInclusions(), "scope": "Actual all-height work-object binding, controlled-hash CalcOrder and donor PoW identity around Polaris. Signatures/real PoW, economic replay, founder allocations and canonical admission are separate."}
}

func nativePolarisInclusions() []interface{} {
	out := []interface{}{}
	for _, prime := range []uint64{params.PolarisForkBlock - 1, params.PolarisForkBlock, params.PolarisForkBlock + 1} {
		for variant := 0; variant < 4; variant++ {
			hc := histHC()
			records := []string{}
			var parent *types.WorkObject
			for i := uint64(0); i < 10; i++ {
				w := hnParent(prime, i, nh(2))
				if parent != nil {
					w.WorkObjectHeader().SetParentHash(parent.Hash())
				}
				hnSync(w)
				hnBind(w)
				w = hnRound(w)
				hstore(hc, w)
				records = append(records, hp(w))
				parent = w
			}
			block := hnParent(prime, 10, nh(2))
			block.WorkObjectHeader().SetParentHash(parent.Hash())
			one := hnParent(prime, 9, nh(2))
			one.WorkObjectHeader().SetParentHash(parent.ParentHash(2))
			one.Header().SetExtra([]byte{0x55})
			hnSync(one)
			hnBind(one)
			one = hnRound(one)
			other := hnRound(one)
			switch variant {
			case 0:
				other.AuxPow().Header().SetNonce64(9)
			case 1:
				other.AuxPow().SetSignature([]byte{1, 2, 3})
			case 2:
				one = hnRound(parent)
				other = hnRound(parent)
				other.AuxPow().SetSignature([]byte{1, 2, 3})
			case 3:
			}
			headers := []*types.WorkObjectHeader{one.WorkObjectHeader(), other.WorkObjectHeader()}
			block.Body().SetUncles(headers)
			block.Header().SetUncleHash(types.CalcUncleHash(headers))
			hnSync(block)
			hnBind(block)
			block = hnRound(block)
			e := hc.VerifyUncles(block)
			kind := "other"
			if e == nil {
				kind = "none"
			}
			if errors.Is(e, consensus.ErrDuplicateUncle) {
				kind = "duplicate"
			}
			if errors.Is(e, consensus.ErrUncleIsAncestor) {
				kind = "ancestor"
			}
			out = append(out, map[string]interface{}{"prime": prime, "variant": variant, "records": records, "block": hp(block), "kind": kind, "error": fmt.Sprint(e)})
			hc.headerDb.Close()
		}
	}
	return out
}
