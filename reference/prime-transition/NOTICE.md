Official prime branch of Slice.Append

The offline driver copies the prime branch of `Slice.Append` from dominant-strategies/go-quai core/slice.go at v0.57.0 (33db82f2891b267ba898ca38bd2dd8bf75cbb072), under GPL-3.0, byte-identical into `core-template.go`. The branch calls the unmodified `verifyParentExchangeRateAndFlowAmount`, `CalculateTokenChoicesSet` and `CalculateBetaFromMiningChoiceAndConversions` of the same revision. The extraction range and hash are recorded with the corpus. No Go code enters the default native build or distribution.
