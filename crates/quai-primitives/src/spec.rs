//! One chain specification and one fork schedule, copied from the pinned
//! go-quai v0.57.0 (`REFERENCE_REVISION`).
//!
//! Every value names its Go source. `tests/chain_spec.rs` compares the
//! colosseum values and the fork schedule with `fixtures/protocol/chain-spec.json`,
//! which `scripts/gen_chain_spec_vectors.py` generates from the Go sources
//! (`reference/chain-spec/main.go`, `go build -overlay`, `GOPROXY=off`,
//! `GOTOOLCHAIN=local`).
//!
//! go-quai keeps its fork heights in package-level variables of `params`; every
//! network (colosseum, garden, orchard, lighthouse, local) uses the same values.
//! The port therefore has one schedule, [`FORKS`], which every `ChainSpec`
//! carries and every crate reads.
use crate::{decode_hex, Hash, Schedule};

/// params.DurationLimit (5 s): `BlocksPerDay = 86400 / DurationLimit`.
pub const BLOCKS_PER_DAY: u64 = 86_400 / 5;
/// params.BlocksPerWeek = 7 * BlocksPerDay.
pub const BLOCKS_PER_WEEK: u64 = 7 * BLOCKS_PER_DAY;
/// params.BlocksPerMonth = 30 * BlocksPerDay.
pub const BLOCKS_PER_MONTH: u64 = 30 * BLOCKS_PER_DAY;
/// params.PrimeBlocksPerMonth = BlocksPerMonth / 4.
pub const PRIME_BLOCKS_PER_MONTH: u64 = BLOCKS_PER_MONTH / 4;
/// params.BlocksPerYear = 365 * BlocksPerDay.
pub const BLOCKS_PER_YEAR: u64 = 365 * BLOCKS_PER_DAY;

/// params.MinGasLimit: the gas and state limit floor once transactions start.
pub const MIN_GAS_LIMIT: u64 = 12_000_000;
/// params.StateCeil.
pub const STATE_CEIL: u64 = 50_000_000;
/// params.MaxTimeDiffBetweenBlocks (seconds; difficulty adjustment clamp).
pub const MAX_TIME_DIFF_BETWEEN_BLOCKS: u64 = 100;
/// params.DifficultyAdjustmentPeriod.
pub const DIFFICULTY_ADJUSTMENT_PERIOD: u64 = 720;
/// params.DifficultyAdjustmentFactor.
pub const DIFFICULTY_ADJUSTMENT_FACTOR: u64 = 40;
/// params.MaximumExtraDataSize.
pub const MAXIMUM_EXTRA_DATA_SIZE: usize = 32;
/// core/headerchain_validation.go allowedFutureBlockTimeSeconds.
pub const ALLOWED_FUTURE_BLOCK_TIME: u64 = 15;
/// params.WorkSharesThresholdDiff.
pub const WORKSHARES_THRESHOLD_DIFF: u16 = 3;
/// params.WorkShareP2PThresholdDiff (the cmd/utils/flags.go default).
pub const WORKSHARE_P2P_THRESHOLD_DIFF: u16 = 7;

