// Explicit offline zone-header oracle; never a runtime dependency.
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
	v := core.NativeHeaderVectors()
	v["reference_revision"] = referenceRevision
	v["scope"] = "Bounded positive reward/conversion/base-fee arithmetic and zone header policy on candidate history. Controlled parent hashes only qualify policy projections; separate native fixtures verify actual parent/candidate proofs. No economic replay, canonical authority or native sync."
	if e := json.NewEncoder(os.Stdout).Encode(v); e != nil {
		panic(e)
	}
}
