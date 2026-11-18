//! Write the Go-encoded fixture chain (fixtures/protocol/canonical.json,
//! branch 0: genesis plus 16 heights) as a QEX1 export with environment
//! `local`, for smoke tests of `quai-node replay`:
//!
//!     cargo run --release -p quai-node --example fixture-export -- OUT_DIR
//!     quai-node replay --export OUT_DIR
use quai_node::qex::{Context, ExportWriter, RawRecord};
use quai_primitives::{decode_hex, Hash};

fn hash(v: &serde_json::Value) -> Hash {
    decode_hex(v.as_str().unwrap()).unwrap().try_into().unwrap()
}
fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("usage: fixture-export OUT_DIR");
    let out = std::path::Path::new(&out);
    if out.join("manifest.json").exists() {
        eprintln!("{} already holds an export", out.display());
        std::process::exit(2);
    }
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json")).unwrap();
    let branch = &corpus["branches"][0];
    let event = corpus["canonical_events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["branch"] == 0 && e["selected"] == true && e["head_number"] == 16)
        .unwrap();
    let go: Vec<Hash> = event["canonical_hashes"]
        .as_array()
        .unwrap()
        .iter()
        .map(hash)
        .collect();
    let genesis = decode_hex(branch["genesis"].as_str().unwrap()).unwrap();
    let mut w = ExportWriter::new(out, "local", go[0], false);
    w.push(
        &[Context::Prime, Context::Region, Context::Zone].map(|c| RawRecord {
            context: c,
            number: 0,
            hash: go[0],
            block: genesis.clone(),
            receipts: vec![],
        }),
    )
    .unwrap();
    let mut numbers = [0u64; 3];
    for (i, v) in branch["vectors"].as_array().unwrap().iter().enumerate() {
        let records: Vec<RawRecord> = v["contexts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let ctx = Context::from_u8(c["context"].as_u64().unwrap() as u8).unwrap();
                numbers[ctx.index()] += 1;
                RawRecord {
                    context: ctx,
                    number: numbers[ctx.index()],
                    hash: go[i + 1],
                    block: decode_hex(c["work_object"].as_str().unwrap()).unwrap(),
                    receipts: vec![],
                }
            })
            .collect();
        w.push(&records).unwrap();
    }
    let manifest = w.finish().unwrap();
    println!("{}", manifest.display());
}
