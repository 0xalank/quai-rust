#[path = "../tests/prime_forward_support/mod.rs"]
mod support;
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    assert_eq!(args.len(), 3, "VECTOR_FILE DIRECTORY required");
    let v = serde_json::from_slice(&std::fs::read(&args[1]).unwrap()).unwrap();
    let dir = std::path::Path::new(&args[2]);
    std::fs::create_dir_all(dir).unwrap();
    println!("{}", support::verify(&v, dir));
}
