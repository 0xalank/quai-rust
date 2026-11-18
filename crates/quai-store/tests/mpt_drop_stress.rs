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

#[test]
fn repeated_path_copy_and_materialization_drop_safely() {
    const REPETITIONS: usize = 256;
    let fixture: Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/state-db.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    let mut updates = 0usize;
    let mut materializations = 0usize;

    for _ in 0..REPETITIONS {
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
                let actual = if case["mode"] == "hashed" {
                    trie.get_hashed(&nodes, key.as_slice().try_into().unwrap())
                        .unwrap()
                } else {
                    trie.get(&nodes, &key).unwrap()
                };
                assert_eq!(encode_hex(&actual.unwrap_or_default()), row["get"]);
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
                    materializations += 1;
                }
                updates += 1;
            }
            assert_eq!(encode_hex(&retained.unwrap().root()), case["retained_root"]);
        }
    }

    assert_eq!(updates, 387_072);
    assert_eq!(materializations, 8_448);
    println!(
        "MPT_DROP_STRESS repetitions={REPETITIONS} updates={updates} materializations={materializations}"
    );
}
