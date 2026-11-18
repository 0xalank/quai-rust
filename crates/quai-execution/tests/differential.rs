mod support;
#[test]
fn frozen_go_execution_preconditions_and_etx_state() {
    let corpus =
        serde_json::from_slice(include_bytes!("../../../fixtures/protocol/foundation.json"))
            .unwrap();
    let report = support::verify(&corpus).unwrap();
    assert_eq!(report["passed"], true, "{report}");
    assert_eq!(report["qi_input_cases"], 192);
    assert!(report["live_etx_steps"].as_u64().unwrap() > 1000);
}
#[test]
fn failed_etx_push_preserves_the_original_generation() {
    use quai_execution::etx::EtxState;
    use quai_primitives::{decode_hex, transaction::Transaction};
    let corpus: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../fixtures/protocol/foundation.json"))
            .unwrap();
    let encoded = corpus["execution"]["etx_traces"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|t| t["steps"].as_array().unwrap())
        .filter_map(|s| s["transactions"].as_array())
        .find(|v| !v.is_empty())
        .unwrap()[0]
        .as_str()
        .unwrap();
    let good = Transaction::decode(&decode_hex(encoded).unwrap()).unwrap();
    let old: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../fixtures/protocol/transactions.json"
    ))
    .unwrap();
    let wrong = old["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["type"] == 0)
        .unwrap();
    let wrong =
        Transaction::decode(&decode_hex(wrong["canonical"].as_str().unwrap()).unwrap()).unwrap();
    let mut state = EtxState::default();
    state.push(&[good.clone()]).unwrap();
    let checkpoint = state.clone();
    assert!(state.push(&[good, wrong]).is_err());
    assert_eq!(state, checkpoint);
    assert_eq!(state.root().unwrap(), checkpoint.root().unwrap());
}
#[test]
fn invalid_qi_scaling_contexts_are_rejected() {
    use quai_primitives::{decode_hex, transaction::Transaction};
    let corpus: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../fixtures/protocol/foundation.json"))
            .unwrap();
    let encoded = corpus["execution"]["qi_inputs"][0]["transaction"]
        .as_str()
        .unwrap();
    let transaction = Transaction::decode(&decode_hex(encoded).unwrap()).unwrap();
    for factor in [f64::NAN, f64::INFINITY, -1.0, 1e300] {
        assert!(quai_execution::gas::intrinsic_qi_gas(&transaction, factor).is_err());
    }
}
