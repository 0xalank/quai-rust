//! Zone header policy components on supplied candidate history. No chain authority.
use crate::{
    history::{History, Record},
    rules::Uint,
    share_rules::{self, Triple},
    validation::{self, Profile},
    verifier::Verifier,
    Error,
};
use quai_primitives::spec::FORKS;
use quai_primitives::Hash;
#[derive(Clone, Debug)]
pub struct ParentEntropyProjection {
    pub order: u8,
    pub total: Uint,
    pub delta: Uint,
    pub uncled_delta: Uint,
}
#[derive(Clone, Debug)]
pub struct ZoneHeaderReport {
    pub parent: Hash,
    pub parent_order: u8,
    pub expansion: u8,
    pub base_fee: Uint,
    pub candidate_proof_checked: bool,
    pub fully_validated: bool,
    pub history_verified: bool,
}
pub fn expected_expansion(
    history: &History,
    parent: &Record,
    profile: &Profile,
    order: u8,
) -> Result<u8, Error> {
    if order > 2 {
        return Err(Error::Invalid("parent order"));
    }
    let id = if order == 0 {
        parent.id
    } else {
        parent
            .object
            .header()
            .wire()
            .prime_terminus_hash
            .as_ref()
            .unwrap()
            .value
            .as_slice()
            .try_into()
            .unwrap()
    };
    let prime = history.get(&id)?;
    let b = prime.object.header().wire();
    if profile.genesis.contains(&id) && profile.location == [0, 0] {
        return Ok(b.expansion_number.unwrap() as u8);
    }
    if b.threshold_count.unwrap() == 1168 {
        return Ok((b.expansion_number.unwrap() as u8).wrapping_add(1));
    }
    Ok(history
        .get(&prime.parents[0])?
        .object
        .header()
        .wire()
        .expansion_number
        .unwrap() as u8)
}
/// Controlled-parent-entropy projection for offline comparison. It verifies signed
/// auxiliary bindings, but supplied entropy is not a native proof/authority token.
pub fn check_zone_header_projection(
    verifier: &mut Verifier,
    history: &History,
    header: &Record,
    profile: &Profile,
    now: u64,
    uncle: bool,
    metrics: &ParentEntropyProjection,
) -> Result<ZoneHeaderReport, Error> {
    header.verify_binding()?;
    if now > i64::MAX as u64 - quai_primitives::spec::ALLOWED_FUTURE_BLOCK_TIME {
        return Err(Error::Unsupported("header clock domain"));
    }
    validation::check_header_ancestry(history, header, profile, now, uncle)?;
    let parent = history.get(&header.parents[2])?;
    let b = header.object.header().wire();
    let w = header.header.wire();
    if metrics.order > 2 {
        return Err(Error::Invalid("parent order"));
    }
    let actual = share_rules::uint(&b.parent_entropy[2])?;
    if actual != metrics.total {
        return Err(Error::Invalid("parent total entropy"));
    }
    let delta = if metrics.order < 2 {
        Uint::ZERO
    } else {
        metrics.delta
    };
    if share_rules::uint(&b.parent_delta_entropy[2])? != delta {
        return Err(Error::Invalid("parent delta entropy"));
    }
    let uncled = if metrics.order < 2 {
        Uint::ZERO
    } else {
        metrics.uncled_delta
    };
    if share_rules::uint(&b.parent_uncled_delta_entropy[2])? != uncled {
        return Err(Error::Invalid("parent uncled delta entropy"));
    }
    let expansion = expected_expansion(history, &parent, profile, metrics.order)?;
    if b.expansion_number.unwrap() as u8 != expansion {
        return Err(Error::Invalid("zone expansion"));
    }
    if header.prime >= FORKS.kawpow && w.aux_pow.is_some() {
        verifier.verify_auxiliary_components(&header.header, false)?;
    }
    let base_fee = crate::economics::expected_base_fee(history, &parent, profile)?;
    if share_rules::uint(b.base_fee.as_deref().unwrap())? != base_fee {
        return Err(Error::Invalid("base fee"));
    }
    let (ph, pn) = if metrics.order == 0 || profile.genesis.contains(&parent.id) {
        (parent.id, parent.numbers[0])
    } else {
        (
            parent
                .object
                .header()
                .wire()
                .prime_terminus_hash
                .as_ref()
                .unwrap()
                .value
                .as_slice()
                .try_into()
                .unwrap(),
            parent.prime,
        )
    };
    if b.prime_terminus_hash.as_ref().unwrap().value.as_slice() != ph || header.prime != pn {
        return Err(Error::Invalid("prime terminus binding"));
    }
    if header.prime >= FORKS.kawpow {
        let (sha, scrypt) =
            validation::expected_share_triples(&parent, header.prime, &profile.location)?;
        if Triple::from_header(&header.header, true)? != sha {
            return Err(Error::Invalid("header SHA controller"));
        }
        if Triple::from_header(&header.header, false)? != scrypt {
            return Err(Error::Invalid("header scrypt controller"));
        }
        let pw = parent.header.wire();
        let target = share_rules::share_target(
            parent.difficulty()?,
            share_rules::uint(pw.kawpow_difficulty.as_deref().unwrap_or_default())?,
            share_rules::uint(pw.sha_share_target.as_deref().unwrap_or_default())?,
            header.prime,
        )?;
        if share_rules::uint(w.sha_share_target.as_deref().unwrap())? != target
            || share_rules::uint(w.scrypt_share_target.as_deref().unwrap())? != target
        {
            return Err(Error::Invalid("header share target"));
        }
        let aux = pw
            .aux_pow
            .as_ref()
            .map(|b| crate::auxiliary::Auxiliary::decode(b))
            .transpose()?;
        let kaw = share_rules::next_kawpow_difficulty(
            share_rules::uint(pw.kawpow_difficulty.as_deref().unwrap_or_default())?,
            header.prime,
            aux.as_ref(),
        )?;
        if share_rules::uint(w.kawpow_difficulty.as_deref().unwrap())? != kaw {
            return Err(Error::Invalid("header KAWPOW controller"));
        }
    }
    Ok(ZoneHeaderReport {
        parent: parent.id,
        parent_order: metrics.order,
        expansion,
        base_fee,
        candidate_proof_checked: false,
        fully_validated: false,
        history_verified: false,
    })
}
/// Native parent proof/order and bounded zone policy. Candidate proof, body/state
/// execution, full hierarchy and canonical history are separate stages.
pub fn verify_zone_header_components(
    verifier: &mut Verifier,
    history: &History,
    header: &Record,
    profile: &Profile,
    now: u64,
    uncle: bool,
) -> Result<ZoneHeaderReport, Error> {
    profile.validate()?;
    if now > i64::MAX as u64 - quai_primitives::spec::ALLOWED_FUTURE_BLOCK_TIME {
        return Err(Error::Unsupported("header clock domain"));
    }
    validation::check_header_ancestry(history, header, profile, now, uncle)?;
    let parent = history.get(&header.parents[2])?;
    let metrics = validation::native_parent_metrics(verifier, history, &parent, profile)?;
    check_zone_header_projection(verifier, history, header, profile, now, uncle, &metrics)
}
/// Compose the existing native proof/body/uncle stages with zone header policy.
/// Reports remain unvalidated until economic replay and canonical gates close.
pub fn inspect_zone_candidate(
    verifier: &mut Verifier,
    history: &History,
    header: &Record,
    profile: &Profile,
    now: u64,
) -> Result<ZoneHeaderReport, Error> {
    let mut report = verify_zone_header_components(verifier, history, header, profile, now, false)?;
    verifier.inspect_components(header.bytes(), quai_primitives::work::Context::Zone)?;
    validation::verify_inclusions(verifier, history, header, profile)?;
    report.candidate_proof_checked = true;
    Ok(report)
}
