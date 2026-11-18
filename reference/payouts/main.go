package main

import (
	"encoding/json"
	"github.com/dominant-strategies/go-quai/core"
	"os"
)

var referenceRevision string

func main() {
	json.NewEncoder(os.Stdout).Encode(map[string]interface{}{"reference_revision": referenceRevision, "scope": "Exact StateProcessor.Process fee feedback and reward-depth branch with controlled work hashes; arithmetic, not proof or complete block admission.", "vectors": core.NativePayoutVectors()})
}
