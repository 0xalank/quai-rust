#[allow(dead_code)]
#[path = "../examples/check-coincident.rs"]
mod checker;
#[test]
fn activated_coincident_hierarchy_execution_forks_and_local_recovery_match_go() {
    let v =
        serde_json::from_str(include_str!("../../../fixtures/protocol/coincident.json")).unwrap();
    let p = std::env::temp_dir().join(format!(
        "quai-coincident-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&p).unwrap();
    let report = checker::verify(&v, &p);
    assert_eq!(report["passed"], true);
    assert_eq!(report["rejected_attempts"], 492);
    println!("COINCIDENT_CHECKS {report}");
    std::fs::remove_dir_all(p).unwrap();
}
