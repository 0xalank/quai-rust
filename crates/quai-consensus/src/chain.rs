//! A chain spec (`quai_primitives::spec::ChainSpec`) with its rebuilt and
//! checked genesis, and the consensus profiles derived from it. The colosseum
//! spec and the reviewed private profiles use the same code paths.
use crate::{
    history::Record,
    initialization::{build_genesis, GenesisSpec},
    placement,
    private_profile::PrivateChain,
    rules::Uint,
    validation, zone_execution, Error,
};
use quai_primitives::{
    spec::{Allocations, ChainSpec},
    Hash,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

#[derive(Clone)]
pub struct Chain {
    spec: ChainSpec,
    genesis: Record,
    fingerprint: Hash,
    private: Option<Arc<PrivateChain>>,
}
impl std::fmt::Debug for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chain")
            .field("environment", &self.spec.environment)
            .field("chain_id", &self.spec.chain_id)
            .field("genesis", &quai_primitives::encode_hex(&self.genesis.id()))
            .finish()
    }
}
impl Chain {
    /// Rebuild the genesis of a spec (core/genesis.go Genesis.ToBlock) and
    /// require the spec's genesis hash.
    pub fn new(spec: ChainSpec) -> Result<Self, Error> {
        let g = &spec.genesis;
        let genesis = build_genesis(&GenesisSpec {
            context: 2,
            nonce: g.nonce,
            timestamp: g.timestamp,
            extra: g.extra.clone(),
            allocation_hash: g.allocation_hash,
            gas_limit: g.gas_limit,
            difficulty: Uint::from_u128(g.difficulty),
            starting_expansion: g.starting_expansion as u64,
        })?;
        if genesis.id() != g.hash {
            return Err(Error::Invalid("chain spec genesis hash"));
        }
        let fingerprint = Sha256::digest(format!("{spec:?}").as_bytes()).into();
        Ok(Self {
            spec,
            genesis,
            fingerprint,
            private: None,
        })
    }
    /// Quai mainnet, slice [0, 0].
    pub fn colosseum() -> Result<Arc<Self>, Error> {
        static CHAIN: OnceLock<Arc<Chain>> = OnceLock::new();
        if let Some(c) = CHAIN.get() {
            return Ok(c.clone());
        }
        let c = Arc::new(Self::new(ChainSpec::colosseum())?);
        let _ = CHAIN.set(c);
        Ok(CHAIN.get().unwrap().clone())
    }
    /// A reviewed private profile as a chain. Its fingerprint stays the
    /// profile fingerprint, which binds permits and stored branches.
    pub fn from_private(p: Arc<PrivateChain>) -> Self {
        Self {
            spec: p.spec(),
            genesis: p.genesis[2].clone(),
            fingerprint: p.fingerprint,
            private: Some(p),
        }
    }
    /// The reviewed private hierarchy profile (chain 1337).
    pub fn private_hierarchy() -> Result<Arc<Self>, Error> {
        static CHAIN: OnceLock<Arc<Chain>> = OnceLock::new();
        if let Some(c) = CHAIN.get() {
            return Ok(c.clone());
        }
        let c = Arc::new(Self::from_private(PrivateChain::hierarchy_template()?));
        let _ = CHAIN.set(c);
        Ok(CHAIN.get().unwrap().clone())
    }
    /// The chain for an export environment name.
    pub fn for_environment(environment: &str) -> Result<Arc<Self>, Error> {
        match environment {
            "colosseum" => Self::colosseum(),
            "local" => Self::private_hierarchy(),
            _ => Err(Error::Unsupported("unknown chain environment")),
        }
    }
    pub fn spec(&self) -> &ChainSpec {
        &self.spec
    }
    /// The genesis block (the same record in every context).
    pub fn genesis(&self) -> &Record {
        &self.genesis
    }
    pub fn genesis_hash(&self) -> Hash {
        self.genesis.id()
    }
    pub fn location(&self) -> [u8; 2] {
        self.spec.location
    }
    /// Identity that binds hierarchy permits to this chain.
    pub fn fingerprint(&self) -> Hash {
        self.fingerprint
    }
    /// The reviewed private profile, if this chain is one.
    pub fn private(&self) -> Option<&Arc<PrivateChain>> {
        self.private.as_ref()
    }
    /// Header and append policy of one context (0 prime, 1 region, 2 zone).
    pub fn placement_profile(&self, context: usize) -> placement::Profile {
        let h = &self.spec.header;
        placement::Profile {
            location: self.spec.context_location(context).to_vec(),
            genesis: BTreeSet::from([self.genesis.id()]),
            default_genesis: h.default_genesis,
            minimum_difficulty: Uint::from_u128(h.minimum_difficulty),
            duration: h.duration,
            gas_ceiling: h.gas_ceiling,
            p2p_threshold: h.p2p_threshold,
        }
    }
    /// Zone header policy.
    pub fn zone_profile(&self) -> validation::Profile {
        let h = &self.spec.header;
        validation::Profile {
            location: self.spec.location.to_vec(),
            minimum_difficulty: Uint::from_u128(h.minimum_difficulty),
            duration: h.duration,
            gas_ceiling: h.gas_ceiling,
            p2p_threshold: h.p2p_threshold,
            genesis: BTreeSet::from([self.genesis.id()]),
            default_genesis: h.default_genesis,
        }
    }
    /// Zone execution policy: chain ID, location, genesis and allocations.
    pub fn execution_profile(&self) -> Result<zone_execution::Profile, Error> {
        zone_execution::Profile::new(
            Uint::from_u64(self.spec.chain_id),
            self.spec.location,
            BTreeSet::from([self.genesis.id()]),
            self.spec.allocations == Allocations::Official,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn colosseum_genesis_rebuilds_to_the_go_hash() {
        let c = Chain::colosseum().unwrap();
        assert_eq!(
            quai_primitives::encode_hex(&c.genesis_hash()),
            quai_primitives::spec::COLOSSEUM_GENESIS_HASH
        );
        assert!(c.private().is_none());
        let mut wrong = ChainSpec::colosseum();
        wrong.genesis.nonce ^= 1;
        assert!(matches!(Chain::new(wrong), Err(Error::Invalid(_))));
    }
    #[test]
    fn private_profiles_are_specs_with_the_go_fork_schedule() {
        for p in [
            PrivateChain::reviewed().unwrap(),
            PrivateChain::reviewed_admission().unwrap(),
            PrivateChain::reviewed_hierarchy().unwrap(),
            PrivateChain::reviewed_execution().unwrap(),
            PrivateChain::reviewed_all_etx().unwrap(),
        ] {
            let spec = p.spec();
            assert_eq!(spec.chain_id, 1337);
            assert_eq!(spec.forks, quai_primitives::spec::FORKS);
            assert_eq!(spec.allocations, Allocations::None);
            // The spec rebuilds the profile's genesis.
            let rebuilt = Chain::new(spec).unwrap();
            assert_eq!(rebuilt.genesis_hash(), p.genesis[2].id());
            let c = Chain::from_private(Arc::new(p.clone()));
            assert_eq!(c.fingerprint(), p.fingerprint);
            assert_eq!(c.placement_profile(1).location, vec![0]);
            assert_eq!(c.zone_profile().gas_ceiling, p.gas_limit);
        }
    }
}
