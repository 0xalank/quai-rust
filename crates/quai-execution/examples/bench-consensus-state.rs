//! Fixed workload for comparing the installed pre-change source and this round.
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
    let start = Instant::now();
    for _ in 0..1000 {
        black_box(black_box(&world).clone());
    }
    let clone_ns = start.elapsed().as_nanos() / 1000;
    let start = Instant::now();
    let mut gas = 0;
    for _ in 0..100 {
        let mut w = black_box(&world).clone();
        let o = calls::execute_call_with_env(&mut w, &c, &access, &[], 300000, &env).unwrap();
        gas += o.gas_remaining;
        black_box(w);
    }
    let execution_ns = start.elapsed().as_nanos() / 100;
    let start = Instant::now();
    for _ in 0..10 {
        black_box(world.root().unwrap());
    }
    let root_ns = start.elapsed().as_nanos() / 10;
    println!("{{\"accounts\":4096,\"slots_per_account\":8,\"code_bytes_per_account\":128,\"clone_ns\":{clone_ns},\"execution_ns\":{execution_ns},\"root_ns\":{root_ns},\"gas_checksum\":{gas},\"before_root\":\"{before}\",\"after_root\":\"{root}\"}}");
}
