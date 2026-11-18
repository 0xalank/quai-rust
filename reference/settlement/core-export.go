// Observation-only adapter for the actual private applyTransaction entrypoint.
package core

import (
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core/state"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/core/vm"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"math/big"
)

var NativeSettlementLast *ExecutionResult

func NativeApplySettlement(msg types.Message, config *params.ChainConfig, gp *types.GasPool, db *state.StateDB, tx *types.Transaction, evm *vm.EVM, usedGas, usedState, r, p *uint64) (*types.Receipt, *big.Int, error) {
	NativeSettlementLast = nil
	return applyTransaction(msg, nil, config, nil, gp, db, big.NewInt(4000000), common.Hash{}, tx, usedGas, usedState, evm, r, p, log.Global)
}
