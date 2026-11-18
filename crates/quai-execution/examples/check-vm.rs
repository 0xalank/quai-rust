//! Explicit offline VM fixture checker, excluded from the shipping distribution.
#[path = "../tests/vm_support/mod.rs"]
mod support;
fn main() {
    let result = std::env::args()
        .nth(1)
        .ok_or_else(|| "VECTOR_FILE".to_string())
        .and_then(|p| std::fs::read(p).map_err(|e| e.to_string()))
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
        .and_then(|v| support::verify(&v));
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
