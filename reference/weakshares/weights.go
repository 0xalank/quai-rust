// Offline projections through the pinned big.Float/TwoToTheX operation sequence.
package core

import (
	"crypto/sha256"
	"encoding/binary"
	"github.com/dominant-strategies/go-quai/common"
	bigMath "github.com/dominant-strategies/go-quai/common/math"
	"math/big"
)

func weakWeight(deficit *big.Int, distance uint64) *big.Int {
	extra := new(big.Float).Quo(new(big.Float).SetInt(deficit), new(big.Float).SetInt(common.Big2e64))
	adjusted := new(big.Float).Quo(new(big.Float).SetInt(common.Big2e64), bigMath.TwoToTheX(extra))
	value, _ := adjusted.Int(nil)
	return value.Div(value, new(big.Int).Lsh(big.NewInt(1), uint(distance)))
}
func weakArithmetic() []interface{} {
	rows := []interface{}{}
	seen := map[string]bool{}
	add := func(n *big.Int) {
		if n.Sign() < 0 || n.Cmp(new(big.Int).Lsh(big.NewInt(256), 64)) > 0 || seen[n.String()] {
			return
		}
		seen[n.String()] = true
		for d := uint64(0); d <= 4; d++ {
			rows = append(rows, map[string]interface{}{"deficit": n.String(), "distance": d, "weight": weakWeight(n, d).String()})
		}
	}
	fractions := []*big.Int{big.NewInt(0), big.NewInt(1), big.NewInt(2), big.NewInt(3), new(big.Int).Sub(new(big.Int).Lsh(big.NewInt(1), 63), big.NewInt(1)), new(big.Int).Lsh(big.NewInt(1), 63), new(big.Int).Add(new(big.Int).Lsh(big.NewInt(1), 63), big.NewInt(1)), new(big.Int).Sub(common.Big2e64, big.NewInt(2)), new(big.Int).Sub(common.Big2e64, big.NewInt(1))}
	for _, i := range []int64{0, 1, 2, 3, 7, 15, 31, 63, 64, 127, 128, 191, 255, 256} {
		for _, f := range fractions {
			n := new(big.Int).Add(new(big.Int).Lsh(big.NewInt(i), 64), f)
			add(n)
			add(new(big.Int).Sub(n, big.NewInt(1)))
		}
	}
	for i := uint64(0); i < 1024; i++ {
		var seed [8]byte
		binary.BigEndian.PutUint64(seed[:], i)
		hash := sha256.Sum256(seed[:])
		n := new(big.Int).SetBytes(hash[:])
		limit := int64(65)
		if i%2 == 1 {
			limit = 256
		}
		n.Mod(n, new(big.Int).Lsh(big.NewInt(limit), 64))
		add(n)
	}
	return rows
}
