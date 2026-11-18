//! Offline handler/allocation profiler. Instrumentation is confined to this example.
use quai_rpc::{App, Config};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Instant,
};

struct Counted;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, n: usize) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(n as u64, Ordering::Relaxed);
        }
        unsafe { System.realloc(p, layout, n) }
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;
#[derive(Deserialize)]
struct Case {
    name: String,
    request: Value,
    expected: Value,
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("profile-rpc SNAPSHOT CASES ITERATIONS".into());
    }
    let iterations: usize = args[2].parse()?;
    if !(100..=1_000_000).contains(&iterations) {
        return Err("iterations outside 100..1000000".into());
    }
    let cases: Vec<Case> = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    let app = App::new(Config {
        mode: "snapshot".into(),
        snapshot: Some(args[0].clone()),
        ..Config::default()
    })?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let values=rt.block_on(async {
        let mut values=Vec::new();
        for c in cases {
            let body=serde_json::to_vec(&c.request).unwrap();
            let expected=app.handle(&body).await.expect("read response");
            assert_eq!(serde_json::from_slice::<Value>(&expected).unwrap(),c.expected,"{}",c.name);
            for _ in 0..128 {assert_eq!(app.handle(&body).await.as_deref(),Some(expected.as_slice()));}
            let start=Instant::now();
            for _ in 0..iterations {
                let response=app.handle(std::hint::black_box(&body)).await;
                assert_eq!(response.as_deref(),Some(expected.as_slice()),"{}",c.name);
                std::hint::black_box(response);
            }
            let elapsed=start.elapsed().as_secs_f64();
            let allocation_iterations=256;
            ALLOCS.store(0,Ordering::Relaxed);BYTES.store(0,Ordering::Relaxed);
            ACTIVE.store(true,Ordering::Relaxed);
            for _ in 0..allocation_iterations {
                let response=app.handle(&body).await;
                assert_eq!(response.as_deref(),Some(expected.as_slice()));
                std::hint::black_box(response);
            }
            ACTIVE.store(false,Ordering::Relaxed);
            values.push(json!({"name":c.name,"iterations":iterations,"validated_calls":iterations+128+allocation_iterations+1,
                "elapsed_seconds":elapsed,"ns_per_request":elapsed*1e9/iterations as f64,
                "allocation_calls_per_request":ALLOCS.load(Ordering::Relaxed) as f64/allocation_iterations as f64,
                "gross_allocated_bytes_per_request":BYTES.load(Ordering::Relaxed) as f64/allocation_iterations as f64,
                "response_bytes":expected.len(),"allocation_samples":allocation_iterations}));
        }
        values
    });
    println!(
        "{}",
        serde_json::to_string(
            &json!({"passed":true,"workloads":values,"scope":"Single-thread in-process handler with a test-only allocation wrapper. Every reply matches a structurally validated frozen reference response. Timings include byte comparison, exclude HTTP, sockets, external client parsing and JSON reference comparison. Allocation pass is separate from timing; counts include reallocations and gross requested bytes, not live memory or RSS. No Go runtime."})
        )?
    );
    Ok(())
}
