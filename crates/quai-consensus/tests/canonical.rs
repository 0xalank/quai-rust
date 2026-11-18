#[path = "../examples/check-canonical.rs"]
mod check;
#[test]
fn native_admission_fork_choice_canonical_hashes_roots_and_recovery_match_go() {
    let v =
        serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json")).unwrap();
    let dir = std::env::temp_dir().join(format!(
        "quai-canonical-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let report = check::verify(&v, &dir);
    println!("CANONICAL_CHECKS {report}");
    std::fs::remove_dir_all(dir).unwrap();
}
