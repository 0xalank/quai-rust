// Explicit offline chain-spec oracle. It prints the go-quai values that the
// Rust ChainSpec and ForkSchedule copy. Never part of the runtime or build.
package main

import (
	"encoding/json"
	"fmt"
	"math/big"
	"os"

	"github.com/dominant-strategies/go-quai/cmd/utils"
	"github.com/dominant-strategies/go-quai/common"
	"github.com/dominant-strategies/go-quai/core"
	"github.com/dominant-strategies/go-quai/core/types"
	"github.com/dominant-strategies/go-quai/log"
	"github.com/dominant-strategies/go-quai/params"
	"github.com/spf13/viper"
)

var referenceRevision string

func dec(v *big.Int) string { return v.String() }
func hex(b []byte) string   { return fmt.Sprintf("0x%x", b) }

func main() {
	log.Global.SetOutput(os.Stderr)
	// cmd/utils/flags.go reads the flag defaults through viper.
	viper.SetDefault(utils.GenesisNonce.Name, utils.GenesisNonce.Value)
	viper.SetDefault(utils.ConsensusEngineFlag.Name, utils.ConsensusEngineFlag.Value)
	engine := viper.GetString(utils.ConsensusEngineFlag.Name)
	nonce, extra := utils.GetGenesisNonce()
	g := core.DefaultColosseumGenesisBlock(engine, nonce, extra)
	g.AllocHash = params.AllocHash // cmd/utils/flags.go setGenesis
	block := g.ToBlock(0)
	trim := map[string]uint64{}
	for d, depth := range types.TrimDepths {
		trim[fmt.Sprint(d)] = depth
	}
	hierarchy := [][]uint64{}
	for e := 0; e <= 30; e++ {
		r, z := common.GetHierarchySizeForExpansionNumber(uint8(e))
		hierarchy = append(hierarchy, []uint64{r, z})
	}
	v := map[string]interface{}{
		"reference_revision": referenceRevision,
		"scope":              "Pinned go-quai v0.57.0 chain parameters, fork heights and the colosseum genesis. Values only; no block acceptance, state or network authority.",
		"colosseum": map[string]interface{}{
			"name":                   params.ColosseumName,
			"chain_id":               dec(params.ProgpowColosseumChainConfig.ChainID),
			"consensus_engine":       engine,
			"genesis_hash":           params.ProgpowColosseumGenesisHash.Hex(),
			"rebuilt_genesis_hash":   block.Hash().Hex(),
			"genesis_nonce_flag":     utils.GenesisNonce.Value,
			"genesis_nonce":          fmt.Sprint(g.Nonce),
			"genesis_extra":          hex(g.ExtraData),
			"genesis_timestamp":      g.Timestamp,
			"genesis_gas_limit":      g.GasLimit,
			"genesis_difficulty":     dec(g.Difficulty),
			"allocation_hash":        params.AllocHash.Hex(),
			"min_difficulty":         dec(new(big.Int).Div(g.Difficulty, common.Big2)),
			"duration_limit":         dec(params.DurationLimit),
			"gas_ceil":               params.ColosseumGasCeil,
			"workshare_p2p_threshold": utils.WorkShareP2PThreshold.Value,
		},
		"params": map[string]interface{}{
			"blocks_per_day":                   params.BlocksPerDay,
			"blocks_per_week":                  params.BlocksPerWeek,
			"blocks_per_month":                 params.BlocksPerMonth,
			"prime_blocks_per_month":           params.PrimeBlocksPerMonth,
			"blocks_per_year":                  params.BlocksPerYear,
			"state_ceil":                       params.StateCeil,
			"min_gas_limit":                    params.MinGasLimit(0),
			"genesis_gas_limit":                params.GenesisGasLimit,
			"initial_state_limit":              params.InitialStateLimit,
			"initial_base_fee":                 params.InitialBaseFee,
			"maximum_extra_data_size":          params.MaximumExtraDataSize,
			"max_time_diff_between_blocks":     params.MaxTimeDiffBetweenBlocks,
			"difficulty_adjustment_period":     dec(params.DifficultyAdjustmentPeriod),
			"difficulty_adjustment_factor":     params.DifficultyAdjustmentFactor,
			"workshares_threshold_diff":        params.WorkSharesThresholdDiff,
			"workshare_p2p_threshold_diff":     params.WorkShareP2PThresholdDiff,
			"max_workshare_p2p_threshold_diff": params.MaxWorkShareP2PThresholdDiff,
			"expected_workshares_per_block":    params.ExpectedWorksharesPerBlock,
			"workshares_inclusion_depth":       params.WorkSharesInclusionDepth,
			"new_workshares_inclusion_depth":   params.NewWorkSharesInclusionDepth,
			"max_workshare_count":              params.MaxWorkShareCount,
			"new_max_workshare_count":          params.NewMaxWorkShareCount,
			"max_sha_shares_count":             params.MaxShaSharesCount,
			"new_max_sha_shares_count":         params.NewMaxShaSharesCount,
			"max_scrypt_shares_count":          params.MaxScryptSharesCount,
			"new_max_scrypt_shares_count":      params.NewMaxScryptSharesCount,
			"max_lockup_byte":                  params.MaxLockupByte,
			"lockup_byte_to_block_depth":       params.LockupByteToBlockDepth,
			"conversion_lock_period":           params.ConversionLockPeriod,
			"unwrap_qi_lock_period":            params.UnwrapQiLockPeriod,
			"trim_depths":                      trim,
			"exchange_rate":                    dec(params.ExchangeRate),
			"starting_k_quai_discount":         dec(params.StartingKQuaiDiscount),
			"starting_conversion_flow_amount":  dec(params.StartingConversionFlowAmount),
			"musig2_public_keys":               params.MuSig2PublicKeys,
			"max_gossipsub_packet_size":        params.MaxGossipsubPacketSize,
			"hierarchy_size_by_expansion":      hierarchy,
		},
		"core": core.NativeChainSpecCoreLimits(),
		"forks": map[string]interface{}{
			"kawpow":                                  params.KawPowForkBlock,
			"kawpow_transition":                       params.KawPowTransitionPeriod,
			"controller_kick_in":                      params.ControllerKickInBlock,
			"conversion_slip_change":                  params.ConversionSlipChangeBlock,
			"k_quai_change":                           params.KQuaiChangeBlock,
			"k_quai_change_hold":                      params.KQuaiChangeHoldInterval,
			"k_quai_change_table":                     params.KQuaiChangeTable,
			"k_quai_reset_after_kawpow":               params.KQuaiResetAfterKawPowForkBlock,
			"exchange_rate_hold_after_kawpow":         params.ExchangeRateHoldInterval,
			"exchange_rate_hold_after_sha_equivalent": params.ExchangeRateHoldIntervalAfterShaEquivalentDifficulty,
			"inclusion_depth_change":                  params.InclusionDepthChangeBlock,
			"inclusion_depth_update_period":           params.InclusionDepthUpdatePeriod,
			"singularity":                             params.SingularityForkBlock,
			"qi_wrapping_change":                      params.QiWrappingChangeBlock,
			"sha_equivalent_difficulty":               params.ShaEquivalentDifficultyForkBlock,
			"conversion_stability":                    params.ConversionStabilityForkBlock,
			"self_destruct_refund":                    params.SelfDestructRefundForkBlock,
			"conversion_lock_change":                  params.ConversionLockChangeForkBlock,
			"k_quai_freeze":                           params.KQuaiFreezeForkBlock,
			"security_hardening":                      params.SecurityHardeningForkBlock,
			"polaris":                                 params.PolarisForkBlock,
			"start_tx":                                params.TimeToStartTx,
			"coinbase_lockup_precompile":              params.CoinbaseLockupPrecompileKickInHeight,
			"qi_activation":                           params.QiActivationBlock,
			"max_grind_increase":                      dec(params.MaxGrindIncreaseForkBlock),
			"max_code_size":                           params.MaxCodeSizeForkHeight,
			"unwrap_qi_lock_period_before":            params.UnwrapQiLockPeriodAt(params.ConversionLockChangeForkBlock - 1),
			"unwrap_qi_lock_period_at":                params.UnwrapQiLockPeriodAt(params.ConversionLockChangeForkBlock),
			"max_code_size_before":                    params.GetMaxCodeSize(params.MaxCodeSizeForkHeight - 1),
			"max_code_size_at":                        params.GetMaxCodeSize(params.MaxCodeSizeForkHeight),
		},
	}
	if err := json.NewEncoder(os.Stdout).Encode(v); err != nil {
		panic(err)
	}
}
