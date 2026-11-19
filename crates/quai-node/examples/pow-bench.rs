//! Recompute the ProgPoW proof of every block header and workshare in a range
//! of a QEX1 export, single-threaded, and print one line per input in the
//! format of the Go benchmark (`reference/replay-bench`, mode `powbench`):
//!
//!     zone index variant height seal nonce mix hash verdict
//!
//! verdict 0: the computed mix is not the claimed mix; 1: the mix matches but
//! the hash is above the block target (2^256 / difficulty); 2: valid.
//!
//!     pow-bench EXPORT FROM TO [--variants] [--batch] [--out FILE] [--full] [--threads N] [--force]
//!
//! --variants adds, per header, the nonce with its low bit flipped, the seal
//! with its last bit flipped, a claimed mix with one bit flipped, and the
//! same seal and nonce in each later ProgPoW epoch. --batch also computes
//! every input again with the batched kernels of `LightCache::prefetch`, per
//! zone height, and compares the results. --full also builds the
//! full dataset of the range's epoch (only when the free memory holds it and
//! a 1 GiB margin, unless --force) and recomputes every input of that epoch
//! from it; each result must equal the light result.
use quai_consensus::history::Record;
use quai_consensus::pow::{
    available_memory, Algorithm, FullDataset, LightCache, DAG_MEMORY_MARGIN,
};
use quai_node::qex::{ExportReader, Manifest};
use quai_primitives::{encode_hex, Hash};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

struct Input {
    zone: u64,
    index: usize,
    variant: String,
    height: u64,
    seal: Hash,
    nonce: u64,
    claimed: Hash,
    difficulty: Vec<u8>,
}

