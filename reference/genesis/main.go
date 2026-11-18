// Explicit offline genesis and candidate branch oracle. Never part of runtime/build.
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
	v := core.NativeGenesisVectors()
	v["reference_revision"] = referenceRevision
	v["scope"] = "Bounded Genesis.ToBlock/Commit payloads and candidate seed projections, expansion geometry and genesis difficulty arithmetic; native multi-block competing candidate branches with atomic storage and no canonical fork choice or state replay."
	if e := json.NewEncoder(os.Stdout).Encode(v); e != nil {
		panic(e)
	}
}
