#[allow(dead_code)]
#[path = "frontier_support/mod.rs"]
mod support;
use crypto_bigint::U512;
use prost::Message;
use quai_consensus::{
    candidate_state::CandidateState,
    frontier::{Frontier, PendingFrontiers, Tip},
    history::Record,
    rules::Uint,
    scoring,
    verifier::Verifier,
    Error,
};
fn corpus() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/frontier.json")).unwrap()
}
fn simple(value: u64, id: u8) -> Frontier {
    Frontier::from_imported(
        0,
        [vec![], vec![0], vec![0, 0]]
            .into_iter()
            .map(|l| {
                (
                    l,
                    Tip {
                        id: [id; 32],
                        numbers: [0; 3],
                        entropy: Uint::from_u64(value),
                    },
                )
            })
            .collect(),
    )
    .unwrap()
}
fn anchor() -> (Record, quai_consensus::placement::Profile) {
    let c = corpus();
    let row = &c["profiles"][0];
    let root = support::genesis::placement::history::record(&row["root"]).unwrap();
    let p = support::genesis::placement::profile(row).unwrap();
    (root, p)
}
#[test]
fn frozen_poem_coordinator_and_pending_projections_match_go() {
    let r = support::policy(&corpus()).unwrap();
    assert_eq!(r["mismatches"], 0, "policy mismatch");
}
#[test]
fn complete_frontiers_reject_missing_and_duplicate_locations() {
    assert!(Frontier::from_imported(0, vec![]).is_err());
    let f = simple(1, 1);
    let mut entries = f
        .tips()
        .iter()
        .map(|(l, t)| (l.clone(), t.clone()))
        .collect::<Vec<_>>();
    entries[2].0 = vec![0];
    assert!(matches!(
        Frontier::from_imported(0, entries),
        Err(Error::Invalid(_))
    ));
}
#[test]
fn unsupported_hierarchy_and_pending_capacities_fail_closed() {
    assert!(Frontier::from_imported(31, vec![]).is_err());
    assert!(PendingFrontiers::new(0).is_err());
    assert!(PendingFrontiers::new(2001).is_err());
}
#[test]
fn entropy_sum_retains_more_than_256_bits() {
    let mut entries = vec![(
        vec![],
        Tip {
            id: [1; 32],
            numbers: [0; 3],
            entropy: Uint::MAX,
        },
    )];
    for r in 0..16 {
        entries.push((vec![r], entries[0].1.clone()));
        for z in 0..16 {
            entries.push((vec![r, z], entries[0].1.clone()));
        }
    }
    let f = Frontier::from_legacy_imported(30, entries).unwrap();
    let max: U512 = Uint::MAX.resize();
    assert_eq!(f.total(), max.wrapping_mul(&U512::from_u64(273)));
    assert!(f.total().bits_vartime() > 256);
}
#[test]
fn zone_extension_ignores_unrelated_prime_parent_but_prime_extension_requires_it() {
    let c = corpus();
    let row = c["coordinator"]["updates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| {
            r["expansion"] == 0
                && r["order"] == 2
                && r["extendable"] == true
                && r["before"] != r["after"]
        })
        .unwrap();
    let f = support::frontier(0, &row["before"]).unwrap();
    let b = support::genesis::placement::history::record(&row["block"]).unwrap();
    let mut w = quai_primitives::schema::block::ProtoWorkObject::decode(b.bytes()).unwrap();
    w.wo_body
        .as_mut()
        .unwrap()
        .header
        .as_mut()
        .unwrap()
        .parent_hash[0]
        .value = vec![0x99; 32];
    let b = Record::decode(&w.encode_to_vec()).unwrap();
    assert!(f.extendable(&b, 2).unwrap());
    assert!(!f.extendable(&b, 0).unwrap());
}
#[test]
fn failed_projection_leaves_every_tip_unchanged() {
    let c = corpus();
    let row = c["coordinator"]["updates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["extendable"] == false)
        .unwrap();
    let f = support::frontier(row["expansion"].as_u64().unwrap() as u8, &row["before"]).unwrap();
    let before = f.clone();
    let b = support::genesis::placement::history::record(&row["block"]).unwrap();
    assert!(f
        .updated_projection(&b, row["order"].as_u64().unwrap() as u8, Uint::ONE)
        .is_err());
    assert_eq!(f, before);
}
#[test]
fn updates_touch_only_the_required_context_slots() {
    let c = corpus();
    for order in 0..3 {
        let row = c["coordinator"]["updates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["expansion"] == 2 && r["order"] == order && r["extendable"] == true)
            .unwrap();
        let f = support::frontier(2, &row["before"]).unwrap();
        let b = support::genesis::placement::history::record(&row["block"]).unwrap();
        let next = f.updated_projection(&b, order as u8, Uint::ONE).unwrap();
        assert_eq!(
            next.tips()
                .iter()
                .filter(|(l, t)| *t != &f.tips()[*l])
                .count(),
            3 - order as usize
        );
    }
}
#[test]
fn equal_entropy_retains_the_first_frontier() {
    let mut p = PendingFrontiers::new(2).unwrap();
    assert!(p.offer(simple(1, 1)).inserted);
    assert!(!p.offer(simple(1, 2)).inserted);
    assert_eq!(
        p.peek(&U512::from_u64(3)).unwrap().tips()[&vec![]].id,
        [1; 32]
    );
}
#[test]
fn peeking_does_not_refresh_pending_eviction_order() {
    let mut p = PendingFrontiers::new(2).unwrap();
    p.offer(simple(1, 1));
    p.offer(simple(2, 2));
    assert!(p.peek(&U512::from_u64(3)).is_some());
    assert_eq!(p.offer(simple(3, 3)).evicted, Some(U512::from_u64(3)));
    assert!(p.peek(&U512::from_u64(3)).is_none());
}
#[test]
fn historical_best_can_outlive_its_pending_entry() {
    let mut p = PendingFrontiers::new(1).unwrap();
    p.offer(simple(100, 1));
    p.offer(simple(1, 2));
    assert_eq!(p.best_ever(), U512::from_u64(300));
    assert!(p.peek(&p.best_ever()).is_none());
    assert_eq!(*p.descending().next().unwrap().0, U512::from_u64(3));
}
#[test]
fn candidate_scores_cannot_be_compared_across_profiles() {
    let (r, p) = anchor();
    let mut s = CandidateState::new(p.clone()).unwrap();
    s.seed_trusted(r.clone()).unwrap();
    let a = scoring::score_candidate_components(
        &mut Verifier::default(),
        s.history(),
        s.index(),
        &r,
        &p,
        1769112040,
    )
    .unwrap();
    let mut different = p.clone();
    different.gas_ceiling += 1;
    let b = scoring::score_candidate_components(
        &mut Verifier::default(),
        s.history(),
        s.index(),
        &r,
        &different,
        1769112040,
    )
    .unwrap();
    assert!(a.compare(&b).is_err());
    assert_eq!(a.compare(&a).unwrap(), std::cmp::Ordering::Equal);
}
#[test]
fn trusted_anchor_scores_cannot_authorize_frontier_updates() {
    let (r, p) = anchor();
    let a = scoring::score_candidate_components(
        &mut Verifier::default(),
        &Default::default(),
        &Default::default(),
        &r,
        &p,
        1769112040,
    )
    .unwrap();
    assert!(!a.work_checked() && !a.fully_validated() && !a.history_verified());
    assert!(simple(0, 1).updated_with_score(&r, &a).is_err());
}
#[test]
fn anchors_are_seeded_explicitly_and_not_inserted_through_scoring() {
    let (r, p) = anchor();
    let mut s = CandidateState::new(p).unwrap();
    assert!(matches!(
        s.score_and_insert(&mut Verifier::default(), r.clone(), 1769112040),
        Err(Error::Unsupported(_))
    ));
    assert!(s.history().is_empty() && s.index().is_empty());
    s.seed_trusted(r).unwrap();
}
#[test]
fn nonempty_prefork_weight_is_an_explicit_unsupported_domain() {
    let c = corpus();
    let row = c["scores"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["context"] == 2 && r["metrics"]["workshares"] != "0")
        .unwrap();
    let b = support::genesis::placement::history::record(&row["block"]).unwrap();
    let mut w = quai_primitives::schema::block::ProtoWorkObject::decode(b.bytes()).unwrap();
    w.wo_header.as_mut().unwrap().prime_terminus_number = Some(vec![]);
    let b = Record::decode(&w.encode_to_vec()).unwrap();
    assert!(matches!(
        scoring::project_entropy(&b, support::genesis::hash(&row["pow_hash"]).unwrap(), 2),
        Err(Error::Unsupported(_))
    ));
}
#[test]
fn zero_height_and_invalid_context_do_not_gain_projected_scores() {
    let (r, _) = anchor();
    assert!(matches!(
        scoring::project_entropy(&r, [1; 32], 0),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        scoring::project_entropy(&r, [1; 32], 3),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn legacy_aliasing_requires_an_explicit_consistent_view() {
    let tip = Tip {
        id: [1; 32],
        numbers: [0; 3],
        entropy: Uint::ONE,
    };
    let mut entries = vec![(vec![], tip.clone())];
    for r in 0..5 {
        entries.push((vec![r], tip.clone()));
        for z in 0..5 {
            entries.push((vec![r, z], tip.clone()));
        }
    }
    assert!(matches!(
        Frontier::from_imported(8, entries.clone()),
        Err(Error::Unsupported(_))
    ));
    assert!(Frontier::from_legacy_imported(8, entries.clone())
        .unwrap()
        .uses_legacy_aliases());
    entries
        .iter_mut()
        .find(|(l, _)| *l == vec![4])
        .unwrap()
        .1
        .id = [2; 32];
    assert!(matches!(
        Frontier::from_legacy_imported(8, entries),
        Err(Error::Invalid(_))
    ));
}
