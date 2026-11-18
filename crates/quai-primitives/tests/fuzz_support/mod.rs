//! Deterministic fuzz-style harness for stable Rust. Shared by integration
//! tests through `#[path]`. It has a splitmix64 generator, byte mutators, a
//! per-thread peak-allocation counter, and a fixture hex corpus reader.
#![allow(dead_code)]
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};

pub struct Counting;
thread_local! {
    static CUR: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}
fn add(n: usize) {
    CUR.with(|c| {
        let v = c.get() + n;
        c.set(v);
        PEAK.with(|p| p.set(p.get().max(v)));
    });
}
fn sub(n: usize) {
    CUR.with(|c| c.set(c.get().saturating_sub(n)));
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        add(l.size());
        System.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        add(l.size());
        System.alloc_zeroed(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        sub(l.size());
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if n >= l.size() {
            add(n - l.size())
        } else {
            sub(l.size() - n)
        }
        System.realloc(p, l, n)
    }
}
#[global_allocator]
static ALLOC: Counting = Counting;

/// Peak extra live bytes while `f` runs on this thread.
pub fn peak_during<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let base = CUR.with(|c| c.get());
    PEAK.with(|p| p.set(base));
    let r = f();
    let peak = PEAK.with(|p| p.get());
    (r, peak.saturating_sub(base))
}

pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    pub fn byte(&mut self) -> u8 {
        self.next() as u8
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.byte()).collect()
    }
}

const SPECIAL: [u8; 9] = [0x00, 0x01, 0x7f, 0x80, 0xff, 0xfe, 0x0a, 0x12, 0x22];

