// Offline go-quai v0.57.0 oracle for precompile addresses, the access list of
// a creation, address-scope errors in nested frames, lockup-contract calls and
// precompile inputs. Never part of the Rust runtime.
//
// Signed Quai transactions run through the actual applyTransaction
// (core.NativeApplySettlement, reference/settlement/core-export.go) on
// committed private state. Precompile inputs run through
// vm.RunPrecompiledContract with the zone's own precompile objects.
package main

import (
	"bytes"
	"encoding/binary"
	"crypto/ecdsa"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"math/big"
	"math/rand"
	"os"
	"sort"
	"strings"

	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/core/vm"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/crypto/bn256"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"google.golang.org/protobuf/proto"
)

var referenceRevision string

func must(e error) {
	if e != nil {
		panic(e)
	}
}
func u(v uint64) *big.Int { return new(big.Int).SetUint64(v) }
func hx(b []byte) string  { return "0x" + hex.EncodeToString(b) }
func word(v *big.Int) []byte {
	return new(big.Int).Mod(v, new(big.Int).Lsh(big.NewInt(1), 256)).FillBytes(make([]byte, 32))
}
func wu(v uint64) []byte { return word(u(v)) }
func join(parts ...[]byte) []byte {
	var b []byte
	for _, p := range parts {
		b = append(b, p...)
	}
	return b
}

// p is the shortest PUSH of v (PUSH1 0 for zero).
func p(v uint64) []byte {
	b := u(v).Bytes()
	if len(b) == 0 {
		b = []byte{0}
	}
	return append([]byte{0x5f + byte(len(b))}, b...)
}
func pushAddr(a common.Address) []byte { return append([]byte{0x73}, a.Bytes()...) }

// mstore writes b to memory at 0 with PUSH32/MSTORE pairs.
func mstore(b []byte) []byte {
	var out []byte
	for i := 0; i < len(b); i += 32 {
		chunk := make([]byte, 32)
		copy(chunk, b[i:])
		out = append(out, 0x7f)
		out = append(out, chunk...)
		out = append(out, p(uint64(i))...)
		out = append(out, 0x52)
	}
	return out
}

// record stores the success flag on the stack in slot 0, RETURNDATASIZE in
// slot 1 and the Keccak-256 of the return data in slot 2.
func record() []byte {
	return join(p(0), []byte{0x55},
		[]byte{0x3d}, p(1), []byte{0x55},
		[]byte{0x3d}, p(0), p(0x1000), []byte{0x3e},
		[]byte{0x3d}, p(0x1000), []byte{0x20}, p(2), []byte{0x55})
}

// call runs op (CALL, CALLCODE, DELEGATECALL or STATICCALL) on target with
// all gas and input in memory at 0, then records the result.
func call(op byte, target common.Address, value uint64, input []byte) []byte {
	args := [][]byte{p(0), p(0), p(uint64(len(input))), p(0)}
	if op == 0xf1 || op == 0xf2 {
		args = append(args, p(value))
	}
	args = append(args, pushAddr(target), []byte{0x5a, op})
	return join(mstore(input), join(args...), record())
}

// account reads BALANCE (0x31), EXTCODESIZE (0x3b) or EXTCODEHASH (0x3f) of
// target into slot 3; EXTCODECOPY (0x3c) copies 32 bytes and stores them.
func account(op byte, target common.Address) []byte {
	if op == 0x3c {
		return join(p(32), p(0), p(0), pushAddr(target), []byte{0x3c}, p(0), []byte{0x51}, p(3), []byte{0x55})
	}
	return join(pushAddr(target), []byte{op}, p(3), []byte{0x55})
}

// marker sets slot 9 to 1: it shows that a frame went on after a call.
func marker() []byte { return join(p(1), p(9), []byte{0x55}) }

func key(loc common.Location) *ecdsa.PrivateKey {
	for i := uint64(1); ; i++ {
		k, e := crypto.ToECDSA(u(i).FillBytes(make([]byte, 32)))
		must(e)
		a := crypto.PubkeyToAddress(k.PublicKey, loc)
		if _, e := a.InternalAndQuaiAddress(); e == nil {
			return k
		}
	}
}

// at is an address of loc's zone that no test uses as a precompile.
func at(loc common.Location, n byte) common.Address {
	b := make([]byte, 20)
	b[0] = loc.BytePrefix()
	b[18] = 0x22
	b[19] = n
	return common.BytesToAddress(b, loc)
}

// form is the address first || 0...0 || last.
func form(loc common.Location, first, last byte) common.Address {
	var b [20]byte
	b[0] = first
	b[19] = last
	return common.Bytes20ToAddress(b, loc)
}

// remote is an address of another zone (Qi ledger when qi is set).
func remote(loc common.Location, first byte, qi bool, n byte) common.Address {
	var b [20]byte
	b[0] = first
	if qi {
		b[1] = 0x80
	}
	b[18] = 0x33
	b[19] = n
	return common.Bytes20ToAddress(b, loc)
}
func otherZone(loc common.Location) byte {
	return common.Location{loc[0], (loc[1] + 1) % 16}.BytePrefix()
}

// created is the CREATE address of code by creator at nonce, ground into the
// zone as EVM.Create does.
func created(creator common.Address, nonce uint64, code []byte, number uint64, loc common.Location) common.Address {
	a := crypto.CreateAddress(creator, nonce, code, loc)
	if _, e := a.InternalAndQuaiAddress(); e == nil {
		return a
	}
	a, _, e := vm.GrindContract(creator, nonce, 1<<40, 30+6*int64((len(code)+31)/32), crypto.Keccak256Hash(code), u(number), loc)
	must(e)
	return a
}

// grinds tells whether EVM.Create finds an address of the zone for code.
func grinds(creator common.Address, nonce uint64, code []byte, number uint64, loc common.Location) bool {
	if _, e := crypto.CreateAddress(creator, nonce, code, loc).InternalAndQuaiAddress(); e == nil {
		return true
	}
	_, _, e := vm.GrindContract(creator, nonce, 1<<40, 30+6*int64((len(code)+31)/32), crypto.Keccak256Hash(code), u(number), loc)
	return e == nil
}

// --- precompile inputs ---------------------------------------------------

var (
	fieldP, _ = new(big.Int).SetString("21888242871839275222246405745257275088696311157297823662689037894645226208583", 10)
	orderR, _ = new(big.Int).SetString("21888242871839275222246405745257275088548364400416034343698204186575808495617", 10)
	twoTo256  = new(big.Int).Lsh(big.NewInt(1), 256)
)

