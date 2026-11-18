//! Separate fixed workload: modify one account and compute a new state root.
use quai_execution::evm::{
    self,
    calls::{self, Environment},
    state::{Account, World},
    word::{bytes, Word},
};
use quai_primitives::{encode_hex, transaction::AccessTuple};
use std::{
    collections::{BTreeMap, BTreeSet},
    hint::black_box,
    time::Instant,
};
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
        let mut storage = BTreeMap::new();
        for k in 0..8 {
            storage.insert(bytes(Word::from_u64(k)), bytes(Word::from_u64(k + 1)));
        }
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
    let c = evm::Context {
        address: address(16),
        caller: address(17),
        origin: address(17),
        coinbase: address(18),
        etx_sender: [0; 20],
        location: [0, 0],
        tx_type: 0,
        value: Word::ZERO,
        gas_price: Word::ONE,
        block_number: Word::from_u64(4000000),
        timestamp: Word::from_u64(1769112000),
        difficulty: Word::ONE,
        base_fee: Word::ONE,
        chain_id: Word::from_u64(1337),
        state_size: Word::from_u64(4096),
        block_gas_limit: 30000000,
        address_listed: false,
        warm_slots: BTreeSet::new(),
        read_only: false,
        trace: false,
    };
    let access = [AccessTuple {
        address: address(16),
        keys: vec![[0; 32]],
    }];
    let env = Environment {
        prime_number: Some(2444574),
        ..Default::default()
    };
    let mut next = world.clone();
    let result = calls::execute_call_with_env(&mut next, &c, &access, &[], 300000, &env).unwrap();
    assert!(result.error.is_none() && !result.reverted);
    assert_eq!(
        next.account(&address(16))
            .unwrap()
            .unwrap()
            .get([0; 32])
            .unwrap(),
        bytes(Word::from_u64(7))
    );
    let root = encode_hex(&next.root().unwrap());
    let before = encode_hex(&world.root().unwrap());
    assert_ne!(before, root);
    let start = Instant::now();
    let mut checksum = 0u64;
    for _ in 0..100 {
        let mut w = black_box(&world).clone();
        let o = calls::execute_call_with_env(&mut w, &c, &access, &[], 300000, &env).unwrap();
        let committed = black_box(w.root().unwrap());
        checksum = checksum
            .wrapping_add(u64::from_be_bytes(committed[..8].try_into().unwrap()))
            .wrapping_add(o.gas_remaining);
    }
    let dirty_commit_ns = start.elapsed().as_nanos() / 100;
    println!("{{\"accounts\":4096,\"changed_accounts\":1,\"dirty_commit_ns\":{dirty_commit_ns},\"checksum\":{checksum},\"before_root\":\"{before}\",\"after_root\":\"{root}\"}}");
}
