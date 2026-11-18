// Compiled only into the isolated local-fixture CLI via a Go overlay.
// It changes genesis allocations and activates transactions at height zero. All block,
// transaction, execution, proof-of-work and state validation stays in go-quai.
package main

import (
	"fmt"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/params"
	"os"
	"strings"
)

func init() {
	hash := os.Getenv("QUAI_LOCAL_FIXTURE_ALLOC_HASH")
	if hash == "" {
		return
	}
	required := []string{"start", "--node.environment=local", "--node.consensus-engine=blake3", "--node.solo=true", "--node.ipaddr=127.0.0.1", "--rpc.http-addr=127.0.0.1"}
	for _, want := range required {
		found := false
		for _, arg := range os.Args[1:] {
			if arg == want {
				found = true
			}
		}
		if !found {
			fmt.Fprintln(os.Stderr, "local fixture requires isolated local blake3 configuration")
			os.Exit(2)
		}
	}
	if len(hash) != 66 || !strings.HasPrefix(hash, "0x") {
		panic("invalid fixture allocation hash")
	}
	params.AllocHash = common.HexToHash(hash)
	params.TimeToStartTx = 0
	g := core.DefaultLocalGenesisBlock("blake3", 0, nil)
	g.AllocHash = params.AllocHash
	params.Blake3PowLocalGenesisHash = g.ToBlock(0).Hash()
}
