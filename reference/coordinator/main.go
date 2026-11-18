// Explicit offline bounded coordinator oracle. No live peers or production backends.
package main

import (
	"encoding/json"
	"github.com/dominant-strategies/go-quai/cmd/utils"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/sirupsen/logrus"
	"os"
)

var referenceRevision string

func main() {
	log.Global.SetOutput(os.Stderr)
	log.Global.SetLevel(logrus.FatalLevel)
	f, e := os.Open(os.Args[1])
	if e != nil {
		panic(e)
	}
	defer f.Close()
	var input map[string]interface{}
	if e = json.NewDecoder(f).Decode(&input); e != nil {
		panic(e)
	}
	value := map[string]interface{}{"reference_revision": referenceRevision, "policy": utils.NativeSearchVectors(), "native": utils.NativeSearchReplay(input["native"].(map[string]interface{})), "scope": "Bounded recent-cache/leader ordering, actual PendingHeadersMap search with observation hooks, actual BuildPendingHeaders replay with worker dispatch suppressed, and virtual-time ReapplicationLoop. Imported metadata remains unverified; no generation workers, canonical state or sync."}
	if e = json.NewEncoder(os.Stdout).Encode(value); e != nil {
		panic(e)
	}
}
