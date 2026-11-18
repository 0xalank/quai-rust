//! Offline-only fixture checker, excluded from the standalone distribution.
#[path = "../tests/support/mod.rs"]
mod support;
fn main() {
    let result = std::env::args()
        .nth(1)
        .ok_or_else(|| "VECTOR_FILE".to_string())
        .and_then(|path| std::fs::read(path).map_err(|e| e.to_string()))
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
        .and_then(|corpus| support::verify(&corpus));
    match result {
        Ok(report) => {
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            if report["passed"] != true {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
