// Explicit offline controller oracle. No chain validity is asserted.
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
	v := core.NativeControllerVectors()
	v["reference_revision"] = referenceRevision
	v["scope"] = "Controlled economic arithmetic through unmodified official HeaderChain methods. Historical rates are supplied; no block acceptance, checkpoint authority or canonical synchronization."
	if err := json.NewEncoder(os.Stdout).Encode(v); err != nil {
		panic(err)
	}
}
