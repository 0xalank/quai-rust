#[allow(dead_code)]
#[path = "../examples/check-etx-family.rs"]
mod checker;
#[test]
fn activated_all_etx_hierarchy_execution_forks_and_local_recovery_match_go() {
    let v =
        serde_json::from_str(include_str!("../../../fixtures/protocol/etx-family.json")).unwrap();
    let p = std::env::temp_dir().join(format!(
        "quai-etx-family-{}-{}",
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
    assert_eq!(
        report["external_kinds_executed"],
        serde_json::json!([0, 1, 2, 3, 4, 5, 6])
    );
    println!("ETX_FAMILY_CHECKS {report}");
    std::fs::remove_dir_all(p).unwrap();
}