func g1(k int64) []byte { return new(bn256.G1).ScalarBaseMult(big.NewInt(k)).Marshal() }
func g2(k *big.Int) []byte {
	return new(bn256.G2).ScalarBaseMult(k).Marshal()
}
func negG1(k int64) []byte {
	return new(bn256.G1).Neg(new(bn256.G1).ScalarBaseMult(big.NewInt(k))).Marshal()
}
func seq(n int) []byte {
	b := make([]byte, n)
	for i := range b {
		b[i] = byte(i*7 + 1)
	}
	return b
}

// blake2 is EIP-152 test vector 5 (BLAKE2b-512 of "abc") with the given
// round count and final-block flag: rounds, h, m, t, f.
func blake2(rounds uint32, last byte) []byte {
	iv := []uint64{0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1, 0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179}
	h := make([]byte, 64)
	for i, v := range iv {
		if i == 0 {
			v ^= 0x01010040
		}
		binary.LittleEndian.PutUint64(h[i*8:], v)
	}
	m := make([]byte, 128)
	copy(m, "abc")
	t := make([]byte, 16)
	t[0] = 3
	r := make([]byte, 4)
	binary.BigEndian.PutUint32(r, rounds)
	return join(r, h, m, t, []byte{last})
}
func ecrecoverInput() []byte {
	k := key(common.Location{0, 0})
	h := crypto.Keccak256([]byte("quai precompile"))
	sig, e := crypto.Sign(h, k)
	must(e)
	return join(h, wu(uint64(sig[64])+27), sig[:32], sig[32:64])
}
func modexp(base, exp, mod []byte) []byte {
	return join(wu(uint64(len(base))), wu(uint64(len(exp))), wu(uint64(len(mod))), base, exp, mod)
}

// valid is a successful input of precompile n.
func valid(n byte) []byte {
	switch n {
	case 1:
		return ecrecoverInput()
	case 2, 3, 4:
		return seq(32)
	case 5:
		return modexp([]byte{3}, []byte{5}, []byte{7})
	case 6:
		return join(g1(1), g1(1))
	case 7:
		return join(g1(1), wu(3))
	case 8:
		return join(g1(1), g2(big.NewInt(1)), negG1(1), g2(big.NewInt(1)))
	case 9:
		return blake2(12, 1)
	}
	panic("precompile")
}

// fp2 is a + b·i over the BN254 base field; the twist is y² = x³ + 3/(9+i).
type fp2 struct{ a, b *big.Int }

func f2(a, b *big.Int) fp2 {
	return fp2{new(big.Int).Mod(a, fieldP), new(big.Int).Mod(b, fieldP)}
}
func (x fp2) add(y fp2) fp2 { return f2(new(big.Int).Add(x.a, y.a), new(big.Int).Add(x.b, y.b)) }
func (x fp2) mul(y fp2) fp2 {
	ac := new(big.Int).Mul(x.a, y.a)
	bd := new(big.Int).Mul(x.b, y.b)
	ad := new(big.Int).Mul(x.a, y.b)
	bc := new(big.Int).Mul(x.b, y.a)
	return f2(ac.Sub(ac, bd), ad.Add(ad, bc))
}
func (x fp2) exp(e *big.Int) fp2 {
	r := f2(big.NewInt(1), big.NewInt(0))
	for i := e.BitLen() - 1; i >= 0; i-- {
		r = r.mul(r)
		if e.Bit(i) == 1 {
			r = r.mul(x)
		}
	}
	return r
}
func (x fp2) eq(y fp2) bool { return x.a.Cmp(y.a) == 0 && x.b.Cmp(y.b) == 0 }

// sqrt for p ≡ 3 mod 4 (Adj and Rodríguez-Henríquez 2012, algorithm 9),
// checked by squaring.
func (x fp2) sqrt() (fp2, bool) {
	a1 := x.exp(new(big.Int).Rsh(new(big.Int).Sub(fieldP, big.NewInt(3)), 2))
	alpha := a1.mul(a1).mul(x)
	x0 := a1.mul(x)
	minus := f2(big.NewInt(-1), big.NewInt(0))
	var r fp2
	if alpha.eq(minus) {
		r = f2(big.NewInt(0), big.NewInt(1)).mul(x0)
	} else {
		r = f2(big.NewInt(1), big.NewInt(0)).add(alpha).exp(new(big.Int).Rsh(new(big.Int).Sub(fieldP, big.NewInt(1)), 1)).mul(x0)
	}
	return r, r.mul(r).eq(x)
}

var twistB = func() fp2 {
	inv := new(big.Int).ModInverse(big.NewInt(82), fieldP)
	return f2(new(big.Int).Mul(big.NewInt(27), inv), new(big.Int).Neg(new(big.Int).Mul(big.NewInt(3), inv)))
}()

func onTwist(x, y fp2) bool { return y.mul(y).eq(x.mul(x).mul(x).add(twistB)) }

// encodeG2 is the EIP-197 order: x imaginary, x real, y imaginary, y real.
func encodeG2(x, y fp2) []byte { return join(word(x.b), word(x.a), word(y.b), word(y.a)) }
func decodeG2(b []byte) (fp2, fp2) {
	n := func(i int) *big.Int { return new(big.Int).SetBytes(b[i*32 : i*32+32]) }
	return fp2{n(1), n(0)}, fp2{n(3), n(2)}
}

// twistPoints are points on the twist found from x = seed, seed+1, ...; a
// random twist point lies outside the order-r subgroup (cofactor 2p - r).
func twistPoints(seed int64, count int) [][]byte {
	out := [][]byte{}
	for i := seed; len(out) < count; i++ {
		x := f2(big.NewInt(i), big.NewInt(i*i+5))
		if y, ok := x.mul(x).mul(x).add(twistB).sqrt(); ok {
			if !onTwist(x, y) {
				panic("twist point")
			}
			out = append(out, encodeG2(x, y))
		}
	}
	return out
}

type pre struct {
	id    string
	n     byte
	input []byte
	gas   uint64
	edges bool // also run with exactly the required gas and one less
}

