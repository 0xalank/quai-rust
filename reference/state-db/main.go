// Explicit offline oracle using the pinned native Go trie implementation.
package main

import (
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/rawdb"
	"github.com/dominant-strategies/go-quai/crypto"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/trie"
	"os"
)

var referenceRevision string

func must(e error) {
	if e != nil {
		panic(e)
	}
}
func hexBytes(b []byte) string { return "0x" + hex.EncodeToString(b) }
func key(n uint64) []byte      { b := make([]byte, 8); binary.BigEndian.PutUint64(b, n); return b }
func value(n uint64) []byte {
	b := make([]byte, 16)
	binary.BigEndian.PutUint64(b, n)
	binary.BigEndian.PutUint64(b[8:], n^0x5a5a5a5a5a5a5a5a)
	return b
}
func fresh() (*trie.Trie, *trie.Database) {
	db := trie.NewDatabase(rawdb.NewMemoryDatabase(log.Global))
	t, e := trie.New(common.Hash{}, db)
	must(e)
	return t, db
}
func snapshot(t *trie.Trie, db *trie.Database) *trie.Trie {
	root, e := t.Commit(nil)
	must(e)
	copy, e := trie.New(root, db)
	must(e)
	return copy
}
func main() {
	cases := []interface{}{}
	for _, mode := range []string{"secure", "hashed"} {
		t, db := fresh()
		steps := []interface{}{}
		var retained *trie.Trie
		apply := func(k, v []byte) {
			actual := k
			if mode == "secure" {
				actual = crypto.Keccak256(k)
			}
			if len(v) == 0 {
				must(t.TryDelete(actual))
			} else {
				must(t.TryUpdate(actual, v))
			}
			got, e := t.TryGet(actual)
			must(e)
			steps = append(steps, map[string]interface{}{"key": hexBytes(k), "value": hexBytes(v), "get": hexBytes(got), "root": t.Hash().Hex()})
		}
		if mode == "hashed" {
			for _, last := range []byte{0, 1, 2, 15, 16, 255} {
				k := make([]byte, 32)
				k[31] = last
				apply(k, []byte{last, 1})
			}
			retained = snapshot(t, db)
			for _, last := range []byte{2, 16, 1, 255, 0, 15} {
				k := make([]byte, 32)
				k[31] = last
				apply(k, nil)
			}
		} else {
			sizes := []int{1, 2, 31, 32, 33, 55, 56, 255, 256, 4096}
			for i := uint64(0); i < 1500; i++ {
				k := key((i * 7919) % 521)
				var v []byte
				if i%7 != 0 {
					v = bytes.Repeat([]byte{byte(i % 251)}, sizes[i%uint64(len(sizes))])
				}
				apply(k, v)
				if i == 749 {
					retained = snapshot(t, db)
				}
			}
		}
		cases = append(cases, map[string]interface{}{"mode": mode, "steps": steps, "retained_root": retained.Hash().Hex(), "retained_at": map[bool]int{true: 5, false: 749}[mode == "hashed"]})
	}
	t, db := fresh()
	checkpoints := []interface{}{}
	for i := uint64(0); i < 70000; i++ {
		must(t.TryUpdate(crypto.Keccak256(key(i)), value(i)))
		if (i+1)%5000 == 0 {
			checkpoints = append(checkpoints, map[string]interface{}{"count": i + 1, "root": t.Hash().Hex()})
		}
	}
	base := snapshot(t, db)
	baseRoot := base.Hash().Hex()
	for i := uint64(0); i < 100; i++ {
		k := crypto.Keccak256(key(i * 677))
		if i%3 == 0 {
			must(t.TryDelete(k))
		} else {
			must(t.TryUpdate(k, value(100000+i)))
		}
	}
	queries := []interface{}{}
	for i := uint64(0); i < 100; i++ {
		k := key(i * 677)
		a, e := base.TryGet(crypto.Keccak256(k))
		must(e)
		b, e := t.TryGet(crypto.Keccak256(k))
		must(e)
		queries = append(queries, map[string]interface{}{"key": hexBytes(k), "base": hexBytes(a), "changed": hexBytes(b)})
	}
	history := []interface{}{}
	ht, _ := fresh()
	for i := uint64(0); i < 5000; i++ {
		must(ht.TryUpdate(crypto.Keccak256(key(i%256)), value(i)))
		if (i+1)%500 == 0 {
			history = append(history, map[string]interface{}{"count": i + 1, "root": ht.Hash().Hex(), "key": hexBytes(key(i % 256)), "value": hexBytes(value(i))})
		}
	}
	out := map[string]interface{}{"reference_revision": referenceRevision, "cases": cases, "history": history, "bulk": map[string]interface{}{"count": 70000, "checkpoints": checkpoints, "base_root": baseRoot, "changed_root": t.Hash().Hex(), "queries": queries}, "scope": "Actual Go TryUpdate/TryDelete/TryGet/Hash on secure and fixed hashed keys; storage primitives only, not block consensus or network synchronization."}
	must(json.NewEncoder(os.Stdout).Encode(out))
}
