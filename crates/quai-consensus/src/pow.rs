//! Pinned ProgPoW and KAWPOW kernels, including native cache/DAG generation.
//! KAWPOW seal input uses natural SHA256 byte order; header-path reversal is here.
//!
//! Each of the kernel's 64 rounds reads four 64-byte dataset items. The light
//! path computes each item from 512 rows of the epoch cache, as go-quai's
//! `progpowLight` does. A [`FullDataset`] holds every item of one epoch in
//! memory, as `progpowFull` reads them; it is built here from the same cache
//! with the same item function, never read from a file or a peer. The kernel
//! is the same code for both paths.
use crate::{Error, VerifiedPow};
use quai_primitives::Hash;
use sha3::{Digest, Keccak256, Keccak512};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algorithm {
    Progpow,
    Kawpow,
}
impl Algorithm {
    pub fn epoch_length(self) -> u64 {
        match self {
            Self::Progpow => 388_800,
            Self::Kawpow => 7500,
        }
    }
    pub fn max_height(self) -> u64 {
        match self {
            Self::Progpow => 1_301_100,
            Self::Kawpow => 14_000_000,
        }
    }
    fn cache_cap(self) -> u64 {
        match self {
            Self::Progpow => 64 << 20,
            Self::Kawpow => 256 << 20,
        }
    }
    pub fn sizes(self, height: u64) -> Result<(u64, u64), Error> {
        if height > self.max_height() {
            return Err(Error::Invalid("proof height exceeds pinned bound"));
        }
        let epoch = height / self.epoch_length();
        let (cache_growth, dag_init, dag_growth) = match self {
            Self::Progpow => (1u64 << 21, 1u64 << 32, 1u64 << 26),
            Self::Kawpow => (1u64 << 17, 1u64 << 30, 1u64 << 23),
        };
        let adjust = |base: u64, unit: u64| {
            let mut n = base / unit - 1;
            while !prime(n) {
                n -= 2;
            }
            n * unit
        };
        let cache = adjust((1 << 24) + epoch * cache_growth, 64);
        let dag = adjust(dag_init + epoch * dag_growth, 128);
        if cache > self.cache_cap() {
            return Err(Error::ResourceLimit("verification cache"));
        }
        Ok((cache, dag))
    }
}
fn prime(n: u64) -> bool {
    if n < 2 {
        return false;
    }
    if n % 2 == 0 {
        return n == 2;
    }
    let mut d = 3;
    while d <= n / d {
        if n % d == 0 {
            return false;
        }
        d += 2;
    }
    true
}
fn encode_words<const N: usize>(v: &[u32; N]) -> Vec<u8> {
    v.iter().flat_map(|n| n.to_le_bytes()).collect()
}
fn decode_words<const N: usize>(b: &[u8]) -> [u32; N] {
    std::array::from_fn(|i| {
        u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]])
    })
}
fn words_to_hash(v: &[u32]) -> Hash {
    let mut h = [0u8; 32];
    for (chunk, w) in h.chunks_exact_mut(4).zip(v) {
        chunk.copy_from_slice(&w.to_le_bytes());
    }
    h
}
fn hash512(b: &[u8]) -> [u32; 16] {
    decode_words(&Keccak512::digest(b))
}
/// Keccak-512 of 16 little-endian words, without a heap buffer.
fn hash512_words(v: &[u32; 16]) -> [u32; 16] {
    let mut bytes = [0u8; 64];
    for (chunk, w) in bytes.chunks_exact_mut(4).zip(v) {
        chunk.copy_from_slice(&w.to_le_bytes());
    }
    decode_words(&Keccak512::digest(bytes))
}
fn fnv(a: u32, b: u32) -> u32 {
    a.wrapping_mul(0x01000193) ^ b
}
fn fnv1a(h: &mut u32, d: u32) -> u32 {
    *h = (*h ^ d).wrapping_mul(0x01000193);
    *h
}
/// Owns one bounded epoch cache, generated locally; no peer-supplied cache bytes.
/// A full dataset of the same epoch can be attached and detached at any time;
/// the kernel then reads its items instead of computing them.
pub struct LightCache {
    algorithm: Algorithm,
    epoch: u64,
    cache: Vec<[u32; 16]>,
    cdag: [u32; 4096],
    dataset_size: u64,
    full: RwLock<Option<Arc<FullDataset>>>,
    prefetched: Mutex<std::collections::VecDeque<KernelResult>>,
}
type KernelResult = ((Hash, u64, u64), (Hash, Hash));
/// Where the kernel takes the four items of a round from.
trait Loads {
    fn load(&self, cache: &LightCache, index: [u32; 4]) -> [[u32; 16]; 4];
}
struct LightLoads;
impl Loads for LightLoads {
    fn load(&self, cache: &LightCache, index: [u32; 4]) -> [[u32; 16]; 4] {
        cache.items(index)
    }
}
impl Loads for FullDataset {
    fn load(&self, cache: &LightCache, index: [u32; 4]) -> [[u32; 16]; 4] {
        // Every round index is below the item count; the light item is a
        // fallback that keeps the result equal if that ever did not hold.
        index.map(|i| {
            self.items
                .get(i as usize)
                .copied()
                .unwrap_or_else(|| cache.item(i))
        })
    }
}
impl LightCache {
    pub fn new(algorithm: Algorithm, height: u64) -> Result<Self, Error> {
        let (size, dataset_size) = algorithm.sizes(height)?;
        let epoch = height / algorithm.epoch_length();
        let mut seed = [0u8; 32];
        for _ in 0..epoch {
            seed = Keccak256::digest(seed).into();
        }
        let rows = size as usize / 64;
        let mut cache = vec![[0; 16]; rows];
        cache[0] = hash512(&seed);
        for i in 1..rows {
            cache[i] = hash512_words(&cache[i - 1]);
        }
        for _ in 0..3 {
            for i in 0..rows {
                let previous = (i + rows - 1) % rows;
                let other = cache[i][0] as usize % rows;
                let words: [u32; 16] =
                    std::array::from_fn(|j| cache[previous][j] ^ cache[other][j]);
                cache[i] = hash512_words(&words);
            }
        }
        let mut out = Self {
            algorithm,
            epoch,
            cache,
            cdag: [0; 4096],
            dataset_size,
            full: RwLock::new(None),
            prefetched: Default::default(),
        };
        for i in 0..256 {
            let item = out.item(i as u32);
            out.cdag[i * 16..i * 16 + 16].copy_from_slice(&item);
        }
        Ok(out)
    }
    pub fn dataset_item(&self, index: u32) -> [u8; 64] {
        let mut out = [0u8; 64];
        for (chunk, w) in out.chunks_exact_mut(4).zip(self.item(index)) {
            chunk.copy_from_slice(&w.to_le_bytes());
        }
        out
    }
    pub(crate) fn key(&self) -> (Algorithm, u64) {
        (self.algorithm, self.epoch)
    }
    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn cache_digest(&self) -> Hash {
        let mut h = Keccak256::new();
        for row in &self.cache {
            h.update(encode_words(row));
        }
        h.finalize().into()
    }
    pub fn cdag_digest(&self) -> Hash {
        Keccak256::digest(encode_words(&self.cdag)).into()
    }
    pub fn cache_bytes(&self) -> usize {
        self.cache.len() * 64
    }
    pub fn dataset_size(&self) -> u64 {
        self.dataset_size
    }
    /// One dataset item: 512 dependent reads of cache rows, then Keccak-512
    /// (go-quai's generateDatasetItem).
    fn item(&self, index: u32) -> [u32; 16] {
        self.items([index])[0]
    }
    /// The items `index[k]`, computed in lockstep. Each is exactly
    /// [`item`](Self::item); the K chains of dependent cache reads are
    /// independent, so their memory loads overlap.
    fn items<const K: usize>(&self, index: [u32; K]) -> [[u32; 16]; K] {
        let rows = self.cache.len();
        let mut mix: [[u32; 16]; K] = std::array::from_fn(|k| {
            let mut m = self.cache[index[k] as usize % rows];
            m[0] ^= index[k];
            hash512_words(&m)
        });
        for i in 0..512u32 {
            for k in 0..K {
                let parent = fnv(index[k] ^ i, mix[k][i as usize % 16]) as usize % rows;
                for (v, b) in mix[k].iter_mut().zip(&self.cache[parent]) {
                    *v = fnv(*v, *b);
                }
            }
        }
        std::array::from_fn(|k| hash512_words(&mix[k]))
    }
    /// The items `first..first + out.len()`, eight chains at a time.
    fn fill(&self, out: &mut [[u32; 16]], first: u32) {
        let mut groups = out.chunks_exact_mut(8);
        let mut index = first;
        for group in &mut groups {
            group.copy_from_slice(
                &self.items::<8>(std::array::from_fn(|k| index.wrapping_add(k as u32))),
            );
            index = index.wrapping_add(8);
        }
        for (k, slot) in groups.into_remainder().iter_mut().enumerate() {
            *slot = self.item(index.wrapping_add(k as u32));
        }
    }
    /// The attached full dataset, if any.
    pub fn full(&self) -> Option<Arc<FullDataset>> {
        self.full.read().ok().and_then(|g| g.clone())
    }
    /// Read items from `dataset` from now on. It must be this epoch's.
    pub fn attach(&self, dataset: Arc<FullDataset>) -> Result<(), Error> {
        if dataset.key != self.key() || dataset.bytes() != self.dataset_size {
            return Err(Error::Invalid("full dataset of another epoch"));
        }
        let mut g = self
            .full
            .write()
            .map_err(|_| Error::Unavailable("full dataset slot"))?;
        *g = Some(dataset);
        Ok(())
    }
    /// Go back to the light path. Kernels already running keep their reference.
    pub fn detach(&self) -> Option<Arc<FullDataset>> {
        self.full.write().ok().and_then(|mut g| g.take())
    }
    fn check_height(&self, height: u64) -> Result<(), Error> {
        self.algorithm.sizes(height)?;
        if height / self.algorithm.epoch_length() != self.epoch {
            return Err(Error::Unsupported("different verification epoch"));
        }
        Ok(())
    }
    /// Recompute the complete kernel; returns header-format mix and PoW hash.
    /// This does not check the aux template, donor header or Quai target.
    /// It reads the full dataset when one is attached, else the light cache.
    pub fn compute(&self, seal: Hash, nonce: u64, height: u64) -> Result<(Hash, Hash), Error> {
        self.check_height(height)?;
        Ok(match self.full() {
            Some(full) => self.kernel(&*full, seal, nonce, height),
            None => match self.take_prefetched(seal, nonce, height) {
                Some(r) => r,
                None => self.kernel(&LightLoads, seal, nonce, height),
            },
        })
    }
    /// [`compute`](Self::compute) on the light cache only.
    pub fn compute_light(
        &self,
        seal: Hash,
        nonce: u64,
        height: u64,
    ) -> Result<(Hash, Hash), Error> {
        self.check_height(height)?;
        Ok(self.kernel(&LightLoads, seal, nonce, height))
    }
    /// [`compute`](Self::compute) on `dataset`, which must be this epoch's.
    pub fn compute_full(
        &self,
        dataset: &FullDataset,
        seal: Hash,
        nonce: u64,
        height: u64,
    ) -> Result<(Hash, Hash), Error> {
        self.check_height(height)?;
        if dataset.key != self.key() || dataset.bytes() != self.dataset_size {
            return Err(Error::Invalid("full dataset of another epoch"));
        }
        Ok(self.kernel(dataset, seal, nonce, height))
    }
    /// The PoW hash of an input whose header claims `mix`. The kernel
    /// recomputes the mix, and the claim must equal it (go-quai's
    /// ComputePowHash); a different mix is `Invalid("mix hash")`.
    pub fn hash_with_mix(
        &self,
        seal: Hash,
        nonce: u64,
        height: u64,
        mix: Hash,
    ) -> Result<Hash, Error> {
        let (actual, hash) = self.compute(seal, nonce, height)?;
        if actual != mix {
            return Err(Error::Invalid("mix hash"));
        }
        Ok(hash)
    }
    pub fn verify(
        &self,
        seal: Hash,
        nonce: u64,
        height: u64,
        mix: Hash,
        difficulty: &[u8],
    ) -> Result<VerifiedPow, Error> {
        if difficulty.len() > 32 {
            return Err(Error::Unsupported("difficulty exceeds 256-bit domain"));
        }
        if difficulty.iter().all(|b| *b == 0) {
            return Err(Error::Invalid("difficulty must be positive"));
        }
        let hash = self.hash_with_mix(seal, nonce, height, mix)?;
        if !quai_primitives::meets_blake3_target(&hash, difficulty)
            .map_err(|_| Error::Invalid("difficulty"))?
        {
            return Err(Error::Invalid("proof target"));
        }
        Ok(VerifiedPow {
            algorithm: self.algorithm,
            seal,
            height,
            nonce,
            hash,
        })
    }
    /// The kernel input words, the permuted start state and the seed.
    fn start(&self, seal: Hash, nonce: u64) -> ([u32; 8], [u32; 25], u64) {
        let mut input = seal;
        if self.algorithm == Algorithm::Kawpow {
            input.reverse();
        }
        let input = decode_words::<8>(&input);
        let mut start = [0u32; 25];
        start[..8].copy_from_slice(&input);
        start[8] = nonce as u32;
        start[9] = (nonce >> 32) as u32;
        if self.algorithm == Algorithm::Kawpow {
            start[10..].copy_from_slice(&RAVEN);
        }
        permute(&mut start);
        let seed = if self.algorithm == Algorithm::Progpow {
            ((start[0].swap_bytes() as u64) << 32) | start[1].swap_bytes() as u64
        } else {
            start[0] as u64 | ((start[1] as u64) << 32)
        };
        (input, start, seed)
    }
    /// The header-format mix and the PoW hash of the kernel's result words.
    fn finish(
        &self,
        input: &[u32; 8],
        start: &[u32; 25],
        seed: u64,
        result: [u32; 8],
    ) -> (Hash, Hash) {
        let mut final_state = [0u32; 25];
        if self.algorithm == Algorithm::Progpow {
            final_state[..8].copy_from_slice(input);
            final_state[8] = seed as u32;
            final_state[9] = (seed >> 32) as u32;
            final_state[10..18].copy_from_slice(&result);
        } else {
            final_state[..8].copy_from_slice(&start[..8]);
            final_state[8..16].copy_from_slice(&result);
            final_state[16..].copy_from_slice(&RAVEN[..9]);
        }
        permute(&mut final_state);
        let mut digest = words_to_hash(&result);
        if self.algorithm == Algorithm::Kawpow {
            digest.reverse();
        }
        (digest, words_to_hash(&final_state[..8]))
    }
    fn kernel(&self, loads: &impl Loads, seal: Hash, nonce: u64, height: u64) -> (Hash, Hash) {
        let mut k = Kernel::new(self, seal, nonce, height);
        for round in 0..64 {
            let index = k.index(self, round);
            k.round(self, round, &loads.load(self, index));
        }
        k.finish(self)
    }
    /// The light kernels of up to four inputs in lockstep: the dataset items
    /// of one round of all of them are computed together, as up to 16
    /// independent chains whose memory loads overlap. Each result equals
    /// [`compute_light`](Self::compute_light)'s.
    fn kernels_light(&self, inputs: &[(Hash, u64, u64)]) -> Vec<(Hash, Hash)> {
        let mut ks: Vec<Kernel> = inputs
            .iter()
            .take(4)
            .map(|(seal, nonce, height)| Kernel::new(self, *seal, *nonce, *height))
            .collect();
        for round in 0..64 {
            let mut index = [0u32; 16];
            for (j, k) in ks.iter().enumerate() {
                index[4 * j..4 * j + 4].copy_from_slice(&k.index(self, round));
            }
            let mut items = [[0u32; 16]; 16];
            match ks.len() {
                1 => {
                    items[..4].copy_from_slice(&self.items::<4>(std::array::from_fn(|i| index[i])))
                }
                2 => {
                    items[..8].copy_from_slice(&self.items::<8>(std::array::from_fn(|i| index[i])))
                }
                3 => items[..12]
                    .copy_from_slice(&self.items::<12>(std::array::from_fn(|i| index[i]))),
                _ => items = self.items::<16>(index),
            }
            for (j, k) in ks.iter_mut().enumerate() {
                k.round(self, round, &std::array::from_fn(|i| items[4 * j + i]));
            }
        }
        ks.iter().map(|k| k.finish(self)).collect()
    }
    /// Compute the kernels of `inputs` (seal, nonce, height) of this epoch
    /// together and keep their results for the next [`compute`](Self::compute)
    /// of the same input, which takes them instead of computing again. Only
    /// this cache's own kernel results enter; inputs of another epoch or out
    /// of bound are left out. With a full dataset attached it does nothing.
    pub fn prefetch(&self, inputs: &[(Hash, u64, u64)]) {
        if self.full().is_some() {
            return;
        }
        let inputs: Vec<(Hash, u64, u64)> = inputs
            .iter()
            .filter(|(_, _, h)| self.check_height(*h).is_ok())
            .copied()
            .collect();
        for group in inputs.chunks(4) {
            let results = self.kernels_light(group);
            if let Ok(mut buffer) = self.prefetched.lock() {
                for (input, result) in group.iter().zip(results) {
                    while buffer.len() >= PREFETCH_ENTRIES {
                        buffer.pop_front();
                    }
                    buffer.push_back((*input, result));
                }
            }
        }
    }
    fn take_prefetched(&self, seal: Hash, nonce: u64, height: u64) -> Option<(Hash, Hash)> {
        let mut buffer = self.prefetched.lock().ok()?;
        let i = buffer
            .iter()
            .position(|(k, _)| *k == (seal, nonce, height))?;
        buffer.remove(i).map(|(_, r)| r)
    }
}
/// Kernel results that [`LightCache::prefetch`] keeps for later `compute`s.
const PREFETCH_ENTRIES: usize = 256;
/// The random program of one ProgPoW period. Every lane in every round
/// starts KISS99 from `init(period)` and draws the same values in the same
/// order, independent of the data; go-quai draws them again per lane and
/// round (progpowLoop). Drawn once, in that order, the program applies the
/// same merges and math to the same registers.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Program {
    /// Per step i < 11: source register, destination register, merge value.
    cache: [(u8, u8, u32); 11],
    /// Per step: two source registers, math value, destination, merge value.
    math: [(u8, u8, u32, u8, u32); 18],
    /// The merge value of the first DAG word into register 0.
    dag_first: u32,
    /// Destination and merge value of the other three DAG words.
    dag_rest: [(u8, u32); 3],
}
impl Program {
    fn new(period: u64) -> Self {
        let (mut rng, dest, src) = init(period);
        let (mut sc, mut dc) = (0usize, 0usize);
        let mut cache = [(0, 0, 0); 11];
        let mut math = [(0, 0, 0, 0, 0); 18];
        for (i, step) in math.iter_mut().enumerate() {
            if i < 11 {
                let source = src[sc % 32] as u8;
                sc += 1;
                let d = dest[dc % 32] as u8;
                dc += 1;
                cache[i] = (source, d, rng.next());
            }
            let r = rng.next() % (32 * 31);
            let s1 = r % 32;
            let mut s2 = r / 32;
            if s2 >= s1 {
                s2 += 1;
            }
            let value = rng.next();
            let d = dest[dc % 32] as u8;
            dc += 1;
            *step = (s1 as u8, s2 as u8, value, d, rng.next());
        }
        let dag_first = rng.next();
        let dag_rest = std::array::from_fn(|_| {
            let d = dest[dc % 32] as u8;
            dc += 1;
            (d, rng.next())
        });
        Self {
            cache,
            math,
            dag_first,
            dag_rest,
        }
    }
    /// One lane of one round (the body of go-quai's progpowLoop lane loop).
    fn run(&self, registers: &mut [u32; 32], cdag: &[u32; 4096], dag: &[u32; 64], index: usize) {
        for (i, &(s1, s2, value, d, r)) in self.math.iter().enumerate() {
            if let Some(&(source, cd, cr)) = self.cache.get(i) {
                let word = cdag[registers[source as usize] as usize % 4096];
                merge(&mut registers[cd as usize], word, cr);
            }
            let m = math(registers[s1 as usize], registers[s2 as usize], value);
            merge(&mut registers[d as usize], m, r);
        }
        merge(&mut registers[0], dag[index], self.dag_first);
        for (k, &(d, r)) in self.dag_rest.iter().enumerate() {
            merge(&mut registers[d as usize], dag[index + 1 + k], r);
        }
    }
}
/// One kernel run between its rounds.
struct Kernel {
    input: [u32; 8],
    start: [u32; 25],
    seed: u64,
    program: Program,
    mix: [[u32; 32]; 16],
}
impl Kernel {
    fn new(cache: &LightCache, seal: Hash, nonce: u64, height: u64) -> Self {
        let (input, start, seed) = cache.start(seal, nonce);
        let period = height
            / if cache.algorithm == Algorithm::Progpow {
                10
            } else {
                3
            };
        Self {
            input,
            start,
            seed,
            program: Program::new(period),
            mix: std::array::from_fn(|lane| fill(seed, lane as u32)),
        }
    }
    /// The four dataset items that `round` reads.
    fn index(&self, cache: &LightCache, round: u32) -> [u32; 4] {
        // Go evaluates this expression in uint32, including overflow at high epochs.
        let rows = (cache.dataset_size / 256) as u32;
        let offset = self.mix[round as usize % 16][0] % (64u32.wrapping_mul(rows) / 64);
        let base = offset.wrapping_mul(64);
        std::array::from_fn(|i| base.wrapping_add(i as u32 * 16) / 16)
    }
    fn round(&mut self, cache: &LightCache, round: u32, items: &[[u32; 16]; 4]) {
        let mut dag = [0u32; 64];
        for (i, item) in items.iter().enumerate() {
            dag[i * 16..i * 16 + 16].copy_from_slice(item);
        }
        for (lane, registers) in self.mix.iter_mut().enumerate() {
            let index = ((lane as u32 ^ round) % 16) as usize * 4;
            self.program.run(registers, &cache.cdag, &dag, index);
        }
    }
    fn finish(&self, cache: &LightCache) -> (Hash, Hash) {
        let mut result = [0x811c9dc5u32; 8];
        for (lane, values) in self.mix.iter().enumerate() {
            let mut r = 0x811c9dc5;
            for v in values {
                fnv1a(&mut r, *v);
            }
            fnv1a(&mut result[lane % 8], r);
        }
        cache.finish(&self.input, &self.start, self.seed, result)
    }
}
/// Every item of one epoch's dataset in memory (go-quai's `generateDataset`
/// without the file): 64 bytes per item, `dataset_size` bytes in all. It is
/// built from the epoch cache with the light path's item function.
pub struct FullDataset {
    key: (Algorithm, u64),
    items: Vec<[u32; 16]>,
}
impl FullDataset {
    /// Build the dataset of `cache`'s epoch on `threads` threads.
    pub fn build(cache: &LightCache, threads: usize) -> Result<Self, Error> {
        Self::build_until(cache, threads, &AtomicBool::new(false))
    }
    /// As [`build`](Self::build); stops with `Unavailable` once `stop` is set.
    pub fn build_until(
        cache: &LightCache,
        threads: usize,
        stop: &AtomicBool,
    ) -> Result<Self, Error> {
        const CHUNK: usize = 1 << 14;
        let len = usize::try_from(cache.dataset_size / 64)
            .ok()
            .filter(|n| *n as u64 <= u32::MAX as u64 + 1)
            .ok_or(Error::ResourceLimit("full dataset"))?;
        let mut items: Vec<[u32; 16]> = Vec::new();
        items
            .try_reserve_exact(len)
            .map_err(|_| Error::ResourceLimit("full dataset memory"))?;
        items.resize(len, [0; 16]);
        let parts = Mutex::new(items.chunks_mut(CHUNK).enumerate());
        let failed = AtomicBool::new(false);
        std::thread::scope(|s| {
            let mut started = 0;
            for i in 0..threads.max(1) {
                let worker = || loop {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let next = match parts.lock() {
                        Ok(mut p) => p.next(),
                        Err(_) => {
                            failed.store(true, Ordering::Relaxed);
                            return;
                        }
                    };
                    let Some((n, part)) = next else { return };
                    cache.fill(part, (n * CHUNK) as u32);
                };
                if std::thread::Builder::new()
                    .name(format!("pow-dag-{i}"))
                    .spawn_scoped(s, worker)
                    .is_ok()
                {
                    started += 1;
                }
            }
            if started == 0 {
                failed.store(true, Ordering::Relaxed);
            }
        });
        drop(parts);
        if failed.load(Ordering::Relaxed) {
            return Err(Error::Unavailable("full dataset build threads"));
        }
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Unavailable("full dataset build stopped"));
        }
        Ok(Self {
            key: cache.key(),
            items,
        })
    }
    pub fn algorithm(&self) -> Algorithm {
        self.key.0
    }
    pub fn epoch(&self) -> u64 {
        self.key.1
    }
    pub fn bytes(&self) -> u64 {
        self.items.len() as u64 * 64
    }
    /// Item `index` as stored, little-endian (as `LightCache::dataset_item`).
    pub fn item(&self, index: u32) -> Option<[u8; 64]> {
        let words = self.items.get(index as usize)?;
        let mut out = [0u8; 64];
        for (chunk, w) in out.chunks_exact_mut(4).zip(words) {
            chunk.copy_from_slice(&w.to_le_bytes());
        }
        Some(out)
    }
}
/// When the kernel reads a full dataset instead of the light cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DagMode {
    /// The light cache only.
    Light,
    /// Build the full dataset of each new epoch in memory and use it once it
    /// is complete; the light cache serves until then.
    Full,
    /// As `Full` when the free memory holds the dataset and
    /// [`DAG_MEMORY_MARGIN`] more when the epoch starts; else `Light`.
    Auto,
}
impl std::str::FromStr for DagMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "light" => Ok(Self::Light),
            "full" => Ok(Self::Full),
            "auto" => Ok(Self::Auto),
            _ => Err(format!("unknown dataset mode {s:?} (light, full or auto)")),
        }
    }
}
/// Free memory that `DagMode::Auto` leaves beside a full dataset.
pub const DAG_MEMORY_MARGIN: u64 = 1 << 30;
/// Physical memory the system can hand out without compressing or swapping
/// other memory: on macOS the free, speculative and purgeable pages and the
/// inactive file-backed pages (`vm_stat`); on Linux `MemAvailable`. None when
/// it cannot be read.
pub fn available_memory() -> Option<u64> {
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        let line = text.lines().find(|l| l.starts_with("MemAvailable:"))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        return Some(kib * 1024);
    }
    let out = std::process::Command::new("/usr/bin/vm_stat")
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let page: u64 = text
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |name: &str| -> Option<u64> {
        let line = text.lines().find(|l| l.starts_with(name))?;
        line.split(':')
            .nth(1)?
            .trim()
            .trim_end_matches('.')
            .parse()
            .ok()
    };
    let inactive = pages("Pages inactive")?;
    let file_backed = pages("File-backed pages").unwrap_or(0);
    let free = pages("Pages free")?
        + pages("Pages speculative").unwrap_or(0)
        + pages("Pages purgeable").unwrap_or(0)
        + inactive.min(file_backed);
    Some(free * page)
}
/// Messages about full datasets (built, dropped, not built and why).
pub type DagListener = Arc<dyn Fn(&str) + Send + Sync>;
/// The full dataset of the newest epoch of one algorithm: being built, in
/// use, or not built (`stop` set).
struct Active {
    key: CacheKey,
    cache: Arc<LightCache>,
    stop: Arc<AtomicBool>,
}
type ActiveList = Arc<Mutex<Vec<Active>>>;
/// Locally generated epoch caches shared by verifiers on several threads.
/// Each epoch is generated once; the least recently used epoch is dropped
/// when the store is full. Cache bytes never come from a peer or a file.
/// With `DagMode::Full` or `Auto` the store also builds the full dataset of
/// the newest epoch of each algorithm on background threads and drops the
/// previous epoch's dataset when a newer epoch starts.
pub struct SharedCaches {
    capacity: usize,
    slots: Mutex<std::collections::VecDeque<(CacheKey, CacheSlot)>>,
    dag: DagMode,
    dag_threads: usize,
    active: ActiveList,
    listener: Option<DagListener>,
    memory: fn() -> Option<u64>,
}
type CacheKey = (Algorithm, u64);
type CacheSlot = Arc<std::sync::OnceLock<Result<Arc<LightCache>, Error>>>;
impl SharedCaches {
    pub const MAX_EPOCHS: usize = 8;
    pub fn new(capacity: usize) -> Result<Self, Error> {
        Self::with_dag(capacity, DagMode::Light, 1, None)
    }
    /// A store that follows `dag` and builds datasets on `threads` threads.
    pub fn with_dag(
        capacity: usize,
        dag: DagMode,
        threads: usize,
        listener: Option<DagListener>,
    ) -> Result<Self, Error> {
        if capacity == 0 || capacity > Self::MAX_EPOCHS {
            return Err(Error::Invalid("shared epoch cache capacity"));
        }
        Ok(Self {
            capacity,
            slots: Default::default(),
            dag,
            dag_threads: threads.max(1),
            active: Default::default(),
            listener,
            memory: available_memory,
        })
    }
    pub fn dag_mode(&self) -> DagMode {
        self.dag
    }
    /// Return the epoch cache for `height`, generating it at most once.
    pub fn get(&self, algorithm: Algorithm, height: u64) -> Result<Arc<LightCache>, Error> {
        algorithm.sizes(height)?;
        let key = (algorithm, height / algorithm.epoch_length());
        let (slot, created) = {
            let mut slots = self
                .slots
                .lock()
                .map_err(|_| Error::Unavailable("shared epoch caches"))?;
            let hit = slots
                .iter()
                .position(|(k, _)| *k == key)
                .and_then(|index| slots.remove(index));
            if let Some(entry) = hit {
                let slot = entry.1.clone();
                slots.push_back(entry);
                (slot, false)
            } else {
                while slots.len() >= self.capacity {
                    slots.pop_front();
                }
                let slot = CacheSlot::default();
                slots.push_back((key, slot.clone()));
                (slot, true)
            }
        };
        let cache = slot
            .get_or_init(|| LightCache::new(algorithm, height).map(Arc::new))
            .clone()?;
        if created && self.dag != DagMode::Light {
            self.start_dataset(&cache);
        }
        Ok(cache)
    }
    /// Epochs currently held, oldest first.
    pub fn epochs(&self) -> Vec<(Algorithm, u64)> {
        self.slots
            .lock()
            .map(|s| s.iter().map(|(k, _)| *k).collect())
            .unwrap_or_default()
    }
    /// The newest epoch of each algorithm, and whether its full dataset is
    /// complete and in use.
    pub fn datasets(&self) -> Vec<((Algorithm, u64), bool)> {
        self.active
            .lock()
            .map(|a| {
                a.iter()
                    .map(|x| (x.key, x.cache.full().is_some()))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn tell(&self, message: &str) {
        if let Some(l) = &self.listener {
            l(message);
        }
    }
    /// A new epoch cache: drop the dataset of an older epoch of the same
    /// algorithm and start building this one's, if the mode allows it.
    fn start_dataset(&self, cache: &Arc<LightCache>) {
        let key = cache.key();
        let Ok(mut active) = self.active.lock() else {
            return;
        };
        if let Some(i) = active.iter().position(|a| a.key.0 == key.0) {
            if active[i].key == key {
                // The epoch's cache was generated again after the LRU dropped
                // it: the dataset (built or being built) follows the new one.
                if !Arc::ptr_eq(&active[i].cache, cache) {
                    if let Some(d) = active[i].cache.full() {
                        let _ = cache.attach(d);
                    }
                    active[i].cache = cache.clone();
                }
                return;
            }
            if active[i].key.1 > key.1 {
                // An older epoch: the light path serves it.
                return;
            }
            let old = active.remove(i);
            old.stop.store(true, Ordering::Relaxed);
            if let Some(d) = old.cache.detach() {
                self.tell(&format!(
                    "{:?} epoch {}: full dataset dropped ({:.2} GiB)",
                    old.key.0,
                    old.key.1,
                    gib(d.bytes())
                ));
            }
        }
        let need = cache.dataset_size();
        let stop = Arc::new(AtomicBool::new(false));
        active.push(Active {
            key,
            cache: cache.clone(),
            stop: stop.clone(),
        });
        if self.dag == DagMode::Auto {
            match (self.memory)() {
                Some(free) if free >= need.saturating_add(DAG_MEMORY_MARGIN) => {
                    self.tell(&format!(
                        "{:?} epoch {}: {:.2} GiB free, building the full dataset ({:.2} GiB) on {} threads",
                        key.0, key.1, gib(free), gib(need), self.dag_threads
                    ));
                }
                free => {
                    stop.store(true, Ordering::Relaxed);
                    self.tell(&format!(
                        "{:?} epoch {}: light cache only: the full dataset needs {:.2} GiB and a {:.2} GiB margin, {} free",
                        key.0,
                        key.1,
                        gib(need),
                        gib(DAG_MEMORY_MARGIN),
                        free.map_or("unknown".to_string(), |f| format!("{:.2} GiB", gib(f)))
                    ));
                    return;
                }
            }
        } else {
            self.tell(&format!(
                "{:?} epoch {}: building the full dataset ({:.2} GiB) on {} threads",
                key.0,
                key.1,
                gib(need),
                self.dag_threads
            ));
        }
        let source = cache.clone();
        let threads = self.dag_threads;
        let list = self.active.clone();
        let listener = self.listener.clone();
        let tell = move |m: &str| {
            if let Some(l) = &listener {
                l(m);
            }
        };
        let spawned = std::thread::Builder::new()
            .name("pow-dag".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                let dataset = match FullDataset::build_until(&source, threads, &stop) {
                    Ok(d) => Arc::new(d),
                    Err(e) => {
                        if !stop.load(Ordering::Relaxed) {
                            tell(&format!(
                                "{:?} epoch {}: full dataset not built ({e}); light cache only",
                                key.0, key.1
                            ));
                        }
                        return;
                    }
                };
                // Attach to the epoch's current cache, unless a newer epoch
                // has taken its place meanwhile.
                let Ok(active) = list.lock() else { return };
                let Some(entry) = active
                    .iter()
                    .find(|a| a.key == key && Arc::ptr_eq(&a.stop, &stop))
                else {
                    return;
                };
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                match entry.cache.attach(dataset.clone()) {
                    Ok(()) => tell(&format!(
                        "{:?} epoch {}: full dataset ready ({:.2} GiB) in {:.1}s; the kernel reads it from now on",
                        key.0,
                        key.1,
                        gib(dataset.bytes()),
                        started.elapsed().as_secs_f64()
                    )),
                    Err(e) => tell(&format!(
                        "{:?} epoch {}: full dataset not used: {e}",
                        key.0, key.1
                    )),
                }
            });
        if spawned.is_err() {
            self.tell("full dataset thread did not start; light cache only");
        }
    }
}
fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 30) as f64
}
impl Drop for SharedCaches {
    fn drop(&mut self) {
        if let Ok(active) = self.active.lock() {
            for a in active.iter() {
                a.stop.store(true, Ordering::Relaxed);
            }
        }
    }
}
const RAVEN: [u32; 15] = [
    0x72, 0x41, 0x56, 0x45, 0x4e, 0x43, 0x4f, 0x49, 0x4e, 0x4b, 0x41, 0x57, 0x50, 0x4f, 0x57,
];
const ROUND: [u32; 24] = [
    0x00000001, 0x00008082, 0x0000808a, 0x80008000, 0x0000808b, 0x80000001, 0x80008081, 0x00008009,
    0x0000008a, 0x00000088, 0x80008009, 0x8000000a, 0x8000808b, 0x0000008b, 0x00008089, 0x00008003,
    0x00008002, 0x00000080, 0x0000800a, 0x8000000a, 0x80008081, 0x00008080, 0x80000001, 0x80008008,
];
fn permute(st: &mut [u32; 25]) {
    const ROT: [u32; 24] = [
        1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14, 27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44,
    ];
    const PIL: [usize; 24] = [
        10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4, 15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1,
    ];
    for rc in ROUND.iter().take(22) {
        let bc: [u32; 5] =
            std::array::from_fn(|i| st[i] ^ st[i + 5] ^ st[i + 10] ^ st[i + 15] ^ st[i + 20]);
        for i in 0..5 {
            let t = bc[(i + 4) % 5] ^ bc[(i + 1) % 5].rotate_left(1);
            for j in (0..25).step_by(5) {
                st[i + j] ^= t;
            }
        }
        let mut t = st[1];
        for i in 0..24 {
            let old = st[PIL[i]];
            st[PIL[i]] = t.rotate_left(ROT[i]);
            t = old;
        }
        for j in (0..25).step_by(5) {
            let bc: [u32; 5] = std::array::from_fn(|i| st[j + i]);
            for i in 0..5 {
                st[j + i] ^= (!bc[(i + 1) % 5]) & bc[(i + 2) % 5];
            }
        }
        st[0] ^= rc;
    }
}
#[derive(Clone, Copy)]
struct Kiss {
    z: u32,
    w: u32,
    jsr: u32,
    jcong: u32,
}
impl Kiss {
    fn next(&mut self) -> u32 {
        self.z = 36969u32
            .wrapping_mul(self.z & 65535)
            .wrapping_add(self.z >> 16);
        self.w = 18000u32
            .wrapping_mul(self.w & 65535)
            .wrapping_add(self.w >> 16);
        let mwc = (self.z << 16).wrapping_add(self.w);
        self.jsr ^= self.jsr << 17;
        self.jsr ^= self.jsr >> 13;
        self.jsr ^= self.jsr << 5;
        self.jcong = 69069u32.wrapping_mul(self.jcong).wrapping_add(1234567);
        (mwc ^ self.jcong).wrapping_add(self.jsr)
    }
}
fn initial(seed: u64, a: u32, b: u32) -> Kiss {
    let mut h = 0x811c9dc5;
    Kiss {
        z: fnv1a(&mut h, seed as u32),
        w: fnv1a(&mut h, (seed >> 32) as u32),
        jsr: fnv1a(&mut h, a),
        jcong: fnv1a(&mut h, b),
    }
}
fn fill(seed: u64, lane: u32) -> [u32; 32] {
    let mut r = initial(seed, lane, lane);
    std::array::from_fn(|_| r.next())
}
fn init(seed: u64) -> (Kiss, [u32; 32], [u32; 32]) {
    let mut r = initial(seed, seed as u32, (seed >> 32) as u32);
    let mut dst = std::array::from_fn(|i| i as u32);
    let mut src = dst;
    for i in (1..32).rev() {
        let j = r.next() as usize % (i + 1);
        dst.swap(i, j);
        let j = r.next() as usize % (i + 1);
        src.swap(i, j);
    }
    (r, dst, src)
}
fn merge(a: &mut u32, b: u32, r: u32) {
    *a = match r % 4 {
        0 => a.wrapping_mul(33).wrapping_add(b),
        1 => (*a ^ b).wrapping_mul(33),
        2 => a.rotate_left((r >> 16) % 31 + 1) ^ b,
        _ => a.rotate_right((r >> 16) % 31 + 1) ^ b,
    };
}
fn math(a: u32, b: u32, r: u32) -> u32 {
    match r % 11 {
        0 => a.wrapping_add(b),
        1 => a.wrapping_mul(b),
        2 => ((a as u64 * b as u64) >> 32) as u32,
        3 => a.min(b),
        4 => a.rotate_left(b),
        5 => a.rotate_right(b),
        6 => a & b,
        7 => a | b,
        8 => a ^ b,
        9 => a.leading_zeros() + b.leading_zeros(),
        _ => a.count_ones() + b.count_ones(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// A cache of a small, made-up size, so that a full dataset is cheap to
    /// build; the kernel code and the item function are the real ones.
    fn small(algorithm: Algorithm, rows: usize, dataset_size: u64) -> LightCache {
        let mut cache = vec![[0; 16]; rows];
        cache[0] = hash512(&[7u8; 32]);
        for i in 1..rows {
            cache[i] = hash512_words(&cache[i - 1]);
        }
        let mut out = LightCache {
            algorithm,
            epoch: 0,
            cache,
            cdag: [0; 4096],
            dataset_size,
            full: RwLock::new(None),
            prefetched: Default::default(),
        };
        for i in 0..256 {
            let item = out.item(i as u32);
            out.cdag[i * 16..i * 16 + 16].copy_from_slice(&item);
        }
        out
    }
    /// The single-chain item, written as go-quai's generateDatasetItem.
    fn reference_item(c: &LightCache, index: u32) -> [u32; 16] {
        let rows = c.cache.len();
        let mut mix = c.cache[index as usize % rows];
        mix[0] ^= index;
        mix = hash512(&encode_words(&mix));
        for i in 0..512 {
            let parent = fnv(index ^ i, mix[i as usize % 16]) as usize % rows;
            for (v, b) in mix.iter_mut().zip(&c.cache[parent]) {
                *v = fnv(*v, *b);
            }
        }
        hash512(&encode_words(&mix))
    }
    #[test]
    fn interleaved_items_equal_the_single_chain_item() {
        let c = small(Algorithm::Progpow, 1021, 1021 * 64 * 4);
        for first in [0u32, 1, 255, 1019, 4000, u32::MAX - 9] {
            let four = c.items::<4>(std::array::from_fn(|k| first.wrapping_add(k as u32 * 3)));
            let eight = c.items::<8>(std::array::from_fn(|k| first.wrapping_add(k as u32)));
            for (k, item) in four.iter().enumerate() {
                assert_eq!(*item, reference_item(&c, first.wrapping_add(k as u32 * 3)));
            }
            for (k, item) in eight.iter().enumerate() {
                assert_eq!(*item, reference_item(&c, first.wrapping_add(k as u32)));
            }
        }
        let mut out = vec![[0; 16]; 37];
        c.fill(&mut out, 11);
        for (k, item) in out.iter().enumerate() {
            assert_eq!(*item, reference_item(&c, 11 + k as u32));
        }
    }
    #[test]
    fn full_dataset_kernel_equals_light_kernel() {
        for algorithm in [Algorithm::Progpow, Algorithm::Kawpow] {
            // 2,048 rows of 256 bytes: 8,192 items, 512 KiB.
            let c = small(algorithm, 509, 2048 * 256);
            let d = FullDataset::build(&c, 3).unwrap();
            assert_eq!(d.bytes(), c.dataset_size());
            for i in [0u32, 1, 4095, 8191] {
                assert_eq!(d.item(i).unwrap(), c.dataset_item(i));
            }
            assert!(d.item(8192).is_none());
            for n in 0..64u64 {
                let mut seal = [0u8; 32];
                seal[..8].copy_from_slice(&n.to_be_bytes());
                seal[31] = 0x5a;
                let nonce = n.wrapping_mul(0x9e37_79b9_7f4a_7c15);
                let light = c.compute_light(seal, nonce, n).unwrap();
                let full = c.compute_full(&d, seal, nonce, n).unwrap();
                assert_eq!(light, full, "{algorithm:?} input {n}");
            }
            // compute follows the attached dataset and goes back on detach.
            let d = Arc::new(d);
            let probe = ([3u8; 32], 9u64, 5u64);
            let light = c.compute(probe.0, probe.1, probe.2).unwrap();
            c.attach(d.clone()).unwrap();
            assert!(c.full().is_some());
            assert_eq!(c.compute(probe.0, probe.1, probe.2).unwrap(), light);
            assert!(c.detach().is_some());
            assert!(c.full().is_none());
            let other = small(algorithm, 509, 1024 * 256);
            assert!(other.attach(d).is_err());
        }
    }
    #[test]
    fn batched_light_kernels_and_prefetch_equal_single_kernels() {
        for algorithm in [Algorithm::Progpow, Algorithm::Kawpow] {
            let c = small(algorithm, 509, 2048 * 256);
            let inputs: Vec<(Hash, u64, u64)> = (0..11u64)
                .map(|n| {
                    let mut seal = [0u8; 32];
                    seal[..8].copy_from_slice(&(n * 77).to_be_bytes());
                    (seal, n.wrapping_mul(0x517c_c1b7_2722_0a95), n * 13)
                })
                .collect();
            let single: Vec<(Hash, Hash)> = inputs
                .iter()
                .map(|(s, n, h)| c.compute_light(*s, *n, *h).unwrap())
                .collect();
            for len in 1..=4 {
                assert_eq!(c.kernels_light(&inputs[..len]), single[..len].to_vec());
            }
            c.prefetch(&inputs);
            assert_eq!(c.prefetched.lock().unwrap().len(), inputs.len());
            for ((s, n, h), expected) in inputs.iter().zip(&single) {
                assert_eq!(c.compute(*s, *n, *h).unwrap(), *expected);
            }
            assert!(c.prefetched.lock().unwrap().is_empty());
            // Inputs of another epoch are left out.
            c.prefetch(&[([1u8; 32], 1, 400_000)]);
            assert!(c.prefetched.lock().unwrap().is_empty());
        }
    }
    /// The lane loop as go-quai's progpowLoop writes it: KISS99 drawn again
    /// for every lane of every round.
    fn reference_lane(
        period: u64,
        registers: &mut [u32; 32],
        cdag: &[u32; 4096],
        dag: &[u32; 64],
        index: usize,
    ) {
        let (mut rng, dest, src) = init(period);
        let (mut sc, mut dc) = (0usize, 0usize);
        for i in 0..18 {
            if i < 11 {
                let source = src[sc % 32] as usize;
                sc += 1;
                let value = cdag[registers[source] as usize % 4096];
                let d = dest[dc % 32] as usize;
                dc += 1;
                merge(&mut registers[d], value, rng.next());
            }
            let r = rng.next() % (32 * 31);
            let s1 = r % 32;
            let mut s2 = r / 32;
            if s2 >= s1 {
                s2 += 1;
            }
            let value = math(registers[s1 as usize], registers[s2 as usize], rng.next());
            let d = dest[dc % 32] as usize;
            dc += 1;
            merge(&mut registers[d], value, rng.next());
        }
        merge(&mut registers[0], dag[index], rng.next());
        for value in &dag[index + 1..index + 4] {
            let d = dest[dc % 32] as usize;
            dc += 1;
            merge(&mut registers[d], *value, rng.next());
        }
    }
    #[test]
    fn period_program_equals_the_per_lane_draws() {
        let mut x = 0x0123_4567_89ab_cdefu64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let cdag: [u32; 4096] = std::array::from_fn(|_| next() as u32);
        for period in [0u64, 1, 7, 130_109, 1_000_003, u64::MAX, next(), next()] {
            let program = Program::new(period);
            for round in 0..64u32 {
                let dag: [u32; 64] = std::array::from_fn(|_| next() as u32);
                for lane in 0..16u32 {
                    let start: [u32; 32] = std::array::from_fn(|_| next() as u32);
                    let index = ((lane ^ round) % 16) as usize * 4;
                    let (mut a, mut b) = (start, start);
                    program.run(&mut a, &cdag, &dag, index);
                    reference_lane(period, &mut b, &cdag, &dag, index);
                    assert_eq!(a, b, "period {period} round {round} lane {lane}");
                }
            }
        }
    }
    #[test]
    fn dataset_build_stops_when_asked() {
        let c = small(Algorithm::Progpow, 509, 4096 * 256);
        let stop = AtomicBool::new(true);
        assert!(matches!(
            FullDataset::build_until(&c, 2, &stop),
            Err(Error::Unavailable(_))
        ));
    }
    #[test]
    fn dag_mode_parses_and_light_store_builds_nothing() {
        assert_eq!("light".parse::<DagMode>(), Ok(DagMode::Light));
        assert_eq!("full".parse::<DagMode>(), Ok(DagMode::Full));
        assert_eq!("auto".parse::<DagMode>(), Ok(DagMode::Auto));
        assert!("fast".parse::<DagMode>().is_err());
        let s = SharedCaches::new(2).unwrap();
        assert_eq!(s.dag_mode(), DagMode::Light);
        s.get(Algorithm::Progpow, 5).unwrap();
        assert!(s.datasets().is_empty());
        assert!(SharedCaches::with_dag(0, DagMode::Full, 1, None).is_err());
    }
    #[test]
    fn auto_mode_stays_light_without_free_memory_and_follows_the_newest_epoch() {
        let said: Arc<Mutex<Vec<String>>> = Default::default();
        let log = said.clone();
        let listener: DagListener = Arc::new(move |m: &str| log.lock().unwrap().push(m.into()));
        let mut s = SharedCaches::with_dag(4, DagMode::Auto, 2, Some(listener)).unwrap();
        s.memory = || Some(1 << 20);
        let e0 = s.get(Algorithm::Progpow, 7).unwrap();
        assert!(e0.full().is_none());
        assert_eq!(s.datasets(), vec![((Algorithm::Progpow, 0), false)]);
        assert!(said.lock().unwrap()[0].contains("light cache only"));
        // The same epoch again: no second decision.
        s.get(Algorithm::Progpow, 9).unwrap();
        assert_eq!(said.lock().unwrap().len(), 1);
        // A newer epoch replaces the older one; an older one is left alone.
        s.get(Algorithm::Progpow, 388_800).unwrap();
        assert_eq!(s.datasets(), vec![((Algorithm::Progpow, 1), false)]);
        s.get(Algorithm::Progpow, 1).unwrap();
        assert_eq!(s.datasets(), vec![((Algorithm::Progpow, 1), false)]);
        s.memory = || None;
        s.get(Algorithm::Kawpow, 0).unwrap();
        assert!(said
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .contains("unknown free"));
    }
    #[test]
    fn available_memory_is_readable_here() {
        // macOS (vm_stat) and Linux (/proc/meminfo) report a positive amount.
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(available_memory().is_some_and(|b| b > 0));
        }
    }
}
