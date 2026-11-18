#[path = "../examples/check-admission.rs"]
mod check;
#[test]
fn native_local_headers_execution_and_recovery_match_actual_go_validation() {
    let c =
        serde_json::from_str(include_str!("../../../fixtures/protocol/admission.json")).unwrap();
    let d = std::env::temp_dir().join(format!(
        "quai-admission-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&d).unwrap();
    let r = check::verify(&c, &d);
    assert_eq!(r["passed"], true);
    assert_eq!(r["valid_work_forgeries_rejected"], 31);
    println!("ADMISSION_CHECKS {r}");
    std::fs::remove_dir_all(d).unwrap();
}
