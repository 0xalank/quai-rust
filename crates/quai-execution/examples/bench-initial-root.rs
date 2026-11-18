//! First-ever root on fresh independently constructed state in each sample.
use quai_execution::evm::{
    state::{Account, World},
    word::{bytes, Word},
};
use quai_primitives::{encode_hex, trie};
use std::{collections::BTreeMap, hint::black_box, time::Instant};
fn address(n: u64) -> [u8; 20] {
    let mut a = [0; 20];
    a[12..].copy_from_slice(&n.to_be_bytes());
    a
}
fn main() {
    let mut code = vec![0x60, 7, 0x60, 0, 0x55, 0];
    code.resize(128, 0);
    let entries = (16..4112u64)
        .map(|n| {
            let storage = (0..8)
                .map(|k| (bytes(Word::from_u64(k)), bytes(Word::from_u64(k + 1))))
                .collect::<BTreeMap<_, _>>();
            (
                address(n),
                Account {
                    nonce: 1,
                    balance: Word::from_u64(1000000),
                    code: code.clone(),
                    storage,
                    storage_size: Word::from_u64(8),
                    backing: None,
                },
            )
        })
        .collect::<Vec<_>>();
    let expected = trie::secure_root(
        &entries
            .iter()
            .map(|(a, v)| (a.to_vec(), v.consensus_rlp().unwrap()))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let mut elapsed = 0;
    let mut checksum = 0u64;
    for _ in 0..10 {
        let mut fresh = World::default();
        for (address, account) in &entries {
            fresh.insert(*address, account.clone()).unwrap();
        }
        fresh.validate().unwrap();
        let start = Instant::now();
        let root = black_box(black_box(&fresh).root().unwrap());
        elapsed += start.elapsed().as_nanos();
        assert_eq!(root, expected);
        checksum = checksum.wrapping_add(u64::from_be_bytes(root[..8].try_into().unwrap()));
    }
    let initial_root_ns = elapsed / 10;
    println!("{{\"accounts\":4096,\"slots_per_account\":8,\"initial_root_ns\":{initial_root_ns},\"checksum\":{checksum},\"root\":\"{}\"}}",encode_hex(&expected));
}
