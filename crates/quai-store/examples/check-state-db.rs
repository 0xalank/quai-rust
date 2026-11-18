use quai_primitives::{
    decode_hex, encode_hex,
    mpt::{self, ReadNodes, Trie},
    Hash,
};
use quai_store::state_db::StateDatabase;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};
#[derive(Default)]
struct Nodes(BTreeMap<Hash, Vec<u8>>);
impl ReadNodes for Nodes {
    fn get_node(&self, h: &Hash) -> Result<Option<Vec<u8>>, mpt::Error> {
        Ok(self.0.get(h).cloned())
    }
}
fn bytes(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
fn check(ok: bool, n: &mut usize, why: &str) {
    *n += 1;
    assert!(ok, "{why}")
}
fn id(tag: &str, n: u64) -> Hash {
    let mut h = Sha256::new();
    h.update(tag);
    h.update(n.to_be_bytes());
    h.finalize().into()
}
fn key(n: u64) -> [u8; 8] {
    n.to_be_bytes()
}
fn value(n: u64) -> Vec<u8> {
    let mut v = key(n).to_vec();
    v.extend(key(n ^ 0x5a5a5a5a5a5a5a5a));
    v
}
pub fn verify(v: &Value, path: &Path) -> Value {
    let mut checks = 0;
    let mut nodes = Nodes::default();
    let mut steps = 0;
    for case in v["cases"].as_array().unwrap() {
        let mut trie = Trie::default();
        let mut retained = None;
        for (i, row) in case["steps"].as_array().unwrap().iter().enumerate() {
            let k = bytes(&row["key"]);
            let val = bytes(&row["value"]);
            if case["mode"] == "hashed" {
                trie.update_hashed(&nodes, k.as_slice().try_into().unwrap(), &val)
                    .unwrap();
            } else {
                trie.update(&nodes, &k, &val).unwrap();
            }
            check(
                encode_hex(&trie.root()) == row["root"],
                &mut checks,
                "native incremental root equals actual Go update/delete",
            );
            let got = if case["mode"] == "hashed" {
                trie.get_hashed(&nodes, k.as_slice().try_into().unwrap())
                    .unwrap()
            } else {
                trie.get(&nodes, &k).unwrap()
            };
            check(
                encode_hex(&got.unwrap_or_default()) == row["get"],
                &mut checks,
                "actual Go lookup",
            );
            if i as u64 == case["retained_at"].as_u64().unwrap() {
                retained = Some(trie.clone());
            }
            if i % 47 == 0 {
                trie.visit_new_nodes(&mut |h, b| {
                    nodes.0.insert(h, b.to_vec());
                    Ok(())
                })
                .unwrap();
                trie = Trie::from_root(trie.root());
            }
            steps += 1;
        }
        check(
            encode_hex(&retained.unwrap().root()) == case["retained_root"],
            &mut checks,
            "pinned Go native trie root",
        );
    }
    let profile = id("state-db-profile", 0);
    let db = StateDatabase::open(path, profile).unwrap();
    let g = db
        .commit(None, None, id("genesis", 0), b"empty", true, |_| Ok(()))
        .unwrap()
        .version
        .id;
    let mut head = g;
    let mut retained = None;
    let secure = v["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["mode"] == "secure")
        .unwrap();
    let rows = secure["steps"].as_array().unwrap();
    for (start, chunk) in rows.chunks(25).enumerate() {
        let done = db
            .commit(
                Some(head),
                Some(head),
                id("secure", start as u64),
                b"go-mutations",
                true,
                |w| {
                    for row in chunk {
                        let k = bytes(&row["key"]);
                        w.put(b"secure", &k, &bytes(&row["value"]))?;
                        check(
                            encode_hex(&w.root(b"secure")) == row["root"],
                            &mut checks,
                            "transactional root equals Go",
                        );
                        check(
                            encode_hex(&w.get(b"secure", &k)?.unwrap_or_default()) == row["get"],
                            &mut checks,
                            "transactional lookup equals Go",
                        );
                    }
                    Ok(())
                },
            )
            .unwrap();
        head = done.version.id;
        if (start + 1) * 25 == 750 {
            retained = Some(db.snapshot(Some(head)).unwrap());
        }
    }
    check(
        encode_hex(&retained.as_ref().unwrap().root(b"secure")) == secure["retained_root"],
        &mut checks,
        "database pinned earlier Go root",
    );
    drop(retained);
    let bulk = &v["bulk"];
    let count = bulk["count"].as_u64().unwrap();
    let big = db
        .commit(
            Some(head),
            Some(head),
            id("bulk", 0),
            b"seventy-thousand-keys",
            true,
            |w| {
                for i in 0..count {
                    w.put(b"bulk", &key(i), &value(i))?;
                    if (i + 1) % 5000 == 0 {
                        let expected = &bulk["checkpoints"][(i / 5000) as usize]["root"];
                        check(
                            encode_hex(&w.root(b"bulk")) == *expected,
                            &mut checks,
                            "bulk Go root beyond former account bound",
                        );
                    }
                }
                Ok(())
            },
        )
        .unwrap();
    head = big.version.id;
    let base = db.snapshot(Some(head)).unwrap();
    check(
        encode_hex(&base.root(b"bulk")) == bulk["base_root"],
        &mut checks,
        "persisted bulk root equals Go",
    );
    let changed = db
        .commit(
            Some(head),
            Some(head),
            id("bulk", 1),
            b"one-hundred-changes",
            true,
            |w| {
                for i in 0u64..100 {
                    let next = if i % 3 == 0 {
                        vec![]
                    } else {
                        value(100000 + i)
                    };
                    w.put(b"bulk", &key(i * 677), &next)?;
                }
                Ok(())
            },
        )
        .unwrap();
    head = changed.version.id;
    check(
        changed.new_nodes.nodes <= 6500,
        &mut checks,
        "updates persist a bounded changed path",
    );
    let current = db.snapshot(None).unwrap();
    check(
        encode_hex(&current.root(b"bulk")) == bulk["changed_root"],
        &mut checks,
        "changed persisted root equals Go",
    );
    for q in bulk["queries"].as_array().unwrap() {
        let k = bytes(&q["key"]);
        check(
            encode_hex(&base.get(b"bulk", &k).unwrap().unwrap_or_default()) == q["base"],
            &mut checks,
            "pinned bulk value equals Go",
        );
        check(
            encode_hex(&current.get(b"bulk", &k).unwrap().unwrap_or_default()) == q["changed"],
            &mut checks,
            "changed bulk value equals Go",
        );
    }
    drop(base);
    drop(current);
    let mut saved = vec![];
    for i in 0u64..5000 {
        let next = db
            .commit(
                Some(head),
                Some(head),
                id("history", i),
                b"incremental-version",
                true,
                |w| w.put(b"history", &key(i % 256), &value(i)),
            )
            .unwrap();
        head = next.version.id;
        if (i + 1) % 500 == 0 {
            let row = &v["history"][(i / 500) as usize];
            check(
                encode_hex(&next.version.roots[b"history".as_slice()]) == row["root"],
                &mut checks,
                "long version history root equals Go",
            );
            saved.push((head, row.clone()));
        }
    }
    let final_height = db.snapshot(None).unwrap().version().height;
    check(
        final_height > 4096,
        &mut checks,
        "state versions exceed old history bound",
    );
    drop(db);
    let db = StateDatabase::open(path, profile).unwrap();
    check(
        db.head().unwrap() == Some(head),
        &mut checks,
        "incremental head reopens without a history scan",
    );
    let tip = db.snapshot(None).unwrap();
    check(
        tip.metadata().unwrap() == b"incremental-version",
        &mut checks,
        "metadata payload digest after reopen",
    );
    check(
        tip.ancestor(0).unwrap().id == g,
        &mut checks,
        "persisted binary index reaches genesis",
    );
    check(
        tip.ancestor(final_height).unwrap().id == head,
        &mut checks,
        "binary index preserves selected generation",
    );
    check(
        tip.ancestor(final_height + 1).is_err(),
        &mut checks,
        "future version is unavailable",
    );
    for (state, row) in saved {
        let s = db.snapshot(Some(state)).unwrap();
        check(
            tip.ancestor(s.version().height).unwrap().id == state,
            &mut checks,
            "binary height lookup equals persisted version",
        );
        check(
            encode_hex(&s.root(b"history")) == row["root"],
            &mut checks,
            "historical root survives reopen",
        );
        check(
            s.get(b"history", &bytes(&row["key"])).unwrap() == Some(bytes(&row["value"])),
            &mut checks,
            "historical Go value survives reopen",
        );
    }
    json!({"passed":true,"checks":checks,"go_mutation_steps":steps,"persisted_secure_steps":rows.len(),"bulk_keys":count,"state_versions":final_height+1,"long_history_commits":5000,"bulk_unique_nodes_inserted":big.new_nodes.nodes,"bulk_encoded_node_bytes_inserted":big.new_nodes.bytes,"changed_unique_nodes_inserted":changed.new_nodes.nodes,"changed_encoded_node_bytes_inserted":changed.new_nodes.bytes,"file_bytes":std::fs::metadata(path).unwrap().len(),"block_validation":false,"global_full_consensus":false,"canonical_public_sync":false,"runtime_go_dependency":false})
}
#[allow(dead_code)]
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let v = serde_json::from_slice(&std::fs::read(&args[0]).unwrap()).unwrap();
    println!("{}", verify(&v, Path::new(&args[1])));
}
