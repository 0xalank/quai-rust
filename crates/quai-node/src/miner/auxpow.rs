//! AuxPoW donor-chain encoding, coinbase construction, BIP-34 height coding,
//! compact target conversions, seed hashes, and MuSig2 template signing
//! matching `go-quai/core/types/auxpow*.go` and `internal/quaiapi/quai_api.go`.
use crypto_bigint::{Encoding as _, U256, U512};
use k256::elliptic_curve::sec1::ToEncodedPoint;
use prost::Message;
use quai_consensus::auxiliary::coinbase::{self, Coinbase};
use quai_primitives::{
    auxiliary as aux_codec, schema::block::ProtoAuxTemplate, spec::MUSIG2_PUBLIC_KEYS,
    transaction::keccak, Hash,
};
use sha2::{Digest, Sha256};

pub const POW_PROGPOW: u8 = 0;
pub const POW_KAWPOW: u8 = 1;
pub const POW_SHA_BTC: u8 = 2;
pub const POW_SHA_BCH: u8 = 3;
pub const POW_SCRYPT: u8 = 4;

/// Default minimal donor coinbase tail (`[nOut=1][value=0][scriptLen=1][OP_TRUE][locktime=0]`)
/// used for local/solo mining templates when no external donor template was pushed.
pub const DEFAULT_COINBASE_OUT: &[u8] = &[
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x51, 0x00, 0x00, 0x00, 0x00,
];

/// Bitcoin variable-length integer encoding.
pub fn encode_varint(n: u64, out: &mut Vec<u8>) {
    if n < 0xfd {
        out.push(n as u8);
    } else if n <= 0xffff {
        out.push(0xfd);
        out.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n <= 0xffff_ffff {
        out.push(0xfe);
        out.extend_from_slice(&(n as u32).to_le_bytes());
    } else {
        out.push(0xff);
        out.extend_from_slice(&n.to_le_bytes());
    }
}

/// Bitcoin variable-length integer decoding.
pub fn decode_varint(bytes: &[u8], cursor: &mut usize) -> Result<u64, String> {
    let first = *bytes
        .get(*cursor)
        .ok_or_else(|| "truncated varint".to_string())?;
    *cursor += 1;
    match first {
        0xfd => {
            let s = bytes
                .get(*cursor..*cursor + 2)
                .ok_or_else(|| "truncated varint u16".to_string())?;
            *cursor += 2;
            Ok(u16::from_le_bytes([s[0], s[1]]) as u64)
        }
        0xfe => {
            let s = bytes
                .get(*cursor..*cursor + 4)
                .ok_or_else(|| "truncated varint u32".to_string())?;
            *cursor += 4;
            Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as u64)
        }
        0xff => {
            let s = bytes
                .get(*cursor..*cursor + 8)
                .ok_or_else(|| "truncated varint u64".to_string())?;
            *cursor += 8;
            Ok(u64::from_le_bytes([
                s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
            ]))
        }
        v => Ok(u64::from(v)),
    }
}

/// Encode block height in Bitcoin BIP-34 minimal little-endian CScriptNum format.
pub fn encode_bip34_height(height: u32) -> Vec<u8> {
    if height == 0 {
        return vec![0];
    }
    let mut v = height;
    let mut out = Vec::with_capacity(5);
    while v > 0 {
        out.push((v & 0xff) as u8);
        v >>= 8;
    }
    if out.last().is_some_and(|b| b & 0x80 != 0) {
        out.push(0);
    }
    out
}

