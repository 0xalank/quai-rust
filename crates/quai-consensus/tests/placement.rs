#[allow(dead_code)]
#[path = "placement_support/mod.rs"]
mod support;
use prost::Message;
use quai_consensus::{
    history::Record,
    placement::{self, CandidateIndex, Snapshot, Termini},
    verifier::Verifier,
    Error,
};
use quai_primitives::{
    schema::{block::ProtoTermini, common::ProtoHash},
    Hash,
};
fn corpus() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/placement.json")).unwrap()
}
fn empty() -> Snapshot {
    Snapshot {
        manifest: None,
        interlinks: None,
        termini: None,
    }
}
fn key(n: usize) -> Hash {
    let mut out = [0; 32];
    out[..8].copy_from_slice(&(n as u64).to_be_bytes());
    out
}
#[test]
fn frozen_go_orders_append_commitments_and_pcrc_match() {
    let r = support::verify_policy(&corpus()).unwrap();
    assert_eq!(r["mismatches"], 0, "policy corpus mismatch");
    assert_eq!(r["checks"], 5301);
}
#[test]
fn every_context_order_has_an_explicit_route() {
    use placement::Route::*;
    let expected = [
        [Local, OtherContext, OtherContext],
        [Dominant, Local, OtherContext],
        [Dominant, Dominant, Local],
    ];
    for (ctx, row) in expected.iter().enumerate() {
        for (order, route) in row.iter().enumerate() {
            assert_eq!(placement::route(ctx, order as u8).unwrap(), *route);
        }
    }
    assert!(placement::route(3, 0).is_err());
    assert!(placement::route(0, 3).is_err());
}
#[test]
fn manifests_distinguish_missing_empty_reset_and_growth_limit() {
    let id = [9; 32];
    assert!(matches!(
        placement::next_manifest(2, 2, id, None),
        Err(Error::Unavailable(_))
    ));
    assert_eq!(
        placement::next_manifest(2, 2, id, Some(&[])).unwrap(),
        vec![id]
    );
    assert_eq!(placement::next_manifest(2, 1, id, None).unwrap(), vec![id]);
    assert!(placement::next_manifest(0, 0, id, None).unwrap().is_empty());
    // go-quai has no manifest length limit: a region manifest grows with
    // every zone block until the next region block.
    let parent = vec![[1; 32]; placement::MAX_MANIFEST];
    let next = placement::next_manifest(1, 1, id, Some(&parent)).unwrap();
    assert_eq!(next.len(), placement::MAX_MANIFEST + 1);
    assert_eq!(next.last(), Some(&id));
    assert!(placement::hash_list_root(&next).is_ok());
}
#[test]
fn interlink_rank_replaces_only_the_leading_slots() {
    let parent = [[1; 32], [2; 32], [3; 32], [4; 32]];
    for rank in 0..=4 {
        let mut expected = parent;
        expected[..rank].fill([9; 32]);
        assert_eq!(
            placement::next_interlinks([9; 32], Some(&parent), rank as u8, false).unwrap(),
            expected
        );
    }
    assert!(matches!(
        placement::next_interlinks([9; 32], None, 0, false),
        Err(Error::Unavailable(_))
    ));
    assert!(placement::next_interlinks([9; 32], Some(&parent), 5, false).is_err());
    assert_eq!(
        placement::next_interlinks([9; 32], None, 0, true).unwrap(),
        [[9; 32]; 4]
    );
}
#[test]
fn pcrc_uses_distinct_node_and_header_indexes_and_returns_the_parent_reference() {
    let parent = Termini {
        dominant: std::array::from_fn(key),
        subordinate: std::array::from_fn(|i| key(i + 16)),
    };
    let before = parent.clone();
    let id = [99; 32];
    let (returned, region) = placement::pcrc(
        &parent,
        id,
        &[15, 2],
        &[3],
        false,
        [0; 32],
        &Default::default(),
    )
    .unwrap();
    assert_eq!(returned, parent.subordinate[2]);
    assert_eq!(region.dominant[15], parent.dominant[3]);
    assert_eq!(region.subordinate[2], id);
    let (returned, zone) = placement::pcrc(
        &parent,
        id,
        &[15, 15],
        &[1, 2],
        false,
        [0; 32],
        &Default::default(),
    )
    .unwrap();
    assert_eq!(returned, [0; 32]);
    assert_eq!(zone.dominant[15], parent.dominant[2]);
    assert_eq!(zone.subordinate, parent.subordinate);
    assert_eq!(parent, before);
}
#[test]
fn pcrc_cycles_fail_atomically_and_trusted_genesis_bypass_is_explicit() {
    let parent = Termini::genesis([7; 32]);
    let before = parent.clone();
    assert!(matches!(
        placement::pcrc(
            &parent,
            [9; 32],
            &[1, 2],
            &[1],
            true,
            [8; 32],
            &Default::default()
        ),
        Err(Error::Invalid(_))
    ));
    assert_eq!(parent, before);
    let (_, next) = placement::pcrc(
        &parent,
        [9; 32],
        &[1, 2],
        &[1],
        true,
        [8; 32],
        &[[7; 32]].into_iter().collect(),
    )
    .unwrap();
    assert_eq!(next.dominant[1], [9; 32]);
    for (header, node) in [
        (vec![], vec![]),
        (vec![1, 16], vec![1]),
        (vec![1, 2], vec![1, 16]),
        (vec![1, 2], vec![1, 2, 3]),
    ] {
        assert!(placement::pcrc(
            &parent,
            [9; 32],
            &header,
            &node,
            false,
            [7; 32],
            &Default::default()
        )
        .is_err());
    }
}
#[test]
fn candidate_index_duplicates_conflicts_and_record_capacity_are_atomic() {
    let mut index = CandidateIndex::default();
    index.insert_imported(key(0), empty()).unwrap();
    let before = index.tracked_payload_bytes();
    index.insert_imported(key(0), empty()).unwrap();
    let mut conflict = empty();
    conflict.manifest = Some(vec![]);
    assert!(index.insert_imported(key(0), conflict).is_err());
    assert_eq!((index.len(), index.tracked_payload_bytes()), (1, before));
    for n in 1..placement::MAX_AUX_RECORDS {
        index.insert_imported(key(n), empty()).unwrap()
    }
    let before = index.tracked_payload_bytes();
    assert!(matches!(
        index.insert_imported(key(placement::MAX_AUX_RECORDS), empty()),
        Err(Error::ResourceLimit(_))
    ));
    assert_eq!(
        (index.len(), index.tracked_payload_bytes()),
        (placement::MAX_AUX_RECORDS, before)
    );
}
#[test]
fn candidate_index_payload_budget_is_checked_before_insertion() {
    let mut index = CandidateIndex::default();
    let large = Snapshot {
        manifest: Some(vec![[1; 32]; placement::MAX_MANIFEST]),
        interlinks: Some([[2; 32]; 4]),
        termini: Some(Termini::genesis([3; 32])),
    };
    let mut inserted = 0;
    loop {
        let n = index.len();
        let bytes = index.tracked_payload_bytes();
        match index.insert_imported(key(inserted), large.clone()) {
            Ok(()) => inserted += 1,
            Err(Error::ResourceLimit(_)) => {
                assert_eq!((index.len(), index.tracked_payload_bytes()), (n, bytes));
                break;
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert!(inserted < placement::MAX_AUX_RECORDS);
    assert!(index.tracked_payload_bytes() <= placement::MAX_AUX_BYTES);
}
#[test]
fn termini_decode_bounds_width_and_canonicalizes_short_hashes() {
    let mut p = ProtoTermini {
        dom_termini: vec![ProtoHash { value: vec![1] }; 16],
        sub_termini: vec![ProtoHash { value: vec![] }; 16],
    };
    let t = Termini::decode(&p.encode_to_vec()).unwrap();
    assert_eq!(t.dominant[0][31], 1);
    assert_eq!(t.subordinate, [[0; 32]; 16]);
    assert_eq!(Termini::decode(&t.encode()).unwrap(), t);
    p.dom_termini.pop();
    assert!(Termini::decode(&p.encode_to_vec()).is_err());
    p.dom_termini.push(ProtoHash { value: vec![1; 33] });
    assert!(Termini::decode(&p.encode_to_vec()).is_err());
    assert!(matches!(
        Termini::decode(&vec![0; 8193]),
        Err(Error::ResourceLimit(_))
    ));
}
#[test]
fn bootstrap_and_zero_number_placement_require_explicit_trust() {
    let c = corpus();
    let row = &c["native"][0];
    let root = support::history::record(&row["records"][0]).unwrap();
    let mut profile = support::profile(row).unwrap();
    let t = placement::bootstrap_candidate(&root, &profile).unwrap();
    assert_eq!(t.termini, Some(Termini::genesis(root.id())));
    let r = placement::verify_placement(&mut Verifier::default(), &root, &profile).unwrap();
    assert!(r.candidate_order_checked && !r.candidate_work_checked);
    assert!(matches!(
        placement::prepare_append_components(
            &mut Verifier::default(),
            &Default::default(),
            &Default::default(),
            &root,
            &profile,
            1769112040,
            true,
            [0; 32]
        ),
        Err(Error::Unsupported(_))
    ));
    profile.genesis.clear();
    assert!(placement::bootstrap_candidate(&root, &profile).is_err());
    assert!(matches!(
        placement::verify_placement(&mut Verifier::default(), &root, &profile),
        Err(Error::Unsupported(_))
    ));
    let b = Record::decode(root.bytes()).unwrap();
    assert!(matches!(
        Verifier::default().validate_block(b.bytes(), quai_primitives::work::Context::Prime),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn importing_a_candidate_does_not_skip_header_policy() {
    let c = corpus();
    let row = c["native"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "native/0/0/false/5")
        .unwrap();
    let b = support::history::record(&row["block"]).unwrap();
    let mut history = support::history::history(&row["records"]).unwrap();
    history.insert(b.clone()).unwrap();
    let mut index = CandidateIndex::default();
    index
        .insert_imported(
            support::parent_id(&b, 0),
            support::snapshot(&row["parent_snapshot"]).unwrap(),
        )
        .unwrap();
    let count = history.len();
    let bytes = index.tracked_payload_bytes();
    let result = quai_consensus::hierarchy_header::verify_header_components(
        &mut Verifier::default(),
        &history,
        &b,
        &quai_consensus::hierarchy_header::Profile {
            location: vec![],
            genesis: support::profile(row).unwrap().genesis,
        },
        1769112040,
        false,
    );
    assert!(matches!(result, Err(Error::Invalid(_))));
    assert_eq!(history.len(), count);
    assert_eq!(index.tracked_payload_bytes(), bytes);
}
