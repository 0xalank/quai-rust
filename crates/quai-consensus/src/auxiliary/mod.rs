//! Signed auxiliary binding checks. Does not establish full header/block validity.
pub mod coinbase;
pub mod quorum;
use crate::Error;
use prost::Message;
use quai_primitives::{
    auxiliary::{self, canonical_pow},
    schema::block::{ProtoAuxPow, ProtoAuxTemplate},
    Hash,
};
pub use quorum::Quorum;
#[derive(Clone, Debug)]
pub struct Auxiliary {
    wire: ProtoAuxPow,
}
impl Auxiliary {
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        Ok(Self {
            wire: canonical_pow(bytes)
                .map_err(|_| Error::Unsupported("bounded auxiliary semantic domain"))?,
        })
    }
    pub fn encode(&self) -> Vec<u8> {
        self.wire.encode_to_vec()
    }
    pub fn id(&self) -> u8 {
        self.wire.chain_id.unwrap() as u8
    }
    pub fn header(&self) -> &[u8] {
        self.wire.header.as_ref().unwrap()
    }
    pub fn signature(&self) -> &[u8] {
        self.wire.signature.as_deref().unwrap_or_default()
    }
    pub fn transaction(&self) -> &[u8] {
        self.wire.transaction.as_deref().unwrap_or_default()
    }
    pub fn template(&self) -> Result<ProtoAuxTemplate, Error> {
        let c = coinbase::Coinbase::decode(self.transaction())?;
        let h = self.header();
        Ok(ProtoAuxTemplate {
            chain_id: Some(self.id() as u32),
            prev_hash: Some(h[4..36].to_vec()),
            version: Some(u32::from_le_bytes(h[..4].try_into().unwrap())),
            bits: Some(u32::from_le_bytes(h[72..76].try_into().unwrap())),
            signature_time: Some(c.signature_time),
            height: Some(if self.id() == 1 {
                u32::from_le_bytes(h[76..80].try_into().unwrap())
            } else {
                c.height
            }),
            coinbase_out: Some(c.outputs),
            merkle_branch: self.wire.merkle_branch.clone(),
            sigs: self.wire.signature.clone(),
            aux_pow2: Some(self.wire.auxpow2.clone().unwrap_or_default()),
        })
    }
    pub fn admit(
        &self,
        quai_seal: Hash,
        quai_time: u64,
        quorum: &Quorum,
    ) -> Result<Admission, Error> {
        self.admit_policy(quai_seal, quai_time, quorum, false)
    }
    /// Historical consensus exception, available only to the inclusion pipeline.
    pub(crate) fn admit_inclusion(
        &self,
        quai_seal: Hash,
        quai_time: u64,
        quorum: &Quorum,
        external_coinbase: bool,
    ) -> Result<Admission, Error> {
        self.admit_policy(
            quai_seal,
            quai_time,
            quorum,
            external_coinbase && matches!(self.id(), 2..=4),
        )
    }
    fn admit_policy(
        &self,
        quai_seal: Hash,
        quai_time: u64,
        quorum: &Quorum,
        allow_unpaid: bool,
    ) -> Result<Admission, Error> {
        let h = self.header();
        let c = coinbase::Coinbase::decode(self.transaction())?;
        if u32::from_le_bytes(h[68..72].try_into().unwrap()) < c.signature_time
            || quai_time < u64::from(c.signature_time)
        {
            return Err(Error::Invalid("auxiliary signature time"));
        }
        self.verify_binding(quai_seal)?;
        let template = self.template()?;
        let template_hash = auxiliary::template_hash(&template);
        let signature_verified = quorum.verify(template_hash, self.signature());
        if !signature_verified && !allow_unpaid {
            return Err(Error::Invalid("auxiliary quorum signature"));
        }
        Ok(Admission {
            quai_seal,
            template_hash,
            pow_id: self.id(),
            signature_time: c.signature_time,
            signature_verified,
        })
    }
    /// Seal/coinbase/donor commitments, without signature, time or proof admission.
    pub fn verify_binding(&self, quai_seal: Hash) -> Result<(), Error> {
        let h = self.header();
        let c = coinbase::Coinbase::decode(self.transaction())?;
        let expected = if self.id() == 4 {
            let doge: Hash = self
                .wire
                .auxpow2
                .as_ref()
                .unwrap()
                .as_slice()
                .try_into()
                .unwrap();
            if doge == [0; 32] {
                return Err(Error::Invalid("scrypt auxiliary hash is zero"));
            }
            if c.merkle_size != 2 || c.merkle_nonce != 0 {
                return Err(Error::Invalid("scrypt merged-mining parameters"));
            }
            coinbase::scrypt_commitment(doge, quai_seal)
        } else {
            quai_seal
        };
        if c.commitment != expected {
            return Err(Error::Invalid("auxiliary Quai seal commitment"));
        }
        if coinbase::merkle_root(self.transaction(), &self.wire.merkle_branch)? != h[36..68] {
            return Err(Error::Invalid("auxiliary donor Merkle root"));
        }
        Ok(())
    }
    pub fn kawpow_input(&self) -> Result<(Hash, u64, u64, Hash), Error> {
        if self.id() != 1 {
            return Err(Error::Invalid("not a KAWPOW proof"));
        }
        let h = self.header();
        let seal = coinbase::double_sha(&h[..80]);
        let nonce = u64::from_le_bytes(h[80..88].try_into().unwrap());
        let height = u32::from_le_bytes(h[76..80].try_into().unwrap()) as u64;
        let mix = h[88..120].try_into().unwrap();
        Ok((seal, nonce, height, mix))
    }
}
#[derive(Clone, Debug)]
pub struct Admission {
    quai_seal: Hash,
    template_hash: Hash,
    pow_id: u8,
    signature_time: u32,
    signature_verified: bool,
}
impl Admission {
    pub fn signature_verified(&self) -> bool {
        self.signature_verified
    }
    pub fn quai_seal(&self) -> Hash {
        self.quai_seal
    }
    pub fn template_hash(&self) -> Hash {
        self.template_hash
    }
    pub fn pow_id(&self) -> u8 {
        self.pow_id
    }
    pub fn signature_time(&self) -> u32 {
        self.signature_time
    }
}

#[cfg(test)]
mod inclusion_tests {
    use super::*;
    #[test]
    fn external_donor_signature_exception_does_not_bypass_bindings_or_block_policy() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../../../fixtures/protocol/auxiliary.json"))
                .unwrap();
        let quorum = Quorum::pinned();
        for id in [1, 3, 4] {
            let v = corpus["auxiliaries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| {
                    v["profile"] == "pinned"
                        && v["valid"] == true
                        && v["id"].as_str().unwrap().eq(&format!("pinned-{id}-0"))
                })
                .unwrap();
            let mut a = Auxiliary::decode(
                &quai_primitives::decode_hex(v["aux"].as_str().unwrap()).unwrap(),
            )
            .unwrap();
            a.wire.signature.as_mut().unwrap()[0] ^= 1;
            let seal: Hash = quai_primitives::decode_hex(v["quai_seal"].as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            let time = v["quai_time"].as_u64().unwrap();
            assert!(a.admit(seal, time, &quorum).is_err());
            assert!(a.admit_inclusion(seal, time, &quorum, false).is_err());
            let result = a.admit_inclusion(seal, time, &quorum, true);
            if id == 1 {
                assert!(result.is_err())
            } else {
                assert!(!result.unwrap().signature_verified());
                let mut bad = seal;
                bad[0] ^= 1;
                assert!(a.admit_inclusion(bad, time, &quorum, true).is_err());
            }
        }
    }
}