func precompileInputs() []pre {
	var v []pre
	add := func(id string, n byte, input []byte, edges bool) {
		v = append(v, pre{id: id, n: n, input: input, gas: 50000000, edges: edges})
	}
	// The generator satisfies the twist equation with this twistB.
	if x, y := decodeG2(g2(big.NewInt(1))); !onTwist(x, y) {
		panic("twist constant")
	}
	// 1: ecrecover. go-quai rejects s > n/2 (crypto.ValidateSignatureValues).
	n := crypto.S256().Params().N
	half := new(big.Int).Rsh(n, 1)
	good := ecrecoverInput()
	add("ecrecover-valid", 1, good, true)
	add("ecrecover-empty", 1, nil, false)
	for _, l := range []int{1, 31, 32, 63, 64, 96, 127} {
		add(fmt.Sprintf("ecrecover-short-%d", l), 1, good[:l], false)
	}
	add("ecrecover-long", 1, join(good, seq(40)), false)
	for _, v := range []uint64{0, 1, 26, 29, 255, 27 + 256} {
		b := append([]byte{}, good...)
		copy(b[32:64], wu(v))
		add(fmt.Sprintf("ecrecover-v-%d", v), 1, b, false)
	}
	for i, field := range []struct {
		at  int
		val *big.Int
	}{{64, big.NewInt(0)}, {64, n}, {64, new(big.Int).Sub(n, big.NewInt(1))}, {64, new(big.Int).Sub(twoTo256, big.NewInt(1))}, {96, big.NewInt(0)}, {96, half}, {96, new(big.Int).Add(half, big.NewInt(1))}, {96, new(big.Int).Sub(n, big.NewInt(1))}, {96, n}, {0, big.NewInt(0)}, {0, n}, {0, new(big.Int).Sub(twoTo256, big.NewInt(1))}} {
		b := append([]byte{}, good...)
		copy(b[field.at:field.at+32], word(field.val))
		add(fmt.Sprintf("ecrecover-field-%d", i), 1, b, false)
	}
	// R = 5G and s = 1, z = 5: the recovered key s·R - z·G is the point at infinity.
	rx, ry := crypto.S256().ScalarBaseMult(wu(5))
	add("ecrecover-infinity", 1, join(wu(5), wu(27+uint64(ry.Bit(0))), word(rx), wu(1)), false)
	for r := uint64(1); r <= 8; r++ {
		add(fmt.Sprintf("ecrecover-small-r-%d", r), 1, join(wu(1), wu(27), wu(r), wu(1)), false)
	}
	rng := rand.New(rand.NewSource(57))
	for i := 0; i < 16; i++ {
		h := make([]byte, 32)
		rng.Read(h)
		k, e := crypto.ToECDSA(word(new(big.Int).Add(big.NewInt(int64(i)+2), new(big.Int).SetBytes(h[:16]))))
		must(e)
		sig, e := crypto.Sign(h, k)
		must(e)
		add(fmt.Sprintf("ecrecover-random-%d", i), 1, join(h, wu(uint64(sig[64])+27), sig[:32], sig[32:64]), false)
	}
	// 2-4: hashes and identity, with word boundaries.
	for _, id := range []byte{2, 3, 4} {
		for _, l := range []int{0, 1, 31, 32, 33, 55, 56, 64, 65, 1000} {
			add(fmt.Sprintf("hash-%d-len-%d", id, l), id, seq(l), true)
		}
	}
	// 5: modexp (EIP-198 lengths, EIP-2565 gas).
	add("modexp-valid", 5, valid(5), true)
	add("modexp-empty", 5, nil, true)
	add("modexp-zero-lengths-long-exp", 5, join(wu(0), word(new(big.Int).Lsh(big.NewInt(1), 255)), wu(0)), true)
	add("modexp-zero-base-mod", 5, join(wu(0), wu(32), wu(0), seq(32)), true)
	add("modexp-huge-base-len", 5, join(word(new(big.Int).Add(new(big.Int).Lsh(big.NewInt(1), 64), big.NewInt(1))), wu(1), wu(1), []byte{2, 3, 5}), false)
	add("modexp-huge-mod-len", 5, join(wu(1), wu(1), wu(1<<32), []byte{2, 3, 5}), false)
	add("modexp-truncated", 5, join(wu(32), wu(32), wu(32), seq(40)), true)
	add("modexp-mod-zero", 5, modexp([]byte{7}, []byte{3}, []byte{0, 0}), true)
	add("modexp-mod-one", 5, modexp([]byte{7}, []byte{3}, []byte{1}), true)
	add("modexp-exp-zero", 5, modexp([]byte{7}, []byte{0}, []byte{9}), true)
	add("modexp-base-zero-exp-zero", 5, modexp([]byte{0}, []byte{}, []byte{9}), true)
	add("modexp-base-above-mod", 5, modexp([]byte{0xff, 0xff}, []byte{3}, []byte{0x10}), true)
	add("modexp-even-mod", 5, modexp(seq(40), seq(33), join(seq(39), []byte{0x80})), true)
	add("modexp-long-exp-head", 5, modexp([]byte{3}, join([]byte{0, 0, 1}, seq(37)), seq(32)), true)
	add("modexp-long-exp-short-input", 5, join(wu(4), wu(40), wu(4), []byte{1, 2}), true)
	add("modexp-exp-len-33-zero-head", 5, modexp([]byte{5}, make([]byte, 33), []byte{11}), true)
	add("modexp-large", 5, modexp(seq(256), seq(256), join(seq(255), []byte{0x81})), true)
	add("modexp-max-gas", 5, modexp(seq(1024), seq(1024), join(seq(1023), []byte{0x81})), false)
	for i := 0; i < 64; i++ {
		lens := []int{rng.Intn(70), rng.Intn(70), rng.Intn(70)}
		parts := make([][]byte, 3)
		for j := range parts {
			parts[j] = make([]byte, lens[j])
			rng.Read(parts[j])
		}
		add(fmt.Sprintf("modexp-random-%d", i), 5, modexp(parts[0], parts[1], parts[2]), i%8 == 0)
	}
	// 6, 7: bn256 add and scalar multiplication.
	pBytes := word(fieldP)
	pPlus := word(new(big.Int).Add(fieldP, big.NewInt(1)))
	all := word(new(big.Int).Sub(twoTo256, big.NewInt(1)))
	notOnCurve := join(wu(1), wu(1))
	sqrt3 := new(big.Int).ModSqrt(big.NewInt(3), fieldP)
	g1Points := map[string][]byte{"zero": make([]byte, 64), "g": g1(1), "2g": g1(2), "neg-g": negG1(1), "x-p": join(pBytes, wu(2)), "y-p": join(wu(1), pBytes), "x-p1": join(pPlus, wu(2)), "y-max": join(wu(1), all), "off-curve": notOnCurve, "y-zero": join(wu(1), wu(0)), "x-zero": join(wu(0), wu(1))}
	if sqrt3 != nil {
		g1Points["x-zero-sqrt3"] = join(wu(0), word(sqrt3))
	}
	names := make([]string, 0, len(g1Points))
	for name := range g1Points {
		names = append(names, name)
	}
	sort.Strings(names)
	for _, a := range names {
		for _, b := range names {
			add(fmt.Sprintf("bn256-add-%s-%s", a, b), 6, join(g1Points[a], g1Points[b]), false)
		}
	}
	add("bn256-add-empty", 6, nil, true)
	add("bn256-add-short", 6, g1(1), false)
	add("bn256-add-long", 6, join(g1(1), g1(2), seq(30)), false)
	scalars := map[string]*big.Int{"0": big.NewInt(0), "1": big.NewInt(1), "2": big.NewInt(2), "r-1": new(big.Int).Sub(orderR, big.NewInt(1)), "r": orderR, "r+1": new(big.Int).Add(orderR, big.NewInt(1)), "p": fieldP, "2^256-1": new(big.Int).Sub(twoTo256, big.NewInt(1)), "2^255": new(big.Int).Lsh(big.NewInt(1), 255), "3r": new(big.Int).Mul(orderR, big.NewInt(3)), "5r-1": new(big.Int).Sub(new(big.Int).Mul(orderR, big.NewInt(5)), big.NewInt(1))}
	snames := make([]string, 0, len(scalars))
	for name := range scalars {
		snames = append(snames, name)
	}
	sort.Strings(snames)
	for _, a := range names {
		for _, s := range snames {
			add(fmt.Sprintf("bn256-mul-%s-%s", a, s), 7, join(g1Points[a], word(scalars[s])), false)
		}
	}
	for i := 0; i < 24; i++ {
		s := make([]byte, 32)
		rng.Read(s)
		add(fmt.Sprintf("bn256-mul-random-%d", i), 7, join(g1(int64(i+1)), s), false)
	}
	add("bn256-mul-empty", 7, nil, true)
	add("bn256-mul-short-scalar", 7, join(g1(1), []byte{1, 2}), false)
	add("bn256-mul-long", 7, join(g1(1), wu(5), seq(17)), false)
	// 8: pairing. Twist points outside the subgroup, at infinity, off the
	// curve and with coordinates at or above the field modulus.
	one := big.NewInt(1)
	gen2 := g2(one)
	outside := twistPoints(1, 4)
	zx := f2(big.NewInt(0), big.NewInt(0))
	if y, ok := twistB.sqrt(); ok {
		outside = append(outside, encodeG2(zx, y))
	}
	gx, gy := decodeG2(gen2)
	negGen := encodeG2(gx, f2(new(big.Int).Neg(gy.a), new(big.Int).Neg(gy.b)))
	swapped := join(gen2[32:64], gen2[0:32], gen2[96:128], gen2[64:96])
	g2Points := map[string][]byte{"zero": make([]byte, 128), "g": gen2, "2g": g2(big.NewInt(2)), "neg-g": negGen, "r-1": g2(new(big.Int).Sub(orderR, one)), "swapped": swapped, "off-curve": join(gen2[:96], wu(5)), "coord-p": join(pBytes, gen2[32:]), "coord-p1": join(gen2[:32], pPlus, gen2[64:]), "coord-max": join(gen2[:96], all)}
	for i, b := range outside {
		g2Points[fmt.Sprintf("outside-%d", i)] = b
	}
	g2Names := make([]string, 0, len(g2Points))
	for name := range g2Points {
		g2Names = append(g2Names, name)
	}
	sort.Strings(g2Names)
	for _, a := range []string{"zero", "g", "neg-g", "off-curve", "x-p"} {
		for _, b := range g2Names {
			add(fmt.Sprintf("pairing-%s-%s", a, b), 8, join(g1Points[a], g2Points[b]), false)
		}
	}
	add("pairing-empty", 8, nil, true)
	add("pairing-true", 8, valid(8), true)
	add("pairing-true-scaled", 8, join(g1(2), g2(big.NewInt(3)), negG1(6), gen2), false)
	add("pairing-false", 8, join(g1(1), gen2), false)
	add("pairing-skip-zero", 8, join(make([]byte, 64), gen2, g1(3), make([]byte, 128)), false)
	add("pairing-valid-then-outside", 8, join(valid(8), g1(1), outside[0]), false)
	add("pairing-zero-then-outside", 8, join(make([]byte, 64), outside[1]), false)
	add("pairing-outside-sum", 8, join(g1(1), outside[2], negG1(1), outside[2]), false)
	for _, l := range []int{1, 191, 193, 383} {
		b := join(valid(8), valid(8))
		add(fmt.Sprintf("pairing-length-%d", l), 8, b[:l], false)
	}
	// 9: blake2f (EIP-152).
	add("blake2f-valid", 9, blake2(12, 1), true)
	add("blake2f-empty", 9, nil, false)
	add("blake2f-212", 9, blake2(12, 1)[:212], false)
	add("blake2f-214", 9, append(blake2(12, 1), 0), false)
	add("blake2f-flag-2", 9, blake2(12, 2), false)
	add("blake2f-flag-ff", 9, blake2(12, 0xff), false)
	add("blake2f-not-final", 9, blake2(12, 0), true)
	add("blake2f-rounds-0", 9, blake2(0, 1), true)
	add("blake2f-rounds-1", 9, blake2(1, 1), true)
	add("blake2f-rounds-max", 9, blake2(0xffffffff, 1), false)
	for i := 0; i < 16; i++ {
		b := make([]byte, 213)
		rng.Read(b)
		b[0], b[1], b[2] = 0, 0, 0
		b[3] = byte(rng.Intn(40))
		b[212] = byte(i % 2)
		add(fmt.Sprintf("blake2f-random-%d", i), 9, b, false)
	}
	var out []pre
	for _, c := range v {
		out = append(out, c)
		if !c.edges {
			continue
		}
		var a common.AddressBytes
		a[19] = c.n
		required := vm.PrecompiledContracts[a].RequiredGas(c.input)
		out = append(out, pre{id: c.id + "-gas-exact", n: c.n, input: c.input, gas: required})
		if required > 0 {
			out = append(out, pre{id: c.id + "-gas-short", n: c.n, input: c.input, gas: required - 1})
		}
	}
	return out
}

