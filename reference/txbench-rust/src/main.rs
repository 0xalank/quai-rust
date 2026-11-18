//! Offline transaction pipeline benchmark. Never part of the shipping workspace.
use quai_primitives::{
    decode_hex,
    transaction::{Kind, Transaction},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{self, BufRead, Write},
    sync::Barrier,
    time::Instant,
};

#[cfg(feature = "alloc-count")]
mod allocation {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    pub static ACTIVE: AtomicBool = AtomicBool::new(false);
    pub static CALLS: AtomicU64 = AtomicU64::new(0);
    pub static BYTES: AtomicU64 = AtomicU64::new(0);
    pub struct Counted;
    unsafe impl GlobalAlloc for Counted {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if ACTIVE.load(Ordering::Relaxed) {
                CALLS.fetch_add(1, Ordering::Relaxed);
                BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
            }
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            if ACTIVE.load(Ordering::Relaxed) {
                CALLS.fetch_add(1, Ordering::Relaxed);
                BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
            }
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            if ACTIVE.load(Ordering::Relaxed) {
                CALLS.fetch_add(1, Ordering::Relaxed);
                BYTES.fetch_add(size as u64, Ordering::Relaxed);
            }
            unsafe { System.realloc(p, layout, size) }
        }
        unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
            unsafe { System.dealloc(p, layout) }
        }
    }
}
#[cfg(feature = "alloc-count")]
#[global_allocator]
static ALLOCATOR: allocation::Counted = allocation::Counted;

