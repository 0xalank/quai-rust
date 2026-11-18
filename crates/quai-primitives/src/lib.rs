//! Native bounded Quai protocol primitives. No RPC client, FFI crypto or Go runtime.
//! Header behavior follows the pinned go-quai sources documented in NOTICE.md.
pub mod header;
pub mod vectors;
pub mod wire;
pub use header::{Header, Schedule};

pub type Hash = [u8; 32];

pub fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    let value = value.strip_prefix("0x").ok_or("hex prefix required")?;
    if value.len() % 2 != 0 {
        return Err("odd hex length".into());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |v: u8| -> Result<u8, String> {
                match v {
                    b'0'..=b'9' => Ok(v - b'0'),
                    b'a'..=b'f' => Ok(v - b'a' + 10),
                    b'A'..=b'F' => Ok(v - b'A' + 10),
                    _ => Err("invalid hex".into()),
                }
            };
            Ok(digit(pair[0])? * 16 + digit(pair[1])?)
        })
        .collect()
}
pub fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut result = String::with_capacity(2 + bytes.len() * 2);
    result.push_str("0x");
    for b in bytes {
        result.push(DIGITS[(b >> 4) as usize] as char);
        result.push(DIGITS[(b & 15) as usize] as char);
    }
    result
}

/// Exact unsigned threshold comparison for the Blake3 engine: H*D <= 2^256.
/// This does not validate ProgPoW, KAWPOW or auxiliary proof-of-work.
pub fn meets_blake3_target(hash: &Hash, difficulty: &[u8]) -> Result<bool, String> {
    if difficulty.len() > 32 || difficulty.iter().all(|v| *v == 0) {
        return Err("difficulty must be positive and at most 256 bits".into());
    }
    let mut d = [0u8; 32];
    d[32 - difficulty.len()..].copy_from_slice(difficulty);
    let mut product = [0u32; 64];
    for (i, a) in hash.iter().enumerate() {
        for (j, b) in d.iter().enumerate() {
            product[i + j + 1] += (*a as u32) * (*b as u32);
        }
    }
    for i in (1..64).rev() {
        product[i - 1] += product[i] >> 8;
        product[i] &= 255;
    }
    let mut target = [0u32; 64];
    target[31] = 1;
    Ok(product <= target)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn threshold_edges_are_integer_exact() {
        assert!(meets_blake3_target(&[255; 32], &[1]).unwrap());
        let mut half = [0u8; 32];
        half[0] = 128;
        assert!(meets_blake3_target(&half, &[2]).unwrap());
        half[31] = 1;
        assert!(!meets_blake3_target(&half, &[2]).unwrap());
        assert!(meets_blake3_target(&[0; 32], &[255; 32]).unwrap());
        assert!(meets_blake3_target(&[0; 32], &[0]).is_err());
        assert_eq!(
            decode_hex(&encode_hex(&[0, 1, 128, 255])).unwrap(),
            vec![0, 1, 128, 255]
        );
    }
}

pub mod transaction;

pub mod transaction_vectors;

pub mod receipt;
pub mod schema;
pub mod trie;
pub mod work;

pub mod foundation_vectors;

pub mod auxiliary;

/// Official upstream source revision qualified by the current frozen corpora.
pub const REFERENCE_REVISION: &str = "33db82f2891b267ba898ca38bd2dd8bf75cbb072";
pub mod rlp;

pub mod mpt;