func runPrecompile(c pre) map[string]interface{} {
	var a common.AddressBytes
	a[19] = c.n
	contract := vm.PrecompiledContracts[a]
	output, left, err := vm.RunPrecompiledContract(contract, c.input, c.gas)
	row := map[string]interface{}{"id": c.id, "precompile": c.n, "input": hx(c.input), "gas": c.gas, "required_gas": contract.RequiredGas(c.input), "gas_left": left, "output": hx(output), "error": nil, "out_of_gas": errors.Is(err, vm.ErrOutOfGas)}
	if err != nil {
		row["error"] = err.Error()
		row["output"] = nil
	}
	return row
}

// --- signed transactions ---------------------------------------------------

type acct struct {
	a       common.Address
	code    []byte
	balance uint64
}

type tcase struct {
	id     string
	loc    common.Location
	prime  uint64
	number uint64
	accts  []acct
	to     *common.Address
	data   []byte
	value  uint64
	gas    uint64
	list   []common.Address
	// Init codes whose CREATE addresses the access list names: the first is
	// the transaction's own creation, each next one is created by the one
	// before it (nonce 1).
	creates [][]byte
	// Contracts of accts that run CREATE with the given init code (nonce 1).
	inner []creation
}

type creation struct {
	owner common.Address
	code  []byte
}

