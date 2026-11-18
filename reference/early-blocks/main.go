package main

import (
	"encoding/json"
	"github.com/dominant-strategies/go-quai/core"
	"os"
)

var referenceRevision string

func main() {
	v := core.NativeEarlyBlocks()
	v["reference_revision"] = referenceRevision
	if e := json.NewEncoder(os.Stdout).Encode(v); e != nil {
		panic(e)
	}
}
