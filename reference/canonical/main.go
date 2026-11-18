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
	b, e := os.ReadFile(os.Args[1])
	if e != nil {
		panic(e)
	}
	v := map[string]interface{}{}
	if e = json.Unmarshal(b, &v); e != nil {
		panic(e)
	}
	utils.NativeCanonicalReplay(v)
	core.NativeCanonicalTopology(v)
	delete(v, "arithmetic")
	v["reference_revision"] = referenceRevision
	v["canonical_scope"] = "Actual NodeSet/BuildPendingHeaders and SetCurrentHeader topology on previously native-Go executed early private branches; no generation workers, production target or complete coordinator/sync qualification."
	json.NewEncoder(os.Stdout).Encode(v)
}