func vmClass(e error) interface{} {
	if e == nil || errors.Is(e, vm.ErrExecutionReverted) {
		return nil
	}
	for _, x := range []struct {
		e error
		n string
	}{{vm.ErrOutOfGas, "out_of_gas"}, {vm.ErrWriteProtection, "write_protection"}, {vm.ErrInvalidAccessList, "invalid_access_list"}, {vm.ErrCodeStoreOutOfGas, "code_store_out_of_gas"}, {vm.ErrInsufficientBalance, "insufficient_balance"}, {vm.ErrReturnDataOutOfBounds, "return_data_bounds"}, {vm.ErrDepth, "depth"}, {common.ErrExternalAddress, "external_address"}} {
		if errors.Is(e, x.e) {
			return x.n
		}
	}
	var op *vm.ErrInvalidOpCode
	if errors.As(e, &op) {
		return "invalid_opcode"
	}
	if errors.Is(e, vm.ErrInvalidJump) {
		return "invalid_jump"
	}
	m := e.Error()
	if strings.HasPrefix(m, "bn256: ") || m == "bad elliptic curve pairing size" || m == "invalid input length" || m == "invalid final flag" {
		return "precompile"
	}
	if strings.Contains(m, "CreateETX") {
		return "etx"
	}
	return "vm_error"
}

func runTx(c tcase) map[string]interface{} {
	loc := c.loc
	k := key(loc)
	sender := crypto.PubkeyToAddress(k.PublicKey, loc)
	coinbase := at(loc, 0xcb)
	disk := rawdb.NewMemoryDatabase(log.Global)
	defer disk.Close()
	cache := state.NewDatabase(disk)
	s, e := state.New(types.EmptyRootHash, types.EmptyRootHash, u(0), cache, cache, nil, loc, log.Global)
	must(e)
	nonce := uint64(7)
	accounts := append([]acct{{a: sender}}, c.accts...)
	for _, a := range accounts {
		ia, e := a.a.InternalAndQuaiAddress()
		must(e)
		s.CreateAccount(ia)
		s.SetNonce(ia, 1)
		s.SetBalance(ia, u(a.balance))
		s.SetCode(ia, a.code)
	}
	from, _ := sender.InternalAndQuaiAddress()
	s.SetNonce(from, nonce)
	s.SetBalance(from, new(big.Int).Lsh(big.NewInt(1), 80))
	before, e := s.Commit(false)
	must(e)
	must(cache.TrieDB().Commit(before, false, nil))
	s, e = state.New(before, types.EmptyRootHash, u(0), cache, cache, nil, loc, log.Global)
	must(e)
	world := []interface{}{}
	for _, a := range accounts {
		ia, _ := a.a.InternalAndQuaiAddress()
		world = append(world, map[string]interface{}{"address": hx(a.a.Bytes()), "nonce": s.GetNonce(ia), "balance": hx(s.GetBalance(ia).FillBytes(make([]byte, 32))), "code": hx(s.GetCode(ia)), "storage_size": hx(s.GetSize(ia).FillBytes(make([]byte, 32))), "storage": []interface{}{}})
	}
	list := types.AccessList{}
	for _, a := range c.list {
		list = append(list, types.AccessTuple{Address: a, StorageKeys: []common.Hash{}})
	}
	creator, creatorNonce := sender, nonce
	for _, code := range c.creates {
		a := created(creator, creatorNonce, code, c.number, loc)
		list = append(list, types.AccessTuple{Address: a, StorageKeys: []common.Hash{}})
		creator, creatorNonce = a, 1
	}
	for _, x := range c.inner {
		list = append(list, types.AccessTuple{Address: created(x.owner, 1, x.code, c.number, loc), StorageKeys: []common.Hash{}})
	}
	tx := types.NewTx(&types.QuaiTx{ChainID: u(1337), Nonce: nonce, GasPrice: u(17), Gas: c.gas, To: c.to, Value: u(c.value), Data: c.data, AccessList: list})
	chain := *params.TestChainConfig
	chain.ChainID = u(1337)
	chain.Location = loc
	signer := types.MakeSigner(&chain, u(c.number))
	tx, e = types.SignTx(tx, signer, k)
	must(e)
	pb, e := tx.ProtoEncode()
	must(e)
	wire, e := proto.Marshal(pb)
	must(e)
	vm.PrecompiledContracts = make(map[common.AddressBytes]vm.PrecompiledContract)
	vm.PrecompiledAddresses = make(map[string][]common.Address)
	vm.InitializePrecompiles(loc)
	eligible := common.Hash{}
	for i := range eligible {
		eligible[i] = 255
	}
	hc := &core.HeaderChain{}
	context := vm.BlockContext{CanTransfer: core.CanTransfer, Transfer: core.Transfer, CheckIfEtxEligible: hc.CheckIfEtxIsEligible, EtxEligibleSlices: eligible, BlockNumber: u(c.number), Time: u(1769112000), Difficulty: u(1), BaseFee: u(13), QuaiStateSize: u(1 << 20), GasLimit: 30000000, PrimaryCoinbase: coinbase, PrimeTerminusNumber: c.prime, GetHash: func(n uint64) common.Hash { return common.BigToHash(u(n)) }}
	msg, e := tx.AsMessage(signer, u(13))
	must(e)
	s.Prepare(tx.Hash(), 0)
	env := vm.NewEVM(context, vm.TxContext{Origin: sender, GasPrice: u(17), TxType: 0, Hash: tx.Hash()}, s, &chain, vm.Config{}, nil)
	pool := uint64(30000000)
	gp := new(types.GasPool).AddGas(pool)
	revision := s.Snapshot()
	used, usedState, r, pr := uint64(1000), uint64(11), uint64(1000000), uint64(1000000)
	receipt, _, err := core.NativeApplySettlement(msg, &chain, gp, s, tx, env, &used, &usedState, &r, &pr)
	result := core.NativeSettlementLast
	if err != nil {
		s.RevertToSnapshot(revision)
		gp = new(types.GasPool).AddGas(pool)
	}
	logs := []interface{}{}
	for _, l := range s.Logs() {
		topics := []string{}
		for _, h := range l.Topics {
			topics = append(topics, h.Hex())
		}
		logs = append(logs, map[string]interface{}{"address": hx(l.Address.Bytes()), "topics": topics, "data": hx(l.Data)})
	}
	address := at(loc, 0)
	if c.to != nil {
		address = *c.to
	}
	out := map[string]interface{}{"id": fmt.Sprintf("%s-%02x", c.id, loc.BytePrefix()), "finalize_empty": true, "world": world, "transaction": hx(wire), "tx_hash": tx.Hash().Hex(), "eligible_slices": eligible.Hex(), "context": map[string]interface{}{"address": hx(address.Bytes()), "caller": hx(sender.Bytes()), "origin": hx(sender.Bytes()), "coinbase": hx(coinbase.Bytes()), "etx_sender": hx(sender.Bytes()), "location": []int{int(loc[0]), int(loc[1])}, "tx_type": 0, "value": hx(wu(c.value)), "gas_price": hx(wu(17)), "block_number": hx(wu(c.number)), "timestamp": hx(wu(1769112000)), "difficulty": hx(wu(1)), "base_fee": hx(wu(13)), "chain_id": hx(wu(1337)), "state_size": hx(wu(1 << 20)), "block_gas_limit": 30000000, "address_listed": true, "warm_slots": []string{}, "read_only": false, "prime_number": c.prime}, "before_root": before.Hex(), "consensus_error": nil, "initial_gas_pool": pool, "initial_cumulative_gas": 1000, "initial_state_used": 11, "gas_pool": gp.Gas(), "logs": logs}
	if err != nil {
		out["consensus_error"] = "consensus_error"
		out["reference_error"] = err.Error()
	} else {
		out["gas_used"] = result.UsedGas
		out["state_used"] = result.UsedState
		out["fees"] = hx(result.QuaiFees.FillBytes(make([]byte, 32)))
		out["output"] = hx(result.ReturnData)
		out["error"] = vmClass(result.Err)
		out["reverted"] = errors.Is(result.Err, vm.ErrExecutionReverted)
		out["cumulative_gas"] = 1000 + result.UsedGas
		out["cumulative_state"] = 11 + result.UsedState
		var contract interface{}
		if result.ContractAddr != nil {
			contract = hx(result.ContractAddr.Bytes())
		}
		out["contract_address"] = contract
		var message interface{}
		if result.Err != nil {
			message = result.Err.Error()
		}
		out["reference_error"] = message
		// The receipt RLP holds a 10,240-byte bloom; its Keccak-256 keeps
		// the corpus small and the comparison exact.
		var buf bytes.Buffer
		types.Receipts{receipt}.EncodeIndex(0, &buf)
		out["receipt_rlp_keccak"] = hx(crypto.Keccak256(buf.Bytes()))
		out["status"] = receipt.Status
	}
	out["after_root"] = s.IntermediateRoot(false).Hex()
	return out
}

