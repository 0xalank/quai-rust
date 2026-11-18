// Explicit offline compatibility corpus; never a runtime dependency.
package main

import (
	"encoding/json"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/log"
	"os"
)

var referenceRevision string

func main() {
	log.Global.SetOutput(os.Stderr)
	v := core.NativeHistoryVectors()
	v["reference_revision"] = referenceRevision
	v["scope"] = "Bounded positive controller arithmetic, native donor hashes, controlled-hash classification and pre-KAW inclusion policy, stored complete zone ancestry distances. Also actual native signed KAWPOW inclusion cases on supplied candidate ancestry at four forks. Controlled hashes do not establish valid PoW or canonical history."
	if e := json.NewEncoder(os.Stdout).Encode(v); e != nil {
		panic(e)
	}
}
