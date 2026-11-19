//! Build the full dataset of one real epoch and recompute inputs on it and on
//! the light cache; every result must be equal. Prints one JSON line.
//!
//!     pow-full-check progpow|kawpow HEIGHT INPUTS THREADS [--force]
//!
//! The inputs are made up (seal, nonce, height inside the epoch); the epoch
//! cache and the dataset are the real ones of HEIGHT. Without --force the
//! dataset is built only when the free memory holds it and 64 MiB more.
//! Stderr carries "pow-full-check: <phase> start|end" markers for a driver
//! that samples the process CPU time.
use quai_consensus::pow::{available_memory, Algorithm, FullDataset, LightCache};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!("usage: pow-full-check progpow|kawpow HEIGHT INPUTS THREADS [--force]");
        std::process::exit(2);
    }
    let algorithm = match args[0].as_str() {
        "progpow" => Algorithm::Progpow,
        "kawpow" => Algorithm::Kawpow,
        other => panic!("unknown algorithm {other}"),
    };
    let height: u64 = args[1].parse().expect("HEIGHT");
    let count: usize = args[2].parse().expect("INPUTS");
    let threads: usize = args[3].parse().expect("THREADS");
    let force = args.iter().any(|a| a == "--force");

    let cache = LightCache::new(algorithm, height).expect("epoch cache");
    let need = cache.dataset_size();
    let free = available_memory();
    if !force && !free.is_some_and(|f| f >= need + (64 << 20)) {
        println!(
            "{}",
            serde_json::json!({"built": false, "dataset_bytes": need, "free_bytes": free})
        );
        return;
    }
    let epoch = height / algorithm.epoch_length();
    let first = epoch * algorithm.epoch_length();
    let mut seed = 0x2545_f491_4f6c_dd1du64 ^ height;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let inputs: Vec<([u8; 32], u64, u64)> = (0..count)
        .map(|_| {
            let mut seal = [0u8; 32];
            for chunk in seal.chunks_exact_mut(8) {
                chunk.copy_from_slice(&next().to_le_bytes());
            }
            let h = (first + next() % algorithm.epoch_length()).min(algorithm.max_height());
            (seal, next(), h)
        })
        .collect();

    eprintln!("pow-full-check: build start");
    let t = Instant::now();
    let dataset = FullDataset::build(&cache, threads).expect("full dataset");
    let build_seconds = t.elapsed().as_secs_f64();
    eprintln!("pow-full-check: build end");

    eprintln!("pow-full-check: light start");
    let t = Instant::now();
    let light: Vec<_> = inputs
        .iter()
        .map(|(s, n, h)| cache.compute_light(*s, *n, *h).expect("light"))
        .collect();
    let light_seconds = t.elapsed().as_secs_f64();
    eprintln!("pow-full-check: light end");

    eprintln!("pow-full-check: full start");
    let t = Instant::now();
    let full: Vec<_> = inputs
        .iter()
        .map(|(s, n, h)| cache.compute_full(&dataset, *s, *n, *h).expect("full"))
        .collect();
    let full_seconds = t.elapsed().as_secs_f64();
    eprintln!("pow-full-check: full end");

    let mismatches = light.iter().zip(&full).filter(|(a, b)| a != b).count();
    let items_equal = (0..64u32)
        .map(|k| (k as u64 * 1_000_003 % (need / 64)) as u32)
        .all(|i| dataset.item(i) == Some(cache.dataset_item(i)));
    println!(
        "{}",
        serde_json::json!({
            "built": true,
            "algorithm": format!("{algorithm:?}"),
            "epoch": epoch,
            "dataset_bytes": dataset.bytes(),
            "free_bytes_before": free,
            "threads": threads,
            "build_seconds": build_seconds,
            "inputs": count,
            "mismatches": mismatches,
            "sampled_items_equal": items_equal,
            "light_ms_per_hash_wall": 1e3 * light_seconds / count as f64,
            "full_ms_per_hash_wall": 1e3 * full_seconds / count as f64,
        })
    );
    if mismatches != 0 || !items_equal {
        std::process::exit(1);
    }
}