struct Fixture {
    input: Vec<u8>,
    decoded: Transaction,
    sender: Option<[u8; 20]>,
    digest: [u8; 32],
    aggregate: Option<Vec<u8>>,
    signature_valid: Option<bool>,
}
fn prepare(path: &str) -> Result<(Vec<Fixture>, HashMap<String, Vec<usize>>, Value), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let report = quai_primitives::vectors::verify(&bytes)?;
    if report["passed"] != true {
        return Err(report.to_string());
    }
    let corpus: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let mut fixtures = Vec::new();
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for v in corpus["transactions"].as_array().ok_or("missing vectors")? {
        let input = decode_hex(v["input"].as_str().ok_or("missing input")?)?;
        let decoded = Transaction::decode(&input)?;
        let sender = v["sender"]
            .as_str()
            .map(decode_hex)
            .transpose()?
            .map(|v| v.try_into().map_err(|_| "bad sender"))
            .transpose()?;
        let digest = decode_hex(v["signing_hash"].as_str().ok_or("missing digest")?)?
            .try_into()
            .map_err(|_| "bad digest")?;
        let aggregate = v["aggregate_key"].as_str().map(decode_hex).transpose()?;
        let valid = v["signature_valid"].as_bool();
        let index = fixtures.len();
        let names: Vec<&str> = match decoded.kind() {
            Kind::Quai(_) if sender.is_some() => {
                vec!["quai_full", "quai_decode", "quai_signing_hash"]
            }
            Kind::Qi(q) if valid == Some(true) => {
                if q.inputs.len() == 1 {
                    vec!["qi_single_full", "qi_decode", "qi_signing_hash"]
                } else {
                    vec![
                        "qi_multi_full",
                        "qi_decode",
                        "qi_signing_hash",
                        "qi_aggregate",
                    ]
                }
            }
            Kind::Qi(_) => vec!["qi_invalid_full"],
            _ => vec![],
        };
        for name in names {
            groups.entry(name.to_owned()).or_default().push(index);
        }
        fixtures.push(Fixture {
            input,
            decoded,
            sender,
            digest,
            aggregate,
            signature_valid: valid,
        });
    }
    Ok((fixtures, groups, report))
}
fn operation(name: &str, f: &Fixture) -> Result<u64, String> {
    let result = match name {
        "quai_full" => {
            let tx = Transaction::decode(std::hint::black_box(&f.input))?;
            let actual = tx.sender()?;
            if Some(actual) != f.sender {
                return Err("sender mismatch".into());
            }
            actual[0] as u64
        }
        "qi_single_full" | "qi_multi_full" | "qi_invalid_full" => {
            let tx = Transaction::decode(std::hint::black_box(&f.input))?;
            let actual = tx.verify_qi_signature()?;
            if Some(actual) != f.signature_valid {
                return Err("signature result mismatch".into());
            }
            u64::from(actual)
        }
        "quai_decode" | "qi_decode" => {
            let tx = Transaction::decode(std::hint::black_box(&f.input))?;
            if tx.type_id() != f.decoded.type_id() {
                return Err("decoded type mismatch".into());
            }
            let result = tx.type_id() as u64;
            std::hint::black_box(tx);
            result
        }
        "quai_signing_hash" | "qi_signing_hash" => {
            let actual = f.decoded.signing_hash();
            if actual != f.digest {
                return Err("signing digest mismatch".into());
            }
            actual[0] as u64
        }
        "qi_aggregate" => {
            let actual = f.decoded.qi_aggregate_key()?;
            if Some(&actual) != f.aggregate.as_ref() {
                return Err("aggregate key mismatch".into());
            }
            actual[0] as u64
        }
        _ => return Err("unknown workload".into()),
    };
    Ok(std::hint::black_box(result))
}
fn measure(
    command: &Value,
    fixtures: &[Fixture],
    groups: &HashMap<String, Vec<usize>>,
) -> Result<Value, String> {
    let name = command["workload"].as_str().ok_or("missing workload")?;
    let workers = command["workers"].as_u64().ok_or("missing workers")? as usize;
    let count = command["iterations"].as_u64().ok_or("missing iterations")? as usize;
    if ![1, 4].contains(&workers) || !(32..=2_000_000).contains(&count) {
        return Err("invalid limits".into());
    }
    let indices = groups.get(name).ok_or("unknown group")?;
    for i in 0..32 {
        operation(name, &fixtures[indices[i % indices.len()]])?;
    }
    if command["measure"] == "alloc" {
        if workers != 1 {
            return Err("allocation measurement is sequential".into());
        }
        #[cfg(feature = "alloc-count")]
        {
            use std::sync::atomic::Ordering;
            allocation::CALLS.store(0, Ordering::Relaxed);
            allocation::BYTES.store(0, Ordering::Relaxed);
            allocation::ACTIVE.store(true, Ordering::Relaxed);
            let mut checksum = 0u64;
            let mut error = None;
            for i in 0..count {
                match operation(name, &fixtures[indices[i % indices.len()]]) {
                    Ok(v) => checksum = checksum.wrapping_add(v),
                    Err(e) => {
                        error = Some(e);
                        break;
                    }
                }
            }
            allocation::ACTIVE.store(false, Ordering::Relaxed);
            if let Some(e) = error {
                return Err(e);
            }
            return Ok(
                json!({"passed":true,"workload":name,"workers":workers,"iterations":count,"measure":"alloc","allocation_calls_per_transaction":allocation::CALLS.load(Ordering::Relaxed) as f64/count as f64,"gross_allocated_bytes_per_transaction":allocation::BYTES.load(Ordering::Relaxed) as f64/count as f64,"checksum":checksum,"validated_transactions":count}),
            );
        }
        #[cfg(not(feature = "alloc-count"))]
        return Err("use the separate alloc-count binary".into());
    }
    let ready = Barrier::new(workers + 1);
    let start = Barrier::new(workers + 1);
    let (elapsed, checksum) = std::thread::scope(|scope| -> Result<_, String> {
        let handles = (0..workers)
            .map(|worker| {
                let (ready, start) = (&ready, &start);
                scope.spawn(move || -> Result<u64, String> {
                    ready.wait();
                    start.wait();
                    let mut checksum = 0u64;
                    for i in (worker..count).step_by(workers) {
                        checksum = checksum
                            .wrapping_add(operation(name, &fixtures[indices[i % indices.len()]])?);
                    }
                    Ok(checksum)
                })
            })
            .collect::<Vec<_>>();
        ready.wait();
        let now = Instant::now();
        start.wait();
        let mut checksum = 0u64;
        for h in handles {
            checksum = checksum.wrapping_add(h.join().map_err(|_| "worker panic")??);
        }
        Ok((now.elapsed().as_secs_f64(), checksum))
    })?;
    Ok(
        json!({"passed":true,"workload":name,"workers":workers,"iterations":count,"measure":"time","elapsed_seconds":elapsed,"ns_per_transaction":elapsed*1e9/count as f64,"transactions_per_second":count as f64/elapsed,"checksum":checksum,"validated_transactions":count}),
    )
}
fn main() -> Result<(), String> {
    let path = std::env::args().nth(1).ok_or("tx-pipeline-bench CORPUS")?;
    let (fixtures, groups, report) = prepare(&path)?;
    let counts = groups
        .iter()
        .map(|(k, v)| (k.clone(), json!(v.len())))
        .collect::<serde_json::Map<_, _>>();
    println!(
        "{}",
        json!({"ready":true,"language":"rust","allocation_instrumented":cfg!(feature="alloc-count"),"groups":counts,"preflight":report,"crypto_backend":"pure Rust k256 0.13.4"})
    );
    io::stdout().flush().map_err(|e| e.to_string())?;
    for line in io::stdin().lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        let command: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if command["quit"] == true {
            break;
        }
        let result = match measure(&command, &fixtures, &groups) {
            Ok(v) => v,
            Err(e) => json!({"passed":false,"error":e}),
        };
        println!("{}", result);
        io::stdout().flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}
