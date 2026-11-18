#[allow(dead_code)]
#[path = "genesis_support/mod.rs"]
mod support;
use crypto_bigint::U512;
use prost::Message;
use quai_consensus::{
    candidate_state::CandidateState,
    history::Record,
    initialization::{self, GenesisSpec},
    placement::{self, Profile, Snapshot},
    rules::Uint,
    validation,
    verifier::Verifier,
    Error,
};
fn corpus() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/genesis.json")).unwrap()
}
fn spec() -> GenesisSpec {
    GenesisSpec {
        context: 0,
        nonce: 0,
        timestamp: 1769112000,
        extra: vec![],
        allocation_hash: [1; 32],
        gas_limit: 0,
        difficulty: Uint::from_u64(2),
        starting_expansion: 0,
    }
}
fn profile(r: &Record, ctx: usize) -> Profile {
    Profile {
        location: vec![0; ctx],
        genesis: [r.id()].into_iter().collect(),
        default_genesis: Some(r.id()),
        minimum_difficulty: Uint::ONE,
        duration: 5,
        gas_ceiling: 50_000_000,
        p2p_threshold: 7,
    }
}
fn empty() -> Snapshot {
    Snapshot {
        manifest: None,
        interlinks: None,
        termini: None,
    }
}
#[test]
fn frozen_genesis_geometry_and_difficulty_match_go() {
    let r = support::policy(&corpus()).unwrap();
    assert_eq!(r["mismatches"], 0, "policy mismatch");
    assert!(r["wide_difficulty_cases"].as_u64().unwrap() > 0);
}
#[test]
fn construction_keeps_the_allocation_hash_and_input_bytes() {
    let mut s = spec();
    s.extra = vec![3; 32];
    let before = s.extra.clone();
    let r = initialization::build_genesis(&s).unwrap();
    let mut expected = before.clone();
    expected.extend_from_slice(&s.allocation_hash);
    assert_eq!(
        r.object().header().wire().extra.as_ref().unwrap(),
        &expected
    );
    assert_eq!(s.extra, before);
    assert_eq!(r.object().header().wire().gas_limit, Some(12_000_000));
}
#[test]
fn genesis_bounds_fail_before_construction() {
    let mut s = spec();
    s.context = 3;
    assert!(initialization::build_genesis(&s).is_err());
    s.context = 0;
    s.extra = vec![1; initialization::MAX_GENESIS_EXTRA + 1];
    assert!(matches!(
        initialization::build_genesis(&s),
        Err(Error::ResourceLimit(_))
    ));
}
#[test]
fn expansion_cast_and_eligibility_use_different_source_conditions() {
    let mut s = spec();
    s.starting_expansion = 256;
    let r = initialization::build_genesis(&s).unwrap();
    let h = r.object().header().wire();
    assert_eq!(h.expansion_number, Some(0));
    assert_eq!(h.etx_eligible_slices.as_ref().unwrap().value, vec![255; 32]);
    assert!(initialization::new_chains(0).is_err());
    assert!(initialization::active_slices(31).is_err());
    assert_eq!(initialization::hierarchy_size(255), (128, 129));
}
#[test]
fn each_supported_expansion_adds_exactly_the_reported_zones() {
    for e in 1..=30 {
        let old = initialization::active_slices(e - 1).unwrap();
        let next = initialization::active_slices(e).unwrap();
        let added = initialization::new_chains(e).unwrap();
        let new_zones = added
            .into_iter()
            .filter(|v| v.len() == 2)
            .collect::<Vec<_>>();
        assert_eq!(
            next.iter()
                .filter(|v| !old.contains(v))
                .cloned()
                .collect::<Vec<_>>(),
            new_zones
        );
    }
}
#[test]
fn default_genesis_binding_preserves_expanded_difficulty_without_truncation() {
    let mut s = spec();
    s.starting_expansion = 2;
    let r = initialization::build_genesis(&s).unwrap();
    assert_eq!(
        initialization::genesis_difficulty(&r, 2, Some(r.id())).unwrap(),
        U512::from_u64(2)
    );
    let wide = initialization::genesis_difficulty(&r, 2, None).unwrap();
    assert_eq!(wide, U512::ONE.shl_vartime(256));
    let mut p = support::placement::history::profile();
    p.genesis.insert(r.id());
    assert!(matches!(
        validation::expected_difficulty(&Default::default(), &r, &p),
        Err(Error::Unsupported(_))
    ));
    p.default_genesis = Some(r.id());
    assert_eq!(
        validation::expected_difficulty(&Default::default(), &r, &p).unwrap(),
        Uint::from_u64(2)
    );
}
#[test]
fn genesis_seed_manifest_is_a_singleton_in_all_contexts() {
    let r = initialization::build_genesis(&spec()).unwrap();
    for ctx in 0..3 {
        let v = placement::bootstrap_candidate(&r, &profile(&r, ctx)).unwrap();
        assert_eq!(v.manifest, Some(vec![r.id()]));
        assert_eq!(v.interlinks.is_some(), ctx == 0);
    }
}
#[test]
fn candidate_store_requires_explicit_genesis_trust() {
    let r = initialization::build_genesis(&spec()).unwrap();
    let mut p = profile(&r, 0);
    p.genesis.clear();
    let mut s = CandidateState::new(p).unwrap();
    assert!(s.seed_trusted(r).is_err());
    assert!(s.history().is_empty() && s.index().is_empty());
}
#[test]
fn candidate_import_conflicts_leave_both_stores_unchanged() {
    let r = initialization::build_genesis(&spec()).unwrap();
    let mut s = CandidateState::new(profile(&r, 0)).unwrap();
    s.seed_trusted(r.clone()).unwrap();
    let before = (
        s.history().len(),
        s.history().encoded_bytes(),
        s.index().tracked_payload_bytes(),
    );
    assert!(s.import_unverified_ancestry(r.clone(), empty()).is_err());
    let mut w = quai_primitives::schema::block::ProtoWorkObject::decode(r.bytes()).unwrap();
    w.wo_body.as_mut().unwrap().header.as_mut().unwrap().extra = Some(vec![99]);
    let conflict = Record::decode(&w.encode_to_vec()).unwrap();
    assert_eq!(conflict.id(), r.id());
    assert!(s.import_unverified_ancestry(conflict, empty()).is_err());
    assert_eq!(
        before,
        (
            s.history().len(),
            s.history().encoded_bytes(),
            s.index().tracked_payload_bytes()
        )
    );
    s.seed_trusted(r).unwrap();
    assert_eq!(
        before,
        (
            s.history().len(),
            s.history().encoded_bytes(),
            s.index().tracked_payload_bytes()
        )
    );
}
#[test]
fn ancestry_index_budget_cannot_leave_a_history_record_behind() {
    let mut g = spec();
    let root = initialization::build_genesis(&g).unwrap();
    let mut s = CandidateState::new(profile(&root, 0)).unwrap();
    let large = Snapshot {
        manifest: Some(vec![[1; 32]; placement::MAX_MANIFEST]),
        interlinks: None,
        termini: None,
    };
    loop {
        let before = (
            s.history().len(),
            s.history().encoded_bytes(),
            s.index().len(),
            s.index().tracked_payload_bytes(),
        );
        let r = initialization::build_genesis(&g).unwrap();
        let id = r.id();
        match s.import_unverified_ancestry(r, large.clone()) {
            Ok(_) => g.nonce += 1,
            Err(Error::ResourceLimit(_)) => {
                assert_eq!(
                    before,
                    (
                        s.history().len(),
                        s.history().encoded_bytes(),
                        s.index().len(),
                        s.index().tracked_payload_bytes()
                    )
                );
                assert!(s.history().get(&id).is_err());
                break;
            }
            Err(e) => panic!("{e}"),
        };
    }
    assert!(s.history().len() < 4096);
}
#[test]
fn history_capacity_failure_does_not_insert_an_index_entry() {
    let mut g = spec();
    let root = initialization::build_genesis(&g).unwrap();
    let mut s = CandidateState::new(profile(&root, 0)).unwrap();
    for n in 0..quai_consensus::history::MAX_RECORDS {
        g.nonce = n as u64;
        s.import_unverified_ancestry(initialization::build_genesis(&g).unwrap(), empty())
            .unwrap();
    }
    let before = s.index().tracked_payload_bytes();
    g.nonce = quai_consensus::history::MAX_RECORDS as u64;
    let r = initialization::build_genesis(&g).unwrap();
    let id = r.id();
    assert!(matches!(
        s.import_unverified_ancestry(r, empty()),
        Err(Error::ResourceLimit(_))
    ));
    assert_eq!(s.index().tracked_payload_bytes(), before);
    assert!(s.index().get(&id).is_err());
}
#[test]
fn trusted_candidates_still_cannot_use_full_block_admission() {
    let r = initialization::build_genesis(&spec()).unwrap();
    let mut s = CandidateState::new(profile(&r, 0)).unwrap();
    s.seed_trusted(r.clone()).unwrap();
    // A trusted genesis candidate is not a block that extends a genesis.
    assert!(quai_consensus::block::validate_on_genesis(
        &mut Verifier::default(),
        quai_consensus::chain::Chain::private_hierarchy().unwrap(),
        [None, None, Some(std::sync::Arc::new(r.clone()))],
        1_800_000_000,
    )
    .is_err());
}
