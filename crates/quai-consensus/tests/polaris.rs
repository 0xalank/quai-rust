mod polaris_support;
use quai_consensus::{history::Record, placement, rules::Uint, verifier::Verifier, Error};
use serde_json::Value;
fn corpus() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/polaris.json")).unwrap()
}
#[test]
fn frozen_upstream_polaris_bindings_identities_and_inclusion_gates_match() {
    let r = polaris_support::check(&corpus());
    assert_eq!(r["passed"], true, "{}", r["failures"]);
    assert_eq!(r["checks"], 198);
    assert_eq!(r["reference_revision"], quai_primitives::REFERENCE_REVISION);
}
#[test]
fn polaris_identity_deduplication_uses_prime_activation_height() {
    let v = corpus();
    for row in v["inclusions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["variant"] == 1)
    {
        assert_eq!(
            row["kind"] == "duplicate",
            row["prime"].as_u64().unwrap() >= quai_primitives::spec::FORKS.polaris
        );
    }
}
#[test]
fn trusted_zero_number_cannot_bypass_body_binding_before_order() {
    let v = corpus();
    for row in v["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["zero"] == true && r["id"].as_str().unwrap().ends_with("/1"))
    {
        let r =
            Record::decode(&quai_primitives::decode_hex(row["block"].as_str().unwrap()).unwrap())
                .unwrap();
        let p = placement::Profile {
            location: vec![0, 0],
            genesis: [r.id()].into_iter().collect(),
            default_genesis: Some(r.id()),
            minimum_difficulty: Uint::ONE,
            duration: 5,
            gas_ceiling: 50_000_000,
            p2p_threshold: 7,
        };
        assert!(matches!(
            placement::verify_placement(&mut Verifier::default(), &r, &p),
            Err(Error::Invalid("body-header commitment"))
        ));
    }
}
