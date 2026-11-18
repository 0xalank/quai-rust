mod prime_forward_support;
#[test]
fn official_prime_forward_quotes_refunds_linked_windows_and_atomic_recovery_match_go() {
    let v = serde_json::from_str(include_str!(
        "../../../fixtures/protocol/prime-forward.json"
    ))
    .unwrap();
    let dir = std::env::temp_dir().join(format!(
        "quai-prime-forward-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    assert!(!dir.exists());
    std::fs::create_dir(&dir).unwrap();
    println!("{}", prime_forward_support::verify(&v, &dir));
    std::fs::remove_dir_all(dir).unwrap();
}
