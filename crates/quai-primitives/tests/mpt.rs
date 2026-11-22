use quai_primitives::{
    mpt::{self, ReadNodes, Trie},
    trie, Hash,
};
use std::collections::BTreeMap;
#[derive(Default)]
struct Nodes(BTreeMap<Hash, Vec<u8>>);
impl ReadNodes for Nodes {
    fn get_node(&self, h: &Hash) -> Result<Option<Vec<u8>>, mpt::Error> {
        Ok(self.0.get(h).cloned())
    }
}
fn flush(t: &Trie, db: &mut Nodes) -> mpt::Batch {
    t.visit_new_nodes(&mut |h, b| {
        db.0.insert(h, b.to_vec());
        Ok(())
    })
    .unwrap()
}
fn root(map: &BTreeMap<Vec<u8>, Vec<u8>>) -> Hash {
    trie::secure_root(
        &map.iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Vec<_>>(),
    )
    .unwrap()
}
#[test]
fn path_updates_deletes_cold_nodes_and_pinned_versions_match_full_recomputation() {
    let mut t = Trie::default();
    let mut map = BTreeMap::new();
    let mut db = Nodes::default();
    let mut snapshots = vec![];
    for i in 0u64..1200 {
        let key = (i.wrapping_mul(7919) % 331).to_be_bytes().to_vec();
        let value = if i % 7 == 0 {
            vec![]
        } else {
            vec![(i % 251) as u8; [1, 2, 31, 32, 33, 55, 56, 255, 256][i as usize % 9]]
        };
        t.update(&db, &key, &value).unwrap();
        if value.is_empty() {
            map.remove(&key);
        } else {
            map.insert(key.clone(), value.clone());
        }
        assert_eq!(t.root(), root(&map), "step {i}");
        assert_eq!(t.get(&db, &key).unwrap(), map.get(&key).cloned());
        if i % 43 == 0 {
            snapshots.push((t.clone(), map.clone()));
            flush(&t, &mut db);
            t = Trie::from_root(t.root());
        }
    }
    flush(&t, &mut db);
    for (snapshot, values) in snapshots {
        assert_eq!(snapshot.root(), root(&values));
        for (k, v) in values {
            assert_eq!(snapshot.get(&db, &k).unwrap(), Some(v));
        }
    }
    let cold = Trie::from_root(t.root());
    assert_eq!(cold.root(), root(&map));
    for (k, v) in &map {
        assert_eq!(cold.get(&db, k).unwrap(), Some(v.clone()));
    }
    let mut changed = cold.clone();
    changed.update(&db, b"last change", b"value").unwrap();
    let batch = flush(&changed, &mut db);
    assert!(batch.nodes <= 65);
    assert_eq!(cold.root(), t.root());
}
#[test]
fn deepest_secure_prefix_collapses_inline_children_and_restores_empty_root() {
    let mut t = Trie::default();
    let mut map = BTreeMap::new();
    let mut db = Nodes::default();
    for last in [0u8, 1, 2, 15, 16, 255] {
        let mut key = [0; 32];
        key[31] = last;
        t.update_hashed(&db, key, &[last + u8::from(last == 0)])
            .unwrap();
        map.insert(key.to_vec(), vec![last + u8::from(last == 0)]);
        flush(&t, &mut db);
        t = Trie::from_root(t.root());
        assert_eq!(
            t.root(),
            trie::key_value_root(
                &map.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>()
            )
            .unwrap()
        );
    }
    for last in [2u8, 16, 1, 255, 0, 15] {
        let mut key = [0; 32];
        key[31] = last;
        t.update_hashed(&db, key, &[]).unwrap();
        map.remove(key.as_slice());
        flush(&t, &mut db);
        t = Trie::from_root(t.root());
        assert_eq!(
            t.root(),
            trie::key_value_root(
                &map.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>()
            )
            .unwrap()
        );
    }
    assert_eq!(t.root(), mpt::empty_root());
}
#[test]
fn missing_corrupt_and_noncanonical_nodes_fail_without_changing_root() {
    let mut t = Trie::default();
    t.update(&mpt::NoNodes, b"key", b"value").unwrap();
    let old = t.root();
    let mut missing = Trie::from_root(old);
    assert!(matches!(
        missing.get(&mpt::NoNodes, b"key"),
        Err(mpt::Error::Missing(_))
    ));
    assert!(missing.update(&mpt::NoNodes, b"key", b"next").is_err());
    assert_eq!(missing.root(), old);
    let mut db = Nodes::default();
    flush(&t, &mut db);
    db.0.get_mut(&old).unwrap()[0] ^= 1;
    assert!(Trie::from_root(old).get(&db, b"key").is_err());
    for malformed in [
        vec![0xc0],
        trie::list([trie::bytes(&[0x00]), trie::bytes(&[1])]),
        trie::list([trie::bytes(&[0x21]), trie::bytes(&[1])]),
    ] {
        let hash = quai_primitives::transaction::keccak(&malformed);
        db.0.insert(hash, malformed);
        assert!(Trie::from_root(hash).get(&db, b"key").is_err());
    }
}