fn verdict(input: &Input, mix: &Hash, hash: &Hash) -> u8 {
    if *mix != input.claimed {
        return 0;
    }
    match quai_primitives::meets_blake3_target(hash, &input.difficulty) {
        Ok(true) => 2,
        _ => 1,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: pow-bench EXPORT FROM TO [--variants] [--out FILE] [--full] [--threads N] [--force]");
        std::process::exit(2);
    }
    let export = Path::new(&args[0]);
    let from: u64 = args[1].parse().expect("FROM");
    let to: u64 = args[2].parse().expect("TO");
    let flag = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let variants = flag("--variants");
    let full = flag("--full");
    let force = flag("--force");
    let threads: usize = value("--threads")
        .map(|v| v.parse().expect("--threads"))
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));

    let manifest = Manifest::load(export).unwrap_or_else(|e| panic!("{e}"));
    let mut reader = ExportReader::starting_at(export, manifest, from, Some(to));
    let mut inputs: Vec<Input> = vec![];
    let mut auxpow = 0usize;
    let epoch_length = Algorithm::Progpow.epoch_length();
    while let Some(h) = reader.next_height().unwrap_or_else(|e| panic!("{e}")) {
        if h.zone_height < from {
            continue;
        }
        let record = Record::decode(&h.zone().block).unwrap_or_else(|e| panic!("{e:?}"));
        let mut headers = vec![record.header().clone()];
        headers.extend(record.uncles().unwrap_or_else(|e| panic!("{e:?}")));
        for (index, header) in headers.iter().enumerate() {
            let w = header.wire();
            if w.aux_pow.is_some() {
                auxpow += 1;
                continue;
            }
            let height = quai_consensus::share_rules::number(
                w.prime_terminus_number.as_deref().unwrap_or_default(),
            )
            .expect("prime terminus number");
            let mut claimed = [0u8; 32];
            let m = w.mix_hash.as_ref().map_or(&[][..], |m| m.value.as_slice());
            claimed[32 - m.len().min(32)..].copy_from_slice(&m[m.len().saturating_sub(32)..]);
            let base = Input {
                zone: h.zone_height,
                index,
                variant: "orig".into(),
                height,
                seal: header.seal_hash(),
                nonce: w.nonce.unwrap_or_default(),
                claimed,
                difficulty: w.difficulty.clone().unwrap_or_default(),
            };
            let with = |variant: &str, f: &dyn Fn(&mut Input)| {
                let mut i = Input {
                    variant: variant.into(),
                    difficulty: base.difficulty.clone(),
                    ..base
                };
                f(&mut i);
                i
            };
            if variants {
                let extra = [
                    with("nonce", &|i| i.nonce ^= 1),
                    with("seal", &|i| i.seal[31] ^= 1),
                    with("mix", &|i| i.claimed[0] ^= 1),
                ];
                let epochs: Vec<Input> = (1..=3u64)
                    .map(|k| with(&format!("e{k}"), &|i| i.height += k * epoch_length))
                    .filter(|i| i.height <= Algorithm::Progpow.max_height())
                    .collect();
                inputs.push(base);
                inputs.extend(extra);
                inputs.extend(epochs);
            } else {
                inputs.push(base);
            }
        }
    }
    let mut caches: BTreeMap<u64, LightCache> = BTreeMap::new();
    let started = Instant::now();
    for i in &inputs {
        let e = i.height / epoch_length;
        caches
            .entry(e)
            .or_insert_with(|| LightCache::new(Algorithm::Progpow, i.height).expect("epoch cache"));
    }
    let cache_seconds = started.elapsed().as_secs_f64();

    // Light path, single-threaded.
    let mut light = Vec::with_capacity(inputs.len());
    let mut orig_time = Duration::ZERO;
    let mut orig = 0usize;
    // Markers for a driver that samples the process CPU time.
    eprintln!("pow-bench: light loop start");
    for i in &inputs {
        let c = &caches[&(i.height / epoch_length)];
        let t = Instant::now();
        let r = c
            .compute_light(i.seal, i.nonce, i.height)
            .expect("light kernel");
        if i.variant == "orig" {
            orig_time += t.elapsed();
            orig += 1;
        }
        light.push(r);
    }
    eprintln!("pow-bench: light loop end");

    // Batched light kernels: per zone height, the block header and its
    // workshares together (LightCache::prefetch), as the replay's workers
    // do; each result must equal the single kernel's.
    let mut batch_mismatches = 0usize;
    let mut batch_time = Duration::ZERO;
    if flag("--batch") {
        eprintln!("pow-bench: batch loop start");
        let t = Instant::now();
        let mut start = 0;
        while start < inputs.len() {
            let mut end = start;
            while end < inputs.len() && inputs[end].zone == inputs[start].zone {
                end += 1;
            }
            let group = &inputs[start..end];
            let mut by_epoch: BTreeMap<u64, Vec<(Hash, u64, u64)>> = BTreeMap::new();
            for i in group {
                by_epoch
                    .entry(i.height / epoch_length)
                    .or_default()
                    .push((i.seal, i.nonce, i.height));
            }
            for (e, v) in &by_epoch {
                caches[e].prefetch(v);
            }
            for (k, i) in group.iter().enumerate() {
                let c = &caches[&(i.height / epoch_length)];
                let r = c
                    .compute(i.seal, i.nonce, i.height)
                    .expect("batched kernel");
                if r != light[start + k] {
                    batch_mismatches += 1;
                }
            }
            start = end;
        }
        batch_time = t.elapsed();
        eprintln!("pow-bench: batch loop end");
    }

    // Full path, for the inputs in the first input's epoch.
    let mut full_report = serde_json::Value::Null;
    if full {
        let epoch = inputs.first().map(|i| i.height / epoch_length).unwrap_or(0);
        let cache = &caches[&epoch];
        let need = cache.dataset_size();
        let free = available_memory();
        let fits = free.is_some_and(|f| f >= need + DAG_MEMORY_MARGIN);
        if !fits && !force {
            full_report = serde_json::json!({
                "built": false,
                "reason": "not enough free memory",
                "dataset_bytes": need,
                "free_bytes": free,
            });
        } else {
            eprintln!("pow-bench: dataset build start");
            let t = Instant::now();
            let dataset = FullDataset::build(cache, threads).expect("full dataset");
            let build_seconds = t.elapsed().as_secs_f64();
            eprintln!("pow-bench: dataset build end");
            eprintln!("pow-bench: full loop start");
            let mut mismatches = 0usize;
            let mut compared = 0usize;
            let mut full_time = Duration::ZERO;
            let mut full_orig = 0usize;
            for (i, l) in inputs.iter().zip(&light) {
                if i.height / epoch_length != epoch {
                    continue;
                }
                let t = Instant::now();
                let r = cache
                    .compute_full(&dataset, i.seal, i.nonce, i.height)
                    .expect("full kernel");
                if i.variant == "orig" {
                    full_time += t.elapsed();
                    full_orig += 1;
                }
                compared += 1;
                if r != *l {
                    mismatches += 1;
                    eprintln!(
                        "full/light mismatch at zone {} index {} {}",
                        i.zone, i.index, i.variant
                    );
                }
            }
            eprintln!("pow-bench: full loop end");
            full_report = serde_json::json!({
                "built": true,
                "epoch": epoch,
                "dataset_bytes": dataset.bytes(),
                "free_bytes_before": free,
                "threads": threads,
                "build_seconds": build_seconds,
                "compared": compared,
                "mismatches": mismatches,
                "full_us_per_hash": full_time.as_secs_f64() * 1e6 / full_orig.max(1) as f64,
            });
        }
    }

    let mut out = value("--out")
        .map(|p| std::io::BufWriter::new(std::fs::File::create(p).expect("--out file")));
    let mut digest = Sha256::new();
    let mut verdicts: BTreeMap<String, [usize; 3]> = BTreeMap::new();
    for (i, (mix, hash)) in inputs.iter().zip(&light) {
        let v = verdict(i, mix, hash);
        verdicts.entry(i.variant.clone()).or_default()[v as usize] += 1;
        let line = format!(
            "{} {} {} {} {} {} {} {} {}\n",
            i.zone,
            i.index,
            i.variant,
            i.height,
            encode_hex(&i.seal).trim_start_matches("0x"),
            i.nonce,
            encode_hex(mix).trim_start_matches("0x"),
            encode_hex(hash).trim_start_matches("0x"),
            v
        );
        digest.update(line.as_bytes());
        if let Some(o) = out.as_mut() {
            o.write_all(line.as_bytes()).expect("write --out");
        }
    }
    if let Some(mut o) = out {
        o.flush().expect("flush --out");
    }
    let report = serde_json::json!({
        "mode": "pow-bench",
        "from": from,
        "to": to,
        "inputs": inputs.len(),
        "auxpow_headers": auxpow,
        "original": orig,
        "epoch_caches_seconds": cache_seconds,
        "light_us_per_hash": orig_time.as_secs_f64() * 1e6 / orig.max(1) as f64,
        "batch": flag("--batch").then(|| serde_json::json!({
            "mismatches": batch_mismatches,
            "us_per_hash": batch_time.as_secs_f64() * 1e6 / inputs.len().max(1) as f64,
        })),
        "verdicts": verdicts,
        "sha256": encode_hex(&digest.finalize()).trim_start_matches("0x"),
        "full": full_report,
    });
    println!("{report}");
}
