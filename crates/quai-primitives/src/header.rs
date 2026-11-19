//! Work-object header codec and commitment hashing; consensus validation is separate.
use crate::{
    wire::{BytesValue, ShareCount, WorkHeader},
    Hash,
};
use prost::Message;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Schedule {
    pub kawpow_fork: u64,
    pub transition_period: u64,
}

#[derive(Clone)]
pub struct Header {
    wire: WorkHeader,
    activated: bool,
    transition: bool,
}
fn magnitude(value: Option<Vec<u8>>) -> Result<Vec<u8>, String> {
    let value = value.ok_or("required integer missing")?;
    let start = value.iter().position(|v| *v != 0).unwrap_or(value.len());
    if value.len() - start > 32 {
        return Err("integer exceeds supported 256-bit domain".into());
    }
    Ok(value[start..].to_vec())
}
fn fixed(value: Option<BytesValue>, length: usize) -> Result<Option<BytesValue>, String> {
    let value = value.ok_or("required bytes message missing")?.value;
    if value.len() > length {
        return Err("oversized fixed bytes field".into());
    }
    let mut bytes = vec![0; length];
    bytes[length - value.len()..].copy_from_slice(&value);
    Ok(Some(BytesValue { value: bytes }))
}
fn share(value: Option<ShareCount>) -> Result<Option<ShareCount>, String> {
    let value = value.ok_or("post-fork share count missing")?;
    let nonempty = |v: Option<Vec<u8>>| -> Result<Option<Vec<u8>>, String> {
        let v = magnitude(Some(v.unwrap_or_default()))?;
        Ok(Some(if v.is_empty() { vec![0] } else { v }))
    };
    Ok(Some(ShareCount {
        difficulty: nonempty(value.difficulty)?,
        count: nonempty(value.count)?,
        uncled: nonempty(value.uncled)?,
    }))
}

