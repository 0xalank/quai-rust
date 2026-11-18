#[allow(dead_code)]
#[path = "eligibility_support/mod.rs"]
mod support;
use quai_consensus::{
    eligibility::{Constraints, CoordinatorView},
    verifier::Verifier,
    Error,
};
use serde_json::Value;
use std::collections::BTreeMap;
fn corpus() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/eligibility.json")).unwrap()
}
#[test]
fn frozen_coordinator_ancestry_constraints_and_pcrc_match_go() {
    let r = support::policy(&corpus()).unwrap();
    assert_eq!(r["mismatches"], 0, "{}", r["failures"]);
    assert_eq!(r["checks"], 13758);
}
#[test]
fn self_ancestry_is_equality_without_implying_verified_work() {
    let view = CoordinatorView::new(false);
    assert!(view.is_ancestor([9; 32], [9; 32], &[0, 0], 2).unwrap());
    assert!(view.is_ancestor([9; 32], [9; 32], &[16, 0], 2).is_err());
    assert!(matches!(
        view.is_ancestor([8; 32], [9; 32], &[0, 0], 2),
        Err(Error::Unavailable(_))
    ));
}
#[test]
fn missing_middle_ancestry_is_unavailable() {
    let c = corpus();
    let d = &c["policy"]["datasets"][0];
    let e = d["entries"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e["location"] == serde_json::json!([0, 0]))
        .unwrap();
    let r = support::record(&e["block"]).unwrap();
    let mut v = CoordinatorView::new(false);
    v.register(support::profile(
        vec![0, 0],
        support::hash(&d["genesis"]).unwrap(),
    ))
    .unwrap();
    v.import_unverified(&[0, 0], r.clone(), None).unwrap();
    assert!(matches!(
        v.is_ancestor([0x99; 32], r.id(), &[0, 0], 2),
        Err(Error::Unavailable(_))
    ));
    // A declared anchor may have imported predecessors in an expanded context.
    // Go IsAncestor continues through them rather than treating the anchor as a cut.
    let zone = d["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["location"] == serde_json::json!([0, 0]))
        .map(|e| support::record(&e["block"]).unwrap())
        .collect::<Vec<_>>();
    let mut complete = CoordinatorView::new(false);
    complete
        .register(support::profile(vec![0, 0], zone[3].id()))
        .unwrap();
    for r in &zone {
        complete
            .import_unverified(&[0, 0], r.clone(), None)
            .unwrap();
    }
    assert!(complete
        .is_ancestor(zone[0].id(), zone[4].id(), &[0, 0], 2)
        .unwrap());
}
#[test]
fn duplicate_imports_are_idempotent_and_conflicts_are_atomic() {
    let c = corpus();
    let d = &c["policy"]["datasets"][0];
    let mut v = support::make_policy_view(d).unwrap();
    let e = &d["entries"][0];
    let l = support::location(&e["location"]);
    let r = support::record(&e["block"]).unwrap();
    let mut t = support::term(&e["termini"]).unwrap();
    let before = (v.len(), v.tracked_bytes());
    v.import_unverified(&l, r.clone(), Some(t.clone())).unwrap();
    assert_eq!(before, (v.len(), v.tracked_bytes()));
    t.dominant[0] = [0xaa; 32];
    assert!(matches!(
        v.import_unverified(&l, r, Some(t)),
        Err(Error::Invalid(_))
    ));
    assert_eq!(before, (v.len(), v.tracked_bytes()));
}
#[test]
fn registered_profiles_are_immutable_and_legacy_aliases_are_explicit() {
    let mut v = CoordinatorView::new(false);
    let mut p = support::profile(vec![0], [1; 32]);
    v.register(p.clone()).unwrap();
    v.register(p.clone()).unwrap();
    p.duration += 1;
    assert!(matches!(v.register(p), Err(Error::Invalid(_))));
    assert!(v.register(support::profile(vec![4], [1; 32])).is_err());
    let mut legacy = CoordinatorView::new(true);
    assert!(legacy
        .register(support::profile(vec![15], [1; 32]))
        .is_err());
    legacy.register(support::profile(vec![3], [1; 32])).unwrap();
}
#[test]
fn legacy_pcrc_uses_physical_index_in_the_aliased_slice() {
    let c = corpus();
    let d = c["policy"]["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["location"] == serde_json::json!([15, 15]))
        .unwrap();
    let v = support::make_policy_view(d).unwrap();
    let e = d["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["location"] == serde_json::json!([3, 15]))
        .unwrap();
    let t = support::term(&e["termini"]).unwrap();
    let id = support::record(&e["block"]).unwrap().id();
    assert!(v
        .previous_coincident_matches(&[15, 15], id, t.dominant[15])
        .unwrap());
    assert!(!v
        .previous_coincident_matches(&[15, 15], id, t.dominant[3])
        .unwrap());
}
#[test]
fn imported_frontier_checks_reject_mixed_branches_without_mutation() {
    let c = corpus();
    let v = support::native_view(&c).unwrap();
    let gen = support::hash(&c["native"]["genesis"]).unwrap();
    let a = support::record(&c["native"]["steps"][0]["block"])
        .unwrap()
        .id();
    let b = support::record(&c["native"]["steps"][1]["block"])
        .unwrap()
        .id();
    let f = support::selected_frontier(&c, [a; 3]).unwrap();
    let before = f.clone();
    let report = v.check_frontier(&f).unwrap();
    assert_eq!(report.checked_edges, 2);
    assert!(!report.fully_validated && !report.history_verified);
    assert_eq!(f, before);
    assert!(matches!(
        v.check_frontier(&support::selected_frontier(&c, [a, b, gen]).unwrap()),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        CoordinatorView::new(false).check_frontier(&f),
        Err(Error::Unavailable(_))
    ));
}
#[test]
fn completing_constraints_preserves_inputs_and_rejects_alias_conflicts() {
    let v = CoordinatorView::new(true);
    let input: Constraints = [(vec![15, 15], [1; 32])].into_iter().collect();
    let before = input.clone();
    let full = v.complete_constraints(&input, 30, [2; 32]).unwrap();
    assert_eq!(full.len(), 69);
    assert_eq!(full[&vec![3, 15]], [1; 32]);
    assert_eq!(input, before);
    let conflict = [(vec![3, 15], [2; 32]), (vec![15, 15], [1; 32])]
        .into_iter()
        .collect();
    assert!(matches!(
        v.normalize_constraints(&conflict),
        Err(Error::Invalid(_))
    ));
    assert!(CoordinatorView::new(false)
        .complete_constraints(&Constraints::new(), 30, [2; 32])
        .is_err());
}
#[test]
fn failed_constraint_trace_preserves_the_caller_input() {
    let c = corpus();
    let row = c["policy"]["traces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["valid"] == false)
        .unwrap();
    let d = &c["policy"]["datasets"][row["dataset"].as_u64().unwrap() as usize];
    let v = support::make_policy_view(d).unwrap();
    let orders: BTreeMap<_, _> = d["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                support::record(&e["block"]).unwrap().id(),
                e["order"].as_u64().unwrap() as u8,
            )
        })
        .collect();
    let input = support::constraints(&row["constraints"]).unwrap();
    let before = input.clone();
    assert!(v
        .trace_projection(
            &input,
            support::hash(&row["leader"]).unwrap(),
            &support::location(&row["leader_location"]),
            row["first"].as_bool().unwrap(),
            &orders
        )
        .is_err());
    assert_eq!(input, before);
}
#[test]
fn declared_genesis_trace_does_not_invent_work_authority() {
    let c = corpus();
    let v = support::native_view(&c).unwrap();
    let gen = support::hash(&c["native"]["genesis"]).unwrap();
    let input = [(vec![], [3; 32])].into_iter().collect();
    assert_eq!(
        v.trace_native(&mut Verifier::default(), &input, gen, &[0, 0], false)
            .unwrap(),
        input
    );
}
