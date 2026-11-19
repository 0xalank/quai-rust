use quai_primitives::{Header, Schedule};
#[test]
fn frozen_go_header_and_threshold_vectors_match() {
    let report = quai_primitives::vectors::verify(include_bytes!(
        "../../../fixtures/protocol/work-headers.json"
    ))
    .unwrap();
    assert_eq!(report["passed"], true, "{report}");
    assert!(report["checks"].as_u64().unwrap() > 1000);
    assert_eq!(report["aux_shape_vectors"], 75);
}
#[test]
fn malformed_and_oversized_wire_data_is_rejected() {
    let schedule = Schedule {
        kawpow_fork: 1,
        transition_period: 1,
    };
    for bytes in [vec![], vec![255], vec![0; 65537]] {
        assert!(Header::decode(&bytes, schedule).is_err());
    }
}
