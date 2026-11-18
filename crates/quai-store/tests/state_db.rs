#[path = "../examples/check-state-db.rs"]
mod check;
#[test]
fn persisted_incremental_tries_large_state_and_long_history_match_actual_go() {
    let v = serde_json::from_str(include_str!("../../../fixtures/protocol/state-db.json")).unwrap();
    let d = std::env::temp_dir().join(format!(
        "quai-state-db-go-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&d).unwrap();
    let r = check::verify(&v, &d.join("state.redb"));
    println!("STATE_DB_CHECKS {r}");
    std::fs::remove_dir_all(d).unwrap();
}
