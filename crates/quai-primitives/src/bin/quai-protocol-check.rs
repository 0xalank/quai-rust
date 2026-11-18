fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 1 {
        eprintln!("quai-protocol-check VECTOR_FILE");
        std::process::exit(2);
    }
    let result = std::fs::read(&args[0])
        .map_err(|e| e.to_string())
        .and_then(|bytes| quai_primitives::vectors::verify(&bytes));
    match result {
        Ok(report) => {
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            if report["passed"] != true {
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
