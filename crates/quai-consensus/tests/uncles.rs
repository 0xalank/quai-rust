#[path = "../examples/check-uncles.rs"]
mod check;
#[test]
fn native_strong_uncles_entropy_rewards_and_confirmation_match_actual_go() {
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/uncles.json")).unwrap();
    let path = std::env::temp_dir().join(format!(
        "quai-uncles-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    let report = check::verify(&corpus, &path);
    assert_eq!(report["unique_native_blocks"], 24);
    assert_eq!(report["native_context_components"], 38);
    assert_eq!(report["executed_inbound_etxs"], 24);
    assert_eq!(report["included_full_target_uncles"], 13);
    println!("UNCLES_CHECKS {report}");
    std::fs::remove_dir_all(path).unwrap();
}
