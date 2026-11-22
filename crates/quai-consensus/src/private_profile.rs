//! Frozen private development contract. This does not qualify a public chain.
use crate::{
    fields,
    history::Record,
    initialization::{build_genesis, GenesisSpec},
    rules::Uint,
    Error,
};
use quai_primitives::{decode_hex, Hash, REFERENCE_REVISION};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};
pub const REVIEWED_BYTES: &[u8] = include_bytes!("../../../configs/private-native-v1.json");
pub const ADMISSION_BYTES: &[u8] = include_bytes!("../../../configs/private-admission-v1.json");
pub const EXECUTION_BYTES: &[u8] = include_bytes!("../../../configs/private-execution-v2.json");
pub const ALL_ETX_BYTES: &[u8] = include_bytes!("../../../configs/private-execution-v3.json");
pub const HIERARCHY_BYTES: &[u8] = include_bytes!("../../../configs/private-hierarchy-v1.json");
#[derive(Clone)]
pub struct PrivateChain {
    pub(crate) activated_hierarchy: bool,
    pub(crate) all_etx_hierarchy: bool,
    pub(crate) header_admission: bool,
    pub(crate) hierarchy_admission: bool,
    pub fingerprint: Hash,
    /// The profile name ("quai-rust-hierarchy-v1", ...).
    pub name: String,
    pub chain_id: u64,
    pub location: [u8; 2],
    pub genesis: [Record; 3],
    /// The genesis fields the records were built from.
    pub genesis_fields: quai_primitives::spec::Genesis,
    pub gas_limit: u64,
    pub state_limit: u64,
    pub listen: String,
}
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str, Error> {
    v[k].as_str().ok_or(Error::Invalid("private profile text"))
}
fn number(v: &Value, k: &str) -> Result<u64, Error> {
    v[k].as_u64()
        .ok_or(Error::Invalid("private profile number"))
}
fn raw(v: &Value, k: &str) -> Result<Vec<u8>, Error> {
    decode_hex(text(v, k)?).map_err(|_| Error::Invalid("private profile encoding"))
}
impl PrivateChain {
    /// The embedded reviewed hierarchy is immutable. Share its decoded genesis
    /// and policy across readers rather than reconstructing them per lookup view.
    pub(crate) fn hierarchy_template() -> Result<Arc<Self>, Error> {
        static TEMPLATE: OnceLock<Arc<PrivateChain>> = OnceLock::new();
        if let Some(profile) = TEMPLATE.get() {
            return Ok(profile.clone());
        }
        let profile = Arc::new(Self::reviewed_hierarchy()?);
        // A value another thread set first is the same.
        Ok(TEMPLATE.get_or_init(|| profile).clone())
    }
    pub(crate) fn execution_template() -> Result<Arc<Self>, Error> {
        static TEMPLATE: OnceLock<Arc<PrivateChain>> = OnceLock::new();
        if let Some(p) = TEMPLATE.get() {
            return Ok(p.clone());
        }
        let p = Arc::new(Self::reviewed_execution()?);
        // A value another thread set first is the same.
        Ok(TEMPLATE.get_or_init(|| p).clone())
    }
    pub(crate) fn hierarchy_for_fingerprint(id: Hash) -> Result<Arc<Self>, Error> {
        let early = Self::hierarchy_template()?;
        if id == early.fingerprint {
            return Ok(early);
        }
        let active = Self::execution_template()?;
        if id == active.fingerprint {
            return Ok(active);
        }
        let all_etx = Self::all_etx_template()?;
        if id == all_etx.fingerprint {
            return Ok(all_etx);
        }
        Err(Error::Invalid("unreviewed hierarchy runtime profile"))
    }
    pub(crate) fn all_etx_template() -> Result<Arc<Self>, Error> {
        static TEMPLATE: OnceLock<Arc<PrivateChain>> = OnceLock::new();
        if let Some(profile) = TEMPLATE.get() {
            return Ok(profile.clone());
        }
        let profile = Arc::new(Self::reviewed_all_etx()?);
        // A value another thread set first is the same.
        Ok(TEMPLATE.get_or_init(|| profile).clone())
    }
    /// Experimental all-defined-ETX contract. Native canonical startup still
    /// begins at the embedded genesis; no supplied state constructor exists.
    pub fn reviewed_all_etx() -> Result<Self, Error> {
        Self::decode(ALL_ETX_BYTES)
    }
    /// Experimental genesis-only private execution profile. It grants no public
    /// chain qualification and has no checkpoint or imported-state constructor.
    pub fn reviewed_execution() -> Result<Self, Error> {
        Self::decode(EXECUTION_BYTES)
    }
    pub fn reviewed() -> Result<Self, Error> {
        Self::decode(REVIEWED_BYTES)
    }
    pub fn reviewed_admission() -> Result<Self, Error> {
        Self::decode(ADMISSION_BYTES)
    }
    pub fn reviewed_hierarchy() -> Result<Self, Error> {
        Self::decode(HIERARCHY_BYTES)
    }
    /// This profile as a chain spec: chain 1337, the Go oracle harness header
    /// policy (reference/*/harness.go PowConfig: DurationLimit 5, MinDifficulty 2,
    /// GasCeil = the profile block gas limit), no allocations and the go-quai
    /// fork schedule (`fork_manifest` must equal it).
    pub fn spec(&self) -> quai_primitives::spec::ChainSpec {
        let mut spec =
            quai_primitives::spec::ChainSpec::private(self.genesis_fields.clone(), self.gas_limit);
        spec.chain_id = self.chain_id;
        spec.location = self.location;
        spec
    }
    pub fn decode(input: &[u8]) -> Result<Self, Error> {
        if input.len() > 65536 {
            return Err(Error::ResourceLimit("private profile"));
        }
        let fingerprint: Hash = Sha256::digest(input).into();
        let all_etx_hierarchy = fingerprint == Hash::from(Sha256::digest(ALL_ETX_BYTES));
        let activated_hierarchy =
            fingerprint == Hash::from(Sha256::digest(EXECUTION_BYTES)) || all_etx_hierarchy;
        let hierarchy_admission =
            fingerprint == Hash::from(Sha256::digest(HIERARCHY_BYTES)) || activated_hierarchy;
        let header_admission =
            fingerprint == Hash::from(Sha256::digest(ADMISSION_BYTES)) || hierarchy_admission;
        if fingerprint != Hash::from(Sha256::digest(REVIEWED_BYTES)) && !header_admission {
            return Err(Error::Invalid("unreviewed private profile"));
        }
        let v: Value =
            serde_json::from_slice(input).map_err(|_| Error::Invalid("private profile JSON"))?;
        if number(&v, "schema")? != 1
            || text(&v, "reference_revision")? != REFERENCE_REVISION
            || text(&v, "reference_version")? != "v0.57.0"
            || number(&v, "chain_id")? != 1337
            || text(&v, "fee_profile")? != "official-upstream-no-local-fee-overlay"
        {
            return Err(Error::Invalid("private profile baseline"));
        }
        if v["location"] != serde_json::json!([0, 0])
            || v["active_hierarchy"] != serde_json::json!([[], [0], [0, 0]])
            || v["transport"]["allow_public_peers"] != false
            || text(&v["transport"], "listen")? != "/ip4/127.0.0.1/tcp/0"
        {
            return Err(Error::Invalid("private network contract"));
        }
        let forks = quai_primitives::spec::FORKS;
        if number(&v["fork_manifest"], "polaris_prime")? != forks.polaris
            || number(&v["fork_manifest"], "kawpow_prime")? != forks.kawpow
            || number(&v["fork_manifest"], "new_opcodes_zone")? != forks.max_code_size
            || number(&v["fork_manifest"], "max_grind_increase_zone")? != forks.max_grind_increase
            || number(&v["fork_manifest"], "transition_duration_prime")? != forks.kawpow_transition
        {
            return Err(Error::Invalid("private fork manifest"));
        }
        if number(&v["fork_manifest"], "progpow_grace_end_prime")? != forks.progpow_grace_end() {
            return Err(Error::Invalid("private transition window"));
        }
        if !v["initial_allocations"]
            .as_array()
            .is_some_and(|v| v.is_empty())
            || !v["initial_utxos"].as_array().is_some_and(|v| v.is_empty())
            || !v["initial_etxs"].as_array().is_some_and(|v| v.is_empty())
        {
            return Err(Error::Unsupported("private genesis allocations"));
        }
        let g = &v["genesis"];
        let mut genesis = vec![];
        let mut genesis_fields = None;
        for context in 0..3 {
            let spec = GenesisSpec {
                context,
                nonce: number(g, "nonce")?,
                timestamp: number(g, "timestamp")?,
                extra: raw(g, "extra")?,
                allocation_hash: raw(g, "allocation_hash")?
                    .try_into()
                    .map_err(|_| Error::Invalid("allocation hash"))?,
                gas_limit: number(g, "gas_limit")?,
                difficulty: Uint::from_u64(
                    text(g, "difficulty")?
                        .parse()
                        .map_err(|_| Error::Invalid("private genesis difficulty"))?,
                ),
                starting_expansion: number(g, "starting_expansion")?,
            };
            let block = build_genesis(&spec)?;
            genesis_fields = Some(quai_primitives::spec::Genesis {
                hash: block.id(),
                nonce: spec.nonce,
                extra: spec.extra.clone(),
                timestamp: spec.timestamp,
                gas_limit: spec.gas_limit,
                difficulty: text(g, "difficulty")?
                    .parse()
                    .map_err(|_| Error::Invalid("private genesis difficulty"))?,
                allocation_hash: spec.allocation_hash,
                starting_expansion: u8::try_from(spec.starting_expansion)
                    .map_err(|_| Error::Invalid("private starting expansion"))?,
            });
            let expected = &g["contexts"][context];
            if raw(expected, "hash")?.as_slice() != block.id().as_slice()
                || raw(expected, "body_hash")?.as_slice()
                    != fields::hash(&block.header.wire().header_hash, "missing header hash")?
            {
                return Err(Error::Invalid("private genesis commitment"));
            }
            genesis.push(block);
        }
        Ok(Self {
            activated_hierarchy,
            all_etx_hierarchy,
            header_admission,
            hierarchy_admission,
            fingerprint,
            name: text(&v, "name")?.into(),
            chain_id: 1337,
            location: [0, 0],
            genesis: genesis
                .try_into()
                .map_err(|_| Error::Invalid("private contexts"))?,
            genesis_fields: genesis_fields.ok_or(Error::Invalid("private contexts"))?,
            gas_limit: number(&v["protocol_limits"], "block_gas_limit")?,
            state_limit: number(&v["protocol_limits"], "state_limit")?,
            listen: text(&v["transport"], "listen")?.into(),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_genesis_and_forks_match_the_go_profile() {
        let p = PrivateChain::reviewed().unwrap();
        assert_eq!(p.chain_id, 1337);
        assert_eq!(p.genesis[0].id(), p.genesis[2].id());
    }
    #[test]
    fn edited_contract_cannot_silently_change_the_private_chain() {
        let mut v = REVIEWED_BYTES.to_vec();
        v.push(b' ');
        assert!(PrivateChain::decode(&v).is_err());
    }
    #[test]
    fn admission_profile_is_distinct_frozen_and_has_nontrivial_difficulty() {
        let old = PrivateChain::reviewed().unwrap();
        let p = PrivateChain::reviewed_admission().unwrap();
        assert_ne!(old.fingerprint, p.fingerprint);
        assert_ne!(old.genesis[2].id(), p.genesis[2].id());
        assert!(p.header_admission);
        assert!(!old.header_admission);
        assert_eq!(p.genesis[2].difficulty().unwrap(), Uint::from_u64(2));
        let mut changed = ADMISSION_BYTES.to_vec();
        changed.push(b' ');
        assert!(PrivateChain::decode(&changed).is_err());
    }
    #[test]
    fn hierarchy_permits_cannot_cross_frozen_runtime_contracts_with_the_same_heads() {
        use crate::{confirmation::Hierarchy, history::Record, verifier::Verifier};
        let corpus: Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json"))
                .unwrap();
        let r = &corpus["branches"][0]["vectors"][0];
        let block =
            Record::decode(&decode_hex(r["work_object"].as_str().unwrap()).unwrap()).unwrap();
        let mut old = Hierarchy::new().unwrap();
        let mut active =
            Hierarchy::new_for_chain(PrivateChain::execution_template().unwrap()).unwrap();
        let before = active.heads();
        let mut verifier = Verifier::default();
        let now = corpus["now"].as_u64().unwrap();
        let p = old.prepare(&mut verifier, &block, &[], now).unwrap();
        assert!(matches!(active.commit(p), Err(Error::Invalid(_))));
        assert_eq!(active.heads(), before);
        let p = active
            .prepare_for_runtime(&mut verifier, &block, &[], now)
            .unwrap();
        assert!(matches!(old.commit(p), Err(Error::Invalid(_))));
        assert_eq!(old.heads(), before);
    }
}