/// Every height at which go-quai v0.57.0 changes a rule. Heights compare with
/// the prime terminus number of a block unless the field says "zone".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForkSchedule {
    /// params.KawPowForkBlock.
    pub kawpow: u64,
    /// params.KawPowTransitionPeriod = BlocksPerMonth / 4 (ProgPoW grace period).
    pub kawpow_transition: u64,
    /// params.ControllerKickInBlock (prime block number).
    pub controller_kick_in: u64,
    /// params.ConversionSlipChangeBlock (prime block number).
    pub conversion_slip_change: u64,
    /// params.KQuaiChangeBlock (prime block number).
    pub k_quai_change: u64,
    /// params.KQuaiChangeHoldInterval (prime blocks).
    pub k_quai_change_hold: u64,
    /// params.KQuaiChangeTable: (prime block, percent of kQuai kept).
    pub k_quai_change_table: [(u64, u64); 4],
    /// params.ExchangeRateHoldInterval = 5 * BlocksPerMonth / 4 (prime blocks).
    pub exchange_rate_hold_after_kawpow: u64,
    /// params.ExchangeRateHoldIntervalAfterShaEquivalentDifficulty = 4 * BlocksPerMonth / 4.
    pub exchange_rate_hold_after_sha_equivalent: u64,
    /// params.InclusionDepthChangeBlock.
    pub inclusion_depth_change: u64,
    /// params.InclusionDepthUpdatePeriod (prime blocks of share-target ramp).
    pub inclusion_depth_update_period: u64,
    /// params.SingularityForkBlock.
    pub singularity: u64,
    /// params.QiWrappingChangeBlock.
    pub qi_wrapping_change: u64,
    /// params.ShaEquivalentDifficultyForkBlock.
    pub sha_equivalent_difficulty: u64,
    /// params.ConversionStabilityForkBlock.
    pub conversion_stability: u64,
    /// params.SelfDestructRefundForkBlock.
    pub self_destruct_refund: u64,
    /// params.ConversionLockChangeForkBlock (also params.KQuaiFreezeForkBlock).
    pub conversion_lock_change: u64,
    /// params.SecurityHardeningForkBlock.
    pub security_hardening: u64,
    /// params.PolarisForkBlock (prime terminus).
    pub polaris: u64,
    /// params.TimeToStartTx = 15 * BlocksPerDay (zone).
    pub start_tx: u64,
    /// params.CoinbaseLockupPrecompileKickInHeight = 5 * BlocksPerWeek (zone).
    pub coinbase_lockup_precompile: u64,
    /// params.QiActivationBlock (zone; `params.OneOverKqi`).
    pub qi_activation: u64,
    /// params.MaxGrindIncreaseForkBlock (zone; core/vm/evm.go).
    pub max_grind_increase: u64,
    /// params.MaxCodeSizeForkHeight (zone; also enables the new opcodes in
    /// core/vm/interpreter.go).
    pub max_code_size: u64,
    /// `2 * params.BlocksPerMonth` (zone): the first height with a non-zero
    /// header lock byte (core/headerchain_validation.go verifyHeader), with a
    /// lockup reward multiple (params.CalculateCoinbaseValueWithLockup) and with
    /// the full gas and state ceilings (core/block_validator.go CalcGasLimit,
    /// consensus/misc/statefee.go CalcStateLimit).
    pub lockup_start: u64,
    /// params.StateRepairForkBlock (prime terminus; u64::MAX leaves it disabled).
    pub state_repair: u64,
}

/// The fork schedule of go-quai v0.57.0 (params/protocol_params.go).
pub const FORKS: ForkSchedule = ForkSchedule::GO_QUAI_V0_57_0;

