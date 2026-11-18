#[allow(dead_code)]
#[path = "../tests/vm_support/mod.rs"]
mod support;
use quai_execution::{
    evm::calls::Environment,
    settlement::{self, BlockContext},
};
use quai_primitives::{decode_hex, encode_hex, transaction::Transaction};
use serde_json::{json, Value};
use std::{hint::black_box, time::Instant};
fn main() {
    let input = std::env::args().nth(1).expect("frozen corpus");
    let corpus: Value = serde_json::from_slice(&std::fs::read(input).unwrap()).unwrap();
    let mut results = vec![];
    for prefix in ["transfer-", "storage-", "create-"] {
        let row = corpus["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| {
                r["id"].as_str().unwrap().starts_with(prefix) && r["consensus_error"].is_null()
            })
            .unwrap();
        let world = support::parse_world(row).unwrap();
        let context = support::parse_context(row).unwrap();
        let tx = Transaction::decode(&decode_hex(row["transaction"].as_str().unwrap()).unwrap())
            .unwrap();
        let block = BlockContext {
            vm: context,
            environment: Environment {
                delete_empty: row["finalize_empty"].as_bool().unwrap_or(false),
                prime_number: row["context"]["prime_number"].as_u64(),
                ..Default::default()
            },
            gas_remaining: row["initial_gas_pool"].as_u64().unwrap(),
            cumulative_gas: row["initial_cumulative_gas"].as_u64().unwrap(),
            state_used: row["initial_state_used"].as_u64().unwrap(),
        };
        let mut w = world.clone();
        let mut b = block.clone();
        let o = settlement::apply_quai(&mut w, &tx, &mut b).unwrap();
        let root = encode_hex(&w.root().unwrap());
        assert_eq!(root, row["after_root"].as_str().unwrap());
        assert_eq!(o.gas_used, row["gas_used"].as_u64().unwrap());
        let mut checksum = 0u64;
        let start = Instant::now();
        for _ in 0..1000 {
            let mut w = black_box(&world).clone();
            let mut b = block.clone();
            let o = settlement::apply_quai(&mut w, black_box(&tx), &mut b).unwrap();
            checksum = checksum.wrapping_add(o.gas_used);
            black_box(w);
        }
        results.push(json!({"id":row["id"],"ns_per_operation":start.elapsed().as_nanos()/1000,"gas_checksum":checksum,"after_root":root}));
    }
    println!("{}", json!({"iterations_per_case":1000,"cases":results}));
}
