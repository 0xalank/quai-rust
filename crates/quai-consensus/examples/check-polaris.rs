#[path = "../tests/polaris_support/mod.rs"]
mod support;
fn main() {
    let p = std::env::args().nth(1).expect("VECTOR_FILE required");
    let v = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
    let r = support::check(&v);
    println!("{}", serde_json::to_string_pretty(&r).unwrap());
    if r["passed"] != true {
        std::process::exit(1)
    }
}