/// One to three structural or bit-level mutations of `seed`.
pub fn mutate(rng: &mut Rng, seed: &[u8], pool: &[Vec<u8>]) -> Vec<u8> {
    let mut v = seed.to_vec();
    for _ in 0..1 + rng.below(3) {
        let len = v.len();
        match rng.below(11) {
            0 if len > 0 => {
                for _ in 0..1 + rng.below(3) {
                    let i = rng.below(len);
                    v[i] ^= 1 << rng.below(8);
                }
            }
            1 if len > 0 => {
                let i = rng.below(len);
                v[i] = rng.byte();
            }
            2 if len > 0 => v.truncate(rng.below(len)),
            3 => {
                let n = 1 + rng.below(64);
                v.extend(rng.bytes(n));
            }
            4 if len > 0 => {
                // length-field style edits: small steps, special bytes, huge varints
                let i = rng.below(len);
                match rng.below(4) {
                    0 => v[i] = v[i].wrapping_add(1),
                    1 => v[i] = v[i].wrapping_sub(1),
                    2 => v[i] = SPECIAL[rng.below(SPECIAL.len())],
                    _ => {
                        let big = [0xff, 0xff, 0xff, 0xff, 0x0f];
                        let end = (i + big.len()).min(len);
                        v[i..end].copy_from_slice(&big[..end - i]);
                    }
                }
            }
            5 if len > 1 => {
                let a = rng.below(len);
                let b = (a + 1 + rng.below(16)).min(len);
                v.drain(a..b);
            }
            6 if len > 1 => {
                let a = rng.below(len);
                let b = (a + 1 + rng.below(32)).min(len);
                let c = v[a..b].to_vec();
                let at = rng.below(v.len() + 1);
                v.splice(at..at, c);
            }
            7 if !pool.is_empty() => {
                let o = &pool[rng.below(pool.len())];
                let cut_a = rng.below(len + 1);
                let cut_b = rng.below(o.len() + 1);
                v.truncate(cut_a);
                v.extend_from_slice(&o[cut_b..]);
            }
            8 => {
                // inject a field with an absurd declared length
                let at = rng.below(len + 1);
                let tag = [0x0a, 0x12, 0x1a, 0x22, 0x2a][rng.below(5)];
                let junk = [tag, 0xff, 0xff, 0xff, 0xff, 0x0f];
                v.splice(at..at, junk);
            }
            9 if len > 1 => {
                let (a, b) = (rng.below(len), rng.below(len));
                v.swap(a, b);
            }
            10 if len > 0 => {
                // repeat a tail chunk many times (amplification attempt)
                let a = rng.below(len);
                let c = v[a..].to_vec();
                for _ in 0..rng.below(40) {
                    v.extend_from_slice(&c);
                    if v.len() > 1 << 20 {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    v
}

fn hex(b: &[u8]) -> String {
    let b = &b[..b.len().min(4096)];
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub struct Stats {
    pub cases: usize,
    pub accepted: usize,
    pub worst_peak: usize,
}

/// Run `f` over random inputs and mutations of `seeds`. A panic or an
/// allocation peak above `budget(len)` fails the test and prints the input.
pub fn drive(
    name: &str,
    seed: u64,
    seeds: &[Vec<u8>],
    iterations: usize,
    budget: impl Fn(usize) -> usize,
    f: impl Fn(&[u8]) -> bool,
) -> Stats {
    // QUAI_FUZZ_SCALE multiplies the case count for longer manual runs.
    let scale = std::env::var("QUAI_FUZZ_SCALE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1);
    let iterations = iterations * scale;
    let mut rng = Rng(seed ^ (scale as u64) << 40);
    let mut stats = Stats {
        cases: 0,
        accepted: 0,
        worst_peak: 0,
    };
    for i in 0..iterations {
        let input = if seeds.is_empty() || rng.below(10) == 0 {
            let n = match rng.below(4) {
                0 => rng.below(8),
                1 => rng.below(64),
                2 => rng.below(300),
                _ => rng.below(2000),
            };
            rng.bytes(n)
        } else {
            let s = &seeds[rng.below(seeds.len())];
            mutate(&mut rng, s, seeds)
        };
        let (r, peak) = peak_during(|| catch_unwind(AssertUnwindSafe(|| f(&input))));
        match r {
            Err(_) => panic!("{name}: panic on case {i} ({} bytes): {}", input.len(), hex(&input)),
            Ok(ok) => stats.accepted += usize::from(ok),
        }
        let allowed = budget(input.len());
        assert!(
            peak <= allowed,
            "{name}: case {i} allocated {peak} bytes for {} input bytes (limit {allowed}): {}",
            input.len(),
            hex(&input)
        );
        stats.worst_peak = stats.worst_peak.max(peak);
        stats.cases += 1;
    }
    // Seeds themselves must also be handled, unmodified and with every prefix cut.
    for s in seeds {
        let _ = catch_unwind(AssertUnwindSafe(|| f(s)))
            .unwrap_or_else(|_| panic!("{name}: panic on seed {}", hex(s)));
        for cut in [0, 1, s.len() / 2, s.len().saturating_sub(1)] {
            if cut < s.len() {
                let _ = catch_unwind(AssertUnwindSafe(|| f(&s[..cut])))
                    .unwrap_or_else(|_| panic!("{name}: panic on cut {cut} of seed {}", hex(s)));
            }
        }
    }
    eprintln!(
        "{name}: {} cases, {} accepted, {} seeds, worst peak {} bytes",
        stats.cases,
        stats.accepted,
        seeds.len(),
        stats.worst_peak
    );
    stats
}

/// All `0x...` hex strings in JSON documents, as bytes.
pub fn hex_corpus(docs: &[&[u8]]) -> Vec<Vec<u8>> {
    fn walk(v: &serde_json::Value, out: &mut Vec<Vec<u8>>) {
        match v {
            serde_json::Value::String(s) => {
                if let Some(h) = s.strip_prefix("0x") {
                    if h.len() >= 2 && h.len() % 2 == 0 && h.len() <= 1 << 20 {
                        if let Some(b) = unhex(h) {
                            out.push(b);
                        }
                    }
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            serde_json::Value::Object(o) => o.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = vec![];
    for d in docs {
        let v: serde_json::Value = serde_json::from_slice(d).unwrap();
        walk(&v, &mut out);
    }
    out.sort();
    out.dedup();
    out
}
fn unhex(h: &str) -> Option<Vec<u8>> {
    h.as_bytes()
        .chunks(2)
        .map(|p| {
            let d = |c: u8| (c as char).to_digit(16);
            Some((d(p[0])? * 16 + d(p[1])?) as u8)
        })
        .collect()
}

/// Keep seeds `keep` accepts; thin to at most `max` evenly spaced entries,
/// each at most `max_len` bytes.
pub fn select(corpus: &[Vec<u8>], max: usize, max_len: usize, keep: impl Fn(&[u8]) -> bool) -> Vec<Vec<u8>> {
    let all: Vec<_> = corpus
        .iter()
        .filter(|b| b.len() <= max_len && keep(b))
        .cloned()
        .collect();
    if all.len() <= max {
        return all;
    }
    let step = all.len() as f64 / max as f64;
    (0..max).map(|i| all[(i as f64 * step) as usize].clone()).collect()
}