var (
	locations = []common.Location{{0, 0}, {1, 2}, {15, 15}}
	// Polaris (prime terminus 2,444,574) and zone 4,000,000: after every fork.
	latePrime, lateZone = uint64(2444574), uint64(4000000)
	// Before every fork that touches the EVM.
	earlyPrime, earlyZone = uint64(100000), uint64(100000)
)

func base(id string, loc common.Location) tcase {
	return tcase{id: id, loc: loc, prime: latePrime, number: lateZone, gas: 2000000}
}

// creationCases: a creation transaction whose init code reaches a precompile
// that the access list does not name (PrepareAccessList adds the
// precompiles to every message; core/state_transition.go:380).
func creationCases() []tcase {
	var v []tcase
	for _, loc := range locations {
		forms := []byte{loc.BytePrefix()}
		if loc.BytePrefix() != 0 {
			forms = append(forms, 0)
		}
		for n := byte(1); n <= 9; n++ {
			for _, f := range forms {
				c := base(fmt.Sprintf("create-static-%d-form-%02x", n, f), loc)
				c.data = call(0xfa, form(loc, f, n), 0, valid(n))
				c.creates = [][]byte{c.data}
				v = append(v, c)
			}
		}
		for _, op := range []byte{0xf1, 0xf2, 0xf4} {
			for _, n := range []byte{2, 4} {
				c := base(fmt.Sprintf("create-op-%02x-%d", op, n), loc)
				c.data = call(op, form(loc, loc.BytePrefix(), n), 0, valid(n))
				c.creates = [][]byte{c.data}
				v = append(v, c)
			}
		}
		for _, op := range []byte{0x31, 0x3b, 0x3c, 0x3f} {
			for _, f := range forms {
				c := base(fmt.Sprintf("create-account-%02x-form-%02x", op, f), loc)
				c.data = account(op, form(loc, f, 3))
				c.creates = [][]byte{c.data}
				v = append(v, c)
			}
		}
		// The precompile listed explicitly: the same result as unlisted in Go.
		c := base("create-static-2-listed", loc)
		c.data = call(0xfa, form(loc, loc.BytePrefix(), 2), 0, valid(2))
		c.creates = [][]byte{c.data}
		c.list = []common.Address{form(loc, loc.BytePrefix(), 2)}
		v = append(v, c)
		// Too little gas for the precompile: the call fails, the creation stays.
		c = base("create-static-8-short-gas", loc)
		c.data = join(mstore(valid(8)), p(0), p(0), p(uint64(len(valid(8)))), p(0), pushAddr(form(loc, loc.BytePrefix(), 8)), p(50000), []byte{0xfa}, record())
		c.creates = [][]byte{c.data}
		v = append(v, c)
		// A nested CREATE in init code whose init code calls a precompile.
		innerInit := call(0xfa, form(loc, loc.BytePrefix(), 2), 0, valid(2))
		outerCode := join(mstore(innerInit), p(uint64(len(innerInit))), p(0), p(0), []byte{0xf0}, p(4), []byte{0x55})
		c = base("create-nested", loc)
		c.data = outerCode
		c.creates = [][]byte{outerCode, innerInit}
		v = append(v, c)
		// The same nested creation from a contract that a call reaches.
		owner := at(loc, 0xa1)
		c = base("call-create-nested", loc)
		c.accts = []acct{{a: owner, code: outerCode}}
		c.to = &owner
		c.inner = []creation{{owner, innerInit}}
		v = append(v, c)
		// Before the forks: PrepareAccessList has no fork gate. Before
		// MaxGrindIncreaseForkBlock grinding has 1,000 attempts; trailing STOP
		// bytes give an init code whose address grinding finds.
		c = base("create-static-2-early", loc)
		c.prime, c.number = earlyPrime, earlyZone
		c.data = call(0xfa, form(loc, loc.BytePrefix(), 2), 0, valid(2))
		k := key(loc)
		for !grinds(crypto.PubkeyToAddress(k.PublicKey, loc), 7, c.data, c.number, loc) {
			c.data = append(c.data, 0x00)
		}
		c.creates = [][]byte{c.data}
		v = append(v, c)
	}
	return v
}

