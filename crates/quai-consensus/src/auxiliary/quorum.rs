use crate::Error;
use k256::{
    elliptic_curve::{bigint::U256, ops::Reduce, sec1::ToEncodedPoint},
    ProjectivePoint, PublicKey, Scalar,
};
use quai_primitives::Hash;
use sha2::{Digest, Sha256};
/// params.MuSig2PublicKeys, from the chain spec.
pub const PINNED_KEYS: [&str; 3] = quai_primitives::spec::MUSIG2_PUBLIC_KEYS;
pub const PAIRS: [[usize; 2]; 6] = [[0, 1], [1, 0], [0, 2], [2, 0], [1, 2], [2, 1]];
fn tagged(tag: &[u8], message: &[u8]) -> Hash {
    let t = Sha256::digest(tag);
    let mut h = Sha256::new();
    h.update(t);
    h.update(t);
    h.update(message);
    h.finalize().into()
}
/// Trusted chain-configuration keys. No key material is read from an incoming proof.
#[derive(Clone)]
pub struct Quorum {
    keys: [[u8; 33]; 3],
    aggregate: [[u8; 33]; 6],
    verifiers: [k256::schnorr::VerifyingKey; 6],
}
/// The compiled-in quorum keys (params.MuSig2PublicKeys).
fn pinned_keys() -> Result<[[u8; 33]; 3], Error> {
    let mut out = [[0u8; 33]; 3];
    for (key, hex) in out.iter_mut().zip(PINNED_KEYS) {
        *key = quai_primitives::decode_hex(&format!("0x{hex}"))
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(Error::Invalid("pinned quorum key"))?;
    }
    Ok(out)
}
impl Quorum {
    /// The quorum of a chain spec (params.MuSig2PublicKeys for go-quai).
    pub fn for_spec(spec: &quai_primitives::spec::ChainSpec) -> Result<Self, Error> {
        Self::from_trusted_keys(spec.quorum_keys)
    }
    #[allow(clippy::expect_used)] // Infallible: compile-time pinned MuSig2 quorum keys verified by unit tests.
    pub fn pinned() -> Self {
        pinned_keys()
            .and_then(Self::from_trusted_keys)
            .expect("pinned quorum keys")
    }
    pub fn from_trusted_keys(keys: [[u8; 33]; 3]) -> Result<Self, Error> {
        let parsed = keys
            .iter()
            .map(|k| PublicKey::from_sec1_bytes(k).map_err(|_| Error::Invalid("quorum public key")))
            .collect::<Result<Vec<_>, _>>()?;
        let mut aggregate = Vec::new();
        let mut verifiers = Vec::new();
        for [a, b] in PAIRS {
            let encoded = [keys[a], keys[b]];
            let fingerprint = tagged(b"KeyAgg list", &encoded.concat());
            let second = encoded.iter().find(|k| **k != encoded[0]);
            let mut sum = ProjectivePoint::IDENTITY;
            for (index, key) in [a, b].iter().enumerate() {
                let coefficient = if second == Some(&encoded[index]) {
                    Scalar::ONE
                } else {
                    let mut message = fingerprint.to_vec();
                    message.extend(encoded[index]);
                    <Scalar as Reduce<U256>>::reduce_bytes(
                        &tagged(b"KeyAgg coefficient", &message).into(),
                    )
                };
                sum += ProjectivePoint::from(*parsed[*key].as_affine()) * coefficient;
            }
            let p = PublicKey::from_affine(sum.to_affine())
                .map_err(|_| Error::Invalid("quorum aggregate at infinity"))?;
            let p: [u8; 33] = p
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .map_err(|_| Error::Invalid("quorum aggregate encoding"))?;
            verifiers.push(
                k256::schnorr::VerifyingKey::from_bytes(&p[1..])
                    .map_err(|_| Error::Invalid("quorum x-only key"))?,
            );
            aggregate.push(p);
        }
        Ok(Self {
            keys,
            aggregate: aggregate
                .try_into()
                .map_err(|_| Error::Invalid("quorum aggregate count"))?,
            verifiers: verifiers
                .try_into()
                .map_err(|_| Error::Invalid("quorum verifier count"))?,
        })
    }
    pub fn keys(&self) -> &[[u8; 33]; 3] {
        &self.keys
    }
    pub fn aggregate_keys(&self) -> &[[u8; 33]; 6] {
        &self.aggregate
    }
    pub fn verify(&self, message: Hash, signature: &[u8]) -> bool {
        if signature.len() != 64 {
            return false;
        }
        let Ok(signature) = k256::schnorr::Signature::try_from(signature) else {
            return false;
        };
        self.verifiers
            .iter()
            .any(|k| k.verify_raw(&message, &signature).is_ok())
    }
}
