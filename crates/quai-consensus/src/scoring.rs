//! PoEM scores of supplied candidate components. Scores are not fork authority.
use crate::{
    fields,
    history::{History, Record},
    placement::{self, CandidateIndex, Prepared, Profile, Route},
    rules::{self, Entropy, EntropyInput, Uint},
    share_rules,
    verifier::Verifier,
    Error,
};
use crypto_bigint::Encoding;
use quai_primitives::spec::FORKS;
use quai_primitives::Hash;
use sha2::{Digest, Sha256};
#[derive(Clone, Debug)]
pub struct CandidateScore {
    id: Hash,
    context: usize,
    profile_tag: Hash,
    metrics: Option<Entropy>,
    components: Option<Prepared>,
}
impl CandidateScore {
    pub fn id(&self) -> Hash {
        self.id
    }
    pub fn context(&self) -> usize {
        self.context
    }
    pub fn metrics(&self) -> Option<&Entropy> {
        self.metrics.as_ref()
    }
    pub fn total(&self) -> Uint {
        self.metrics.as_ref().map_or(Uint::ZERO, |m| m.total)
    }
    pub fn components(&self) -> Option<&Prepared> {
        self.components.as_ref()
    }
    pub fn work_checked(&self) -> bool {
        self.components.is_some()
    }
    pub fn fully_validated(&self) -> bool {
        false
    }
    pub fn history_verified(&self) -> bool {
        false
    }
    /// Comparison is meaningful only within the same supplied chain profile.
    pub fn compare(&self, other: &Self) -> Result<std::cmp::Ordering, Error> {
        if self.profile_tag != other.profile_tag {
            return Err(Error::Unsupported("different candidate score profiles"));
        }
        Ok(self.total().cmp(&other.total()))
    }
}
fn profile_tag(p: &Profile) -> Hash {
    let mut h = Sha256::new();
    h.update([p.location.len() as u8]);
    h.update(&p.location);
    h.update((p.genesis.len() as u64).to_be_bytes());
    for g in &p.genesis {
        h.update(g)
    }
    h.update([p.default_genesis.is_some() as u8]);
    if let Some(g) = p.default_genesis {
        h.update(g)
    }
    h.update(p.minimum_difficulty.to_be_bytes());
    h.update(p.duration.to_be_bytes());
    h.update(p.gas_ceiling.to_be_bytes());
    h.update(p.p2p_threshold.to_be_bytes());
    h.finalize().into()
}
/// Controlled-work arithmetic only. The caller-supplied hash is not a proof.
/// Hash-only projections cannot supply pre-fork share weights. Native scoring
/// derives them independently from verified work and supplied ancestry.
pub fn project_entropy(record: &Record, hash: Hash, context: usize) -> Result<Entropy, Error> {
    if context > 2 {
        return Err(Error::Invalid("score context"));
    }
    if record.number(context)? == 0 {
        return Err(Error::Unsupported("zero-number score projection"));
    }
    let uncles = record.uncles()?;
    if context == 2 && record.prime() < FORKS.kawpow && !uncles.is_empty() {
        return Err(Error::Unsupported("pre-fork nonempty workshare score"));
    }
    base_entropy(record, hash, context)
}
fn base_entropy(record: &Record, hash: Hash, context: usize) -> Result<Entropy, Error> {
    let b = record.object().header().wire();
    let uncles = record.uncles()?;
    let arr = |v: &[Vec<u8>]| -> Result<[Uint; 3], Error> {
        v.iter()
            .map(|n| share_rules::uint(n))
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| Error::Invalid("score entropy array"))
    };
    rules::entropy(&EntropyInput {
        hash,
        difficulty: record.difficulty()?,
        expansion: fields::some(b.expansion_number, "missing expansion number")? as u8,
        parent_entropy: arr(&b.parent_entropy)?,
        parent_delta: arr(&b.parent_delta_entropy)?,
        parent_uncled_delta: arr(&b.parent_uncled_delta_entropy)?,
        uncled_entropy: share_rules::uint(fields::bytes(
            &b.uncled_entropy,
            "missing uncled entropy",
        )?)?,
        postfork_share_count: if context == 2 && record.prime() >= FORKS.kawpow {
            Some(uncles.len() as u32)
        } else {
            None
        },
        node_context: context as u8,
    })
}
/// Native work-derived entropy projection on supplied ancestry. It grants no
/// validated-history, admitted-block or canonical authority.
pub fn native_entropy_components(
    v: &mut Verifier,
    history: &History,
    record: &Record,
    context: usize,
) -> Result<Entropy, Error> {
    if context > 2 || record.number(context)? == 0 {
        return Err(Error::Unsupported("native score context/height"));
    }
    record.verify_binding()?;
    let proof = v.verify_header_proof(record.header())?;
    let mut metrics = base_entropy(record, proof.hash(), context)?;
    if context == 2 && record.prime() < FORKS.kawpow {
        let shares = crate::prefork_shares::native_components(v, history, record)?;
        rules::add_pre_fork_work(&mut metrics, shares.workshares)?;
    }
    Ok(metrics)
}
/// Recompute supported proof/header/body/append components before scoring.
/// Imported ancestry stays unverified, and no canonical branch is published.
pub fn score_candidate_components(
    verifier: &mut Verifier,
    history: &History,
    index: &CandidateIndex,
    record: &Record,
    profile: &Profile,
    now: u64,
) -> Result<CandidateScore, Error> {
    profile.validate()?;
    if profile.genesis.contains(&record.id()) {
        if record.number(profile.context())? != 0 {
            return Err(Error::Unsupported("nonzero candidate genesis score"));
        }
        return Ok(CandidateScore {
            id: record.id(),
            context: profile.context(),
            profile_tag: profile_tag(profile),
            metrics: None,
            components: None,
        });
    }
    let prepared = placement::prepare_append_components(
        verifier, history, index, record, profile, now, false, [0; 32],
    )?;
    if prepared.placement.route != Route::Local {
        return Err(Error::Unsupported("score requires local order"));
    }
    let metrics = native_entropy_components(verifier, history, record, profile.context())?;
    if metrics.order != prepared.placement.order || metrics.rank != prepared.placement.rank {
        return Err(Error::Invalid("score placement binding"));
    }
    Ok(CandidateScore {
        id: record.id(),
        context: profile.context(),
        profile_tag: profile_tag(profile),
        metrics: Some(metrics),
        components: Some(prepared),
    })
}
