// Observation-only adapter for the RPC inventory oracle
// (reference/rpc-inventory): the node configuration that makeConfigNode
// (cmd/utils/cmd.go) builds for a node at a location from the bound flags.
package utils

import (
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/node"
)

// RPCInventoryNodeConfig is makeConfigNode's node configuration: the
// defaults of defaultNodeConfig, the location, then SetNodeConfig.
func RPCInventoryNodeConfig(location common.Location) node.Config {
	cfg := defaultNodeConfig()
	cfg.NodeLocation = location
	SetNodeConfig(&cfg, location, log.Global)
	return cfg
}
