// Explicit offline candidate-append oracle. Never part of runtime/build.
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
	v := core.NativePlacementVectors()
	v["reference_revision"] = referenceRevision
	v["scope"] = "Bounded candidate order/routing, parent manifest/interlink commitments, manifest/interlink advancement and PCRC. Controlled work projections and cached-header ancestry projections are distinct from actual native proof/header/body append preparation. All imported history remains unverified; no canonical or economic state publication."
	if e := json.NewEncoder(os.Stdout).Encode(v); e != nil {
		panic(e)
	}
}
