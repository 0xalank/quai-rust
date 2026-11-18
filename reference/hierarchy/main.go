// Explicit offline prime/region oracle. Never a runtime dependency.
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
	v := core.NativeHierarchyVectors()
	v["reference_revision"] = referenceRevision
	v["scope"] = "Bounded prime/region header policy, uint16 efficiency/threshold control, miner-difficulty EMA and ETX eligibility. Controlled parent-work projections are separate from native proof/body fixtures. No canonical history, economic replay, hierarchy placement or sync."
	if e := json.NewEncoder(os.Stdout).Encode(v); e != nil {
		panic(e)
	}
}
