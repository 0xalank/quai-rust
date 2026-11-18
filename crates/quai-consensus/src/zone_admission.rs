//! Native append components for the pinned local-zone, pre-transaction profile.
//! This closes only that segment: no higher-order confirmation or chain authority.
use crate::{
    history::{History, Record},
    placement::{self, CandidateIndex, Prepared},
    private_profile::PrivateChain,
    verifier::Verifier,
    Error,
};
use quai_primitives::spec::FORKS;
pub fn profile() -> Result<placement::Profile, Error> {
    let p = PrivateChain::reviewed_admission()?;
    Ok(crate::chain::Chain::from_private(std::sync::Arc::new(p)).placement_profile(2))
}
fn domain(block: &Record) -> Result<(), Error> {
    if block.number(2)? > FORKS.start_tx
        || block.prime != 0
        || !block.uncles()?.is_empty()
        || !block.object().transactions().is_empty()
    {
        return Err(Error::Unsupported(
            "local pre-transaction zone segment required; hierarchy confirmation unavailable",
        ));
    }
    Ok(())
}
/// Reconstruct deterministic local manifests/PCRC from a supplied branch. The
/// caller must establish ancestry provenance. Reports retain history_verified=false.
pub fn index(
    history: &History,
    parent: &Record,
    p: &placement::Profile,
) -> Result<CandidateIndex, Error> {
    let mut cursor = crate::history::Cursor::from(parent);
    let mut chain = vec![];
    while !p.genesis.contains(&cursor.id()) {
        if chain.len() >= crate::history::MAX_RECORDS {
            return Err(Error::ResourceLimit("local append ancestry"));
        }
        domain(&cursor)?;
        let parent = cursor.parents[2];
        chain.push(cursor);
        cursor = history.get(&parent)?.into();
    }
    let mut index = CandidateIndex::default();
    index.insert_imported(cursor.id(), placement::bootstrap_candidate(&cursor, p)?)?;
    for row in chain.into_iter().rev() {
        let previous = index.get(&row.parents[2])?;
        placement::check_append_commitments(&index, &row, 2)?;
        let (_, termini) = placement::pcrc(
            previous
                .termini
                .as_ref()
                .ok_or(Error::Unavailable("local termini"))?,
            row.id(),
            &p.location,
            &p.location,
            false,
            [0; 32],
            &p.genesis,
        )?;
        let next = placement::Snapshot {
            manifest: Some(placement::next_manifest(
                2,
                2,
                row.id(),
                previous.manifest.as_deref(),
            )?),
            interlinks: None,
            termini: Some(termini),
        };
        index.insert_imported(row.id(), next)?;
    }
    Ok(index)
}
/// Full-size native proofs, parent entropy/header policy, body/inclusions,
/// parent manifest and PCRC components. A local zone block receives no new
/// confirmed ETXs without dominant confirmation; that other path is Unsupported.
pub fn inspect_local(
    verifier: &mut Verifier,
    history: &History,
    block: &Record,
    now: u64,
) -> Result<Prepared, Error> {
    domain(block)?;
    let p = profile()?;
    let placed = placement::verify_placement(verifier, block, &p)?;
    if placed.order != 2 {
        return Err(Error::Unsupported(
            "dominant confirmation and higher-order admission unavailable",
        ));
    }
    let index = index(history, history.get(&block.parents[2])?.as_ref(), &p)?;
    inspect_indexed_local(verifier, history, &index, block, &p, now)
}

/// Check a local-order zone append against explicit branch metadata. The caller
/// authenticates the history, profile and index. Activated transaction bodies
/// do not turn a supplied checkpoint into validated ancestry or chain authority.
pub fn inspect_indexed_local(
    verifier: &mut Verifier,
    history: &History,
    index: &CandidateIndex,
    block: &Record,
    profile: &placement::Profile,
    now: u64,
) -> Result<Prepared, Error> {
    if profile.location.len() != 2 {
        return Err(Error::Invalid("local zone append profile"));
    }
    let prepared = placement::prepare_append_components(
        verifier, history, index, block, profile, now, false, [0; 32],
    )?;
    let shares = crate::prefork_shares::native_components(verifier, history, block)?;
    if crate::share_rules::uint(
        block
            .object()
            .header()
            .wire()
            .uncled_entropy
            .as_ref()
            .unwrap(),
    )? != shares.uncled
    {
        return Err(Error::Invalid("local uncled entropy"));
    }
    Ok(prepared)
}
