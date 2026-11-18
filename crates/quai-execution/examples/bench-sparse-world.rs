//! Fixed synthetic account/storage parent, journal updates and durable commit.
use quai_execution::{
    evm::{
        journal::Journal,
        state::{Account, World},
        word::{bytes, Word},
    },
    state_store,
    utxo::UtxoState,
};
use quai_primitives::{
    encode_hex,
    transaction::{keccak, Address},
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
fn slot(n: u64) -> [u8; 32] {
    bytes(Word::from_u64(n))
}
fn transition(mut world: World) -> World {
    world.begin_block();
    let mut journal = Journal::new(world).unwrap();
    let revision = journal.snapshot().unwrap();
    journal
        .set_storage(address(1), slot(0), slot(7000))
        .unwrap();
    journal.set_storage(address(1), slot(3), slot(2)).unwrap();
    assert!(journal
        .transfer(address(2), address(3), Word::from_u64(7))
        .unwrap());
    journal.commit(revision).unwrap();
    let mut completed = journal.into_world().unwrap();
    completed.finish_block().unwrap();
    completed
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mode = &args[0];
    assert!(mode == "materialized" || mode == "sparse");
    let count = args[1].parse::<u64>().unwrap();
    assert_eq!(count, 10000);
    let allocations = args.get(2).is_some_and(|v| v == "--allocations");
    let directory = std::env::temp_dir().join(format!(
        "quai-sparse-world-bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let db = quai_store::state_db::StateDatabase::open(
        &directory.join("state.redb"),
        keccak(b"sparse-world-frozen-parent-v1"),
    )
    .unwrap();
    let mut memory = World::default();
    for n in 1..=count {
        let mut a = Account {
            nonce: 1,
            balance: Word::from_u64(100),
            storage_size: Word::from_u64(2),
            ..Default::default()
        };
        a.put(slot(0), slot(n)).unwrap();
        a.put(slot(1), slot(n + 1)).unwrap();
        memory.insert(address(n), a).unwrap();
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
    let expected_root = expected.root().unwrap();
    let start = Instant::now();
    ENABLED.store(allocations, Ordering::Relaxed);
    let snapshot = Arc::new(Mutex::new(db.snapshot(Some(parent)).unwrap()));
    let (before, utxos) = if mode == "sparse" {
        state_store::restore_sparse(snapshot.clone(), &parent_c).unwrap()
    } else {
        state_store::restore(&snapshot.lock().unwrap(), &parent_c).unwrap()
    };
    assert_eq!(before.is_lazy(), mode == "sparse");
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
    let loaded = before.loaded_accounts();
    let completed = completed.unwrap();
    assert_eq!(after.account_count(), count);
    assert_eq!(after.root().unwrap(), expected_root);
    assert_eq!(
        after.materialize().unwrap(),
        expected.materialize().unwrap()
    );
    assert_eq!(completed.roots.evm, expected_root);
    assert_eq!(db.head().unwrap(), Some(committed.version.id));
    if mode == "sparse" {
        assert!(loaded <= 3);
    } else {
        assert_eq!(loaded, count as usize);
    }
    let mut out = json!({"backend":mode,"mode":if allocations{"allocations"}else{"timing"},"parent_accounts":count,"parent_storage_slots":2*count,"touched_accounts":3,"storage_writes":2,"final_accounts":after.account_count(),"final_evm_root":encode_hex(&completed.roots.evm),"final_etx_root":encode_hex(&completed.roots.etx),"final_utxo_root":encode_hex(&completed.roots.utxo),"final_local_digest":encode_hex(&completed.digest),"final_version":encode_hex(&committed.version.id),"loaded_accounts_during_transition":loaded,"native_block_admission":false,"actual_signed_transaction":false});
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
