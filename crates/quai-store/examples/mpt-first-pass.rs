//! Heap-crash reproducer: parse the state-db fixture and run the trie pass
//! of `tests/mpt_drop_stress.rs` `MPT_REPS` times (default 1) on a spawned
//! thread, as the test harness does. A short process gives many early-life
//! allocator windows per hour.
use quai_primitives::{
    decode_hex, encode_hex,
    mpt::{self, ReadNodes, Trie},
    Hash,
};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default)]
struct Nodes(BTreeMap<Hash, Vec<u8>>);

impl ReadNodes for Nodes {
    fn get_node(&self, hash: &Hash) -> Result<Option<Vec<u8>>, mpt::Error> {
        Ok(self.0.get(hash).cloned())
    }
}

fn bytes(value: &Value) -> Vec<u8> {
    decode_hex(value.as_str().unwrap()).unwrap()
}

fn pass(cases: &[Value]) -> usize {
    let mut updates = 0;
    let mut nodes = Nodes::default();
    for case in cases {
        let mut trie = Trie::default();
        let mut retained = None;
        for (index, row) in case["steps"].as_array().unwrap().iter().enumerate() {
            let key = bytes(&row["key"]);
            let value = bytes(&row["value"]);
            if case["mode"] == "hashed" {
                trie.update_hashed(&nodes, key.as_slice().try_into().unwrap(), &value)
                    .unwrap();
            } else {
                trie.update(&nodes, &key, &value).unwrap();
            }
            assert_eq!(encode_hex(&trie.root()), row["root"]);
            if index as u64 == case["retained_at"].as_u64().unwrap() {
                retained = Some(trie.clone());
            }
            if index % 47 == 0 {
                trie.visit_new_nodes(&mut |hash, encoded| {
                    nodes.0.insert(hash, encoded.to_vec());
                    Ok(())
                })
                .unwrap();
                trie = Trie::from_root(trie.root());
            }
            updates += 1;
        }
        assert_eq!(encode_hex(&retained.unwrap().root()), case["retained_root"]);
    }
    updates
}

fn main() {
    let reps: usize = std::env::var("MPT_REPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let worker = std::thread::Builder::new()
        .name("first-pass".into())
        .spawn(move || {
            let fixture: Value =
                serde_json::from_str(include_str!("../../../fixtures/protocol/state-db.json"))
                    .unwrap();
            let cases = fixture["cases"].as_array().unwrap();
            (0..reps).map(|_| pass(cases)).sum::<usize>()
        })
        .unwrap();
    let updates = worker.join().unwrap();
    println!("MPT_FIRST_PASS reps={reps} updates={updates}");
}
