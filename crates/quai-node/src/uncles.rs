//! go-quai's uncle classes (`core.UncleWorkShareClassification`): the uncle's
//! proof-of-work against the block target, then the workshare thresholds.
//! `RPCMarshalBlock` lists `Block` uncles under "uncles" and `Workshare`
//! uncles under "workshares", and leaves out the others.
use quai_consensus::{
    pow::{Algorithm, SharedCaches},
    share_rules::{self, Classification},
    Error,
};
use quai_primitives::{Hash, Header};
use quai_rpc::view::UncleKind;

/// go-quai's default `WorkShareP2PThresholdDiff`.
pub const P2P_THRESHOLD: u16 = 7;

fn hash_of(v: &[u8]) -> Hash {
    let mut h = [0u8; 32];
    let take = v.len().min(32);
    h[32 - take..].copy_from_slice(&v[v.len() - take..]);
    h
}

/// The class of an uncle with proof-of-work hash `pow` (an error: `Other`).
pub fn kind(h: &Header, pow: Result<Hash, Error>) -> UncleKind {
    let Ok(pow) = pow else {
        return UncleKind::Other;
    };
    match share_rules::classify(h, pow, P2P_THRESHOLD) {
        Ok(Classification::Block) => UncleKind::Block,
        Ok(Classification::Valid) => UncleKind::Workshare,
        _ => UncleKind::Other,
    }
}
/// The proof-of-work hash of a header (`ComputePowHash`, or the donor hash
/// of an auxiliary proof). A wrong mix hash is an error, as in Go.
pub fn pow_hash(caches: &SharedCaches, h: &Header) -> Result<Hash, Error> {
    let w = h.wire();
    let height = share_rules::number(w.prime_terminus_number.as_deref().unwrap_or_default())?;
    if let Some(bytes) = &w.aux_pow {
        let aux = quai_consensus::auxiliary::Auxiliary::decode(bytes)?;
        return match aux.id() {
            1 => {
                let (seal, nonce, donor, mix) = aux.kawpow_input()?;
                let (actual, hash) = caches
                    .get(Algorithm::Kawpow, donor)?
                    .compute(seal, nonce, donor)?;
                if actual != mix {
                    return Err(Error::Invalid("mix hash"));
                }
                Ok(hash)
            }
            2 | 3 => quai_consensus::donor::sha256d(aux.header()),
            _ => quai_consensus::donor::scrypt(aux.header()),
        };
    }
    let mix = hash_of(w.mix_hash.as_ref().map_or(&[][..], |m| m.value.as_slice()));
    let (actual, hash) =
        caches
            .get(Algorithm::Progpow, height)?
            .compute(h.seal_hash(), w.nonce.unwrap_or_default(), height)?;
    if actual != mix {
        return Err(Error::Invalid("mix hash"));
    }
    Ok(hash)
}
/// The class as the block store keeps it.
pub fn code(k: UncleKind) -> u8 {
    match k {
        UncleKind::Block => 0,
        UncleKind::Workshare => 1,
        UncleKind::Other => 2,
    }
}
pub fn from_code(c: u8) -> UncleKind {
    match c {
        0 => UncleKind::Block,
        1 => UncleKind::Workshare,
        _ => UncleKind::Other,
    }
}
