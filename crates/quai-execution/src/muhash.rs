//! Quai's Kaspa MuHash3072 accumulator. Inputs are already consensus-encoded
//! UTXO/coinbase-lockup hashes; byte order and domain keys match go-muhash v0.0.4.
use blake2::{
    digest::{consts::U32, Mac},
    Blake2bMac,
};
use chacha20::{
    cipher::{KeyIvInit, StreamCipher},
    ChaCha20,
};
use num_bigint::BigUint;
use quai_primitives::Hash;
fn prime() -> BigUint {
    (BigUint::from(1u8) << 3072usize) - BigUint::from(1_103_717u32)
}
fn keyed(key: &[u8], input: &[u8]) -> Hash {
    // The keys are the constants b"MuHashElement" and b"MuHashFinalize";
    // BLAKE2b refuses only keys longer than 64 bytes.
    let mut m = <Blake2bMac<U32> as Mac>::new_from_slice(key)
        .expect("MuHash keys are 13 and 14 bytes; BLAKE2b allows 64");
    m.update(input);
    m.finalize().into_bytes().into()
}
fn element(data: &[u8]) -> BigUint {
    let key = keyed(b"MuHashElement", data);
    let nonce = [0; 12];
    let mut stream = ChaCha20::new((&key).into(), (&nonce).into());
    let mut bytes = [0; 384];
    stream.apply_keystream(&mut bytes);
    BigUint::from_bytes_le(&bytes) % prime()
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MuHash {
    numerator: BigUint,
    denominator: BigUint,
}
impl Default for MuHash {
    fn default() -> Self {
        Self {
            numerator: BigUint::from(1u8),
            denominator: BigUint::from(1u8),
        }
    }
}
impl MuHash {
    pub fn add(&mut self, data: &[u8]) {
        self.numerator = (&self.numerator * element(data)) % prime();
    }
    pub fn remove(&mut self, data: &[u8]) {
        self.denominator = (&self.denominator * element(data)) % prime();
    }
    pub fn combine(&mut self, other: &Self) {
        let p = prime();
        self.numerator = (&self.numerator * &other.numerator) % &p;
        self.denominator = (&self.denominator * &other.denominator) % p;
    }
    pub fn serialize(&self) -> [u8; 384] {
        let p = prime();
        let value = if self.denominator == BigUint::from(1u8) {
            self.numerator.clone()
        } else {
            (&self.numerator * self.denominator.modpow(&(&p - BigUint::from(2u8)), &p)) % p
        };
        let bytes = value.to_bytes_le();
        let mut out = [0; 384];
        out[..bytes.len()].copy_from_slice(&bytes);
        out
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() != 384 {
            return Err("MuHash encoding must contain 384 bytes".into());
        }
        let value = BigUint::from_bytes_le(bytes);
        if value >= prime() {
            return Err("MuHash field overflow".into());
        }
        Ok(Self {
            numerator: value,
            denominator: BigUint::from(1u8),
        })
    }
    pub fn hash(&self) -> Hash {
        keyed(b"MuHashFinalize", &self.serialize())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_accumulator_matches_the_go_genesis_commitment() {
        assert_eq!(
            quai_primitives::encode_hex(&MuHash::default().hash()),
            "0x544eb3142c000f0ad2c76ac41f4222abbababed830eeafee4b6dc56b52d5cac0"
        );
    }
    #[test]
    fn removing_a_spent_element_and_reordering_preserve_commitments() {
        let mut a = MuHash::default();
        a.add(b"first");
        a.add(b"second");
        a.remove(b"first");
        let mut b = MuHash::default();
        b.add(b"second");
        assert_eq!(a.serialize(), b.serialize());
        let bytes = a.serialize();
        assert_eq!(MuHash::from_bytes(&bytes).unwrap().hash(), b.hash());
    }
}
