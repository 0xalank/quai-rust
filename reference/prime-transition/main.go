// Explicit offline prime transition oracle. No chain validity is asserted.
package main

import (
	"encoding/json"
	"os"

	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/log"
)

var referenceRevision string

func main() {
	log.Global.SetOutput(os.Stderr)
	v := core.NativePrimeTransitionVectors()
	v["reference_revision"] = referenceRevision
	v["scope"] = "Linked prime blocks across ControllerKickInBlock, the first feedback rate, ConversionSlipChangeBlock, the first two KQuaiChangeTable rows and their hold ends, the KawPow reset and the KQuai freeze, through the byte-identical prime branch of Slice.Append (which calls the unmodified verifyParentExchangeRateAndFlowAmount) and HeaderChain.ComputeMinerDifficulty. Header fields come from the unmodified verifier; changed fields and changed history are run through the same code. GetKQuaiAndUpdateBit is StateProcessor.GetKQuaiAndUpdateBit. Proof work, entropy, rollups and canonical ancestry are not established."
	if err := json.NewEncoder(os.Stdout).Encode(v); err != nil {
		panic(err)
	}
}