impl ForkSchedule {
    pub const GO_QUAI_V0_57_0: Self = Self {
        kawpow: 1_171_500,
        kawpow_transition: BLOCKS_PER_MONTH / 4,
        controller_kick_in: 262_000,
        conversion_slip_change: 285_000,
        k_quai_change: 752_000,
        k_quai_change_hold: 20_000,
        k_quai_change_table: [
            (752_000, 50),
            (752_000 + 2 * PRIME_BLOCKS_PER_MONTH, 75),
            (752_000 + 4 * PRIME_BLOCKS_PER_MONTH, 75),
            (752_000 + 6 * PRIME_BLOCKS_PER_MONTH, 75),
        ],
        exchange_rate_hold_after_kawpow: 5 * BLOCKS_PER_MONTH / 4,
        exchange_rate_hold_after_sha_equivalent: 4 * BLOCKS_PER_MONTH / 4,
        inclusion_depth_change: 1_330_000,
        inclusion_depth_update_period: 10_000,
        singularity: 1_530_500,
        qi_wrapping_change: 1_570_000,
        sha_equivalent_difficulty: 1_755_000,
        conversion_stability: 1_872_600,
        self_destruct_refund: 1_919_500,
        conversion_lock_change: 2_237_000,
        security_hardening: 2_317_000,
        polaris: 2_444_574,
        start_tx: 15 * BLOCKS_PER_DAY,
        coinbase_lockup_precompile: 5 * BLOCKS_PER_WEEK,
        qi_activation: 1_220_000,
        max_grind_increase: 1_865_000,
        max_code_size: 3_490_000,
        lockup_start: 2 * BLOCKS_PER_MONTH,
        state_repair: u64::MAX,
    };
    /// params.StateRepairActive(primeHeight): disabled while `state_repair == u64::MAX`.
    pub const fn state_repair_active(&self, prime: u64) -> bool {
        self.state_repair != u64::MAX && prime >= self.state_repair
    }
    /// Last prime terminus of the ProgPoW grace period (inclusive):
    /// KawPowForkBlock + KawPowTransitionPeriod.
    pub const fn progpow_grace_end(&self) -> u64 {
        self.kawpow + self.kawpow_transition
    }
    /// params.KQuaiFreezeForkBlock = ConversionLockChangeForkBlock.
    pub const fn k_quai_freeze(&self) -> u64 {
        self.conversion_lock_change
    }
    /// params.KQuaiResetAfterKawPowForkBlock = KawPowForkBlock.
    pub const fn k_quai_reset_after_kawpow(&self) -> u64 {
        self.kawpow
    }
    /// The header codec schedule (auxiliary PoW and share fields).
    pub const fn header_schedule(&self) -> Schedule {
        Schedule {
            kawpow_fork: self.kawpow,
            transition_period: self.kawpow_transition,
        }
    }
    /// Qi-to-Quai conversions are refused for KQuaiChangeHoldInterval prime
    /// blocks after KawPow and after ShaEquivalentDifficulty
    /// (core/state_processor.go, core/vm/evm.go, core/vm/instructions.go).
    pub const fn conversion_hold(&self, prime: u64) -> bool {
        (prime >= self.kawpow && prime < self.kawpow + self.k_quai_change_hold)
            || (prime >= self.sha_equivalent_difficulty
                && prime < self.sha_equivalent_difficulty + self.k_quai_change_hold)
    }
    /// params.UnwrapQiLockPeriodAt: ConversionLockPeriod before
    /// ConversionLockChangeForkBlock, UnwrapQiLockPeriod from it.
    pub const fn unwrap_qi_lock_period(&self, prime: u64) -> u64 {
        if prime >= self.conversion_lock_change {
            LOCK_PERIODS.unwrap_qi
        } else {
            LOCK_PERIODS.conversion
        }
    }
    /// params.GetMaxCodeSize: params.MaxCodeSize (24,576) before
    /// MaxCodeSizeForkHeight, params.NewMaxCodeSize (32,768) from it.
    pub const fn max_code_size_at(&self, zone: u64) -> usize {
        if zone >= self.max_code_size {
            32_768
        } else {
            24_576
        }
    }
    /// Workshare inclusion depth (core/headerchain_validation.go verifyUncles:
    /// params.WorkSharesInclusionDepth, NewWorkSharesInclusionDepth after
    /// InclusionDepthChangeBlock).
    pub const fn inclusion_depth(&self, prime: u64) -> usize {
        if prime >= self.inclusion_depth_change {
            4
        } else {
            3
        }
    }
    /// (total, SHA, scrypt) workshare counts per block: params.MaxWorkShareCount,
    /// MaxShaSharesCount, MaxScryptSharesCount, and their New* values from
    /// SingularityForkBlock.
    pub const fn workshare_counts(&self, prime: u64) -> (usize, usize, usize) {
        if prime >= self.singularity {
            (32, 16, 16)
        } else {
            (16, 8, 8)
        }
    }
}

