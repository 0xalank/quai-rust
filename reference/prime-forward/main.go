// Explicit offline prime forward oracle. No chain validity is asserted.
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
	v := core.NativePrimeForwardVectors()
	v["reference_revision"] = referenceRevision
	v["scope"] = "Linked supplied-history prime conversion outputs and token choices through the exact official Slice.Append economic branch. Header fields are accepted by the unmodified parent verifier. A one-write parent probe supplies the initial choice state. Proof work, full append acceptance and canonical ancestry are not established."
	if err := json.NewEncoder(os.Stdout).Encode(v); err != nil {
		panic(err)
	}
}
