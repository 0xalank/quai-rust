//! Fixed Go-qualified weak-share arithmetic workload.
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

use quai_consensus::{rules::Uint, weak_float};
use serde_json::{json, Value};
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let allocations = args.get(1).is_some_and(|s| s == "--allocations");
    let data: Value = serde_json::from_slice(&std::fs::read(&args[0]).unwrap()).unwrap();
    let rows = data["arithmetic"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                Uint::from_u128(r["deficit"].as_str().unwrap().parse().unwrap()),
                r["distance"].as_u64().unwrap(),
                Uint::from_u128(r["weight"].as_str().unwrap().parse().unwrap()),
            )
        })
        .collect::<Vec<_>>();
    let mut expected = Uint::ZERO;
    for (d, n, w) in &rows {
        assert_eq!(weak_float::discount(*d, *n).unwrap(), *w);
        expected = expected.wrapping_add(w);
    }
    const ROUNDS: usize = 16;
    let mut checksum = Uint::ZERO;
    let start = Instant::now();
    ENABLED.store(allocations, Ordering::Relaxed);
    for _ in 0..ROUNDS {
        for (d, n, _) in &rows {
            checksum = checksum.wrapping_add(&std::hint::black_box(
                weak_float::discount(std::hint::black_box(*d), *n).unwrap(),
            ));
        }
    }
    ENABLED.store(false, Ordering::Relaxed);
    let elapsed = start.elapsed().as_nanos();
    let bytes = BYTES.load(Ordering::Relaxed);
    let calls = CALLS.load(Ordering::Relaxed);
    let want = (0..ROUNDS).fold(Uint::ZERO, |sum, _| sum.wrapping_add(&expected));
    assert_eq!(checksum, want);
    let mut out = json!({"mode":if allocations{"allocations"}else{"timing"},"cases":rows.len(),"rounds":ROUNDS,"operations":rows.len()*ROUNDS,"checksum":format!("{:X}",checksum),"fully_validated":false});
    if allocations {
        out["gross_allocated_bytes"] = bytes.into();
        out["allocation_requests"] = calls.into();
    } else {
        out["ns_per_weight"] = (elapsed as u64 / (rows.len() * ROUNDS) as u64).into();
    }
    println!("{out}");
}