// firstBytes are first bytes of other zones' forms of a precompile address.
func firstBytes(loc common.Location) []byte {
	return []byte{0, loc.BytePrefix(), otherZone(loc), loc.BytePrefix() ^ 0x10, 0x80, 0xff}
}

// translationCases: EVM.precompile (core/vm/evm.go:55) replaces the first
// byte of any address with the zone prefix before it looks for a precompile.
func translationCases() []tcase {
	var v []tcase
	for _, loc := range locations {
		seen := map[byte]bool{}
		for _, f := range firstBytes(loc) {
			if seen[f] {
				continue
			}
			seen[f] = true
			for n := byte(1); n <= 9; n++ {
				to := form(loc, f, n)
				c := base(fmt.Sprintf("top-%d-form-%02x", n, f), loc)
				c.to = &to
				c.data = valid(n)
				v = append(v, c)
			}
			to := form(loc, f, 2)
			c := base(fmt.Sprintf("top-value-2-form-%02x", f), loc)
			c.to, c.data, c.value = &to, valid(2), 13
			v = append(v, c)
			for _, last := range []byte{0, 10, 11} {
				to := form(loc, f, last)
				c := base(fmt.Sprintf("top-non-precompile-%d-form-%02x", last, f), loc)
				c.to, c.value, c.gas = &to, 5, 200000
				v = append(v, c)
			}
			for _, op := range []byte{0xf1, 0xf2, 0xf4, 0xfa} {
				for _, n := range []byte{2, 9} {
					for _, listed := range []bool{true, false} {
						caller := at(loc, 0xa1)
						c := base(fmt.Sprintf("nested-%02x-%d-form-%02x-listed-%t", op, n, f, listed), loc)
						c.accts = []acct{{a: caller, code: join(call(op, form(loc, f, n), 0, valid(n)), marker())}}
						c.to = &caller
						if listed {
							c.list = []common.Address{form(loc, f, n)}
						}
						v = append(v, c)
					}
				}
			}
			for _, op := range []byte{0x31, 0x3b, 0x3c, 0x3f} {
				caller := at(loc, 0xa1)
				c := base(fmt.Sprintf("account-%02x-form-%02x", op, f), loc)
				c.accts = []acct{{a: caller, code: join(account(op, form(loc, f, 2)), marker())}}
				c.to = &caller
				c.list = []common.Address{form(loc, f, 2)}
				v = append(v, c)
			}
		}
	}
	// Before the forks: EVM.precompile has no fork gate.
	for _, loc := range locations {
		to := form(loc, otherZone(loc), 2)
		c := base("top-2-other-zone-early", loc)
		c.prime, c.number = earlyPrime, earlyZone
		c.to, c.data = &to, valid(2)
		v = append(v, c)
	}
	return v
}

// scopeCases: a frame that fails with common.ErrExternalAddress makes the
// frame that called it fail too (core/vm/instructions.go:722, 762, 794,
// 826); other errors leave the caller running.
func scopeCases() []tcase {
	var v []tcase
	for _, loc := range locations {
		outer, inner, deep := at(loc, 0xa1), at(loc, 0xb1), at(loc, 0xc1)
		targets := map[string]common.Address{
			"other-zone": remote(loc, otherZone(loc), false, 0x77),
			"other-qi":   remote(loc, otherZone(loc), true, 0x77),
			"local-qi":   remote(loc, loc.BytePrefix(), true, 0x77),
			"local":      at(loc, 0x55),
		}
		names := []string{"local", "local-qi", "other-qi", "other-zone"}
		for _, name := range names {
			x := targets[name]
			for _, op := range []byte{0x31, 0x3c, 0x3f} {
				// outer CALLs inner; inner reads x.
				c := base(fmt.Sprintf("scope-%02x-%s", op, name), loc)
				c.accts = []acct{{a: outer, code: join(call(0xf1, inner, 0, nil), marker())}, {a: inner, code: join(account(op, x), marker())}}
				c.to = &outer
				c.list = []common.Address{inner, x}
				v = append(v, c)
			}
			for _, op := range []byte{0xf1, 0xf2, 0xf4, 0xfa} {
				// outer CALLs inner; inner runs op on x.
				c := base(fmt.Sprintf("scope-call-%02x-%s", op, name), loc)
				c.accts = []acct{{a: outer, code: join(call(0xf1, inner, 0, nil), marker())}, {a: inner, code: join(call(op, x, 0, nil), marker())}}
				c.to = &outer
				c.list = []common.Address{inner, x}
				v = append(v, c)
				// The same op at the top frame.
				c = base(fmt.Sprintf("scope-top-%02x-%s", op, name), loc)
				c.accts = []acct{{a: outer, code: join(call(op, x, 0, nil), marker())}}
				c.to = &outer
				c.list = []common.Address{x}
				v = append(v, c)
			}
			// CALLCODE with value 1 from a balance of 1: the value check
			// passes, then the address scope decides.
			c := base(fmt.Sprintf("scope-callcode-value-%s", name), loc)
			c.accts = []acct{{a: outer, code: join(call(0xf2, x, 1, nil), marker()), balance: 1}}
			c.to = &outer
			c.list = []common.Address{x}
			v = append(v, c)
			// Three frames: outer -> inner -> deep -> BALANCE(x).
			c = base(fmt.Sprintf("scope-deep-%s", name), loc)
			c.accts = []acct{{a: outer, code: join(call(0xf1, inner, 0, nil), marker())}, {a: inner, code: join(call(0xf4, deep, 0, nil), marker())}, {a: deep, code: join(account(0x31, x), marker())}}
			c.to = &outer
			c.list = []common.Address{inner, deep, x}
			v = append(v, c)
			// CREATE stops the error: opCreate pushes zero.
			initCode := join(account(0x31, x), []byte{0x00})
			c = base(fmt.Sprintf("scope-create-%s", name), loc)
			c.accts = []acct{{a: outer, code: join(mstore(initCode), p(uint64(len(initCode))), p(0), p(0), []byte{0xf0}, p(4), []byte{0x55}, marker())}}
			c.to = &outer
			c.list = []common.Address{x}
			c.inner = []creation{{outer, initCode}}
			v = append(v, c)
			// SELFDESTRUCT to x: its gas function fails first (out of gas).
			c = base(fmt.Sprintf("scope-selfdestruct-%s", name), loc)
			c.accts = []acct{{a: outer, code: join(call(0xf1, inner, 0, nil), marker())}, {a: inner, code: join(pushAddr(x), []byte{0xff})}}
			c.to = &outer
			c.list = []common.Address{inner, x}
			v = append(v, c)
		}
	}
	return v
}

