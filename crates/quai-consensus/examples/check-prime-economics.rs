#[path = "../tests/prime_economics_support/mod.rs"]
mod support;
fn main() {
    let path = std::env::args().nth(1).expect("VECTOR_FILE required");
    let v = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    println!("{}", support::verify(&v));
}
