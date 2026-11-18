#[path = "../tests/choice_window_support/mod.rs"]
mod support;
fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 3);
    let v = serde_json::from_slice(&std::fs::read(&args[1]).unwrap()).unwrap();
    let dir = std::path::Path::new(&args[2]);
    assert!(dir.is_dir());
    println!("{}", support::verify(&v, dir));
}
