//! Native Ethereum-compatible precompiles used by Quai, with shard translation.
use super::{
    word::{self},
    CapabilityError, Fault,
};
use num_bigint::BigUint;
use quai_primitives::transaction::{keccak, Address};
use sha2::{Digest, Sha256};
use substrate_bn::{AffineG1, AffineG2, Fq, Fq2, Fr, Group, Gt, G1, G2};
pub fn translate(a: Address, loc: [u8; 2]) -> Option<(u8, Address)> {
    if a[1..19].iter().any(|b| *b != 0)
        || !(1..=9).contains(&a[19])
        || a[0] != 0 && a[0] != (loc[0] << 4 | loc[1])
    {
        return None;
    }
    let mut local = a;
    local[0] = loc[0] << 4 | loc[1];
    Some((a[19], local))
}
fn data(input: &[u8], offset: usize, len: usize) -> Vec<u8> {
    let mut b = vec![0; len];
    if offset < input.len() {
        let n = len.min(input.len() - offset);
        b[..n].copy_from_slice(&input[offset..offset + n]);
    }
    b
}
fn small(n: &BigUint) -> Option<u64> {
    let b = n.to_bytes_be();
    if b.len() > 8 {
        None
    } else {
        Some(b.into_iter().fold(0, |a, b| (a << 8) | b as u64))
    }
}
fn modexp_gas(input: &[u8]) -> u64 {
    let base = BigUint::from_bytes_be(&data(input, 0, 32));
    let exp = BigUint::from_bytes_be(&data(input, 32, 32));
    let modu = BigUint::from_bytes_be(&data(input, 64, 32));
    let head = small(&base)
        .and_then(|b| usize::try_from(b).ok())
        .and_then(|b| b.checked_add(96))
        .map(|o| {
            BigUint::from_bytes_be(&data(input, o, small(&exp).unwrap_or(32).min(32) as usize))
        })
        .unwrap_or_default();
    let msb = head.bits().saturating_sub(1);
    let adjusted = if exp > BigUint::from(32u8) {
        (&exp - BigUint::from(32u8)) * BigUint::from(8u8) + BigUint::from(msb)
    } else {
        BigUint::from(msb)
    };
    let length = base.max(modu);
    let words = (length + BigUint::from(7u8)) / BigUint::from(8u8);
    let gas = &words * &words * adjusted.max(BigUint::from(1u8)) / BigUint::from(3u8);
    small(&gas).unwrap_or(u64::MAX).max(200)
}
pub fn required_gas(id: u8, input: &[u8]) -> u64 {
    let words = (input.len() as u64).div_ceil(32);
    match id {
        1 => 3000,
        2 => 60 + 12 * words,
        3 => 600 + 120 * words,
        4 => 15 + 3 * words,
        5 => modexp_gas(input),
        6 => 150,
        7 => 6000,
        8 => 45000 + 34000 * (input.len() as u64 / 192),
        9 => {
            if input.len() == 213 {
                u32::from_be_bytes(input[..4].try_into().unwrap()) as u64
            } else {
                0
            }
        }
        _ => u64::MAX,
    }
}
fn g1(input: &[u8]) -> Result<G1, Fault> {
    if input.iter().all(|b| *b == 0) {
        return Ok(G1::zero());
    }
    let x = Fq::from_slice(&input[..32]).map_err(|_| Fault::Precompile)?;
    let y = Fq::from_slice(&input[32..64]).map_err(|_| Fault::Precompile)?;
    AffineG1::new(x, y)
        .map(Into::into)
        .map_err(|_| Fault::Precompile)
}
fn g2(input: &[u8]) -> Result<G2, Fault> {
    if input.iter().all(|b| *b == 0) {
        return Ok(G2::zero());
    }
    let q = |o: usize| Fq::from_slice(&input[o..o + 32]).map_err(|_| Fault::Precompile);
    let x = Fq2::new(q(32)?, q(0)?);
    let y = Fq2::new(q(96)?, q(64)?);
    AffineG2::new(x, y)
        .map(Into::into)
        .map_err(|_| Fault::Precompile)
}
fn encode_g1(value: G1) -> Vec<u8> {
    let mut out = vec![0; 64];
    if let Some(a) = AffineG1::from_jacobian(value) {
        a.x().to_big_endian(&mut out[..32]).unwrap();
        a.y().to_big_endian(&mut out[32..]).unwrap();
    }
    out
}
fn recover(input: &[u8]) -> Vec<u8> {
    use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
    let b = data(input, 0, 128);
    if b[32..63].iter().any(|b| *b != 0) || !(27..=28).contains(&b[63]) {
        return vec![];
    }
    let Ok(sig) = Signature::from_slice(&b[64..128]) else {
        return vec![];
    };
    let bit = b[63] - 27;
    if sig.normalize_s().is_some() {
        return vec![];
    }
    let Some(rec) = RecoveryId::from_byte(bit) else {
        return vec![];
    };
    let Ok(key) = VerifyingKey::recover_from_prehash(&b[..32], &sig, rec) else {
        return vec![];
    };
    let hash = keccak(&key.to_encoded_point(false).as_bytes()[1..]);
    let mut out = vec![0; 12];
    out.extend(&hash[12..]);
    out
}
pub fn run(
    id: u8,
    input: &[u8],
    gas: u64,
) -> Result<Result<(Vec<u8>, u64), Fault>, CapabilityError> {
    if input.len() > 1 << 20 {
        return Err(CapabilityError::Limit("precompile input"));
    }
    let cost = required_gas(id, input);
    if cost > gas {
        return Ok(Err(Fault::OutOfGas));
    }
    let output: Result<Vec<u8>, Fault> = match id {
        1 => Ok(recover(input)),
        2 => Ok(Sha256::digest(input).to_vec()),
        3 => {
            let mut out = vec![0; 12];
            out.extend(ripemd::Ripemd160::digest(input));
            Ok(out)
        }
        4 => Ok(input.to_vec()),
        5 => {
            let lens = (0..3)
                .map(|i| word::uint(word::from_bytes(&data(input, i * 32, 32))))
                .collect::<Option<Vec<_>>>()
                .ok_or(CapabilityError::Limit("modexp lengths"))?;
            if lens[0] == 0 && lens[2] == 0 {
                Ok(vec![])
            } else {
                if lens.iter().any(|n| *n > 1 << 20) || lens.iter().sum::<u64>() > 1 << 20 {
                    return Err(CapabilityError::Limit("modexp operands"));
                }
                let (b, e, m) = (lens[0] as usize, lens[1] as usize, lens[2] as usize);
                let base = BigUint::from_bytes_be(&data(input, 96, b));
                let exp = BigUint::from_bytes_be(&data(input, 96 + b, e));
                let modu = BigUint::from_bytes_be(&data(input, 96 + b + e, m));
                let mut out = vec![0; m];
                if modu != BigUint::default() {
                    let value = base.modpow(&exp, &modu).to_bytes_be();
                    let n = value.len().min(m);
                    out[m - n..].copy_from_slice(&value[value.len() - n..]);
                }
                Ok(out)
            }
        }
        6 => (|| {
            let b = data(input, 0, 128);
            Ok(encode_g1(g1(&b[..64])? + g1(&b[64..])?))
        })(),
        7 => (|| {
            let b = data(input, 0, 96);
            let scalar = Fr::from_slice(&b[64..]).map_err(|_| Fault::Precompile)?;
            Ok(encode_g1(g1(&b[..64])? * scalar))
        })(),
        8 => (|| {
            if input.len() % 192 != 0 {
                return Err(Fault::Precompile);
            }
            let pairs = input
                .chunks_exact(192)
                .map(|b| Ok((g1(&b[..64])?, g2(&b[64..])?)))
                .collect::<Result<Vec<_>, Fault>>()?;
            let mut out = vec![0; 32];
            if substrate_bn::pairing_batch(&pairs) == Gt::one() {
                out[31] = 1;
            }
            Ok(out)
        })(),
        9 => blake2f(input),
        _ => Err(Fault::Precompile),
    };
    Ok(output.map(|v| (v, gas - cost)))
}
const IV: [u64; 8] = [
    0x6a09e667f3bcc908,
    0xbb67ae8584caa73b,
    0x3c6ef372fe94f82b,
    0xa54ff53a5f1d36f1,
    0x510e527fade682d1,
    0x9b05688c2b3e6c1f,
    0x1f83d9abfb41bd6b,
    0x5be0cd19137e2179,
];
const SIGMA: [[usize; 16]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];
fn mix(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}
fn blake2f(b: &[u8]) -> Result<Vec<u8>, Fault> {
    if b.len() != 213 || b[212] > 1 {
        return Err(Fault::Precompile);
    }
    let rounds = u32::from_be_bytes(b[..4].try_into().unwrap());
    let mut h = [0; 8];
    let mut m = [0; 16];
    for (i, v) in h.iter_mut().enumerate() {
        *v = u64::from_le_bytes(b[4 + i * 8..12 + i * 8].try_into().unwrap());
    }
    for (i, v) in m.iter_mut().enumerate() {
        *v = u64::from_le_bytes(b[68 + i * 8..76 + i * 8].try_into().unwrap());
    }
    let mut v = [0; 16];
    v[..8].copy_from_slice(&h);
    v[8..].copy_from_slice(&IV);
    v[12] ^= u64::from_le_bytes(b[196..204].try_into().unwrap());
    v[13] ^= u64::from_le_bytes(b[204..212].try_into().unwrap());
    if b[212] == 1 {
        v[14] = !v[14];
    }
    for i in 0..rounds {
        let s = SIGMA[i as usize % 10];
        mix(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
        mix(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
        mix(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
        mix(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
        mix(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
        mix(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
        mix(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
        mix(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
    }
    Ok((0..8)
        .flat_map(|i| (h[i] ^ v[i] ^ v[i + 8]).to_le_bytes())
        .collect())
}
