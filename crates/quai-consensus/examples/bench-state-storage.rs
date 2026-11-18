//! Same fixed hierarchy workload with an explicit image/database storage choice.
use quai_consensus::{
    economics,
    history::{History, Record},
    payouts,
    private_branch::PrivateBranch,
    verifier::Verifier,
};
use quai_primitives::{decode_hex, encode_hex};
use serde_json::{json, Value};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
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

fn raw(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
fn views(v: &Value) -> Vec<(u8, Vec<u8>)> {
    v["contexts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| {
            let c = x["context"].as_u64().unwrap() as u8;
            (c < 2).then(|| (c, raw(&x["work_object"])))
        })
        .collect()
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let backend = args.get(1).expect("images or database");
    assert!(backend == "images" || backend == "database");
    let allocations = args.get(2).is_some_and(|v| v == "--allocations");
    let c: Value = serde_json::from_slice(&std::fs::read(&args[0]).unwrap()).unwrap();
    let rows = c["branches"][0]["vectors"].as_array().unwrap();
    let records = rows
        .iter()
        .map(|r| Record::decode(&raw(&r["work_object"])).unwrap())
        .collect::<Vec<_>>();
    let mut history = History::default();
    history
        .insert(Record::decode(&raw(&c["genesis"])).unwrap())
        .unwrap();
    for r in &records {
        history.insert(r.clone()).unwrap();
    }
    let mut verifier = Verifier::default();
    for r in &records {
        verifier.verify_header_proof(r.header()).unwrap();
        payouts::native_components(
            &mut verifier,
            &history,
            r,
            [0, 0],
            quai_consensus::rules::Uint::from_u64(economics::INITIAL_EXCHANGE_RATE),
            0,
        )
        .unwrap();
    }
    let directory = std::env::temp_dir().join(format!(
        "quai-confirmation-bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let mut store = if backend == "images" {
        PrivateBranch::open_hierarchy(&directory).unwrap()
    } else {
        PrivateBranch::open_hierarchy_database(&directory).unwrap()
    };
    assert_eq!(
        store.storage_backend(),
        if backend == "images" {
            "complete-images"
        } else {
            "incremental-trie-database"
        }
    );
    let now = c["now"].as_u64().unwrap();
    let witnesses = rows.iter().map(views).collect::<Vec<_>>();
    let mut received = 0;
    let mut contexts = 0;
    let start = Instant::now();
    ENABLED.store(allocations, Ordering::Relaxed);
    for (r, views) in records.iter().zip(witnesses) {
        let old = store.head();
        let candidate = store
            .execute_hierarchy_candidate(
                &mut verifier,
                old,
                std::hint::black_box(r.bytes()),
                views,
                now,
            )
            .unwrap();
        store
            .select_execution_head(old, candidate.checkpoint)
            .unwrap();
        received += candidate.execution_inbound_etxs;
        contexts += candidate.hierarchy_contexts;
    }
    ENABLED.store(false, Ordering::Relaxed);
    let elapsed = start.elapsed().as_nanos() / records.len() as u128;
    let bytes = BYTES.load(Ordering::Relaxed) / records.len() as u64;
    let calls = CALLS.load(Ordering::Relaxed) / records.len() as u64;
    let state = store.restore(store.head()).unwrap();
    let last = rows.last().unwrap();
    assert_eq!(
        encode_hex(&state.state.world.root().unwrap()),
        last["evm_root"]
    );
    assert_eq!(
        encode_hex(&state.state.world.etxs().root().unwrap()),
        last["etx_root"]
    );
    assert_eq!(received, 8);
    let mut out = json!({"backend":backend,"mode":if allocations{"allocations"}else{"timing"},"blocks":records.len(),"native_contexts":contexts,"executed_inbound_etxs":received,"final_block":encode_hex(&state.anchor.block),"final_etx_root":encode_hex(&state.state.world.etxs().root().unwrap()),"fully_validated":false});
    if allocations {
        out["gross_allocated_bytes_per_block"] = bytes.into();
        out["allocation_requests_per_block"] = calls.into();
    } else {
        out["ns_per_block"] = (elapsed as u64).into();
    }
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
    println!("{out}");
}
