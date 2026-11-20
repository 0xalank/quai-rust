//! Fixed synthetic UTXO parent: restore, eight membership writes and durable commit.
use quai_execution::{
    evm::state::World,
    qi::{OutPoint, Utxo},
    state_store,
    utxo::UtxoState,
};
use quai_primitives::{encode_hex, transaction::keccak};
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
fn point(n: u64) -> OutPoint {
    OutPoint {
        hash: keccak(&n.to_be_bytes()),
        index: 0,
    }
}
fn value(n: u64) -> Utxo {
    let mut address = [3; 20];
    address[1] = 128;
    Utxo {
        denomination: (n % 15) as u8,
        address: address.into(),
        lock: vec![],
    }
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mode = &args[0];
    assert!(mode == "materialized" || mode == "sparse");
    let count = args[1].parse::<u64>().unwrap();
    assert!(count >= 8 && count <= 1_000_000);
    let allocations = args.get(2).is_some_and(|s| s == "--allocations");
    let directory = std::env::temp_dir().join(format!(
        "quai-sparse-bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let db = quai_store::state_db::StateDatabase::open(
        &directory.join("state.redb"),
        keccak(b"qi-sparse-bench-parent-v1"),
    )
    .unwrap();
    let world = World::default();
    let empty = UtxoState::default();
    let genesis = db
        .commit_prepared(None, None, [0; 32], true, |w| {
            Ok(state_store::apply_delta(w, &world, &empty, &world, &empty)?
                .digest
                .to_vec())
        })
        .unwrap()
        .version
        .id;
    let mut initial = UtxoState::default();
    for n in 0..count {
        initial.insert(point(n), value(n)).unwrap();
    }
    let mut parent_state = None;
    let parent = db
        .commit_prepared(Some(genesis), Some(genesis), [1; 32], true, |w| {
            let c = state_store::apply_delta(w, &world, &empty, &world, &initial)?;
            let metadata = c.digest.to_vec();
            parent_state = Some(c);
            Ok(metadata)
        })
        .unwrap()
        .version
        .id;
    let parent_state = parent_state.unwrap();
    let mut expected = initial.clone();
    for n in 0..4 {
        expected.remove(&point(n)).unwrap();
        expected.insert(point(count + n), value(count + n)).unwrap();
    }
    let started = Instant::now();
    ENABLED.store(allocations, Ordering::Relaxed);
    let snapshot = db.snapshot(Some(parent)).unwrap();
    let (before, before_u) = if mode == "sparse" {
        state_store::restore_sparse_qi(Arc::new(Mutex::new(snapshot)), &parent_state).unwrap()
    } else {
        state_store::restore(&snapshot, &parent_state).unwrap()
    };
    let after = before.clone();
    let mut after_u = before_u.clone();
    for n in 0..4 {
        after_u.remove(&point(n)).unwrap();
        after_u.insert(point(count + n), value(count + n)).unwrap();
    }
    let mut completed = None;
    let committed = db
        .commit_prepared(Some(parent), Some(parent), [2; 32], true, |w| {
            let c = state_store::apply_delta(w, &before, &before_u, &after, &after_u)?;
            let metadata = c.digest.to_vec();
            completed = Some(c);
            Ok(metadata)
        })
        .unwrap();
    ENABLED.store(false, Ordering::Relaxed);
    let elapsed = started.elapsed().as_nanos() as u64;
    let loaded = before_u.loaded_entries();
    let completed = completed.unwrap();
    assert_eq!(after_u.raw_len(), count);
    assert_eq!(after_u.root(), expected.root());
    assert_eq!(after_u.accumulator(), expected.accumulator());
    assert_eq!(after_u.consensus_size(), expected.consensus_size());
    assert_eq!(completed.roots.utxo, expected.root());
    if mode == "sparse" {
        assert!(before_u.is_lazy());
        assert!(loaded <= 8);
    } else {
        assert!(!before_u.is_lazy());
    }
    let restored = state_store::restore(
        &db.snapshot(Some(committed.version.id)).unwrap(),
        &completed,
    )
    .unwrap();
    assert_eq!(
        restored.1.materialize().unwrap(),
        expected.materialize().unwrap()
    );
    let mut out = json!({"backend":mode,"mode":if allocations{"allocations"}else{"timing"},"parent_raw_entries":count,"removed":4,"created":4,"touched_membership_keys":8,"final_raw_entries":after_u.raw_len(),"final_consensus_count":after_u.consensus_size(),"final_utxo_root":encode_hex(&after_u.root()),"final_local_digest":encode_hex(&completed.digest),"final_version":encode_hex(&committed.version.id),"point_records_decoded":loaded,"native_block_admission":false});
    if allocations {
        out["gross_allocated_bytes"] = BYTES.load(Ordering::Relaxed).into();
        out["allocation_requests"] = CALLS.load(Ordering::Relaxed).into();
    } else {
        out["ns_per_transition"] = elapsed.into();
    }
    drop(restored);
    drop(after_u);
    drop(before_u);
    drop(db);
    std::fs::remove_dir_all(directory).unwrap();
    println!("{out}");
}