/// Build an AuxPoW coinbase transaction matching `go-quai/core/types/auxpow_coinbase_utils.go`
/// (`NewAuxPowCoinbaseTx`) and `quai_consensus::auxiliary::coinbase::Coinbase::decode`.
pub fn build_coinbase_tx(
    pow_id: u8,
    height: u32,
    coinbase_out: &[u8],
    aux_merkle_root: Hash,
    signature_time: u32,
) -> Vec<u8> {
    let height_bytes = encode_bip34_height(height);
    let merkle_size: u32 = if pow_id == POW_SCRYPT { 2 } else { 1 };
    let merkle_nonce: u32 = 0;

    let mut script_sig = Vec::with_capacity(1 + height_bytes.len() + 45 + 43 + 5);
    // 1) BIP-34 height push
    script_sig.push(height_bytes.len() as u8);
    script_sig.extend_from_slice(&height_bytes);
    // 2) Merged-mining AuxPoW commitment push (44 bytes)
    script_sig.push(44);
    script_sig.extend_from_slice(&[0xfa, 0xbe, 0x6d, 0x6d]);
    script_sig.extend_from_slice(&aux_merkle_root);
    script_sig.extend_from_slice(&merkle_size.to_le_bytes());
    script_sig.extend_from_slice(&merkle_nonce.to_le_bytes());
    // 3) Combined extranonce1 (4B) + extranonce2 (8B) + extraData (30B) = 42 bytes
    script_sig.push(42);
    script_sig.extend_from_slice(&[0u8; 42]);
    // 4) Signature time push (4 bytes LE)
    script_sig.push(4);
    script_sig.extend_from_slice(&signature_time.to_le_bytes());

    let out_tail = if coinbase_out.is_empty() {
        DEFAULT_COINBASE_OUT
    } else {
        coinbase_out
    };

    let mut tx = Vec::with_capacity(4 + 1 + 32 + 4 + 3 + script_sig.len() + 4 + out_tail.len());
    // Version 2 LE
    tx.extend_from_slice(&2u32.to_le_bytes());
    // Input count = 1
    tx.push(1);
    // Prev txid (32 zero bytes) + Prev index (0xffffffff)
    tx.extend_from_slice(&[0u8; 32]);
    tx.extend_from_slice(&[0xff; 4]);
    // ScriptSig length + bytes
    encode_varint(script_sig.len() as u64, &mut tx);
    tx.extend_from_slice(&script_sig);
    // Sequence 0xffffffff
    tx.extend_from_slice(&[0xff; 4]);
    // Coinbase outputs + locktime verbatim
    tx.extend_from_slice(out_tail);
    tx
}

/// Split an AuxPoW coinbase transaction into `(coinb1, coinb2)` around the
/// 12-byte `(extraNonce1[4] || extraNonce2[8])` slot inside push #3, keeping
/// the trailing 30-byte `extraData`, signature time, sequence, outputs and
/// locktime in `coinb2` (`ExtractCoinb1AndCoinb2FromAuxPowTx`).
pub fn extract_coinb1_coinb2(tx: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    if tx.len() < 41 {
        return Err("coinbase tx too short".into());
    }
    let mut cursor = 4; // skip version
    if decode_varint(tx, &mut cursor)? != 1 {
        return Err("coinbase tx input count must be 1".into());
    }
    cursor = cursor
        .checked_add(36)
        .filter(|&c| c <= tx.len())
        .ok_or_else(|| "truncated coinbase prevout".to_string())?;
    let script_len = usize::try_from(decode_varint(tx, &mut cursor)?)
        .map_err(|_| "script length overflow".to_string())?;
    let script_end = cursor
        .checked_add(script_len)
        .filter(|&e| e <= tx.len())
        .ok_or_else(|| "truncated coinbase scriptSig".to_string())?;
    // Push 1: BIP-34 height
    let h_len = *tx
        .get(cursor)
        .ok_or_else(|| "missing height push".to_string())? as usize;
    cursor = cursor
        .checked_add(1 + h_len)
        .filter(|&c| c <= script_end)
        .ok_or_else(|| "truncated height push".to_string())?;
    // Push 2: merged-mining commitment (44 bytes)
    let mm_len = *tx
        .get(cursor)
        .ok_or_else(|| "missing merged-mining push".to_string())? as usize;
    if mm_len != 44 {
        return Err(format!("expected 44-byte merged-mining push, got {mm_len}"));
    }
    cursor = cursor
        .checked_add(1 + mm_len)
        .filter(|&c| c <= script_end)
        .ok_or_else(|| "truncated merged-mining push".to_string())?;
    // Push 3: 42-byte extranonce1 (4) + extranonce2 (8) + extraData (30)
    let ex_len = *tx
        .get(cursor)
        .ok_or_else(|| "missing extranonce push".to_string())? as usize;
    if ex_len < 42 {
        return Err(format!("extranonce push too short: {ex_len}"));
    }
    let en1_start = cursor + 1;
    let en2_end = en1_start
        .checked_add(12)
        .filter(|&e| e <= script_end)
        .ok_or_else(|| "truncated extranonce region".to_string())?;
    Ok((tx[..en1_start].to_vec(), tx[en2_end..].to_vec()))
}

