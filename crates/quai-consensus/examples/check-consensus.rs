#[path = "../tests/support/mod.rs"]
mod support;
fn main() {
    let path = std::env::args().nth(1).expect("VECTOR_FILE required");
    let v = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let mode = std::env::args().nth(2).unwrap_or_else(|| "all".into());
    let rules = support::verify_rules(&v).unwrap();
    if mode == "--rules" {
        println!("{}", serde_json::to_string_pretty(&rules).unwrap());
        if rules["passed"] != true {
            std::process::exit(1);
        }
        return;
    }
    match support::verify_pow(&v) {
        Ok(pow) => {
            let r = serde_json::json!({"passed":pow["passed"]==true&&rules["passed"]==true,"pow":pow,"rules":rules});
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