/// params.LockupByteToBlockDepth and the unwrap lock of params.UnwrapQiLockPeriodAt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LockPeriods {
    /// params.LockupByteToBlockDepth, indexed by the lock byte.
    pub by_lock_byte: [u64; 4],
    /// params.ConversionLockPeriod = 2 * BlocksPerWeek.
    pub conversion: u64,
    /// params.UnwrapQiLockPeriod (from ConversionLockChangeForkBlock).
    pub unwrap_qi: u64,
}
/// The go-quai lock periods (params/protocol_params.go).
pub const LOCK_PERIODS: LockPeriods = LockPeriods {
    by_lock_byte: [
        2 * BLOCKS_PER_WEEK,
        3 * BLOCKS_PER_MONTH,
        6 * BLOCKS_PER_MONTH,
        BLOCKS_PER_YEAR,
    ],
    conversion: 2 * BLOCKS_PER_WEEK,
    unwrap_qi: 10,
};
/// types.TrimDepths (core/types/utxo.go): UTXO trim depth per denomination.
pub const TRIM_DEPTHS: [u64; 6] = [
    2 * BLOCKS_PER_WEEK,
    4 * BLOCKS_PER_WEEK,
    6 * BLOCKS_PER_WEEK,
    8 * BLOCKS_PER_WEEK,
    10 * BLOCKS_PER_WEEK,
    12 * BLOCKS_PER_WEEK,
];
/// params.MuSig2PublicKeys: the AuxPoW template quorum (2-of-3).
pub const MUSIG2_PUBLIC_KEYS: [&str; 3] = [
    "02cae78e4905da54dc93d6009119d56a2ef91b11e129c8fa41342310ec3a9b499c",
    "03374cbfcea6cb8172b2fdc09e9f7ec0cc2ff10aaa2192fb19fa04d9391b0a75c2",
    "03abfe95f7d487a44cac59c9d62fe63e9e01dc3e06dc9fe96b7038d73d99a62fa6",
];

/// common.GetHierarchySizeForExpansionNumber: (regions, zones) active at an
/// expansion number.
pub const fn hierarchy_size(expansion: u8) -> (u64, u64) {
    (expansion as u64 / 2 + 1, (expansion as u64 + 1) / 2 + 1)
}

/// Genesis fields (core/genesis.go Genesis and Genesis.ToBlock).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Genesis {
    /// The genesis block hash the rebuilt block must have.
    pub hash: Hash,
    pub nonce: u64,
    pub extra: Vec<u8>,
    pub timestamp: u64,
    pub gas_limit: u64,
    pub difficulty: u128,
    /// Genesis.AllocHash; ToBlock appends it to the header extra data.
    pub allocation_hash: Hash,
    pub starting_expansion: u8,
}

/// Header and workshare policy (core/headerchain_validation.go verifyHeader,
/// core/block_validator.go, consensus/*).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderPolicy {
    /// PowConfig.MinDifficulty.
    pub minimum_difficulty: u128,
    /// PowConfig.DurationLimit (seconds).
    pub duration: u64,
    /// PowConfig.GasCeil.
    pub gas_ceiling: u64,
    /// params.StateCeil.
    pub state_ceiling: u64,
    /// params.MinGasLimit.
    pub min_gas_limit: u64,
    /// params.WorkSharesThresholdDiff (bits below the block target).
    pub workshare_threshold: u16,
    /// The node's workshare p2p threshold (cmd/utils/flags.go
    /// WorkShareP2PThreshold default = params.WorkShareP2PThresholdDiff).
    pub p2p_threshold: u16,
    /// core/headerchain_validation.go allowedFutureBlockTimeSeconds.
    pub future_block_time: u64,
    /// params.MaxTimeDiffBetweenBlocks (difficulty adjustment clamp, seconds).
    pub max_time_diff: u64,
    /// params.MaximumExtraDataSize.
    pub max_extra_data: usize,
    /// params.DifficultyAdjustmentPeriod.
    pub difficulty_adjustment_period: u64,
    /// params.DifficultyAdjustmentFactor.
    pub difficulty_adjustment_factor: u64,
    /// ChainConfig.DefaultGenesisHash (cmd/utils/flags.go).
    pub default_genesis: Option<Hash>,
}

