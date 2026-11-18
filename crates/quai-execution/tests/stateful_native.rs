#[allow(dead_code)]
#[path = "../examples/check-early-blocks.rs"]
mod blocks;
#[allow(dead_code)]
#[path = "../examples/check-qi-settlement.rs"]
mod qi;
#[allow(dead_code)]
#[path = "../examples/check-settlement.rs"]
mod settlement;
#[allow(dead_code)]
#[path = "../examples/check-stateful.rs"]
mod stateful;
fn passed(result: serde_json::Value) {
    let failures = result["failures"]
        .as_array()
        .unwrap()
        .iter()
        .take(3)
        .map(|v| (&v["id"], &v["field"], &v["error"]))
        .collect::<Vec<_>>();
    assert_eq!(
        result["passed"], true,
        "differential mismatches: {failures:?}"
    );
}
#[test]
fn nested_calls_creation_precompiles_and_etx_match_actual_go() {
    let input =
        serde_json::from_slice(include_bytes!("../../../fixtures/protocol/stateful.json")).unwrap();
    passed(stateful::verify(&input));
}
#[test]
fn signed_quai_settlement_and_transaction_finalization_match_actual_go() {
    let input =
        serde_json::from_slice(include_bytes!("../../../fixtures/protocol/settlement.json"))
            .unwrap();
    passed(settlement::verify(&input));
}
#[test]
fn qi_settlement_utxo_commitments_and_outbound_etxs_match_actual_go() {
    let input = serde_json::from_slice(include_bytes!(
        "../../../fixtures/protocol/qi-settlement.json"
    ))
    .unwrap();
    passed(qi::verify(&input));
}
#[test]
fn linked_early_blocks_match_go_state_processor_and_reject_forged_counters() {
    let input = serde_json::from_slice(include_bytes!(
        "../../../fixtures/protocol/early-blocks.json"
    ))
    .unwrap();
    passed(blocks::verify(&input));
}

#[allow(dead_code)]
#[path = "../examples/check-incoming.rs"]
mod incoming;
#[test]
fn incoming_dispatch_and_block_sizing_match_extracted_go_process() {
    let input =
        serde_json::from_slice(include_bytes!("../../../fixtures/protocol/incoming.json")).unwrap();
    passed(incoming::verify(&input));
}
#[allow(dead_code)]
#[path = "../examples/check-historical.rs"]
mod historical;
#[test]
fn allocations_redemptions_and_historical_trims_match_go() {
    let input =
        serde_json::from_slice(include_bytes!("../../../fixtures/protocol/historical.json"))
            .unwrap();
    passed(historical::verify(&input));
}
