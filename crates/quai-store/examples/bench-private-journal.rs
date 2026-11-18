//! Fixed private journal switches. Gross allocation sampling is a separate run.
use quai_store::journal::{Checkpoint, ScratchJournal};
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
fn main() {
    let allocations = std::env::args()
        .nth(1)
        .is_some_and(|v| v == "--allocations");
    let directory = std::env::temp_dir().join(format!(
        "quai-journal-bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let mut store = ScratchJournal::open(&directory, [31; 32]).unwrap();
    let size = 256 * 1024;
    let switches = 8;
    let mut stage = |parent, height: u64, branch: u8| {
        let mut block = [0; 32];
        block[..8].copy_from_slice(&height.to_be_bytes());
        block[8] = branch;
        let mut payload = vec![height as u8; size];
        payload[0] = branch;
        store
            .stage(&Checkpoint {
                block,
                parent,
                height,
                payload,
            })
            .unwrap()
    };
    let mut shared = None;
    for height in 0..48 {
        shared = Some(stage(shared, height, 0));
    }
    let mut a = shared;
    let mut b = shared;
    for height in 48..64 {
        a = Some(stage(a, height, 1));
        b = Some(stage(b, height, 2));
    }
    let a = a.unwrap();
    let b = b.unwrap();
    store.publish(None, a).unwrap();
    let path = store.reorg_path(b).unwrap();
    assert_eq!((path.0.len(), path.1.len()), (16, 16));
    let start = Instant::now();
    ENABLED.store(allocations, Ordering::Relaxed);
    for n in 0..switches {
        let next = if n % 2 == 0 { b } else { a };
        store
            .publish(store.head(), std::hint::black_box(next))
            .unwrap();
    }
    ENABLED.store(false, Ordering::Relaxed);
    let elapsed = start.elapsed().as_nanos() / switches as u128;
    let bytes = BYTES.load(Ordering::Relaxed) / switches;
    let calls = CALLS.load(Ordering::Relaxed) / switches;
    assert_eq!(store.head(), Some(a));
    let cp = store.load(a).unwrap();
    assert_eq!(cp.height, 63);
    assert_eq!(cp.payload.len(), size);
    let mut report = serde_json::json!({"mode":if allocations {"allocations"}else{"timing"},"depth":64,"shared_checkpoints":48,"payload_bytes":size,"switches":switches,"final_head":a.iter().map(|v|format!("{v:02x}")).collect::<String>(),"final_height":cp.height,"undo_blocks":16,"forward_blocks":16});
    if allocations {
        report["gross_allocated_bytes_per_switch"] = bytes.into();
        report["allocation_requests_per_switch"] = calls.into();
    } else {
        report["ns_per_switch"] = (elapsed as u64).into();
    }
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
    println!("{report}");
}
