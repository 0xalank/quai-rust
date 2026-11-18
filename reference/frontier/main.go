// Explicit offline startup and coordinator oracle. Not part of the Rust runtime.
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
	v := core.NativeFrontierCoreVectors()
	v["coordinator"] = utils.NativeCoordinatorVectors()
	v["native_frontiers"] = utils.NativeCoordinatorReplay(v["profiles"].([]interface{}))
	v["reference_revision"] = referenceRevision
	v["scope"] = "Constructed expanded-genesis startup on supplied candidate histories; native candidate PoEM scores and coordinator NodeSet Extendable/Update/Entropy plus entropy-keyed pending projections. No allocations, durable state, global eligibility, canonical fork choice or sync."
	if e := json.NewEncoder(os.Stdout).Encode(v); e != nil {
		panic(e)
	}
}
