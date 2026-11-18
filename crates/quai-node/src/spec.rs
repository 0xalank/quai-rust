//! Chain specifications the replay can check against.
//!
//! `colosseum` is Quai mainnet. Its values come from the pinned go-quai
//! v0.57.0 sources named next to each constant. `local` is the reviewed private
//! hierarchy profile (chain 1337) used by the fixture tests.
use quai_consensus::{
    history::Record,
    initialization::{build_genesis, GenesisSpec},
    placement,
    private_profile::PrivateChain,
    rules::Uint as U256,
    Error,
};
use quai_primitives::{decode_hex, Hash};
use std::collections::BTreeSet;

/// params/config.go `ProgpowColosseumGenesisHash`.
pub const COLOSSEUM_GENESIS_HASH: &str =
    "0xac81c28f1a72591b87b5f16c9793cdc0e87c45c6d426d1a364c3b8f6386b5b8b";
/// params/config.go `AllocHash` (the official genesis allocations).
pub const COLOSSEUM_ALLOC_HASH: &str =
    "0xa1f3f565b62c83fcc0e61dfd219409fb06526570934a5d94b5db5afdad13f1da";
/// cmd/utils/flags.go `GenesisNonce` default. `common.FromHex` turns it into
/// an 8-byte big-endian nonce followed by the genesis extra data.
pub const COLOSSEUM_GENESIS_NONCE_FLAG: &str = "23621466532946281564673705261963422";
/// core/genesis.go `DefaultColosseumGenesisBlock` (progpow engine).
pub const COLOSSEUM_GENESIS_DIFFICULTY: u64 = 1_500_000_000_000;
pub const COLOSSEUM_GENESIS_GAS_LIMIT: u64 = 12_000_000;
/// params/config.go `ProgpowColosseumChainConfig`.
pub const COLOSSEUM_CHAIN_ID: u64 = 9;
/// params/protocol_params.go `DurationLimit`.
pub const DURATION_LIMIT: u64 = 5;
/// params/protocol_params.go `ColosseumGasCeil`.
pub const COLOSSEUM_GAS_CEIL: u64 = 50_000_000;
/// params/protocol_params.go `WorkShareP2PThresholdDiff` (node default).
pub const WORKSHARE_P2P_THRESHOLD: u16 = 7;

#[derive(Clone)]
pub struct ChainSpec {
    pub environment: &'static str,
    pub chain_id: u64,
    /// Natively built genesis (zone view); its hash is checked at construction.
    pub genesis: Record,
    pub location: [u8; 2],
    pub minimum_difficulty: U256,
    pub duration: u64,
    pub gas_ceiling: u64,
    pub p2p_threshold: u16,
    pub default_genesis: Option<Hash>,
}

/// go-quai `common.FromHex`: optional 0x, odd length gets a leading zero.
fn go_from_hex(value: &str) -> Result<Vec<u8>, Error> {
    let v = value.strip_prefix("0x").unwrap_or(value);
    let v = if v.len() % 2 == 1 {
        format!("0x0{v}")
    } else {
        format!("0x{v}")
    };
    decode_hex(&v).map_err(|_| Error::Invalid("genesis nonce flag"))
}
fn hash(value: &str) -> Hash {
    decode_hex(value).unwrap().try_into().unwrap()
}

impl ChainSpec {
    pub fn for_environment(environment: &str) -> Result<Self, Error> {
        match environment {
            "colosseum" => Self::colosseum(),
            "local" => Self::local(),
            _ => Err(Error::Unsupported("unknown export environment")),
        }
    }
    /// Quai mainnet. The genesis is rebuilt with `build_genesis` and must hash
    /// to the go-quai constant before any block is checked.
    pub fn colosseum() -> Result<Self, Error> {
        let flag = go_from_hex(COLOSSEUM_GENESIS_NONCE_FLAG)?;
        if flag.len() < 8 {
            return Err(Error::Invalid("genesis nonce flag"));
        }
        let genesis = build_genesis(&GenesisSpec {
            context: 2,
            nonce: u64::from_be_bytes(flag[..8].try_into().unwrap()),
            timestamp: 0,
            extra: flag[8..].to_vec(),
            allocation_hash: hash(COLOSSEUM_ALLOC_HASH),
            gas_limit: COLOSSEUM_GENESIS_GAS_LIMIT,
            difficulty: U256::from_u64(COLOSSEUM_GENESIS_DIFFICULTY),
            starting_expansion: 0,
        })?;
        if genesis.id() != hash(COLOSSEUM_GENESIS_HASH) {
            return Err(Error::Invalid("rebuilt colosseum genesis hash"));
        }
        Ok(Self {
            environment: "colosseum",
            chain_id: COLOSSEUM_CHAIN_ID,
            default_genesis: Some(genesis.id()),
            genesis,
            location: [0, 0],
            // cmd/utils/flags.go: MinDifficulty = genesis difficulty / 2.
            minimum_difficulty: U256::from_u64(COLOSSEUM_GENESIS_DIFFICULTY / 2),
            duration: DURATION_LIMIT,
            gas_ceiling: COLOSSEUM_GAS_CEIL,
            p2p_threshold: WORKSHARE_P2P_THRESHOLD,
        })
    }
    /// The reviewed private hierarchy profile, with the header policy values
    /// that `confirmation::Hierarchy` uses for it.
    pub fn local() -> Result<Self, Error> {
        let p = PrivateChain::reviewed_hierarchy()?;
        Ok(Self {
            environment: "local",
            chain_id: p.chain_id,
            genesis: p.genesis[2].clone(),
            location: p.location,
            minimum_difficulty: U256::from_u64(2),
            duration: 5,
            gas_ceiling: p.gas_limit,
            p2p_threshold: 7,
            default_genesis: None,
        })
    }
    pub fn genesis_hash(&self) -> Hash {
        self.genesis.id()
    }
    /// Header policy profile for one context (0 prime, 1 region, 2 zone).
    pub fn profile(&self, context: usize) -> placement::Profile {
        placement::Profile {
            location: self.location[..context].to_vec(),
            genesis: BTreeSet::from([self.genesis.id()]),
            default_genesis: self.default_genesis,
            minimum_difficulty: self.minimum_difficulty,
            duration: self.duration,
            gas_ceiling: self.gas_ceiling,
            p2p_threshold: self.p2p_threshold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn colosseum_genesis_rebuilds_to_the_go_hash() {
        let spec = ChainSpec::colosseum().unwrap();
        assert_eq!(
            quai_primitives::encode_hex(&spec.genesis_hash()),
            COLOSSEUM_GENESIS_HASH
        );
        let flag = go_from_hex(COLOSSEUM_GENESIS_NONCE_FLAG).unwrap();
        assert_eq!(
            u64::from_be_bytes(flag[..8].try_into().unwrap()),
            159_351_423_047_472_226
        );
        assert_eq!(flag[8..], [129, 86, 70, 115, 112, 82, 97, 150, 52, 34]);
    }
    #[test]
    fn local_spec_is_the_reviewed_private_hierarchy() {
        let spec = ChainSpec::local().unwrap();
        assert_eq!(spec.chain_id, 1337);
        assert_eq!(
            quai_primitives::encode_hex(&spec.genesis_hash()),
            "0x53fc82ff80d79fb2f225ecc80fe9b5130dd9f1d248a6140f41096ce822fb10cd"
        );
        assert!(ChainSpec::for_environment("garden").is_err());
    }
}
