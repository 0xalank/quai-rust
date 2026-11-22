//! SHA-256d and Litecoin scrypt(1024,1,1) donor hashes over an exact 80-byte header.
//! Returned bytes match go-quai's reversed donor-header PowHash. No merged-mining
//! coinbase, template, Merkle branch, quorum or donor chain validity is implied.
use crate::Error;
use hmac::{Hmac, Mac};
use quai_primitives::Hash;
use sha2::{Digest, Sha256};
pub fn sha256d(header: &[u8]) -> Result<Hash, Error> {
    if header.len() != 80 {
        return Err(Error::Invalid("donor header must be 80 bytes"));
    }
    let mut out: Hash = Sha256::digest(Sha256::digest(header)).into();
    out.reverse();
    Ok(out)
}
fn pbkdf_one(password: &[u8], salt: &[u8], length: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    for block in 1..=length.div_ceil(32) {
        // HMAC takes a key of any length.
        let mut h = Hmac::<Sha256>::new_from_slice(password)
            .map_err(|_| Error::Invalid("scrypt password"))?;
        h.update(salt);
        h.update(&(block as u32).to_be_bytes());
        out.extend_from_slice(&h.finalize().into_bytes());
    }
    out.truncate(length);
    Ok(out)
}
fn salsa8(x: &mut [u32; 16]) {
    let start = *x;
    for _ in 0..4 {
        for i in 0..4 {
            let a = i * 5;
            let b = (a + 4) % 16;
            let c = (a + 8) % 16;
            let d = (a + 12) % 16;
            x[b] ^= x[a].wrapping_add(x[d]).rotate_left(7);
            x[c] ^= x[b].wrapping_add(x[a]).rotate_left(9);
            x[d] ^= x[c].wrapping_add(x[b]).rotate_left(13);
            x[a] ^= x[d].wrapping_add(x[c]).rotate_left(18);
        }
        for i in 0..4 {
            let a = i * 5;
            let b = i * 4 + (i + 1) % 4;
            let c = i * 4 + (i + 2) % 4;
            let d = i * 4 + (i + 3) % 4;
            x[b] ^= x[a].wrapping_add(x[d]).rotate_left(7);
            x[c] ^= x[b].wrapping_add(x[a]).rotate_left(9);
            x[d] ^= x[c].wrapping_add(x[b]).rotate_left(13);
            x[a] ^= x[d].wrapping_add(x[c]).rotate_left(18);
        }
    }
    for (a, b) in x.iter_mut().zip(start) {
        *a = a.wrapping_add(b);
    }
}
fn blockmix(x: &mut [u32; 32]) {
    let mut t: [u32; 16] = std::array::from_fn(|i| x[16 + i]);
    let mut out = [0; 32];
    for half in 0..2 {
        for i in 0..16 {
            t[i] ^= x[half * 16 + i];
        }
        salsa8(&mut t);
        out[half * 16..half * 16 + 16].copy_from_slice(&t);
    }
    *x = out;
}
pub fn scrypt(header: &[u8]) -> Result<Hash, Error> {
    if header.len() != 80 {
        return Err(Error::Invalid("donor header must be 80 bytes"));
    }
    let b = pbkdf_one(header, header, 128)?;
    let mut x: [u32; 32] = std::array::from_fn(|i| {
        u32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]])
    });
    let mut memory = vec![[0u32; 32]; 1024];
    for row in &mut memory {
        *row = x;
        blockmix(&mut x);
    }
    for _ in 0..1024 {
        let index = x[16] as usize & 1023;
        for (a, b) in x.iter_mut().zip(memory[index]) {
            *a ^= b;
        }
        blockmix(&mut x);
    }
    let salt: Vec<_> = x.iter().flat_map(|n| n.to_le_bytes()).collect();
    let mut out: Hash = pbkdf_one(header, &salt, 32)?
        .try_into()
        .map_err(|_| Error::Invalid("scrypt output length"))?;
    out.reverse();
    Ok(out)
}
