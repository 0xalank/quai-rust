//! Native Ethereum-compatible precompiles used by Quai, with shard translation.
use super::{
    word::{from_bytes, low_u64},
    CapabilityError, Fault,
};
use num_bigint::BigUint;
use quai_primitives::transaction::{keccak, Address};
use sha2::{Digest, Sha256};
use substrate_bn::{AffineG1, AffineG2, Fq, Fq2, Fr, Group, Gt, G1, G2};
/// go-quai `vm.ActivePrecompiles` (core/vm/contracts.go:58-81): `0x…01` to
/// `0x…09` with the zone prefix and with 0x00 as the first byte.
/// `StateDB.PrepareAccessList` (core/state/statedb.go:1295) puts them in the
/// access list of every message: calls, creations and ETXs
/// (core/state_transition.go:380).
pub fn active(location: [u8; 2]) -> Vec<Address> {
    let mut out = vec![];
    for id in 1..=9u8 {
        let mut a = [0; 20];
        a[19] = id;
        out.push(a);
        a[0] = location[0] << 4 | location[1];
        out.push(a);
    }
    out
}
/// go-quai `EVM.precompile` (core/vm/evm.go:55-68). An address that is not
/// one of the zone's precompiles gets the zone prefix as its first byte and
/// is looked up again, so `XX 00…00 0N` (N 1 to 9) is precompile N of this
/// zone for every first byte XX, also another zone's prefix. `EVM.Call`,
/// `CallCode`, `DelegateCall` and `StaticCall` then run it at the zone's form
/// of the address. The checks before the lookup see the address as written:
/// the access list, and the address scope of CALL's gas function
/// (core/vm/gas_table.go:197), which fails for another zone first.
pub fn translate(a: Address, loc: [u8; 2]) -> Option<(u8, Address)> {
    if a[1..19].iter().any(|b| *b != 0) || !(1..=9).contains(&a[19]) {
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
/// go-quai `getData`: `size` bytes from `start`, zero-padded past the end.
fn get_data(input: &[u8], start: u64, size: u64) -> Vec<u8> {
    let length = input.len() as u64;
    let start = start.min(length);
    let end = start.saturating_add(size).min(length);
    let mut out = input[start as usize..end as usize].to_vec();
    out.resize(size as usize, 0);
    out
}
/// go-quai core/vm/contracts.go `bigModExp.Run`. Each length is the low
/// 64 bits of its 32-byte field (Go `big.Int.Uint64`). The gas check in `run`
/// has already passed, so EIP-2565 pricing bounds every length that is read:
/// base and modulus need `ceil(len / 8)² / 3` gas, and a nonzero base or
/// modulus makes a long exponent cost `8 × (len - 32) / 3` gas.
fn modexp(input: &[u8]) -> Vec<u8> {
    let length = |i: u64| low_u64(from_bytes(&get_data(input, i * 32, 32)));
    let (b, e, m) = (length(0), length(1), length(2));
    if b == 0 && m == 0 {
        return vec![];
    }
    let tail = input.get(96..).unwrap_or_default();
    let base = BigUint::from_bytes_be(&get_data(tail, 0, b));
    let exp = BigUint::from_bytes_be(&get_data(tail, b, e));
    let modu = BigUint::from_bytes_be(&get_data(tail, b.wrapping_add(e), m));
    let mut out = vec![0; m as usize];
    if modu != BigUint::default() {
        let value = base.modpow(&exp, &modu).to_bytes_be();
        let n = value.len().min(out.len());
        let start = out.len() - n;
        out[start..].copy_from_slice(&value[value.len() - n..]);
    }
    out
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
        // EIP-152: the round count in the first four bytes of a 213-byte input.
        9 => match input.first_chunk::<4>() {
            Some(rounds) if input.len() == 213 => u32::from_be_bytes(*rounds) as u64,
            _ => 0,
        },
        _ => u64::MAX,
    }
}
// `g1` reads 64 and `g2` 128 bytes: callers pass `data` padded input or
// `chunks_exact(192)` pieces of exactly those lengths.
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
    // `Fq::to_big_endian` fails only when the slice is not 32 bytes long.
    let mut out = vec![0; 64];
    if let Some(a) = AffineG1::from_jacobian(value) {
        let (x, y) = out.split_at_mut(32);
        let _ = a.x().to_big_endian(x);
        let _ = a.y().to_big_endian(y);
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
    // No input cap: go-quai `RunPrecompiledContract` only checks gas. The input
    // comes from paid memory or transaction data.
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
        5 => Ok(modexp(input)),
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
fn blake2f(input: &[u8]) -> Result<Vec<u8>, Fault> {
    // EIP-152 input: rounds (4 bytes, big-endian), h (8 words), m (16 words),
    // t (2 words), final-block flag; each word is a little-endian u64.
    let Ok(b) = <&[u8; 213]>::try_from(input) else {
        return Err(Fault::Precompile);
    };
    if b[212] > 1 {
        return Err(Fault::Precompile);
    }
    // Word `i` occupies bytes 4 + 8i .. 12 + 8i; i <= 25 stays below 212.
    let word = |i: usize| u64::from_le_bytes(std::array::from_fn(|j| b[4 + i * 8 + j]));
    let rounds = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let h: [u64; 8] = std::array::from_fn(word);
    let m: [u64; 16] = std::array::from_fn(|i| word(8 + i));
    let mut v = [0; 16];
    v[..8].copy_from_slice(&h);
    v[8..].copy_from_slice(&IV);
    v[12] ^= word(24);
    v[13] ^= word(25);
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
