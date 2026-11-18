#[path = "../examples/check-confirmation.rs"]
mod check;
#[test]
fn native_hierarchy_rollups_incoming_execution_and_recovery_match_actual_go() {
    let c =
        serde_json::from_str(include_str!("../../../fixtures/protocol/confirmation.json")).unwrap();
    let d = std::env::temp_dir().join(format!(
        "quai-confirmation-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&d).unwrap();
    let r = check::verify(&c, &d);
    assert_eq!(r["passed"], true);
    assert_eq!(r["native_context_components"], 38);
    assert_eq!(r["executed_inbound_etxs"], 16);
    println!("CONFIRMATION_CHECKS {r}");
    std::fs::remove_dir_all(d).unwrap();
}
