//! Quai ECDSA public-key recovery on libsecp256k1 (the `secp256k1` crate).
//!
//! Only the curve recovery step lives here. `transaction.rs` still applies
//! its own scalar, low-S and recovery-bit rules first. This module repeats
//! them so its result equals the former pure-Rust path for every input:
//! that path rejected high S inside its final verification step.
//! `recover_k256` is that former path, kept for differential tests.
use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
use secp256k1::Message;

const ORDER: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,
    0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
];
/// floor(ORDER / 2): the largest accepted S.
const HALF_ORDER: [u8; 32] = [
    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0x5d, 0x57, 0x6e, 0x73, 0x57, 0xa4, 0x50, 0x1d, 0xdf, 0xe9, 0x2f, 0x46, 0x68, 0x1b, 0x20, 0xa0,
];

/// Recover the 64-byte uncompressed public key (X || Y) from a prehash,
/// big-endian `r`, `s` and a recovery id of 0 or 1.
pub fn recover_pubkey(hash: &[u8; 32], r: &[u8; 32], s: &[u8; 32], bit: u8) -> Option<[u8; 64]> {
    // Big-endian arrays compare like integers.
    if *r == [0; 32] || *s == [0; 32] || *r >= ORDER || *s >= ORDER || *s > HALF_ORDER {
        return None;
    }
    let id = RecoveryId::try_from(i32::from(bit)).ok()?;
    let mut compact = [0u8; 64];
    compact[..32].copy_from_slice(r);
    compact[32..].copy_from_slice(s);
    let sig = RecoverableSignature::from_compact(&compact, id).ok()?;
    let key = sig.recover_ecdsa(Message::from_digest(*hash)).ok()?;
    let mut out = [0u8; 64];
    out.copy_from_slice(&key.serialize_uncompressed()[1..]);
    Some(out)
}

/// Former `k256` recovery with the same input contract. Test oracle only.
#[doc(hidden)]
pub fn recover_k256(hash: &[u8; 32], r: &[u8; 32], s: &[u8; 32], bit: u8) -> Option<[u8; 64]> {
    use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
    let sig = Signature::from_scalars(*r, *s).ok()?;
    let rec = RecoveryId::try_from(bit).ok()?;
    let key = VerifyingKey::recover_from_prehash(hash, &sig, rec).ok()?;
    let mut out = [0u8; 64];
    out.copy_from_slice(&key.to_encoded_point(false).as_bytes()[1..]);
    Some(out)
}
