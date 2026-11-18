// Explicit Polaris boundary oracle on the reviewed upstream source.
package main

import (
	"encoding/json"
	"github.com/dominant-strategies/go-quai/core"
	"os"
)

var referenceRevision string

func main() {
	v := core.NativePolarisVectors()
	v["reference_revision"] = referenceRevision
	if e := json.NewEncoder(os.Stdout).Encode(v); e != nil {
		panic(e)
	}
}
