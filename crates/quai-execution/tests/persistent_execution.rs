#[allow(dead_code)]
#[path = "../examples/check-persistent-execution.rs"]
mod checker;
#[test]
fn persisted_quai_qi_and_incoming_transitions_keep_frozen_go_results() {
    let path = std::env::temp_dir().join(format!(
        "quai-persisted-execution-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    let report = checker::verify(&path);
    println!("PERSISTED_EXECUTION_CHECKS {report}");
    assert_eq!(report["passed"], true);
    std::fs::remove_dir_all(path).unwrap();
}
