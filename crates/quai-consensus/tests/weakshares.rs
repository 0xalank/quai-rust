#[path = "../examples/check-weakshares.rs"]
mod check;
#[test]
fn native_weak_shares_poem_rewards_confirmation_and_recovery_match_actual_go() {
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/weakshares.json")).unwrap();
    let dir = std::env::temp_dir().join(format!(
        "quai-weak-shares-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let report = check::verify(&corpus, &dir);
    assert_eq!(report["unique_native_blocks"], 24);
    assert_eq!(report["weak_shares"], 10);
    assert_eq!(report["included_shares"], 23);
    assert_eq!(report["executed_inbound_etxs"], 30);
    assert_eq!(report["native_candidate_scores"], 15);
    println!("WEAKSHARES_CHECKS {report}");
    std::fs::remove_dir_all(dir).unwrap();
}