/// Starting values of the prime economic controllers, which Genesis.ToBlock
/// writes and core/slice.go Append requires up to ControllerKickInBlock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Economics {
    /// params.ExchangeRate.
    pub exchange_rate: u128,
    /// params.StartingKQuaiDiscount.
    pub k_quai_discount: u128,
    /// params.StartingConversionFlowAmount = 10000 * Ether.
    pub conversion_flow_amount: u128,
}
/// The go-quai starting economic values (params/protocol_params.go).
pub const ECONOMICS: Economics = Economics {
    exchange_rate: 221_077_819_000_000_000,
    k_quai_discount: 100,
    conversion_flow_amount: 10_000 * 1_000_000_000_000_000_000,
};

/// Genesis account unlocks (core/state/gen_allocs.go AddLockedBalances).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Allocations {
    /// No unlocks (the private test chains).
    None,
    /// params/genesis_alloc.json, verified by Genesis.AllocHash and unlocked
    /// in zone [0, 0] only (core/headerchain_validation.go Finalize).
    Official,
}

/// One chain: identity, genesis, header policy, AuxPoW quorum, economics and
/// the fork schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainSpec {
    /// The go-quai environment name (params.ColosseumName, params.LocalName).
    pub environment: String,
    /// ChainConfig.ChainID.
    pub chain_id: u64,
    /// The node location of the slice: [region, zone].
    pub location: [u8; 2],
    pub genesis: Genesis,
    pub header: HeaderPolicy,
    /// params.MuSig2PublicKeys (compressed secp256k1).
    pub quorum_keys: [[u8; 33]; 3],
    pub economics: Economics,
    pub allocations: Allocations,
    pub lock_periods: LockPeriods,
    pub trim_depths: [u64; 6],
    pub forks: ForkSchedule,
}

/// go-quai `common.FromHex`: optional 0x, an odd length gets a leading zero.
pub fn go_from_hex(value: &str) -> Result<Vec<u8>, String> {
    let v = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    if v.len() % 2 == 1 {
        decode_hex(&format!("0x0{v}"))
    } else {
        decode_hex(&format!("0x{v}"))
    }
}
fn hash(value: &str) -> Hash {
    let bytes = decode_hex(value).expect("pinned hash constant");
    bytes.try_into().expect("32-byte hash constant")
}
fn quorum_keys() -> [[u8; 33]; 3] {
    std::array::from_fn(|i| {
        decode_hex(&format!("0x{}", MUSIG2_PUBLIC_KEYS[i]))
            .expect("pinned quorum key")
            .try_into()
            .expect("33-byte quorum key")
    })
}

/// params/config.go ProgpowColosseumGenesisHash.
pub const COLOSSEUM_GENESIS_HASH: &str =
    "0xac81c28f1a72591b87b5f16c9793cdc0e87c45c6d426d1a364c3b8f6386b5b8b";
/// params/config.go ProgpowGardenGenesisHash.
pub const GARDEN_GENESIS_HASH: &str =
    "0x8a611a707f8da1129432cf5b97e6f1eccf902797124f26ea7b6a929b63fd8fde";
/// params/config.go ProgpowOrchardGenesisHash.
pub const ORCHARD_GENESIS_HASH: &str =
    "0x3c9e1b5879118260b45ec05aebdd127808e830ecf5fc5ef9cb46405d339f8dff";
/// params/config.go ProgpowLighthouseGenesisHash.
pub const LIGHTHOUSE_GENESIS_HASH: &str =
    "0x83f1a345d1406d8d8b8e83b1d8e8369a3aa3070448d52cdffd30769a512b07c7";
/// params/config.go ProgpowLocalGenesisHash.
pub const LOCAL_GENESIS_HASH: &str =
    "0x718f13b40bb30d77a53dbd2688a6beb253c169fc2bf9114bb7b16e126c21a170";
/// params/config.go AllocHash.
pub const COLOSSEUM_ALLOC_HASH: &str =
    "0xa1f3f565b62c83fcc0e61dfd219409fb06526570934a5d94b5db5afdad13f1da";