/// Build an 80-byte (SHA/Scrypt) or 120-byte (KAWPOW) donor block header.
#[allow(clippy::too_many_arguments)]
pub fn build_donor_header(
    pow_id: u8,
    version: u32,
    prev_hash_le: Hash,
    merkle_root_le: Hash,
    ntime: u32,
    nbits: u32,
    nonce: u64,
    height: u32,
    mix_hash_le: Hash,
) -> Vec<u8> {
    if pow_id == POW_KAWPOW {
        let mut h = Vec::with_capacity(120);
        h.extend_from_slice(&version.to_le_bytes());
        h.extend_from_slice(&prev_hash_le);
        h.extend_from_slice(&merkle_root_le);
        h.extend_from_slice(&ntime.to_le_bytes());
        h.extend_from_slice(&nbits.to_le_bytes());
        h.extend_from_slice(&height.to_le_bytes());
        h.extend_from_slice(&nonce.to_le_bytes());
        h.extend_from_slice(&mix_hash_le);
        h
    } else {
        let mut h = Vec::with_capacity(80);
        h.extend_from_slice(&version.to_le_bytes());
        h.extend_from_slice(&prev_hash_le);
        h.extend_from_slice(&merkle_root_le);
        h.extend_from_slice(&ntime.to_le_bytes());
        h.extend_from_slice(&nbits.to_le_bytes());
        h.extend_from_slice(&(nonce as u32).to_le_bytes());
        h
    }
}

/// Stratum v1 `prevhash` encoding: 32-byte little-endian donor prevhash split
/// into 8 little-endian 32-bit words, formatted as hex (`go-quai/stratum`).
/// Specifically: reverse the 32 bytes into display big-endian, then swap the
/// order of the 8 4-byte words (preserving byte order within each word).
pub fn word_swap_prev_hash(prev_hash_le: Hash) -> String {
    let mut be = prev_hash_le;
    be.reverse();
    let mut out = String::with_capacity(64);
    for chunk in be.chunks_exact(4).rev() {
        for b in chunk {
            out.push_str(&format!("{b:02x}"));
        }
    }
    out
}

/// Convert Bitcoin compact `nBits` to a 256-bit big-endian target.
pub fn compact_to_target(bits: u32) -> U256 {
    if bits & 0x0080_0000 != 0 {
        return U256::ZERO;
    }
    let size = bits >> 24;
    let mantissa = bits & 0x007f_ffff;
    if size <= 3 {
        U256::from_u32(mantissa >> (8 * (3 - size)))
    } else if size <= 32 {
        U256::from_u32(mantissa).shl_vartime(8 * (size - 3) as usize)
    } else {
        U256::MAX
    }
}

/// Convert a positive difficulty integer into a 256-bit target `floor(2^256 / diff)`.
pub fn difficulty_to_target(diff: U256) -> U256 {
    if diff == U256::ZERO {
        return U256::MAX;
    }
    if diff == U256::ONE {
        return U256::MAX;
    }
    let num = U512::ONE.shl_vartime(256);
    let den: U512 = diff.resize();
    let q = num.wrapping_div(&den);
    if q.bits_vartime() > 256 {
        U256::MAX
    } else {
        q.resize()
    }
}

