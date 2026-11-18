// Offline chain-spec evidence: unexported header limits of package core.
package core

// NativeChainSpecCoreLimits exposes the unexported header-time bound that
// verifyHeader applies (core/headerchain_validation.go).
func NativeChainSpecCoreLimits() map[string]interface{} {
	return map[string]interface{}{
		"allowed_future_block_time_seconds": allowedFutureBlockTimeSeconds,
	}
}
