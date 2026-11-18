#[path = "../tests/aux_support/mod.rs"]
mod support;
fn main() {
    let path = std::env::args().nth(1).expect("VECTOR_FILE required");
    let v = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let proofs = std::env::args().nth(2).as_deref() != Some("--bindings-only");
    match support::verify(&v, proofs) {
        Ok(r) => {
            println!("{}", serde_json::to_string_pretty(&r).unwrap());
            if r["passed"] != true {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    }
}
