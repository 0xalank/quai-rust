mod cubic_support;
#[test]
fn official_conversion_cubic_binary_rounding_matches_frozen_go() {
    let corpus = serde_json::from_str(include_str!(
        "../../../fixtures/protocol/cubic-discount.json"
    ))
    .unwrap();
    let result = cubic_support::verify(&corpus);
    println!("{result}");
}
