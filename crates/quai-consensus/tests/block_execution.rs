#[allow(dead_code)]
#[path = "../examples/check-zone-blocks.rs"]
mod blocks;
#[allow(dead_code)]
#[path = "../examples/check-payouts.rs"]
mod payouts;
fn passed(result: serde_json::Value) {
    let failures = result["failures"]
        .as_array()
        .unwrap()
        .iter()
        .take(3)
        .collect::<Vec<_>>();
    assert_eq!(result["passed"], true, "{failures:?}");
}
#[test]
fn fee_feedback_and_workshare_payouts_match_actual_go_branch() {
    let v =
        serde_json::from_slice(include_bytes!("../../../fixtures/protocol/payouts.json")).unwrap();
    passed(payouts::verify(&v));
}
#[test]
fn whole_zone_replay_with_native_work_and_restored_state_matches_go() {
    let v = serde_json::from_slice(include_bytes!(
        "../../../fixtures/protocol/zone-blocks.json"
    ))
    .unwrap();
    passed(blocks::verify(&v));
}
