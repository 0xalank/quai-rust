//! Candidate ancestry and header/inclusion checks. No canonical chain authority.
use crate::{
    fields,
    history::{History, Record},
    rules,
    rules::Uint,
    share_rules::{self, Classification, Triple},
    verifier::{VerifiedWork, Verifier},
    Error,
};
use crypto_bigint::Encoding;
use quai_primitives::spec::FORKS;
use quai_primitives::{Hash, Header};
use std::collections::BTreeSet;
pub fn internal_address(address: &[u8], location: &[u8]) -> bool {
    location.len() == 2 && address.len() == 20 && address[0] == (location[0] << 4 | location[1])
}
fn header_id(h: &Header) -> Result<Hash, Error> {
    h.block_hash()
        .map_err(|_| Error::Unsupported("header hash domain"))
}
fn prime(h: &Header) -> Result<u64, Error> {
    share_rules::number(fields::bytes(
        &h.wire().prime_terminus_number,
        "missing prime terminus number",
    )?)
}
fn num(h: &Header) -> Result<u64, Error> {
    share_rules::number(fields::bytes(&h.wire().number, "missing number")?)
}
fn diff(h: &Header) -> Result<Uint, Error> {
    share_rules::uint(fields::bytes(&h.wire().difficulty, "missing difficulty")?)
}
fn parent(h: &Header) -> Result<Hash, Error> {
    fields::hash32(&h.wire().parent_hash, "missing parent hash")
}
pub fn data_policy(
    header: &Header,
    qi_restriction_height: u64,
    early_lock: bool,
) -> Result<(), Error> {
    let h = header.wire();
    let address = fields::address(&h.primary_coinbase, "missing primary coinbase")?;
    if address[1] > 127 && qi_restriction_height < FORKS.controller_kick_in {
        return Err(Error::Invalid("Qi coinbase before controller activation"));
    }
    if h.data.is_empty() {
        return Err(Error::Invalid("empty header data"));
    }
    // len(params.LockupByteToBlockDepth) - 1 = params.MaxLockupByte.
    if h.data[0] as usize > quai_primitives::spec::LOCK_PERIODS.by_lock_byte.len() - 1 {
        return Err(Error::Invalid("header data lock byte"));
    }
    let location = fields::location(&h.location)?;
    if h.data.len() >= 21 && (!internal_address(&h.data[1..21], location) || h.data[2] > 127) {
        return Err(Error::Invalid("lockup contract scope"));
    }
    if h.data.len() == 41 && !internal_address(&h.data[21..41], location) {
        return Err(Error::Invalid("beneficiary scope"));
    }
    if early_lock && num(header)? < FORKS.lockup_start && fields::some(h.lock, "missing lock")? != 0
    {
        return Err(Error::Invalid("early header lock"));
    }
    Ok(())
}
#[derive(Clone)]
pub struct Profile {
    pub location: Vec<u8>,
    pub minimum_difficulty: Uint,
    pub duration: u64,
    pub gas_ceiling: u64,
    pub p2p_threshold: u16,
    pub genesis: BTreeSet<Hash>,
    pub default_genesis: Option<Hash>,
}
impl Profile {
    pub fn validate(&self) -> Result<(), Error> {
        if self.location.len() != 2 || self.location.iter().any(|n| *n > 15) {
            return Err(Error::Unsupported("zone ancestry profile required"));
        }
        if self.duration == 0
            || self.minimum_difficulty == Uint::ZERO
            || self.p2p_threshold == 0
            || self.p2p_threshold > 255
            || self.genesis.len() > 256
        {
            return Err(Error::Invalid("history profile"));
        }
        Ok(())
    }
}
pub fn expected_difficulty(
    history: &History,
    parent: &Record,
    profile: &Profile,
) -> Result<Uint, Error> {
    if profile.genesis.contains(&parent.id) {
        let value = crate::initialization::genesis_difficulty(
            parent,
            fields::some(
                parent.object.header().wire().expansion_number,
                "missing expansion number",
            )? as u8,
            profile.default_genesis,
        )?;
        if value.bits_vartime() > 256 {
            return Err(Error::Unsupported(
                "genesis difficulty exceeds header domain",
            ));
        }
        return Ok(value.resize());
    }
    let grand = match history.get(&parent.parents[2]) {
        Ok(v) => v,
        Err(Error::Unavailable(_)) => return parent.difficulty(),
        Err(e) => return Err(e),
    };
    if profile.genesis.contains(&grand.id) {
        return parent.difficulty();
    }
    rules::adjust_difficulty(
        parent.difficulty()?,
        profile.minimum_difficulty,
        fields::some(parent.header.wire().time, "missing time")?,
        fields::some(grand.header.wire().time, "missing time")?,
        profile.duration,
    )
}
#[derive(Clone, Debug, Default)]
pub struct AlgoCounts {
    pub kawpow: u32,
    pub sha: u32,
    pub uncled_sha: u32,
    pub scrypt: u32,
    pub uncled_scrypt: u32,
}
pub fn count_algorithms(headers: &[Header], location: &[u8]) -> Result<AlgoCounts, Error> {
    let mut out = AlgoCounts::default();
    if headers.len() > 4096 {
        return Err(Error::ResourceLimit("share count"));
    }
    for h in headers {
        let id = h
            .wire()
            .aux_pow
            .as_ref()
            .map(|b| crate::auxiliary::Auxiliary::decode(b).map(|a| a.id()))
            .transpose()?
            .unwrap_or(0);
        let external = !internal_address(
            fields::address(&h.wire().primary_coinbase, "missing primary coinbase")?,
            location,
        );
        match id {
            0 | 1 => out.kawpow += 1,
            2 | 3 => {
                out.sha += 1;
                if external {
                    out.uncled_sha += 1;
                }
            }
            4 => {
                out.scrypt += 1;
                if external {
                    out.uncled_scrypt += 1;
                }
            }
            _ => return Err(Error::Invalid("unknown share algorithm")),
        }
    }
    Ok(out)
}
/// Fields updated from the parent's *included* workshares, not the candidate list.
pub fn expected_share_triples(
    parent: &Record,
    prime: u64,
    location: &[u8],
) -> Result<(Triple, Triple), Error> {
    let counts = count_algorithms(&parent.uncles()?, location)?;
    let p = parent.header.wire();
    Ok((
        share_rules::next_pow_triple(
            parent.difficulty()?,
            &Triple::from_header(&parent.header, true)?,
            share_rules::uint(p.sha_share_target.as_deref().unwrap_or_default())?,
            counts.sha,
            counts.uncled_sha,
            prime,
            true,
        )?,
        share_rules::next_pow_triple(
            parent.difficulty()?,
            &Triple::from_header(&parent.header, false)?,
            share_rules::uint(p.scrypt_share_target.as_deref().unwrap_or_default())?,
            counts.scrypt,
            counts.uncled_scrypt,
            prime,
            false,
        )?,
    ))
}
/// Cheap structural checks against the supplied parent/grandparent snapshot.
/// Other header entropy/controller/fork checks are explicitly not implied.
pub fn check_header_ancestry(
    history: &History,
    header: &Record,
    profile: &Profile,
    now: u64,
    uncle: bool,
) -> Result<HeaderAncestry, Error> {
    profile.validate()?;
    let parent = history.get(&header.parents[2])?;
    let w = header.header.wire();
    let b = header.object.header().wire();
    if b.extra.as_deref().unwrap_or_default().len() > quai_primitives::spec::MAXIMUM_EXTRA_DATA_SIZE
    {
        return Err(Error::Invalid("extra data size"));
    }
    if fields::hash(&w.header_hash, "missing header hash")? != header.object.header().hash() {
        return Err(Error::Invalid("body header commitment"));
    }
    let time = fields::some(w.time, "missing time")?;
    if !uncle && time > now.saturating_add(quai_primitives::spec::ALLOWED_FUTURE_BLOCK_TIME) {
        return Err(Error::Invalid("future block"));
    }
    if time < fields::some(parent.header.wire().time, "missing time")? {
        return Err(Error::Invalid("older block time"));
    }
    let expected = expected_difficulty(history, &parent, profile)?;
    if header.difficulty() != Ok(expected) {
        return Err(Error::Invalid("header difficulty"));
    }
    let location = fields::location(&w.location)?;
    let n = location.len().min(profile.location.len());
    if location[..n] != profile.location[..n] {
        return Err(Error::Invalid("header location"));
    }
    data_policy(&header.header, header.prime, true)?;
    if !internal_address(
        fields::address(&w.primary_coinbase, "missing primary coinbase")?,
        &profile.location,
    ) {
        return Err(Error::Invalid("coinbase scope"));
    }
    let gas_limit = fields::some(b.gas_limit, "missing gas limit")?;
    if gas_limit > i64::MAX as u64 || fields::some(b.gas_used, "missing gas used")? > gas_limit {
        return Err(Error::Invalid("gas use/limit"));
    }
    if gas_limit
        != rules::next_limit(
            parent.numbers[2],
            fields::some(parent.object.header().wire().gas_limit, "missing gas limit")?,
            profile.gas_ceiling,
        )
    {
        return Err(Error::Invalid("gas limit schedule"));
    }
    let state_limit = fields::some(b.state_limit, "missing state limit")?;
    if fields::some(b.state_used, "missing state used")? > state_limit
        || state_limit
            != rules::next_limit(
                parent.numbers[2],
                fields::some(
                    parent.object.header().wire().state_limit,
                    "missing state limit",
                )?,
                50_000_000,
            )
    {
        return Err(Error::Invalid("state limit schedule"));
    }
    let number = if profile.genesis.contains(&parent.id) {
        0
    } else {
        parent.numbers[2]
    };
    if header.numbers[2]
        != number
            .checked_add(1)
            .ok_or(Error::Unsupported("number overflow"))?
    {
        return Err(Error::Invalid("header number"));
    }
    let id = w
        .aux_pow
        .as_ref()
        .map(|b| crate::auxiliary::Auxiliary::decode(b).map(|a| a.id()))
        .transpose()?;
    if !rules::pow_id_valid(header.prime, id, false) {
        return Err(Error::Invalid("block proof ID"));
    }
    Ok(HeaderAncestry {
        parent: parent.id,
        number: header.numbers[2],
        fully_validated: false,
    })
}
#[derive(Clone, Debug)]
pub struct HeaderAncestry {
    pub parent: Hash,
    pub number: u64,
    pub fully_validated: bool,
}
#[derive(Clone, Debug)]
pub struct InclusionReport {
    pub checked: usize,
    pub distances: Vec<u64>,
    pub classifications: Vec<Classification>,
    pub external_donor_exception_eligible: usize,
    pub fully_validated: bool,
    pub history_verified: bool,
}
/// Runtime inclusion path: proof facts are privately constructed by the native
/// verifier. A controlled hash projection cannot enter this API.
pub fn verify_inclusions(
    verifier: &mut Verifier,
    history: &History,
    block: &Record,
    profile: &Profile,
) -> Result<InclusionReport, Error> {
    profile.validate()?;
    let headers = block.uncles()?;
    if headers.len() > share_rules::count_limits(block.prime).0 {
        return Err(Error::Invalid("too many uncles"));
    }
    block
        .object
        .verify_commitments(quai_primitives::work::Context::Zone)
        .map_err(|_| Error::Invalid("body commitments"))?;
    let mut works = Vec::with_capacity(headers.len());
    for h in &headers {
        works.push(verifier.work_for_inclusion(h, &profile.location)?);
    }
    check_inclusions_verified(verifier, history, block, profile, &headers, &works)
}
fn check_inclusions_verified(
    verifier: &mut Verifier,
    history: &History,
    block: &Record,
    profile: &Profile,
    headers: &[Header],
    works: &[VerifiedWork],
) -> Result<InclusionReport, Error> {
    for (h, w) in headers.iter().zip(works) {
        if header_id(h)? != w.header() {
            return Err(Error::Invalid("work/header binding"));
        }
    }
    check_inclusions_projection(
        history,
        block,
        profile,
        headers,
        &works.iter().map(VerifiedWork::hash).collect::<Vec<_>>(),
        |p| Ok(native_parent_metrics(verifier, history, p, profile)?.order),
    )
}
/// core/poem.go UncledLogEntropy: the intrinsic entropy of every included
/// header whose proof-of-work meets its own difficulty (VerifySeal), except
/// SHA and scrypt shares (PowID above Kawpow). Go applies it after KawPow;
/// before KawPow the port uses `prefork_shares`, which also weighs the shares.
pub fn uncled_log_entropy(verifier: &mut Verifier, block: &Record) -> Result<Uint, Error> {
    let location = fields::location(&block.header.wire().location)?.to_vec();
    let mut total = crypto_bigint::U512::ZERO;
    for h in block.uncles()? {
        if let Some(bytes) = &h.wire().aux_pow {
            if crate::auxiliary::Auxiliary::decode(bytes)?.id() > 1 {
                continue;
            }
        }
        let difficulty = diff(&h)?;
        if difficulty == Uint::ZERO {
            continue;
        }
        let work = verifier.work_for_inclusion(&h, &location)?;
        if quai_primitives::meets_blake3_target(&work.hash(), &difficulty.to_be_bytes())
            .map_err(|_| Error::Invalid("uncle difficulty"))?
        {
            total = total.wrapping_add(&rules::intrinsic(work.hash())?.resize());
        }
    }
    if total.bits_vartime() > 256 {
        return Err(Error::Unsupported("uncled entropy magnitude"));
    }
    Ok(total.resize())
}
/// Native parent proof, order and entropy that the zone header rules expect.
pub fn native_parent_metrics(
    verifier: &mut Verifier,
    history: &History,
    parent: &Record,
    profile: &Profile,
) -> Result<crate::zone_header::ParentEntropyProjection, Error> {
    parent.verify_binding()?;
    let id = parent
        .header
        .wire()
        .aux_pow
        .as_ref()
        .map(|b| crate::auxiliary::Auxiliary::decode(b).map(|a| a.id()))
        .transpose()?;
    if !rules::pow_id_valid(parent.prime, id, false) {
        return Err(Error::Invalid("parent proof-ID policy"));
    }
    if parent.numbers[2] == 0 {
        if !profile.genesis.contains(&parent.id) {
            return Err(Error::Unsupported("unqualified zero-height parent"));
        }
        return Ok(crate::zone_header::ParentEntropyProjection {
            order: 0,
            total: Uint::ZERO,
            delta: Uint::ZERO,
            uncled_delta: Uint::ZERO,
        });
    }

    let proof = verifier.verify_header_proof(&parent.header)?;
    let b = parent.object.header().wire();
    let arr = |v: &[Vec<u8>]| -> Result<[Uint; 3], Error> {
        v.iter()
            .map(|b| share_rules::uint(b))
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| Error::Invalid("entropy array"))
    };
    let pre_work = if parent.prime < FORKS.kawpow {
        crate::prefork_shares::native_components(verifier, history, parent)?.workshares
    } else {
        Uint::ZERO
    };
    let mut entropy = rules::entropy(&rules::EntropyInput {
        hash: proof.hash(),
        difficulty: parent.difficulty()?,
        expansion: fields::some(b.expansion_number, "missing expansion number")? as u8,
        parent_entropy: arr(&b.parent_entropy)?,
        parent_delta: arr(&b.parent_delta_entropy)?,
        parent_uncled_delta: arr(&b.parent_uncled_delta_entropy)?,
        uncled_entropy: share_rules::uint(fields::bytes(
            &b.uncled_entropy,
            "missing uncled entropy",
        )?)?,
        postfork_share_count: if parent.prime >= FORKS.kawpow {
            Some(parent.uncles()?.len() as u32)
        } else {
            None
        },
        node_context: 2,
    })?;
    rules::add_pre_fork_work(&mut entropy, pre_work)?;
    Ok(crate::zone_header::ParentEntropyProjection {
        order: entropy.order,
        total: if profile.genesis.contains(&parent.id) {
            Uint::ZERO
        } else {
            entropy.total
        },
        delta: if profile.genesis.contains(&parent.id) {
            Uint::ZERO
        } else {
            entropy.delta
        },
        uncled_delta: if profile.genesis.contains(&parent.id) {
            Uint::ZERO
        } else {
            entropy.uncled_delta
        },
    })
}
/// What [`verify_workshare`] found for one share.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkshareReport {
    /// The share's block hash (`WorkObjectHeader.Hash`).
    pub hash: Hash,
    /// The proof-of-work hash the classification used.
    pub pow: Hash,
    pub parent: Hash,
    pub number: u64,
    /// The parent was in the history, so the parent rules ran too.
    pub parent_checked: bool,
}
/// A gossiped workshare (go-quai `worker.AddWorkShare`): `validateUncleData`,
/// then `UncleWorkShareClassification` must be `Valid` (a share that meets the
/// block target is a block, not a workshare). When the share's parent is in
/// `history`, also the uncle rules of any block that would include it and that
/// do not depend on that block (`check_inclusions_projection`): the proof ID,
/// the difficulty after the parent, the prime terminus, the SHA and scrypt
/// controllers and the number. go-quai drops shares more than
/// 2 * NewWorkSharesInclusionDepth below its head before these rules; the
/// caller applies that distance.
pub fn verify_workshare(
    verifier: &mut Verifier,
    history: &History,
    header: &Header,
    profile: &Profile,
) -> Result<WorkshareReport, Error> {
    profile.validate()?;
    let h = header;
    let hash = header_id(h)?;
    // validateUncleData has no Qi coinbase and no early lock rule.
    data_policy(h, u64::MAX, false)?;
    let work = verifier.work_for_inclusion(h, &profile.location)?;
    match share_rules::classify(h, work.hash(), profile.p2p_threshold)? {
        Classification::Valid => {}
        Classification::Block => return Err(Error::Invalid("workshare meets the block target")),
        Classification::Sub => return Err(Error::Invalid("workshare below the workshare target")),
        Classification::Invalid => return Err(Error::Invalid("workshare proof classification")),
    }
    let algo = h
        .wire()
        .aux_pow
        .as_ref()
        .map(|b| crate::auxiliary::Auxiliary::decode(b).map(|a| a.id()))
        .transpose()?;
    if !rules::pow_id_valid(prime(h)?, algo, true) {
        return Err(Error::Invalid("uncle proof ID"));
    }
    let mut report = WorkshareReport {
        hash,
        pow: work.hash(),
        parent: parent(h)?,
        number: num(h)?,
        parent_checked: false,
    };
    let parent = match history.get(&report.parent) {
        Ok(p) => p,
        Err(Error::Unavailable(_)) => return Ok(report),
        Err(e) => return Err(e),
    };
    if diff(h)? != expected_difficulty(history, &parent, profile)? {
        return Err(Error::Invalid("uncle difficulty"));
    }
    if prime(h)? >= FORKS.kawpow {
        let order = native_parent_metrics(verifier, history, &parent, profile)?.order;
        let expected = if order == 0 || profile.genesis.contains(&parent.id) {
            parent.numbers[0]
        } else {
            parent.prime
        };
        if prime(h)? != expected {
            return Err(Error::Invalid("uncle prime terminus"));
        }
        let (sha, scrypt) = expected_share_triples(&parent, prime(h)?, &profile.location)?;
        let actual = Triple::from_header(h, true)?;
        if actual.difficulty != sha.difficulty || actual.count != sha.count {
            return Err(Error::Invalid("uncle SHA controller"));
        }
        let actual = Triple::from_header(h, false)?;
        if actual.difficulty != scrypt.difficulty || actual.count != scrypt.count {
            return Err(Error::Invalid("uncle scrypt controller"));
        }
    }
    let pn = if profile.genesis.contains(&parent.id) {
        0
    } else {
        parent.numbers[2]
    };
    if report.number
        != pn
            .checked_add(1)
            .ok_or(Error::Unsupported("uncle number overflow"))?
    {
        return Err(Error::Invalid("uncle number"));
    }
    report.parent_checked = true;
    Ok(report)
}
/// Testable controlled-hash policy projection. It does not verify PoW and never
/// returns a token accepted by the native runtime inclusion path.
pub fn check_inclusions_projection<F: FnMut(&Record) -> Result<u8, Error>>(
    history: &History,
    block: &Record,
    profile: &Profile,
    headers: &[Header],
    hashes: &[Hash],
    mut parent_order: F,
) -> Result<InclusionReport, Error> {
    profile.validate()?;
    if headers.len() != hashes.len() {
        return Err(Error::Invalid("proof projection count"));
    }
    let (total, sha_max, scrypt_max) = share_rules::count_limits(block.prime);
    if headers.len() > total {
        return Err(Error::Invalid("too many uncles"));
    }
    if headers.is_empty() {
        return Ok(InclusionReport {
            checked: 0,
            distances: vec![],
            classifications: vec![],
            external_donor_exception_eligible: 0,
            fully_validated: false,
            history_verified: false,
        });
    }
    let mut ancestors = BTreeSet::new();
    let mut banned = BTreeSet::new();
    let mut ancestor_work = BTreeSet::new();
    let mut banned_work = BTreeSet::new();
    let identity = |h: &Header| {
        h.pow_identity()
            .map_err(|_| Error::Unsupported("proof identity"))
    };
    let mut p = block.parents[2];
    for _ in 0..share_rules::inclusion_depth(block.prime) {
        let ancestor = history.get(&p)?;
        ancestors.insert(p);
        if block.prime >= FORKS.polaris {
            ancestor_work.insert(identity(ancestor.header())?);
        }
        for uncle in ancestor.uncles()? {
            banned.insert(header_id(&uncle)?);
            if block.prime >= FORKS.polaris {
                banned_work.insert(identity(&uncle)?);
            }
        }
        p = ancestor.parents[2];
    }
    ancestors.insert(block.id);
    banned.insert(block.id);
    if block.prime >= FORKS.polaris {
        ancestor_work.insert(identity(block.header())?);
        for h in headers {
            let id = identity(h)?;
            if !banned_work.insert(id) {
                return Err(Error::Invalid("duplicate uncle proof identity"));
            }
            if ancestor_work.contains(&id) {
                return Err(Error::Invalid("uncle proof is ancestor"));
            }
        }
    }
    let (mut sha, mut scrypt) = (0, 0);
    let mut distances = vec![];
    let mut classifications = vec![];
    let mut exceptions = 0;
    for (h, pow) in headers.iter().zip(hashes) {
        let id = header_id(h)?;
        if !banned.insert(id) {
            return Err(Error::Invalid("duplicate uncle"));
        }
        data_policy(h, block.prime, false)?;
        if ancestors.contains(&id) {
            return Err(Error::Invalid("uncle is ancestor"));
        }
        let classification = share_rules::classify(h, *pow, profile.p2p_threshold)?;
        let workshare = if block.prime < FORKS.kawpow {
            !quai_primitives::meets_blake3_target(pow, &diff(h)?.to_be_bytes())
                .map_err(|_| Error::Invalid("difficulty"))?
        } else {
            match classification {
                Classification::Block => false,
                Classification::Valid => true,
                _ => return Err(Error::Invalid("uncle proof classification")),
            }
        };
        let aux = h
            .wire()
            .aux_pow
            .as_ref()
            .map(|b| crate::auxiliary::Auxiliary::decode(b))
            .transpose()?;
        let algo = aux.as_ref().map(|a| a.id());
        if !rules::pow_id_valid(prime(h)?, algo, workshare) {
            return Err(Error::Invalid("uncle proof ID"));
        }
        if block.prime >= FORKS.kawpow {
            if matches!(algo, Some(2 | 3)) {
                sha += 1;
                if sha > sha_max {
                    return Err(Error::Invalid("too many SHA shares"));
                }
            }
            if algo == Some(4) {
                scrypt += 1;
                if scrypt > scrypt_max {
                    return Err(Error::Invalid("too many scrypt shares"));
                }
            }
        }
        let uncle_parent = parent(h)?;
        if !ancestors.contains(&uncle_parent) || (!workshare && uncle_parent == block.parents[2]) {
            return Err(Error::Invalid("dangling uncle"));
        }
        distances.push(history.workshare_distance(block, h)?);
        data_policy(h, block.prime, true)?;
        if let Some(aux) = &aux {
            let external = !internal_address(
                fields::address(&h.wire().primary_coinbase, "missing primary coinbase")?,
                &profile.location,
            );
            // Runtime work computation checks bindings and this exact exception; the
            // projection does not infer that supplied hashes establish signature validity.
            if matches!(aux.id(), 2..=4) && external {
                exceptions += 1;
            }
        }
        let parent = history.get(&parent(h)?)?;
        if diff(h)? != expected_difficulty(history, &parent, profile)? {
            return Err(Error::Invalid("uncle difficulty"));
        }
        if block.prime >= FORKS.kawpow {
            let order = parent_order(&parent)?;
            let expected = if order == 0 || profile.genesis.contains(&parent.id) {
                parent.numbers[0]
            } else {
                parent.prime
            };
            if prime(h)? != expected {
                return Err(Error::Invalid("uncle prime terminus"));
            }
            if prime(h)? >= FORKS.kawpow {
                let (sha, scrypt) = expected_share_triples(&parent, prime(h)?, &profile.location)?;
                let actual = Triple::from_header(h, true)?;
                if actual.difficulty != sha.difficulty || actual.count != sha.count {
                    return Err(Error::Invalid("uncle SHA controller"));
                }
                let actual = Triple::from_header(h, false)?;
                if actual.difficulty != scrypt.difficulty || actual.count != scrypt.count {
                    return Err(Error::Invalid("uncle scrypt controller"));
                }
            }
        }
        let pn = if profile.genesis.contains(&parent.id) {
            0
        } else {
            parent.numbers[2]
        };
        if num(h)?
            != pn
                .checked_add(1)
                .ok_or(Error::Unsupported("uncle number overflow"))?
        {
            return Err(Error::Invalid("uncle number"));
        }
        classifications.push(classification);
    }
    Ok(InclusionReport {
        checked: headers.len(),
        distances,
        classifications,
        external_donor_exception_eligible: exceptions,
        fully_validated: false,
        history_verified: false,
    })
}
