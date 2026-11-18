#[path = "../tests/cubic_support/mod.rs"]
mod support;
fn main() {
    let path = std::env::args().nth(1).expect("VECTOR_FILE required");
    let value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    println!("{}", support::verify(&value));
}