/// params/config.go LighthouseAllocHash.
pub const LIGHTHOUSE_ALLOC_HASH: &str =
    "0x7af9921c72ee23443ec6bd016f92f1a6da0534aa931030c0a24f54107d35e010";
/// cmd/utils/flags.go GenesisNonce default; GetGenesisNonce reads the first
/// 8 bytes of `common.FromHex(flag)` as the nonce and the rest as extra data.
pub const COLOSSEUM_GENESIS_NONCE_FLAG: &str = "23621466532946281564673705261963422";

impl ChainSpec {
    fn official_network(
        environment: &str,
        chain_id: u64,
        genesis_hash: &str,
        difficulty: u128,
        alloc_hash: &str,
        allocations: Allocations,
    ) -> Self {
        let flag = go_from_hex(COLOSSEUM_GENESIS_NONCE_FLAG).expect("pinned genesis nonce flag");
        Self {
            environment: environment.into(),
            chain_id,
            location: [0, 0],
            genesis: Genesis {
                hash: hash(genesis_hash),
                nonce: u64::from_be_bytes(flag[..8].try_into().expect("8-byte nonce")),
                extra: flag[8..].to_vec(),
                timestamp: 0,
                gas_limit: 12_000_000,
                difficulty,
                allocation_hash: hash(alloc_hash),
                starting_expansion: 0,
            },
            header: HeaderPolicy {
                minimum_difficulty: difficulty / 2,
                duration: 5,
                gas_ceiling: 50_000_000,
                state_ceiling: STATE_CEIL,
                min_gas_limit: MIN_GAS_LIMIT,
                workshare_threshold: WORKSHARES_THRESHOLD_DIFF,
                p2p_threshold: WORKSHARE_P2P_THRESHOLD_DIFF,
                future_block_time: ALLOWED_FUTURE_BLOCK_TIME,
                max_time_diff: MAX_TIME_DIFF_BETWEEN_BLOCKS,
                max_extra_data: MAXIMUM_EXTRA_DATA_SIZE,
                difficulty_adjustment_period: DIFFICULTY_ADJUSTMENT_PERIOD,
                difficulty_adjustment_factor: DIFFICULTY_ADJUSTMENT_FACTOR,
                default_genesis: Some(hash(genesis_hash)),
            },
            quorum_keys: quorum_keys(),
            economics: ECONOMICS,
            allocations,
            lock_periods: LOCK_PERIODS,
            trim_depths: TRIM_DEPTHS,
            forks: FORKS,
        }
    }
    /// Quai mainnet, slice [0, 0] (params.ColosseumName with the progpow
    /// engine, cmd/utils/flags.go defaults).
    pub fn colosseum() -> Self {
        Self::official_network(
            "colosseum",
            9,
            COLOSSEUM_GENESIS_HASH,
            1_500_000_000_000,
            COLOSSEUM_ALLOC_HASH,
            Allocations::Official,
        )
    }
    /// Quai Garden testnet, slice [0, 0] (params.GardenName).
    pub fn garden() -> Self {
        Self::official_network(
            "garden",
            12_000,
            GARDEN_GENESIS_HASH,
            300_000_001,
            COLOSSEUM_ALLOC_HASH,
            Allocations::Official,
        )
    }
    /// Quai Orchard testnet, slice [0, 0] (params.OrchardName).
    pub fn orchard() -> Self {
        Self::official_network(
            "orchard",
            15_000,
            ORCHARD_GENESIS_HASH,
            50_000_000_001,
            COLOSSEUM_ALLOC_HASH,
            Allocations::Official,
        )
    }
    /// Quai Lighthouse testnet, slice [0, 0] (params.LighthouseName).
    pub fn lighthouse() -> Self {
        Self::official_network(
            "lighthouse",
            17_000,
            LIGHTHOUSE_GENESIS_HASH,
            750_000_000,
            LIGHTHOUSE_ALLOC_HASH,
            Allocations::None,
        )
    }
    /// Default go-quai local chain (params.LocalName, ProgpowLocalGenesisHash).
    pub fn local_default() -> Self {
        let mut spec = Self::official_network(
            "local",
            1337,
            LOCAL_GENESIS_HASH,
            2_000,
            COLOSSEUM_ALLOC_HASH,
            Allocations::Official,
        );
        spec.genesis.nonce = 0;
        spec.genesis.extra = vec![];
        spec
    }
    /// Override the slice `[region, zone]` on this spec.
    pub fn with_location(mut self, location: [u8; 2]) -> Result<Self, String> {
        if location[0] > 15 || location[1] > 15 {
            return Err(format!(
                "invalid slice location [{}, {}]: region and zone must be 0..=15",
                location[0], location[1]
            ));
        }
        self.location = location;
        if location != [0, 0] {
            self.allocations = Allocations::None;
        }
        Ok(self)
    }
    /// A private test chain: params.LocalName with chain ID 1337
    /// (ProgpowLocalChainConfig), the header policy of the Go oracle harnesses
    /// (reference/confirmation/harness.go PowConfig: DurationLimit 5,
    /// MinDifficulty 2, GasCeil = block gas limit) and no allocations. The
    /// fork schedule is the go-quai one.
    pub fn private(genesis: Genesis, gas_ceiling: u64) -> Self {
        let mut spec = Self::colosseum();
        spec.environment = "local".into();
        spec.chain_id = 1337;
        spec.genesis = genesis;
        spec.header.minimum_difficulty = 2;
        spec.header.gas_ceiling = gas_ceiling;
        spec.header.default_genesis = None;
        spec.allocations = Allocations::None;
        spec
    }
    /// common.GetHierarchySizeForExpansionNumber.
    pub fn active_hierarchy(&self, expansion: u8) -> (u64, u64) {
        hierarchy_size(expansion)
    }
    /// True when this spec's slice is active at an expansion number.
    pub fn slice_active(&self, expansion: u8) -> bool {
        let (regions, zones) = hierarchy_size(expansion);
        (self.location[0] as u64) < regions && (self.location[1] as u64) < zones
    }
    /// Node location of one context: prime `[]`, region `[r]`, zone `[r, z]`.
    pub fn context_location(&self, context: usize) -> &[u8] {
        &self.location[..context.min(2)]
    }
    /// The zone prefix byte of addresses in this slice.
    pub fn address_prefix(&self) -> u8 {
        self.location[0] << 4 | self.location[1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn derived_schedule_helpers_follow_the_go_formulas() {
        let f = FORKS;
        assert_eq!(f.progpow_grace_end(), 1_301_100);
        assert_eq!(f.k_quai_freeze(), f.conversion_lock_change);
        assert!(!f.conversion_hold(f.kawpow - 1));
        assert!(f.conversion_hold(f.kawpow));
        assert!(f.conversion_hold(f.kawpow + 19_999));
        assert!(!f.conversion_hold(f.kawpow + 20_000));
        assert!(f.conversion_hold(f.sha_equivalent_difficulty + 19_999));
        assert!(!f.conversion_hold(f.sha_equivalent_difficulty + 20_000));
        assert_eq!(f.inclusion_depth(f.inclusion_depth_change - 1), 3);
        assert_eq!(f.inclusion_depth(f.inclusion_depth_change), 4);
        assert_eq!(f.workshare_counts(f.singularity - 1), (16, 8, 8));
        assert_eq!(f.workshare_counts(f.singularity), (32, 16, 16));
        assert_eq!(hierarchy_size(0), (1, 1));
        assert_eq!(hierarchy_size(1), (1, 2));
        assert_eq!(hierarchy_size(2), (2, 2));
        let spec = ChainSpec::colosseum();
        assert!(spec.slice_active(0));
        assert_eq!(spec.address_prefix(), 0);
        assert_eq!(spec.genesis.nonce, 159_351_423_047_472_226);
        assert_eq!(
            spec.genesis.extra,
            [129, 86, 70, 115, 112, 82, 97, 150, 52, 34]
        );
    }
}