// lockupCases: the lockup contract (core/vm/contracts.go:494) from a contract
// and from the top frame. Address checks return common.ErrExternalAddress for
// another zone's address; EVM.Call checks the value before the lockup branch
// (core/vm/evm.go:260-272).
func lockupCases() []tcase {
	var v []tcase
	for _, loc := range []common.Location{{0, 0}, {1, 2}} {
		lock := form(loc, loc.BytePrefix(), 10)
		local, localQi := at(loc, 0x55), remote(loc, loc.BytePrefix(), true, 0x56)
		other, otherQi := remote(loc, otherZone(loc), false, 0x57), remote(loc, otherZone(loc), true, 0x58)
		addrs := map[string]common.Address{"local": local, "local-qi": localQi, "other-zone": other, "other-qi": otherQi}
		inputs := map[string][]byte{}
		for name, a := range addrs {
			inputs["get-"+name] = join(a.Bytes(), []byte{2}, []byte{0, 0, 0, 1})
			inputs["claim-"+name] = join(a.Bytes(), local.Bytes(), []byte{2}, []byte{0, 0, 0, 1}, []byte{0, 0, 0, 0, 0, 0, 0x52, 0x08})
			inputs["deposit-"+name] = a.Bytes()
			inputs["unwrap-"+name] = join(a.Bytes(), wu(5), []byte{0, 0, 0, 0, 0, 0, 0x52, 0x08})
		}
		inputs["latest-local"] = join(local.Bytes(), []byte{2})
		for _, l := range []int{0, 19, 22, 54, 61} {
			inputs[fmt.Sprintf("length-%d", l)] = seq(l)
		}
		names := make([]string, 0, len(inputs))
		for name := range inputs {
			names = append(names, name)
		}
		sort.Strings(names)
		outer := at(loc, 0xa1)
		for _, name := range names {
			c := base(fmt.Sprintf("lockup-nested-%s", name), loc)
			c.accts = []acct{{a: outer, code: join(call(0xf1, lock, 0, inputs[name]), marker())}}
			c.to = &outer
			c.list = []common.Address{lock}
			v = append(v, c)
			c = base(fmt.Sprintf("lockup-top-%s", name), loc)
			c.to, c.data, c.list = &lock, inputs[name], []common.Address{lock}
			v = append(v, c)
		}
	}
	return v
}

// balanceCases: EVM.Call checks the value against the balance before the
// lockup branch and before the address scope (core/vm/evm.go:260-282);
// EVM.CallCode checks it before the precompile and the scope
// (core/vm/evm.go:366-381). The failure returns the gas to the caller.
func balanceCases() []tcase {
	var v []tcase
	for _, loc := range []common.Location{{0, 0}, {1, 2}} {
		lock := form(loc, loc.BytePrefix(), 10)
		local, other := at(loc, 0x55), remote(loc, otherZone(loc), false, 0x57)
		latest := join(local.Bytes(), []byte{2})
		outer := at(loc, 0xa1)
		for _, balance := range []uint64{0, 1} {
			c := base(fmt.Sprintf("balance-lockup-%d", balance), loc)
			c.accts = []acct{{a: outer, code: join(call(0xf1, lock, 1, latest), marker()), balance: balance}}
			c.to = &outer
			c.list = []common.Address{lock}
			v = append(v, c)
			for _, target := range []struct {
				name string
				a    common.Address
			}{{"other-zone", other}, {"local", local}, {"precompile", form(loc, loc.BytePrefix(), 4)}, {"other-precompile", form(loc, otherZone(loc), 4)}} {
				if target.name == "other-zone" && balance == 1 {
					continue // the scope group has it
				}
				c := base(fmt.Sprintf("balance-callcode-%s-%d", target.name, balance), loc)
				c.accts = []acct{{a: outer, code: join(call(0xf2, target.a, 1, seq(8)), marker()), balance: balance}}
				c.to = &outer
				c.list = []common.Address{target.a}
				v = append(v, c)
			}
		}
	}
	return v
}

func main() {
	vm.PrecompiledContracts = make(map[common.AddressBytes]vm.PrecompiledContract)
	vm.PrecompiledAddresses = make(map[string][]common.Address)
	vm.InitializePrecompiles(common.Location{0, 0})
	runs := []interface{}{}
	for _, c := range precompileInputs() {
		runs = append(runs, runPrecompile(c))
	}
	groups := map[string][]tcase{"creation": creationCases(), "translation": translationCases(), "balance": balanceCases(), "scope": scopeCases(), "lockup": lockupCases()}
	vectors := []interface{}{}
	counts := map[string]int{}
	for _, name := range []string{"creation", "translation", "balance", "scope", "lockup"} {
		for _, c := range groups[name] {
			row := runTx(c)
			row["group"] = name
			vectors = append(vectors, row)
		}
		counts[name] = len(groups[name])
	}
	must(json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"reference_revision": referenceRevision, "groups": counts, "runs": runs, "vectors": vectors, "scope": "Signed Quai transactions through the actual applyTransaction on committed private state (precompile addresses of every zone form, the access list of a creation, address-scope errors in nested frames, lockup-contract calls), and precompile inputs through vm.RunPrecompiledContract. No block admission or public network."}))
}
