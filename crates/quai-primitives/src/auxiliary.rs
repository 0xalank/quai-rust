//! Bounded semantic auxiliary codecs. Signature/PoW/admission is separate.
use crate::{
    schema::{
        self,
        block::{ProtoAuxPow, ProtoAuxTemplate},
    },
    Hash,
};
use prost::Message;
use sha2::{Digest, Sha256};
pub const MAX_AUX_BYTES: usize = 2 << 20;
pub const MAX_BRANCH: usize = 64;
pub fn canonical_pow(bytes: &[u8]) -> Result<ProtoAuxPow, String> {
    if bytes.len() > MAX_AUX_BYTES {
        return Err("auxiliary bytes exceed supported scope".into());
    }
    schema::preflight(".block.ProtoAuxPow", bytes)?;
    let mut p = ProtoAuxPow::decode(bytes).map_err(|_| "malformed auxiliary protobuf")?;
    let id = p.chain_id.unwrap_or(0);
    let size = match id {
        1 => 120,
        2..=4 => 80,
        _ => return Err("unsupported auxiliary proof ID".into()),
    };
    let header = p.header.as_mut().ok_or("missing donor header")?;
    if header.len() < size {
        return Err("truncated donor header".into());
    }
    header.truncate(size);
    if id == 4 && p.auxpow2.as_ref().map(Vec::len) != Some(32) {
        return Err("scrypt auxpow2 must be 32 bytes".into());
    }
    if p.merkle_branch.len() > MAX_BRANCH
        || p.merkle_branch.iter().any(|h| h.len() > 4096)
        || p.transaction.as_ref().is_some_and(|t| t.len() > 1 << 20)
        || p.signature.as_ref().is_some_and(|s| s.len() > 4096)
    {
        return Err("auxiliary fields exceed supported scope".into());
    }
    p.chain_id = Some(id);
    p.signature_time = None;
    Ok(p)
}
pub fn canonical_template(bytes: &[u8]) -> Result<ProtoAuxTemplate, String> {
    if bytes.len() > MAX_AUX_BYTES {
        return Err("template bytes exceed supported scope".into());
    }
    schema::preflight(".block.ProtoAuxTemplate", bytes)?;
    let mut p = ProtoAuxTemplate::decode(bytes).map_err(|_| "malformed template protobuf")?;
    if p.chain_id.unwrap_or(0) == 4 && p.aux_pow2.as_ref().map(Vec::len) != Some(32) {
        return Err("scrypt template auxpow2 must be 32 bytes".into());
    }
    if p.merkle_branch.len() > MAX_BRANCH
        || p.merkle_branch.iter().any(|h| h.len() > 4096)
        || p.coinbase_out.as_ref().is_some_and(|v| v.len() > 1 << 20)
    {
        return Err("template fields exceed supported scope".into());
    }
    if p.prev_hash.as_ref().map(Vec::len) != Some(32) {
        p.prev_hash = Some(vec![0; 32]);
    }
    p.sigs = Some(p.sigs.take().unwrap_or_default());
    p.chain_id = Some(p.chain_id.unwrap_or(0));
    p.version = Some(p.version.unwrap_or(0));
    p.bits = Some(p.bits.unwrap_or(0));
    p.signature_time = Some(p.signature_time.unwrap_or(0));
    p.height = Some(p.height.unwrap_or(0));
    Ok(p)
}
pub fn template_signing_bytes(template: &ProtoAuxTemplate) -> Vec<u8> {
    let mut p = template.clone();
    p.sigs = None;
    if matches!(p.chain_id, Some(2 | 3)) {
        p.version = Some(p.version.unwrap_or(0) & 0xe0000000);
    }
    p.encode_to_vec()
}
pub fn template_hash(template: &ProtoAuxTemplate) -> Hash {
    Sha256::digest(template_signing_bytes(template)).into()
}