impl Header {
    pub fn decode(bytes: &[u8], schedule: Schedule) -> Result<Self, String> {
        Self::decode_profile(bytes, schedule, false)
    }
    /// Explicit auxiliary-aware codec; does not validate signature, PoW or policy.
    pub fn decode_auxiliary(bytes: &[u8], schedule: Schedule) -> Result<Self, String> {
        Self::decode_profile(bytes, schedule, true)
    }
    fn decode_profile(
        bytes: &[u8],
        schedule: Schedule,
        allow_auxiliary: bool,
    ) -> Result<Self, String> {
        // go-quai bounds a header only by the 3 MiB message that carries it.
        if bytes.len() > crate::schema::MAX_MESSAGE_BYTES {
            return Err("header input exceeds the 3 MiB message bound".into());
        }
        if allow_auxiliary {
            crate::schema::preflight(".block.ProtoWorkObjectHeader", bytes)?;
        }
        let mut wire = WorkHeader::decode(bytes).map_err(|_| "malformed header protobuf")?;
        wire.header_hash = fixed(wire.header_hash, 32)?;
        wire.parent_hash = fixed(wire.parent_hash, 32)?;
        wire.tx_hash = fixed(wire.tx_hash, 32)?;
        wire.mix_hash = fixed(wire.mix_hash, 32)?;
        wire.primary_coinbase = fixed(wire.primary_coinbase, 20)?;
        wire.number = Some(magnitude(wire.number)?);
        wire.difficulty = Some(magnitude(wire.difficulty)?);
        let prime_bytes = magnitude(wire.prime_terminus_number.take())?;
        if prime_bytes.len() > 8 {
            return Err("prime height exceeds supported u64 domain".into());
        }
        let mut padded = [0u8; 8];
        padded[8 - prime_bytes.len()..].copy_from_slice(&prime_bytes);
        wire.prime_terminus_number = Some(prime_bytes);
        let prime = u64::from_be_bytes(padded);
        if wire.nonce.is_none() || wire.time.is_none() {
            return Err("required nonce/time missing".into());
        }
        let location = wire.location.as_ref().ok_or("required location missing")?;
        if location.value.len() > 2 || location.value.iter().any(|n| *n > 15) {
            return Err("unsupported chain location".into());
        }
        wire.lock = Some(wire.lock.unwrap_or(0) as u8 as u32);
        let activated = prime >= schedule.kawpow_fork;
        let transition = activated
            && prime
                < schedule
                    .kawpow_fork
                    .checked_add(schedule.transition_period)
                    .ok_or("fork schedule overflow")?;
        if activated {
            // go-quai decodes an AuxPow only when it has a donor header and a
            // coinbase transaction, and drops any other without checking it
            // (core/types/wo.go:1652): the header then has no AuxPow, in its
            // encoding and in its hash. A present but empty field counts as
            // present, as in protobuf-go.
            if let Some(bytes) = &wire.aux_pow {
                let aux = crate::schema::block::ProtoAuxPow::decode(bytes.as_slice())
                    .map_err(|_| "malformed auxiliary protobuf")?;
                if aux.header.is_none() || aux.transaction.is_none() {
                    wire.aux_pow = None;
                }
            }
            if let Some(bytes) = &mut wire.aux_pow {
                if !allow_auxiliary {
                    return Err("auxiliary header form not implemented".into());
                }
                *bytes = crate::auxiliary::canonical_pow(bytes)?.encode_to_vec();
            }
            wire.scrypt_diff_and_count = share(wire.scrypt_diff_and_count)?;
            wire.sha_diff_and_count = share(wire.sha_diff_and_count)?;
            wire.sha_share_target = Some(magnitude(wire.sha_share_target)?);
            wire.scrypt_share_target = Some(magnitude(wire.scrypt_share_target)?);
            wire.kawpow_difficulty = Some(magnitude(wire.kawpow_difficulty)?);
        } else {
            wire.aux_pow = None;
            wire.scrypt_diff_and_count = None;
            wire.sha_diff_and_count = None;
            wire.sha_share_target = None;
            wire.scrypt_share_target = None;
            wire.kawpow_difficulty = None;
        }
        Ok(Self {
            wire,
            activated,
            transition,
        })
    }
    pub fn wire(&self) -> &WorkHeader {
        &self.wire
    }
    pub fn encode(&self) -> Vec<u8> {
        self.wire.encode_to_vec()
    }
    pub fn seal_bytes(&self) -> Vec<u8> {
        let mut seal = self.wire.clone();
        seal.nonce = None;
        seal.mix_hash = None;
        seal.aux_pow = None;
        if self.activated {
            seal.primary_coinbase = None;
        }
        seal.encode_to_vec()
    }
    pub fn seal_hash(&self) -> Hash {
        let first = blake3::hash(&self.seal_bytes());
        if self.activated {
            let mut hasher = blake3::Hasher::new();
            hasher.update(first.as_bytes());
            hasher.update(&self.wire.primary_coinbase.as_ref().unwrap().value);
            *hasher.finalize().as_bytes()
        } else {
            *first.as_bytes()
        }
    }
    /// Commitment for legacy/transition headers, not the ProgPoW mining algorithm.
    pub fn legacy_hash(&self) -> Hash {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&self.wire.mix_hash.as_ref().unwrap().value);
        hasher.update(&self.seal_hash());
        hasher.update(&self.wire.nonce.unwrap().to_be_bytes());
        *hasher.finalize().as_bytes()
    }
    /// core/types/wo.go `WorkObjectHeader.Hash`: the ProgPoW hash before
    /// KawPow and for transition headers without AuxPow; otherwise
    /// `WoCustomPowHash`, blake3 of the protobuf AuxPow. Without an AuxPow that
    /// encoding is empty: a ProgPoW header at prime terminus
    /// KawPowForkBlock + KawPowTransitionPeriod (1,301,100) is still valid
    /// (`CheckPowIdValidity` and `validateProgpowHeader` use `>`), and Go
    /// names it blake3 of no bytes.
    pub fn block_hash(&self) -> Result<Hash, String> {
        if self.activated && (self.wire.aux_pow.is_some() || !self.transition) {
            let bytes = self.wire.aux_pow.as_deref().unwrap_or_default();
            return Ok(*blake3::hash(bytes).as_bytes());
        }
        Ok(self.legacy_hash())
    }
    /// Proof identity used by Polaris inclusion checks; auxiliary encodings
    /// that carry identical donor work share one identity.
    pub fn pow_identity(&self) -> Result<Hash, String> {
        if self.activated {
            if let Some(bytes) = &self.wire.aux_pow {
                let aux = crate::auxiliary::canonical_pow(bytes)?;
                if let Some(header) = aux.header.as_ref().filter(|b| !b.is_empty()) {
                    return Ok(*blake3::hash(header).as_bytes());
                }
            }
        }
        self.block_hash()
    }
    pub fn blake3_target_valid(&self) -> Result<bool, String> {
        crate::meets_blake3_target(&self.legacy_hash(), self.wire.difficulty.as_ref().unwrap())
    }
}
