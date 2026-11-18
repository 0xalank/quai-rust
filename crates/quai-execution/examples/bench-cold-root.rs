//! Separate fixed cold-root workload; the input has no cached commitments.
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
    let mut world = World::default();
    let mut code = vec![0x60, 7, 0x60, 0, 0x55, 0];
    code.resize(128, 0);
    for n in 16..4112u64 {
        let storage = (0..8)
            .map(|k| (bytes(Word::from_u64(k)), bytes(Word::from_u64(k + 1))))
            .collect::<BTreeMap<_, _>>();
        world
            .insert(
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
            .unwrap();
    }
    world.validate().unwrap();
    let expected = trie::secure_root(
        &world
            .accounts()
            .unwrap()
            .iter()
            .map(|(k, a)| (k.to_vec(), a.consensus_rlp().unwrap()))
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert_eq!(world.clone().root().unwrap(), expected);
    let start = Instant::now();
    let mut checksum = 0u64;
    for _ in 0..10 {
        let uncached = black_box(&world).clone();
        let root = black_box(uncached.root().unwrap());
        checksum = checksum.wrapping_add(u64::from_be_bytes(root[..8].try_into().unwrap()));
    }
    let cold_root_ns = start.elapsed().as_nanos() / 10;
    println!("{{\"accounts\":4096,\"slots_per_account\":8,\"cold_root_ns\":{cold_root_ns},\"checksum\":{checksum},\"root\":\"{}\"}}",encode_hex(&expected));
}
