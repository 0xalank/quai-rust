package main

import (
	"encoding/json"
	"github.com/dominant-strategies/go-quai/core"
	"os"
)

var referenceRevision string

func main() {
	v := core.NativeZoneBlocks()
	v["reference_revision"] = referenceRevision
	json.NewEncoder(os.Stdout).Encode(v)
}