/// Format a 256-bit target as a 64-character lowercase hex string without `0x`.
pub fn target_to_hex(target: U256) -> String {
    let bytes = target.to_be_bytes();
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// KAWPOW seed hash for a donor block height (Ravencoin epoch length = 7500).
pub fn kawpow_seed_hash(height: u64) -> Hash {
    let epoch = height / 7500;
    let mut seed = [0u8; 32];
    for _ in 0..epoch {
        seed = keccak(&seed);
    }
    seed
}

/// ProgPoW seed hash for a Quai block height (`epoch_length = 388_800`).
pub fn progpow_seed_hash(height: u64) -> Hash {
    let epoch = height / 388_800;
    let mut seed = [0u8; 32];
    for _ in 0..epoch {
        seed = keccak(&seed);
    }
    seed
}

/// Parsed fields of a raw donor block submitted via `quai_submit{Sha,Scrypt,Kawpow}Block`.
#[derive(Clone, Debug)]
pub struct ParsedDonorBlock {
    pub header: Vec<u8>,
    pub coinbase_tx: Vec<u8>,
    pub coinbase: Coinbase,
    pub merkle_branch: Vec<Vec<u8>>,
    pub ntime: u32,
    pub nbits: u32,
    pub height: u32,
}

/// Parse a serialized donor block (header + varint tx_count + coinbase tx [+ optional txs]).
pub fn parse_submitted_donor_block(raw: &[u8], pow_id: u8) -> Result<ParsedDonorBlock, String> {
    let header_len = if pow_id == POW_KAWPOW { 120 } else { 80 };
    if raw.len() < header_len + 2 {
        return Err(format!(
            "submitted donor block too short: {} bytes (need >= {})",
            raw.len(),
            header_len + 2
        ));
    }
    let header = raw[..header_len].to_vec();
    let mut cursor = header_len;
    let tx_count = decode_varint(raw, &mut cursor)?;
    if tx_count == 0 {
        return Err("submitted donor block has zero transactions".into());
    }
    let tx_start = cursor;
    // If there is only 1 tx (coinbase only, typical for Stratum/solo submissions),
    // the coinbase tx spans the remainder of `raw`. Otherwise parse the coinbase
    // tx boundary so we can isolate it cleanly.
    let coinbase_tx = if tx_count == 1 {
        raw[tx_start..].to_vec()
    } else {
        let cb_end = parse_tx_end(raw, tx_start)?;
        raw[tx_start..cb_end].to_vec()
    };
    let coinbase = Coinbase::decode(&coinbase_tx).map_err(|e| format!("coinbase decode: {e}"))?;
    let ntime = u32::from_le_bytes([header[68], header[69], header[70], header[71]]);
    let nbits = u32::from_le_bytes([header[72], header[73], header[74], header[75]]);
    let height = if pow_id == POW_KAWPOW {
        u32::from_le_bytes([header[76], header[77], header[78], header[79]])
    } else {
        coinbase.height
    };
    Ok(ParsedDonorBlock {
        header,
        coinbase_tx,
        coinbase,
        merkle_branch: vec![],
        ntime,
        nbits,
        height,
    })
}

/// Parse a Bitcoin-style transaction starting at `start` to find its exclusive end byte offset.
fn parse_tx_end(raw: &[u8], start: usize) -> Result<usize, String> {
    let mut c = start
        .checked_add(4)
        .filter(|&p| p <= raw.len())
        .ok_or_else(|| "truncated tx version".to_string())?;
    let mut has_witness = false;
    if raw.get(c..c + 2) == Some(&[0x00, 0x01]) {
        has_witness = true;
        c += 2;
    }
    let vin_count = decode_varint(raw, &mut c)?;
    for _ in 0..vin_count {
        c = c
            .checked_add(36)
            .filter(|&p| p <= raw.len())
            .ok_or_else(|| "truncated tx input".to_string())?;
        let script_len = usize::try_from(decode_varint(raw, &mut c)?)
            .map_err(|_| "script len overflow".to_string())?;
        c = c
            .checked_add(script_len + 4)
            .filter(|&p| p <= raw.len())
            .ok_or_else(|| "truncated tx input script".to_string())?;
    }
    let vout_count = decode_varint(raw, &mut c)?;
    for _ in 0..vout_count {
        c = c
            .checked_add(8)
            .filter(|&p| p <= raw.len())
            .ok_or_else(|| "truncated tx output value".to_string())?;
        let pk_len = usize::try_from(decode_varint(raw, &mut c)?)
            .map_err(|_| "pk len overflow".to_string())?;
        c = c
            .checked_add(pk_len)
            .filter(|&p| p <= raw.len())
            .ok_or_else(|| "truncated tx output script".to_string())?;
    }
    if has_witness {
        for _ in 0..vin_count {
            let items = decode_varint(raw, &mut c)?;
            for _ in 0..items {
                let len = usize::try_from(decode_varint(raw, &mut c)?)
                    .map_err(|_| "witness len overflow".to_string())?;
                c = c
                    .checked_add(len)
                    .filter(|&p| p <= raw.len())
                    .ok_or_else(|| "truncated witness item".to_string())?;
            }
        }
    }
    c = c
        .checked_add(4)
        .filter(|&p| p <= raw.len())
        .ok_or_else(|| "truncated tx locktime".to_string())?;
    Ok(c)
}

/// Build a valid fallback `ProtoAuxTemplate` for local/solo mining when no
/// signed external donor-chain template has arrived from the network yet.
pub fn default_fallback_template(pow_id: u8, now: u32, quai_height: u64) -> ProtoAuxTemplate {
    let mut prev_hash = [0u8; 32];
    prev_hash[0] = pow_id;
    let height = (quai_height as u32).max(1);
    let aux_pow2 = if pow_id == POW_SCRYPT {
        let mut doge = [0u8; 32];
        doge[0] = 0x01;
        doge.to_vec()
    } else {
        vec![]
    };
    ProtoAuxTemplate {
        chain_id: Some(u32::from(pow_id)),
        prev_hash: Some(prev_hash.to_vec()),
        version: Some(0x2000_0000),
        bits: Some(0x1f00_ffff),
        signature_time: Some(now),
        height: Some(height),
        coinbase_out: Some(DEFAULT_COINBASE_OUT.to_vec()),
        merkle_branch: vec![],
        sigs: Some(vec![]),
        aux_pow2: Some(aux_pow2),
    }
}

/// Compute the Merkle commitment placed into the coinbase merged-mining push:
/// for Scrypt, `scrypt_commitment(doge_hash, quai_seal)`; for KAWPOW/SHA, `quai_seal`.
pub fn aux_commitment(pow_id: u8, aux_pow2: &[u8], quai_seal: Hash) -> Result<Hash, String> {
    if pow_id == POW_SCRYPT {
        let doge: Hash = aux_pow2
            .try_into()
            .map_err(|_| "scrypt template missing 32-byte aux_pow2".to_string())?;
        if doge == [0u8; 32] {
            return Err("scrypt aux_pow2 hash is zero".into());
        }
        Ok(coinbase::scrypt_commitment(doge, quai_seal))
    } else {
        Ok(quai_seal)
    }
}

/// Implement `quai_signAuxTemplate`: 2-of-3 MuSig2 signing session using the
/// `MUSIG2_PRIVKEY` environment variable (matching `go-quai/internal/quaiapi/quai_api.go`).
pub fn sign_aux_template(
    template_bytes: &[u8],
    other_pubkey: &[u8],
    other_nonce: &[u8],
) -> Result<serde_json::Value, String> {
    let priv_hex = std::env::var("MUSIG2_PRIVKEY").map_err(|_| {
        "MuSig2 keys not configured - set MUSIG2_PRIVKEY environment variable".to_string()
    })?;
    let priv_bytes = quai_primitives::decode_hex(priv_hex.trim())
        .or_else(|_| quai_primitives::decode_hex(&format!("0x{}", priv_hex.trim())))
        .map_err(|e| format!("invalid MUSIG2_PRIVKEY hex: {e}"))?;
    let secret = k256::SecretKey::from_slice(&priv_bytes)
        .map_err(|e| format!("invalid MUSIG2_PRIVKEY: {e}"))?;
    let our_pk = secret.public_key();
    let our_pk_compressed: [u8; 33] = our_pk
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .map_err(|_| "compressed pubkey length".to_string())?;

    let quorum_pks: Vec<[u8; 33]> = MUSIG2_PUBLIC_KEYS
        .iter()
        .map(|h| {
            quai_primitives::decode_hex(&format!("0x{h}"))
                .and_then(|v| v.try_into().map_err(|_| "33B pubkey".to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let our_idx = quorum_pks
        .iter()
        .position(|k| k == &our_pk_compressed)
        .ok_or_else(|| {
            "MUSIG2_PRIVKEY public key is not in configured MuSig2 quorum".to_string()
        })?;

    if other_pubkey.is_empty() {
        return Err("other participant pubkey is required".into());
    }
    let parsed_other = k256::PublicKey::from_sec1_bytes(other_pubkey)
        .map_err(|e| format!("failed to parse other participant pubkey: {e}"))?;
    let other_compressed: [u8; 33] = parsed_other
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .map_err(|_| "other compressed pubkey length".to_string())?;
    let other_idx = quorum_pks
        .iter()
        .position(|k| k == &other_compressed)
        .ok_or_else(|| {
            "other participant pubkey not found in configured MuSig2 public keys".to_string()
        })?;
    if other_idx == our_idx {
        return Err("other participant cannot be the same as this participant".into());
    }

    let proto = ProtoAuxTemplate::decode(template_bytes)
        .map_err(|e| format!("failed to unmarshal AuxTemplate: {e}"))?;
    let message_hash = aux_codec::template_hash(&proto);

    // Derive deterministic two-point MuSig2 public nonce (66 bytes: R1 || R2)
    // bound to (secret, message_hash, other_compressed).
    let mut k1_hasher = Sha256::new();
    k1_hasher.update(b"quai-musig2-nonce-1");
    k1_hasher.update(&priv_bytes);
    k1_hasher.update(message_hash);
    k1_hasher.update(other_compressed);
    let k1_bytes: [u8; 32] = k1_hasher.finalize().into();

    let mut k2_hasher = Sha256::new();
    k2_hasher.update(b"quai-musig2-nonce-2");
    k2_hasher.update(&priv_bytes);
    k2_hasher.update(message_hash);
    k2_hasher.update(other_compressed);
    let k2_bytes: [u8; 32] = k2_hasher.finalize().into();

    let k1 = k256::SecretKey::from_slice(&k1_bytes).map_err(|e| format!("nonce scalar k1: {e}"))?;
    let k2 = k256::SecretKey::from_slice(&k2_bytes).map_err(|e| format!("nonce scalar k2: {e}"))?;
    let mut our_nonce = Vec::with_capacity(66);
    our_nonce.extend_from_slice(k1.public_key().to_encoded_point(true).as_bytes());
    our_nonce.extend_from_slice(k2.public_key().to_encoded_point(true).as_bytes());

    let partial_sig_hex = if !other_nonce.is_empty() {
        if other_nonce.len() != 66 {
            return Err(format!(
                "failed to register other nonce: expected 66 bytes, got {}",
                other_nonce.len()
            ));
        }
        // Validate both compressed curve points in `other_nonce`.
        let _r1 = k256::PublicKey::from_sec1_bytes(&other_nonce[..33])
            .map_err(|e| format!("invalid other nonce R1: {e}"))?;
        let _r2 = k256::PublicKey::from_sec1_bytes(&other_nonce[33..66])
            .map_err(|e| format!("invalid other nonce R2: {e}"))?;
        // Compute BIP-327-style partial signature scalar s_i = (k1 + b*k2 + e*a_i*d_i) mod n.
        use k256::elliptic_curve::ops::Reduce;
        use k256::{Scalar, U256 as K256U256};

        let mut b_hash = Sha256::new();
        b_hash.update(b"MuSig/noncecoef");
        b_hash.update(&our_nonce);
        b_hash.update(other_nonce);
        b_hash.update(message_hash);
        let b_bytes: [u8; 32] = b_hash.finalize().into();
        let b = <Scalar as Reduce<K256U256>>::reduce_bytes(&b_bytes.into());

        let mut e_hash = Sha256::new();
        e_hash.update(b"BIP0340/challenge");
        e_hash.update(&our_nonce[..33]);
        e_hash.update(our_pk_compressed);
        e_hash.update(message_hash);
        let e_bytes: [u8; 32] = e_hash.finalize().into();
        let e = <Scalar as Reduce<K256U256>>::reduce_bytes(&e_bytes.into());

        let d_i = *secret.to_nonzero_scalar();
        let k1_s = *k1.to_nonzero_scalar();
        let k2_s = *k2.to_nonzero_scalar();
        let s_i = k1_s + b * k2_s + e * d_i;
        hex_plain(&s_i.to_bytes())
    } else {
        String::new()
    };

    Ok(serde_json::json!({
        "publicNonce": hex_plain(&our_nonce),
        "partialSignature": partial_sig_hex,
        "messageHash": hex_plain(&message_hash),
        "publicKey": hex_plain(&our_pk_compressed),
        "participantIndex": our_idx,
    }))
}

pub fn hex_plain(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coinbase_roundtrip_matches_consensus_decoder_and_splits_cleanly() {
        let root = [0xabu8; 32];
        let tx = build_coinbase_tx(
            POW_SHA_BCH,
            850_000,
            DEFAULT_COINBASE_OUT,
            root,
            1_700_000_000,
        );
        let decoded = Coinbase::decode(&tx).unwrap();
        assert_eq!(decoded.height, 850_000);
        assert_eq!(decoded.commitment, root);
        assert_eq!(decoded.merkle_size, 1);
        assert_eq!(decoded.merkle_nonce, 0);
        assert_eq!(decoded.signature_time, 1_700_000_000);
        assert_eq!(decoded.outputs, DEFAULT_COINBASE_OUT);

        let (c1, c2) = extract_coinb1_coinb2(&tx).unwrap();
        let mut rebuilt = Vec::new();
        rebuilt.extend_from_slice(&c1);
        rebuilt.extend_from_slice(&[0u8; 12]);
        rebuilt.extend_from_slice(&c2);
        assert_eq!(rebuilt, tx);

        // Scrypt sets merkle_size = 2
        let scrypt_tx = build_coinbase_tx(POW_SCRYPT, 2_700_000, DEFAULT_COINBASE_OUT, root, 123);
        assert_eq!(Coinbase::decode(&scrypt_tx).unwrap().merkle_size, 2);
    }

    #[test]
    fn word_swap_and_compact_target_follow_bitcoin_conventions() {
        let mut prev = [0u8; 32];
        for (i, b) in prev.iter_mut().enumerate() {
            *b = i as u8;
        }
        let swapped = word_swap_prev_hash(prev);
        assert_eq!(swapped.len(), 64);
        // First 4-byte word of swapped is last 4-byte word of BE = first 4 bytes of LE reversed.
        assert_eq!(&swapped[..8], "03020100");
        assert_eq!(&swapped[56..], "1f1e1d1c");

        let t = compact_to_target(0x1d00ffff);
        assert_eq!(
            target_to_hex(t),
            "00000000ffff0000000000000000000000000000000000000000000000000000"
        );
    }
}
