// Explicit offline coordinator eligibility oracle. No production backends.
package main

import (
	"encoding/json"
	"github.com/dominant-strategies/go-quai/cmd/utils"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/log"
	"os"
)

var referenceRevision string

func main() {
	log.Global.SetOutput(os.Stderr)
	native := core.NativeEligibilityComponents()
	v := map[string]interface{}{"policy": utils.NativeEligibilityVectors(), "native": utils.NativeEligibilityReplay(native), "reference_revision": referenceRevision, "scope": "Bounded imported multi-context ancestry, constraint tracing and frontier PCRC, using actual coordinator methods and native prime-coincident components. No global leader search, economic replay, canonical publication or sync."}
	if e := json.NewEncoder(os.Stdout).Encode(v); e != nil {
		panic(e)
	}
}