#[test]
fn bulk_snapshot_construction_matches_recomputation_and_remains_mutable() {
    let mut values = (0u64..1200)
        .map(|n| {
            (
                n.to_be_bytes().to_vec(),
                vec![(n % 251) as u8; (n as usize % 80) + 1],
            )
        })
        .collect::<Vec<_>>();
    values.reverse();
    let mut t = Trie::from_secure_pairs(&values).unwrap();
    assert_eq!(t.root(), trie::secure_root(&values).unwrap());
    let pinned = t.clone();
    let mut db = Nodes::default();
    flush(&t, &mut db);
    t = Trie::from_root(t.root());
    let mut map = values.into_iter().collect::<BTreeMap<_, _>>();
    for n in (0u64..1200).step_by(3) {
        let key = n.to_be_bytes().to_vec();
        t.update(&db, &key, &[]).unwrap();
        map.remove(&key);
    }
    assert_eq!(t.root(), root(&map));
    assert_ne!(t.root(), pinned.root());
    for (k, v) in &map {
        assert_eq!(t.get(&db, k).unwrap(), Some(v.clone()));
    }
    assert!(Trie::from_secure_pairs(&[]).unwrap().is_empty());
    assert!(Trie::from_secure_pairs(&[(vec![1], vec![])]).is_err());
    assert!(Trie::from_secure_pairs(&[(vec![0; 65], vec![1])]).is_err());
    assert!(Trie::from_secure_pairs(&[(vec![1], vec![1]), (vec![1], vec![2])]).is_err());
}

#[test]
fn leaf_scans_verify_hashed_keys_and_enforce_entry_and_byte_bounds() {
    let values = (0u64..100)
        .map(|n| (n.to_be_bytes().to_vec(), vec![n as u8; 40]))
        .collect::<Vec<_>>();
    let trie = Trie::from_secure_pairs(&values).unwrap();
    let mut db = Nodes::default();
    flush(&trie, &mut db);
    let cold = Trie::from_root(trie.root());
    let mut found = BTreeMap::new();
    let count = cold
        .visit_leaves(&db, 100, 4000, &mut |key, value| {
            assert!(found.insert(key, value.to_vec()).is_none());
            Ok(())
        })
        .unwrap();
    assert_eq!(count.entries, 100);
    assert_eq!(count.bytes, 4000);
    for (k, v) in values {
        assert_eq!(found[&quai_primitives::transaction::keccak(&k)], v);
    }
    assert!(cold
        .visit_leaves(&db, 99, 4000, &mut |_, _| Ok(()))
        .is_err());
    assert!(cold
        .visit_leaves(&db, 100, 3999, &mut |_, _| Ok(()))
        .is_err());
    db.0.remove(&cold.root());
    assert!(cold
        .visit_leaves(&db, 100, 4000, &mut |_, _| Ok(()))
        .is_err());
}
/// `Trie::prove` keeps go-ethereum's `Trie.Prove` shape: the root first,
/// every later node referenced by hash from the node before it, the same
/// nodes from memory and from a node store, and none for an empty trie.
#[test]
fn proofs_follow_hash_links_from_the_root() {
    use quai_primitives::transaction::keccak;
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0u64..300)
        .map(|i| {
            let value = vec![(i % 251) as u8 + 1; [1, 2, 31, 40, 70][i as usize % 5]];
            (i.to_be_bytes().to_vec(), trie::bytes(&value))
        })
        .collect();
    let t = Trie::from_secure_pairs(&pairs).unwrap();
    let mut db = Nodes::default();
    flush(&t, &mut db);
    let stored = Trie::from_root(t.root());
    for i in 0u64..320 {
        let key = i.to_be_bytes();
        let proof = t.prove(&mpt::NoNodes, &key).unwrap();
        assert_eq!(proof, stored.prove(&db, &key).unwrap(), "key {i}");
        assert_eq!(keccak(&proof[0]), t.root());
        for pair in proof.windows(2) {
            let link = keccak(&pair[1]);
            assert!(pair[0].windows(32).any(|w| w == link), "key {i}");
            assert!(pair[1].len() >= 32);
        }
        if i < 300 {
            let value = &pairs[i as usize].1;
            let last = proof.last().unwrap();
            assert!(
                last.windows(value.len()).any(|w| w == value.as_slice()),
                "key {i}"
            );
        }
    }
    assert!(Trie::default()
        .prove(&mpt::NoNodes, b"x")
        .unwrap()
        .is_empty());
}
