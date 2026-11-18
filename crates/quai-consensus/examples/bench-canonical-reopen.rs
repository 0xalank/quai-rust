//! Fixed native admission history, then a fresh writer reopen on a warm filesystem.
use quai_consensus::private_canonical::{BlockBundle, PrivateCanonical};
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
fn bytes(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let allocations = args.get(1).is_some_and(|s| s == "--allocations");
    let corpus: Value = serde_json::from_slice(&std::fs::read(&args[0]).unwrap()).unwrap();
    let now = corpus["now"].as_u64().unwrap();
    let dir = std::env::temp_dir().join(format!(
        "quai-canonical-reopen-bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut chain = PrivateCanonical::open(&dir, now).unwrap();
    let events = corpus["canonical_events"].as_array().unwrap();
    for event in events {
        let row = &corpus["branches"][event["branch"].as_u64().unwrap() as usize]["vectors"]
            [event["index"].as_u64().unwrap() as usize];
        let bundle = BlockBundle {
            zone: bytes(&row["work_object"]),
            contexts: row["contexts"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| {
                    let c = v["context"].as_u64().unwrap() as u8;
                    (c < 2).then(|| (c, bytes(&v["work_object"])))
                })
                .collect(),
        };
        let imported = chain.import(bundle, now).unwrap();
        assert_eq!(imported.selected, event["selected"].as_bool().unwrap());
        assert_eq!(encode_hex(&chain.view().unwrap().block()), event["head"]);
    }
    let (head, height, roots, score) = {
        let view = chain.view().unwrap();
        (
            view.block(),
            view.height(),
            view.roots().clone(),
            view.score(),
        )
    };
    drop(chain);
    let start = Instant::now();
    ENABLED.store(allocations, Ordering::Relaxed);
    let reopened = PrivateCanonical::open(std::hint::black_box(&dir), now).unwrap();
    ENABLED.store(false, Ordering::Relaxed);
    let elapsed = start.elapsed().as_nanos() as u64;
    let view = reopened.view().unwrap();
    assert_eq!(view.block(), head);
    assert_eq!(view.height(), height);
    assert_eq!(*view.roots(), roots);
    assert_eq!(view.score(), score);
    assert!(!view.global_consensus_qualified());
    let mut out = json!({"mode":if allocations{"allocations"}else{"timing"},"events":events.len(),"unique_native_blocks":24,"head":encode_hex(&head),"height":height,"evm_root":encode_hex(&roots.evm),"etx_root":encode_hex(&roots.etx),"utxo_root":encode_hex(&roots.utxo),"global_full_consensus":false,"canonical_public_sync":false});
    if allocations {
        out["gross_allocated_bytes_per_reopen"] = BYTES.load(Ordering::Relaxed).into();
        out["allocation_requests_per_reopen"] = CALLS.load(Ordering::Relaxed).into();
    } else {
        out["ns_per_reopen"] = elapsed.into();
    }
    drop(view);
    drop(reopened);
    std::fs::remove_dir_all(dir).unwrap();
    println!("{out}");
}
