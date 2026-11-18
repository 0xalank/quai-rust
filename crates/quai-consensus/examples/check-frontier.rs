#[path = "../tests/frontier_support/mod.rs"]
mod support;
fn main() {
    let p = std::env::args().nth(1).expect("VECTOR_FILE required");
    let v = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
    let result = (|| -> Result<serde_json::Value, String> {
        let policy = support::policy(&v)?;
        let native = support::native(&v)?;
        Ok(
            serde_json::json!({"checks":policy["checks"].as_u64().unwrap()+native["checks"].as_u64().unwrap(),"passed":policy["passed"]==true&&native["passed"]==true,"policy":policy,"native":native,"full_consensus":false}),
        )
    })();
    match result {
        Ok(r) => {
            println!("{}", serde_json::to_string_pretty(&r).unwrap());
            if r["passed"] != true {
                std::process::exit(1)
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2)
        }
    }
}
