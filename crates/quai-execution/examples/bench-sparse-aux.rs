//! Fixed synthetic ETX/lockup parent, point updates and one durable commit.
use quai_execution::{
    evm::{state::World, word::Word},
    lockup::{Key, Lockup},
    state_store,
    utxo::UtxoState,
};
use quai_primitives::{
    encode_hex,
    transaction::{keccak, Address, External, Transaction},
};
use serde_json::json;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};
struct Counting;
static ENABLED: AtomicBool = AtomicBool::new(false);
static BYTES: AtomicU64 = AtomicU64::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);
fn track(n: usize) {
    if ENABLED.load(Ordering::Relaxed) {
        BYTES.fetch_add(n as u64, Ordering::Relaxed);
        CALLS.fetch_add(1, Ordering::Relaxed);
    }
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        track(l.size());
        System.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        track(l.size());
        System.alloc_zeroed(l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        track(n);
        System.realloc(p, l, n)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn address(n: u64) -> Address {
    let mut a = [0; 20];
    a[12..].copy_from_slice(&n.to_be_bytes());
    a
}
fn tx(n: u64) -> Transaction {
    Transaction::external(External {
        originating_hash: keccak(&n.to_be_bytes()),
        index: (n % 65536) as u16,
        gas: 21000,
        to: address(0x1101),
        value: vec![1],
        data: vec![],
        access_list: vec![],
        sender: address(0x1102),
        etx_type: 0,
    })
    .unwrap()
}
fn key(n: u32) -> Key {
    Key {
        owner: address(0x1101),
        beneficiary: address(n as u64),
        lock: 1,
        epoch: n,
    }
}
fn value(n: u64) -> Lockup {
    Lockup {
        balance: Word::from_u64(n),
        unlock_height: 50000,
        elements: 1,
        delegate: address(0x1103),
    }
}
fn transition(mut world: World) -> World {
    assert_eq!(world.etxs_mut().pop().unwrap().unwrap(), tx(0));
    assert_eq!(world.etxs_mut().pop().unwrap().unwrap(), tx(1));
    world.etxs_mut().push(&[tx(5000)]).unwrap();
    world
        .etxs_mut()
        .set_auxiliary(keccak(b"aux-benchmark").to_vec(), vec![2])
        .unwrap();
    world.set_lockup(key(1), None).unwrap();
    world.set_lockup(key(2), Some(value(777))).unwrap();
    world.set_lockup(key(6000), Some(value(6000))).unwrap();
    world
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mode = &args[0];
    assert!(mode == "materialized" || mode == "sparse");
    assert_eq!(args[1], "5000");
    let allocations = args.get(2).is_some_and(|s| s == "--allocations");
    let directory = std::env::temp_dir().join(format!(
        "quai-sparse-aux-benchmark-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let db = quai_store::state_db::StateDatabase::open(
        &directory.join("state.redb"),
        keccak(b"sparse-aux-frozen-parent-v1"),
    )
    .unwrap();
    let mut memory = World::default();
    memory
        .etxs_mut()
        .push(&(0..5000).map(tx).collect::<Vec<_>>())
        .unwrap();
    memory
        .etxs_mut()
        .set_auxiliary(keccak(b"aux-benchmark").to_vec(), vec![1])
        .unwrap();
    for n in 1..=5000 {
        memory.set_lockup(key(n), Some(value(n as u64))).unwrap();
    }
    let none = UtxoState::default();
    let mut parent_c = None;
    let parent = db
        .commit_prepared(None, None, [0; 32], true, |w| {
            let c = state_store::apply_delta(w, &World::default(), &none, &memory, &none)?;
            let raw = c.digest.to_vec();
            parent_c = Some(c);
            Ok(raw)
        })
        .unwrap()
        .version
        .id;
    let parent_c = parent_c.unwrap();
    let expected = transition(memory.clone());
    let expected_root = expected.etxs().root().unwrap();
    let start = Instant::now();
    ENABLED.store(allocations, Ordering::Relaxed);
    let snapshot = Arc::new(Mutex::new(db.snapshot(Some(parent)).unwrap()));
    let (before, utxos) = if mode == "sparse" {
        state_store::restore_sparse(snapshot.clone(), &parent_c).unwrap()
    } else {
        state_store::restore(&snapshot.lock().unwrap(), &parent_c).unwrap()
    };
    assert_eq!(before.etxs().is_lazy(), mode == "sparse");
    assert_eq!(before.lazy_lockups(), mode == "sparse");
    let after = transition(before.clone());
    let mut completed = None;
    let committed = db
        .commit_prepared(Some(parent), Some(parent), [1; 32], true, |w| {
            let c = state_store::apply_delta(w, &before, &utxos, &after, &utxos)?;
            let raw = c.digest.to_vec();
            completed = Some(c);
            Ok(raw)
        })
        .unwrap();
    ENABLED.store(false, Ordering::Relaxed);
    let elapsed = start.elapsed().as_nanos() as u64;
    let allocated = BYTES.load(Ordering::Relaxed);
    let calls = CALLS.load(Ordering::Relaxed);
    let loaded_queue = before.etxs().loaded_entries();
    let loaded_lockups = before.loaded_lockups();
    let completed = completed.unwrap();
    assert_eq!(
        after.materialize().unwrap(),
        expected.materialize().unwrap()
    );
    assert_eq!(after.etxs().root().unwrap(), expected_root);
    assert_eq!(db.head().unwrap(), Some(committed.version.id));
    if mode == "sparse" {
        assert!(loaded_queue <= 2 && loaded_lockups <= 2);
    } else {
        assert_eq!(loaded_queue, 5000);
        assert_eq!(loaded_lockups, 5000);
    }
    let mut out = json!({"backend":mode,"mode":if allocations{"allocations"}else{"timing"},"parent_queued_etxs":5000,"parent_lockups":5000,"popped_etxs":2,"pushed_etxs":1,"changed_lockups":3,"economic_metadata_updates":1,"final_queue_length":after.etxs().len(),"final_oldest":after.etxs().oldest(),"final_newest":after.etxs().newest(),"final_evm_root":encode_hex(&completed.roots.evm),"final_etx_root":encode_hex(&completed.roots.etx),"final_utxo_root":encode_hex(&completed.roots.utxo),"final_local_digest":encode_hex(&completed.digest),"final_version":encode_hex(&committed.version.id),"loaded_queued_records":loaded_queue,"loaded_lockup_records":loaded_lockups,"native_block_admission":false});
    if allocations {
        out["gross_allocated_bytes"] = allocated.into();
        out["allocation_requests"] = calls.into();
    } else {
        out["ns_per_transition"] = elapsed.into();
    }
    drop(after);
    drop(before);
    drop(snapshot);
    drop(db);
    std::fs::remove_dir_all(directory).unwrap();
    println!("{out}");
}
