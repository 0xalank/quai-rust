use quai_consensus::{rules::Uint, weak_float};
use serde_json::Value;
#[test]
fn weak_share_binary_rounding_matches_go_cubic_and_precision_boundaries() {
    let corpus: Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/weakshares.json")).unwrap();
    let mut count = 0;
    for row in corpus["arithmetic"].as_array().unwrap() {
        let deficit = Uint::from_u128(row["deficit"].as_str().unwrap().parse().unwrap());
        let expected = Uint::from_u128(row["weight"].as_str().unwrap().parse().unwrap());
        let got = weak_float::discount(deficit, row["distance"].as_u64().unwrap()).unwrap();
        assert_eq!(
            got, expected,
            "deficit={} distance={}",
            row["deficit"], row["distance"]
        );
        count += 1;
    }
    assert!(count > 5000);
    assert!(weak_float::discount(Uint::from_u64(257).shl_vartime(64), 0).is_err());
    assert!(weak_float::discount(Uint::ZERO, 5).is_err());
    println!("WEAK_FLOAT_CHECKS {}", count + 2);
}
