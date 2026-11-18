use crate::Error;
use k256::{
    elliptic_curve::{bigint::U256, ops::Reduce, sec1::ToEncodedPoint},
    ProjectivePoint, PublicKey, Scalar,
};
use quai_primitives::Hash;
use sha2::{Digest, Sha256};
pub const PINNED_KEYS: [&str; 3] = [
    "02cae78e4905da54dc93d6009119d56a2ef91b11e129c8fa41342310ec3a9b499c",
    "03374cbfcea6cb8172b2fdc09e9f7ec0cc2ff10aaa2192fb19fa04d9391b0a75c2",
    "03abfe95f7d487a44cac59c9d62fe63e9e01dc3e06dc9fe96b7038d73d99a62fa6",
];
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
impl Quorum {
    pub fn pinned() -> Self {
        Self::from_trusted_keys(std::array::from_fn(|i| {
            quai_primitives::decode_hex(&format!("0x{}", PINNED_KEYS[i]))
                .unwrap()
                .try_into()
                .unwrap()
        }))
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
            let p: [u8; 33] = p.to_encoded_point(true).as_bytes().try_into().unwrap();
            verifiers.push(
                k256::schnorr::VerifyingKey::from_bytes(&p[1..])
                    .map_err(|_| Error::Invalid("quorum x-only key"))?,
            );
            aggregate.push(p);
        }
        Ok(Self {
            keys,
            aggregate: aggregate.try_into().unwrap(),
            verifiers: verifiers.try_into().unwrap(),
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
