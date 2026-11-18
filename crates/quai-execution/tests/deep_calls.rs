//! go-quai allows 1,025 nested call frames and grows goroutine stacks as they
//! need. A Rust thread has a fixed stack, so a call chain as deep as the gas
//! allows must not depend on the stack of the thread that runs the
//! transaction. The Go oracle checks the same kind of chain in
//! fixtures/protocol/settlement.json (vector `deep-calls-...`).
use quai_execution::evm::{
    calls::{creation_address, execute_call_with_env, execute_creation_with_env, Environment},
    state::{Account, World},
    word::{from_bytes, Word},
    Context, Outcome,
};
use quai_primitives::transaction::{AccessTuple, Address};
use std::collections::BTreeSet;

const CONTRACT: Address = [
    0, 0x11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
];
const CALLER: Address = [
    0, 0x22, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2,
];
/// Frames that 50,000,000 gas reaches through `RECURSE` at state size 1.
const DEPTH: u64 = 645;
/// slot 0 += 1; CALL(GAS, ADDRESS, 0, 0, 0, 0, 0); STOP
const RECURSE: [u8; 23] = [
    0x60, 0, 0x54, 0x60, 1, 0x01, 0x60, 0, 0x55, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x30,
    0x5a, 0xf1, 0x00,
];
/// Below what the chain needs without its own stack: release frames take
/// about 6 KiB and debug frames up to 52 KiB.
const SMALL_STACK: usize = 1 << 20;
const LARGE_STACK: usize = 256 << 20;

fn world() -> World {
    let mut world = World::default();
    let contract = Account {
        code: RECURSE.to_vec(),
        nonce: 1,
        ..Default::default()
    };
    world.insert(CONTRACT, contract).unwrap();
    let caller = Account {
        balance: Word::from_u64(1),
        ..Default::default()
    };
    world.insert(CALLER, caller).unwrap();
    world
}
fn context(address: Address) -> Context {
    Context {
        address,
        caller: CALLER,
        origin: CALLER,
        coinbase: [0; 20],
        etx_sender: [0; 20],
        location: [0, 0],
        tx_type: 0,
        value: Word::ZERO,
        gas_price: Word::ZERO,
        block_number: Word::from_u64(4_000_000),
        timestamp: Word::ZERO,
        difficulty: Word::ZERO,
        base_fee: Word::ZERO,
        chain_id: Word::from_u64(9),
        state_size: Word::ONE,
        block_gas_limit: 50_000_000,
        address_listed: true,
        warm_slots: BTreeSet::new(),
        read_only: false,
        trace: false,
    }
}
fn environment() -> Environment {
    Environment {
        prime_number: Some(2_500_000),
        ..Default::default()
    }
}
fn depth(world: &World) -> Word {
    from_bytes(
        &world
            .account(&CONTRACT)
            .unwrap()
            .unwrap()
            .get([0; 32])
            .unwrap(),
    )
}
fn on_stack<T: Send + 'static>(stack: usize, run: fn() -> T) -> T {
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap()
}
fn deep_call() -> (Outcome, World) {
    let mut world = world();
    let outcome = execute_call_with_env(
        &mut world,
        &context(CONTRACT),
        &[],
        &[],
        50_000_000,
        &environment(),
    )
    .unwrap();
    (outcome, world)
}
/// A creation whose init code calls the recursive contract.
fn deep_creation() -> (Outcome, Option<Address>, World) {
    let mut init = vec![0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x73];
    init.extend(CONTRACT);
    init.extend([0x5a, 0xf1, 0x00]);
    let (created, _) =
        creation_address(CALLER, 0, &init, None, 4_000_000, [0, 0], 50_000_000).unwrap();
    let access = [CONTRACT, created].map(|address| AccessTuple {
        address,
        keys: vec![],
    });
    let mut world = world();
    let (outcome, address) = execute_creation_with_env(
        &mut world,
        &context([0; 20]),
        &access,
        &init,
        50_000_000,
        &environment(),
    )
    .unwrap();
    (outcome, address, world)
}

#[test]
fn deep_call_chain_does_not_depend_on_the_caller_stack() {
    let (small, small_world) = on_stack(SMALL_STACK, deep_call);
    let (large, large_world) = on_stack(LARGE_STACK, deep_call);
    assert!(small.error.is_none() && !small.reverted);
    assert_eq!(depth(&small_world), Word::from_u64(DEPTH));
    assert_eq!(small.gas_remaining, large.gas_remaining);
    assert_eq!(small.state_gas, large.state_gas);
    assert_eq!(small.refund, large.refund);
    assert_eq!(small_world, large_world);
    assert_eq!(small_world.root().unwrap(), large_world.root().unwrap());
}

#[test]
fn deep_creation_does_not_depend_on_the_caller_stack() {
    let (small, small_address, small_world) = on_stack(SMALL_STACK, deep_creation);
    let (large, large_address, large_world) = on_stack(LARGE_STACK, deep_creation);
    assert!(small.error.is_none() && !small.reverted);
    assert!(small_address.is_some());
    assert_eq!(small_address, large_address);
    // The init code's call is frame 1, so the chain stops a few frames sooner.
    assert!(depth(&small_world) > Word::from_u64(DEPTH - 10));
    assert_eq!(depth(&small_world), depth(&large_world));
    assert_eq!(small.gas_remaining, large.gas_remaining);
    assert_eq!(small.state_gas, large.state_gas);
    assert_eq!(small_world, large_world);
    assert_eq!(small_world.root().unwrap(), large_world.root().unwrap());
}
