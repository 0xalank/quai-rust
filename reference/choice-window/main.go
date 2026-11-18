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
	v := core.NativeChoiceWindowVectors()
	v["reference_revision"] = referenceRevision
	if e := json.NewEncoder(os.Stdout).Encode(v); e != nil {
		panic(e)
	}
}
