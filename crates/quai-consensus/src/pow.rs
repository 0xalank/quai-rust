//! Pinned ProgPoW and KAWPOW light kernels, including native cache/DAG generation.
//! KAWPOW seal input uses natural SHA256 byte order; header-path reversal is here.
use crate::{Error, VerifiedPow};
use quai_primitives::Hash;
use sha3::{Digest, Keccak256, Keccak512};
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
    std::array::from_fn(|i| u32::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap()))
}
fn hash512(b: &[u8]) -> [u32; 16] {
    decode_words(&Keccak512::digest(b))
}
fn fnv(a: u32, b: u32) -> u32 {
    a.wrapping_mul(0x01000193) ^ b
}
fn fnv1a(h: &mut u32, d: u32) -> u32 {
    *h = (*h ^ d).wrapping_mul(0x01000193);
    *h
}
/// Owns one bounded epoch cache, generated locally; no peer-supplied cache bytes.
pub struct LightCache {
    algorithm: Algorithm,
    epoch: u64,
    cache: Vec<[u32; 16]>,
    cdag: [u32; 4096],
    dataset_size: u64,
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
            cache[i] = hash512(&encode_words(&cache[i - 1]));
        }
        for _ in 0..3 {
            for i in 0..rows {
                let previous = (i + rows - 1) % rows;
                let other = cache[i][0] as usize % rows;
                let words: [u32; 16] =
                    std::array::from_fn(|j| cache[previous][j] ^ cache[other][j]);
                cache[i] = hash512(&encode_words(&words));
            }
        }
        let mut out = Self {
            algorithm,
            epoch,
            cache,
            cdag: [0; 4096],
            dataset_size,
        };
        for i in 0..256 {
            let item = out.item(i as u32);
            out.cdag[i * 16..i * 16 + 16].copy_from_slice(&item);
        }
        Ok(out)
    }
    pub fn dataset_item(&self, index: u32) -> [u8; 64] {
        encode_words(&self.item(index)).try_into().unwrap()
    }
    pub(crate) fn key(&self) -> (Algorithm, u64) {
        (self.algorithm, self.epoch)
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
    fn item(&self, index: u32) -> [u32; 16] {
        let mut mix = self.cache[index as usize % self.cache.len()];
        mix[0] ^= index;
        mix = hash512(&encode_words(&mix));
        for i in 0..512 {
            let parent = fnv(index ^ i, mix[i as usize % 16]) as usize % self.cache.len();
            for (v, b) in mix.iter_mut().zip(&self.cache[parent]) {
                *v = fnv(*v, *b);
            }
        }
        hash512(&encode_words(&mix))
    }
    /// Recompute the complete light kernel; returns header-format mix and PoW hash.
    /// This does not check the aux template, donor header or Quai target.
    pub fn compute(&self, seal: Hash, nonce: u64, height: u64) -> Result<(Hash, Hash), Error> {
        self.algorithm.sizes(height)?;
        if height / self.algorithm.epoch_length() != self.epoch {
            return Err(Error::Unsupported("different verification epoch"));
        }
        let mut input = seal;
        if self.algorithm == Algorithm::Kawpow {
            input.reverse();
        }
        let mut start = [0u32; 25];
        start[..8].copy_from_slice(&decode_words::<8>(&input));
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
        let mut mix: [[u32; 32]; 16] = std::array::from_fn(|lane| fill(seed, lane as u32));
        let period = height
            / if self.algorithm == Algorithm::Progpow {
                10
            } else {
                3
            };
        for round in 0..64 {
            self.round(period, round, &mut mix);
        }
        let mut result = [0x811c9dc5u32; 8];
        for (lane, values) in mix.iter().enumerate() {
            let mut r = 0x811c9dc5;
            for v in values {
                fnv1a(&mut r, *v);
            }
            fnv1a(&mut result[lane % 8], r);
        }
        let mut final_state = [0u32; 25];
        if self.algorithm == Algorithm::Progpow {
            final_state[..8].copy_from_slice(&decode_words::<8>(&input));
            final_state[8] = seed as u32;
            final_state[9] = (seed >> 32) as u32;
            final_state[10..18].copy_from_slice(&result);
        } else {
            final_state[..8].copy_from_slice(&start[..8]);
            final_state[8..16].copy_from_slice(&result);
            final_state[16..].copy_from_slice(&RAVEN[..9]);
        }
        permute(&mut final_state);
        let mut digest: Hash = encode_words(&result).try_into().unwrap();
        if self.algorithm == Algorithm::Kawpow {
            digest.reverse();
        }
        Ok((
            digest,
            encode_words::<8>(&final_state[..8].try_into().unwrap())
                .try_into()
                .unwrap(),
        ))
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
        let (actual, hash) = self.compute(seal, nonce, height)?;
        if actual != mix {
            return Err(Error::Invalid("mix hash"));
        }
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
    fn round(&self, seed: u64, round: u32, mix: &mut [[u32; 32]; 16]) {
        // Go evaluates this expression in uint32, including overflow at high epochs.
        let rows = (self.dataset_size / 256) as u32;
        let offset = mix[round as usize % 16][0] % (64u32.wrapping_mul(rows) / 64);
        let base = offset.wrapping_mul(64);
        let mut dag = [0u32; 64];
        for i in 0..4 {
            dag[i * 16..i * 16 + 16]
                .copy_from_slice(&self.item(base.wrapping_add(i as u32 * 16) / 16));
        }
        for (lane, registers) in mix.iter_mut().enumerate() {
            let (mut rng, dest, src) = init(seed);
            let (mut sc, mut dc) = (0usize, 0usize);
            for i in 0..18 {
                if i < 11 {
                    let source = src[sc % 32] as usize;
                    sc += 1;
                    let value = self.cdag[registers[source] as usize % 4096];
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
            let index = ((lane as u32 ^ round) % 16) as usize * 4;
            merge(&mut registers[0], dag[index], rng.next());
            for value in &dag[index + 1..index + 4] {
                let d = dest[dc % 32] as usize;
                dc += 1;
                merge(&mut registers[d], *value, rng.next());
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
            let bc: [u32; 5] = st[j..j + 5].try_into().unwrap();
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
