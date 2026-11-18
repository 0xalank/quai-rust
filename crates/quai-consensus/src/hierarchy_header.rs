//! Bounded prime/region header policy on imported candidate history.
//! Component reports never confer canonical or economic validity.
use crate::{
    history::{History, Record},
    rules::{self, Uint},
    share_rules,
    verifier::Verifier,
    zone_header::ParentEntropyProjection,
    Error,
};
use crypto_bigint::{Encoding, U1024};
use quai_primitives::spec::FORKS;
use quai_primitives::{work::Context, Hash};
use std::collections::BTreeSet;
#[derive(Clone, Debug)]
pub struct Profile {
    pub location: Vec<u8>,
    pub genesis: BTreeSet<Hash>,
}
impl Profile {
    pub fn validate(&self) -> Result<(), Error> {
        if self.location.len() > 1 || self.location.iter().any(|b| *b > 15) {
            return Err(Error::Unsupported("prime/region profile required"));
        }
        if self.genesis.len() > 256 {
            return Err(Error::ResourceLimit("genesis profile"));
        }
        Ok(())
    }
    pub fn context(&self) -> usize {
        self.location.len()
    }
}
fn wide(n: Uint) -> U1024 {
    n.resize()
}
fn narrow(n: U1024) -> Result<Uint, Error> {
    if n.bits_vartime() > 256 {
        return Err(Error::Unsupported("hierarchy arithmetic exceeds 256 bits"));
    }
    Ok(n.resize())
}
/// Go evaluates the EWMA expression in uint16, wrapping both multiply and add.
pub fn efficiency_score(
    previous: u16,
    delta: [Uint; 2],
    uncled: [Uint; 2],
    parent_of_parent_genesis: bool,
) -> Result<u16, Error> {
    let numerator = wide(uncled[0])
        .wrapping_add(&wide(uncled[1]))
        .wrapping_mul(&U1024::from_u64(100));
    let ratio = if parent_of_parent_genesis {
        U1024::ZERO
    } else {
        if numerator == U1024::ZERO {
            return Ok(previous);
        }
        let denominator = wide(delta[0]).wrapping_add(&wide(delta[1]));
        if denominator == U1024::ZERO {
            return Err(Error::Unsupported("nonzero uncled entropy with zero delta"));
        }
        numerator.wrapping_div(&denominator)
    };
    let bytes = ratio.to_be_bytes();
    let truncated = u16::from_be_bytes(bytes[126..].try_into().unwrap());
    Ok(previous.wrapping_mul(9).wrapping_add(truncated) / 10)
}
pub fn threshold_count(previous: u16, efficiency: u16) -> u16 {
    if previous == 0 {
        return 0;
    }
    if (previous < 144 && efficiency < u16::MAX) || previous == 1168 {
        0
    } else {
        previous.wrapping_add(1)
    }
}
pub fn miner_difficulty(
    prime_number: u64,
    difficulty: Uint,
    previous: Uint,
) -> Result<Uint, Error> {
    crate::controllers::miner_difficulty(prime_number, difficulty, previous)
}
pub fn update_etx_eligible(
    mut bitmap: Hash,
    location: &[u8],
    zone_number: u64,
) -> Result<Hash, Error> {
    if location.len() != 2 || location.iter().any(|b| *b > 15) {
        return Err(Error::Unsupported(
            "zone location needed for eligible bitmap",
        ));
    }
    let pos = location[0] as usize * 16 + location[1] as usize;
    let bit = 1u8 << (pos % 8);
    if zone_number > FORKS.start_tx {
        bitmap[pos / 8] |= bit
    } else {
        bitmap[pos / 8] &= !bit
    }
    Ok(bitmap)
}
fn role(record: &Record) -> Result<(), Error> {
    let id = record
        .header
        .wire()
        .aux_pow
        .as_ref()
        .map(|b| crate::auxiliary::Auxiliary::decode(b).map(|a| a.id()))
        .transpose()?;
    if !rules::pow_id_valid(record.prime, id, false) {
        return Err(Error::Invalid("hierarchy block proof ID"));
    }
    Ok(())
}
fn cheap(
    history: &History,
    header: &Record,
    profile: &Profile,
    now: u64,
    uncle: bool,
) -> Result<(), Error> {
    profile.validate()?;
    if now > i64::MAX as u64 - quai_primitives::spec::ALLOWED_FUTURE_BLOCK_TIME {
        return Err(Error::Unsupported("header clock domain"));
    }
    let ctx = profile.context();
    let parent = history.get(&header.parents[ctx])?;
    let b = header.object.header().wire();
    let w = header.header.wire();
    if b.extra.as_deref().unwrap_or_default().len() > quai_primitives::spec::MAXIMUM_EXTRA_DATA_SIZE
    {
        return Err(Error::Invalid("extra data size"));
    }
    if w.header_hash.as_ref().unwrap().value.as_slice() != header.object.header().hash() {
        return Err(Error::Invalid("body header commitment"));
    }
    if !uncle && w.time.unwrap() > now + quai_primitives::spec::ALLOWED_FUTURE_BLOCK_TIME {
        return Err(Error::Invalid("future block"));
    }
    if w.time.unwrap() < parent.header.wire().time.unwrap() {
        return Err(Error::Invalid("older block time"));
    }
    Ok(())
}
#[derive(Clone, Debug)]
pub struct Report {
    pub context: u8,
    pub parent: Hash,
    pub parent_order: u8,
    pub parent_work_checked: bool,
    pub candidate_work_checked: bool,
    pub candidate_order_checked: bool,
    pub fully_validated: bool,
    pub history_verified: bool,
}
/// Pure controlled-parent-entropy projection. It checks signed auxiliary
/// bindings, but supplied entropy is not a proof or chain-authority token.
pub fn check_projection(
    verifier: &mut Verifier,
    history: &History,
    header: &Record,
    profile: &Profile,
    now: u64,
    uncle: bool,
    metrics: &ParentEntropyProjection,
) -> Result<Report, Error> {
    header.verify_binding()?;
    cheap(history, header, profile, now, uncle)?;
    let ctx = profile.context();
    let parent = history.get(&header.parents[ctx])?;
    role(&parent)?;
    if metrics.order as usize > ctx {
        return Err(Error::Invalid("parent order exceeds context"));
    }
    let b = header.object.header().wire();
    let p = parent.object.header().wire();
    let w = header.header.wire();
    let pl = parent.header.wire();
    let location = &w.location.as_ref().unwrap().value;
    let n = location.len().min(profile.location.len());
    if location[..n] != profile.location[..n] {
        return Err(Error::Invalid("header slice location"));
    }
    if share_rules::uint(&b.parent_entropy[ctx])? != metrics.total {
        return Err(Error::Invalid("parent total entropy"));
    }
    if ctx == 1 {
        let (delta, uncled) = if metrics.order < 1 {
            (Uint::ZERO, Uint::ZERO)
        } else {
            (metrics.delta, metrics.uncled_delta)
        };
        if share_rules::uint(&b.parent_delta_entropy[1])? != delta {
            return Err(Error::Invalid("parent delta entropy"));
        }
        if share_rules::uint(&b.parent_uncled_delta_entropy[1])? != uncled {
            return Err(Error::Invalid("parent uncled delta entropy"));
        }
    }
    let empty = quai_primitives::trie::ordered_root(&[])
        .map_err(|_| Error::Unsupported("empty root domain"))?;
    if ctx == 0 {
        let (efficiency, threshold) = if parent.numbers[0] == 0 {
            (0, 0)
        } else {
            let score = efficiency_score(
                p.efficiency_score.unwrap() as u16,
                [
                    share_rules::uint(&p.parent_delta_entropy[1])?,
                    share_rules::uint(&p.parent_delta_entropy[2])?,
                ],
                [
                    share_rules::uint(&p.parent_uncled_delta_entropy[1])?,
                    share_rules::uint(&p.parent_uncled_delta_entropy[2])?,
                ],
                profile.genesis.contains(&parent.parents[0]),
            )?;
            (
                score,
                threshold_count(p.threshold_count.unwrap() as u16, score),
            )
        };
        if b.efficiency_score.unwrap() as u16 != efficiency {
            return Err(Error::Invalid("prime efficiency score"));
        }
        if b.threshold_count.unwrap() as u16 != threshold {
            return Err(Error::Invalid("prime threshold count"));
        }
        let previous: Hash = p
            .etx_eligible_slices
            .as_ref()
            .unwrap()
            .value
            .as_slice()
            .try_into()
            .unwrap();
        let eligible = if profile.genesis.contains(&parent.id) {
            previous
        } else {
            update_etx_eligible(
                previous,
                &pl.location.as_ref().unwrap().value,
                parent.numbers[2],
            )?
        };
        if b.etx_eligible_slices.as_ref().unwrap().value.as_slice() != eligible {
            return Err(Error::Invalid("ETX eligible slices"));
        }
        if b.prime_state_root.as_ref().unwrap().value.as_slice() != empty {
            return Err(Error::Invalid("prime state root"));
        }
        if share_rules::uint(b.miner_difficulty.as_deref().unwrap())?
            != miner_difficulty(
                parent.numbers[0],
                parent.difficulty()?,
                share_rules::uint(p.miner_difficulty.as_deref().unwrap())?,
            )?
        {
            return Err(Error::Invalid("prime miner difficulty"));
        }
    } else if b.region_state_root.as_ref().unwrap().value.as_slice() != empty {
        return Err(Error::Invalid("region state root"));
    }
    role(header)?;
    if header.prime >= FORKS.kawpow && w.aux_pow.is_some() {
        verifier.verify_auxiliary_components(&header.header, false)?;
    }
    let number = if profile.genesis.contains(&parent.id) {
        0
    } else {
        parent.numbers[ctx]
    };
    if header.numbers[ctx]
        != number
            .checked_add(1)
            .ok_or(Error::Unsupported("hierarchy number overflow"))?
    {
        return Err(Error::Invalid("hierarchy header number"));
    }
    Ok(Report {
        context: ctx as u8,
        parent: parent.id,
        parent_order: metrics.order,
        parent_work_checked: false,
        candidate_work_checked: false,
        candidate_order_checked: false,
        fully_validated: false,
        history_verified: false,
    })
}
fn native_metrics(
    verifier: &mut Verifier,
    parent: &Record,
    profile: &Profile,
) -> Result<ParentEntropyProjection, Error> {
    role(parent)?;
    parent.verify_binding()?;
    let ctx = profile.context();
    let b = parent.object.header().wire();
    let trusted = profile.genesis.contains(&parent.id);
    if parent.numbers[ctx] == 0 {
        let total = if trusted {
            Uint::ZERO
        } else {
            narrow(
                wide(share_rules::uint(&b.parent_entropy[0])?)
                    .wrapping_add(&wide(share_rules::uint(&b.parent_delta_entropy[1])?))
                    .wrapping_add(&wide(share_rules::uint(&b.parent_delta_entropy[2])?)),
            )?
        };
        return Ok(ParentEntropyProjection {
            order: 0,
            total,
            delta: Uint::ZERO,
            uncled_delta: Uint::ZERO,
        });
    }
    if parent.difficulty()? == Uint::ZERO {
        return Err(Error::Invalid("parent difficulty"));
    }
    let proof = verifier.verify_header_proof(&parent.header)?;
    let array = |values: &[Vec<u8>]| -> Result<[Uint; 3], Error> {
        values
            .iter()
            .map(|v| share_rules::uint(v))
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| Error::Invalid("entropy array"))
    };
    let entropy = rules::entropy(&rules::EntropyInput {
        hash: proof.hash(),
        difficulty: parent.difficulty()?,
        expansion: b.expansion_number.unwrap() as u8,
        parent_entropy: array(&b.parent_entropy)?,
        parent_delta: array(&b.parent_delta_entropy)?,
        parent_uncled_delta: array(&b.parent_uncled_delta_entropy)?,
        uncled_entropy: share_rules::uint(b.uncled_entropy.as_deref().unwrap())?,
        postfork_share_count: None,
        node_context: ctx as u8,
    })?;
    Ok(ParentEntropyProjection {
        order: entropy.order,
        total: if trusted { Uint::ZERO } else { entropy.total },
        delta: if trusted { Uint::ZERO } else { entropy.delta },
        uncled_delta: if trusted {
            Uint::ZERO
        } else {
            entropy.uncled_delta
        },
    })
}
/// Native parent proof/order plus bounded prime/region header policy.
pub fn verify_header_components(
    verifier: &mut Verifier,
    history: &History,
    header: &Record,
    profile: &Profile,
    now: u64,
    uncle: bool,
) -> Result<Report, Error> {
    cheap(history, header, profile, now, uncle)?;
    let parent = history.get(&header.parents[profile.context()])?;
    let metrics = native_metrics(verifier, &parent, profile)?;
    let mut r = check_projection(verifier, history, header, profile, now, uncle, &metrics)?;
    r.parent_work_checked = parent.numbers[profile.context()] != 0;
    Ok(r)
}
/// Header, native candidate proof and non-zone body commitments. Full economic
/// replay, hierarchical placement and canonical publication are not implied.
pub fn inspect_candidate(
    verifier: &mut Verifier,
    history: &History,
    header: &Record,
    profile: &Profile,
    now: u64,
) -> Result<Report, Error> {
    let mut r = verify_header_components(verifier, history, header, profile, now, false)?;
    let ctx = if profile.context() == 0 {
        Context::Prime
    } else {
        Context::Region
    };
    verifier.inspect_components(header.bytes(), ctx)?;
    r.candidate_work_checked = true;
    Ok(r)
}
