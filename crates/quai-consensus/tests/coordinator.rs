#[allow(dead_code)]
#[path = "coordinator_support/mod.rs"]
mod support;
use prost::Message;
use quai_consensus::{
    coordinator::{ReapplicationSchedule, RecentCandidates},
    eligibility::CoordinatorView,
    frontier::{PendingFrontiers, Tip},
    rules::Uint,
    verifier::Verifier,
    Error,
};
use serde_json::Value;
use std::{collections::BTreeSet, sync::OnceLock};
fn corpus() -> &'static Value {
    static V: OnceLock<Value> = OnceLock::new();
    V.get_or_init(|| {
        serde_json::from_str(include_str!("../../../fixtures/protocol/coordinator.json")).unwrap()
    })
}
fn tip(id: u64, e: u64) -> Tip {
    let mut h = [0; 32];
    h[24..].copy_from_slice(&id.to_be_bytes());
    Tip {
        id: h,
        numbers: [id; 3],
        entropy: Uint::from_u64(e),
    }
}
#[test]
fn frozen_coordinator_search_ordering_and_reapplication_match_go() {
    let r = support::policy(corpus()).unwrap();
    assert_eq!(r["mismatches"], 0, "{}", r["failures"]);
    assert_eq!(r["sort_cases"], 135);
    assert_eq!(r["search_cases"], 120);
    assert_eq!(r["recent_events"], 2044);
    assert_eq!(r["reapplication_passes"], 90);
}
#[test]
fn event_admission_compares_oldest_even_below_capacity() {
    let mut r = RecentCandidates::new(0, false, 1000).unwrap();
    r.import_unverified(&[0, 0], tip(1, 10)).unwrap();
    r.import_unverified(&[0, 0], tip(2, 100)).unwrap();
    assert!(r.observe_projection(&[0, 0], tip(3, 11)).unwrap().accepted);
    assert!(!r.observe_projection(&[0, 0], tip(4, 10)).unwrap().accepted);
    assert!(r.observe_projection(&[0, 0], tip(2, 100)).unwrap().accepted);
    assert_eq!(
        r.at(&[0, 0])
            .unwrap()
            .iter()
            .map(|n| n.tip.id)
            .collect::<Vec<_>>(),
        [tip(1, 10).id, tip(3, 11).id, tip(2, 100).id]
    );
}
#[test]
fn conflicting_recent_payloads_leave_arrival_and_size_unchanged() {
    let mut r = RecentCandidates::new(0, false, 1000).unwrap();
    r.import_unverified(&[0, 0], tip(1, 10)).unwrap();
    let before = r.at(&[0, 0]).unwrap();
    assert!(matches!(
        r.import_unverified(&[0, 0], tip(1, 11)),
        Err(Error::Invalid(_))
    ));
    assert_eq!(r.at(&[0, 0]).unwrap(), before);
    assert_eq!(r.len(), 1);
}
#[test]
fn candidate_reads_preserve_fifo_and_duplicates_move_to_newest() {
    let mut r = RecentCandidates::new(0, false, 2).unwrap();
    for i in 1..=2 {
        r.import_unverified(&[0, 0], tip(i, i)).unwrap();
    }
    r.candidates(&[0, 0], &BTreeSet::new()).unwrap();
    r.import_unverified(&[0, 0], tip(1, 1)).unwrap();
    let a = r.import_unverified(&[0, 0], tip(3, 3)).unwrap();
    assert_eq!(a.evicted, Some(tip(2, 2).id));
    assert_eq!(r.at(&[0, 0]).unwrap()[0].tip.id, tip(1, 1).id);
}
#[test]
fn recent_capacity_is_global_but_existing_full_slices_can_evict() {
    let mut r = RecentCandidates::new(4, false, 1000).unwrap();
    let loc = quai_consensus::initialization::active_slices(4).unwrap();
    for i in 0..4096 {
        r.import_unverified(&loc[i / 1000], tip(i as u64 + 1, i as u64 + 1))
            .unwrap();
    }
    let before = r.at(&loc[4]).unwrap();
    assert!(matches!(
        r.import_unverified(&loc[4], tip(99999, 99999)),
        Err(Error::ResourceLimit(_))
    ));
    assert_eq!(r.len(), 4096);
    assert_eq!(r.at(&loc[4]).unwrap(), before);
    assert!(r
        .import_unverified(&loc[0], tip(99999, 99999))
        .unwrap()
        .evicted
        .is_some());
    assert_eq!(r.len(), 4096);
}
#[test]
fn leaders_keep_first_entropy_tie_and_fail_closed_when_all_excluded() {
    let mut r = RecentCandidates::new(0, false, 1000).unwrap();
    r.import_unverified(&[0, 0], tip(2, 7)).unwrap();
    r.import_unverified(&[0, 0], tip(1, 7)).unwrap();
    assert_eq!(r.leaders(&BTreeSet::new()).unwrap()[0].tip.id, tip(2, 7).id);
    let bad = [tip(2, 7).id].into_iter().collect();
    assert_eq!(r.leaders(&bad).unwrap()[0].tip.id, tip(1, 7).id);
    let bad = [tip(2, 7).id, tip(1, 7).id].into_iter().collect();
    assert!(matches!(r.leaders(&bad), Err(Error::Unavailable(_))));
}
#[test]
fn empty_search_bootstraps_active_slices_without_authority() {
    let d = support::Dataset::decode(&corpus()["policy"]["datasets"][0]).unwrap();
    let mut r = RecentCandidates::new(d.expansion, false, 1000).unwrap();
    let found = r
        .search_projection(&d.view, d.genesis, &d.orders, &d.entropy)
        .unwrap();
    assert!(found.frontier.is_some());
    assert_eq!(found.attempts.len(), 1);
    assert_eq!(r.len(), 2);
    assert!(!found.fully_validated && !found.history_verified);
}
#[test]
fn unavailable_bootstrap_metadata_preserves_all_recent_caches() {
    let d = support::Dataset::decode(&corpus()["policy"]["datasets"][0]).unwrap();
    let e = &corpus()["policy"]["datasets"][0]["entries"][0];
    let root = support::eligibility::record(&e["block"]).unwrap();
    let mut view = CoordinatorView::new(false);
    view.register(support::eligibility::profile(vec![0, 0], d.genesis))
        .unwrap();
    view.import_unverified(&[0, 0], root, None).unwrap();
    let mut r = RecentCandidates::new(d.expansion, false, 1000).unwrap();
    assert!(matches!(
        r.search_projection(&view, d.genesis, &d.orders, &d.entropy),
        Err(Error::Unavailable(_))
    ));
    assert!(r.is_empty());
}
#[test]
fn native_node_entropy_rejects_unbound_body_before_proof_work() {
    let input = &corpus()["native"]["input"];
    let original = support::eligibility::record(&input["steps"][0]["block"]).unwrap();
    let mut wire =
        quai_primitives::schema::block::ProtoWorkObject::decode(original.bytes()).unwrap();
    wire.wo_body
        .as_mut()
        .unwrap()
        .header
        .as_mut()
        .unwrap()
        .parent_entropy[0] = vec![0x99];
    let r = quai_consensus::history::Record::decode(&wire.encode_to_vec()).unwrap();
    assert_eq!(r.id(), original.id());
    let gen = support::eligibility::hash(&input["genesis"]).unwrap();
    let mut view = CoordinatorView::new(false);
    view.register(support::eligibility::profile(vec![], gen))
        .unwrap();
    view.import_unverified(&[], r.clone(), None).unwrap();
    assert!(matches!(
        view.tip_native(&mut Verifier::default(), &[], r.id()),
        Err(Error::Invalid("coordinator body commitments"))
    ));
    let graph = support::eligibility::native_view(&serde_json::json!({"native":input})).unwrap();
    let root = support::eligibility::record(&input["root"]).unwrap();
    let mut recent = RecentCandidates::new(0, false, 1000).unwrap();
    recent
        .import_unverified(
            &[0, 0],
            Tip {
                id: gen,
                numbers: [
                    root.number(0).unwrap(),
                    root.number(1).unwrap(),
                    root.number(2).unwrap(),
                ],
                entropy: Uint::ONE,
            },
        )
        .unwrap();
    assert!(matches!(
        recent.search_native(&graph, &mut Verifier::default(), gen),
        Err(Error::Invalid("native recent candidate binding"))
    ));
}
#[test]
fn reapplication_visits_twenty_one_then_sorts_processing_order() {
    let row = corpus()["policy"]["reapplications"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["expansion"] == 0 && r["capacity"] == 2000 && r["order"] == 0)
        .unwrap();
    let mut p = PendingFrontiers::new(2000).unwrap();
    for seed in 1..=60 {
        p.offer(support::synthetic_frontier(0, seed));
    }
    assert!(p.len() > 21);
    let b = support::eligibility::record(&row["block"]).unwrap();
    let r = p.reapply_projection(&b, 0, Uint::from_u64(1111)).unwrap();
    assert_eq!(r.visited, 21);
    assert!(p.processing_order().windows(2).all(|w| w[0] < w[1]));
    assert!(!r.fully_validated && !r.history_verified);
}
#[test]
fn reapplication_component_failure_preserves_the_entire_cache() {
    let mut p = PendingFrontiers::new(4).unwrap();
    for i in 1..=4 {
        p.offer(support::synthetic_frontier(0, i));
    }
    let before = p.processing_order().to_vec();
    let snapshots = p
        .descending()
        .map(|(k, f)| (*k, support::digest(f)))
        .collect::<Vec<_>>();
    let root = support::eligibility::record(&corpus()["native"]["input"]["root"]).unwrap();
    assert!(p.reapply_projection(&root, 0, Uint::ONE).is_err());
    assert_eq!(p.processing_order(), before);
    assert_eq!(
        p.descending()
            .map(|(k, f)| (*k, support::digest(f)))
            .collect::<Vec<_>>(),
        snapshots
    );
}
#[test]
fn virtual_reapplication_schedule_is_bounded_cancellable_and_checks_overflow() {
    let mut s = ReapplicationSchedule::new(42).unwrap();
    for due in [42, 43, 45, 49, 57, 73, 105] {
        assert_eq!(s.due(), Some(due));
        assert!(!s.take_due(due - 1));
        assert!(s.take_due(due));
    }
    assert_eq!(s.due(), None);
    assert_eq!(s.finished_at(), 169);
    assert!(!s.take_due(1000));
    let mut s = ReapplicationSchedule::new(0).unwrap();
    s.cancel();
    assert!(s.due().is_none());
    assert!(ReapplicationSchedule::new(u64::MAX - 100).is_err());
}
