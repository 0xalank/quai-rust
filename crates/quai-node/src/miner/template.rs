//! Bitcoin/Stratum-compatible `getblocktemplate` marshaling matching
//! `go-quai/internal/quaiapi/quai_api.go` (`MarshalAuxPowTemplate`).
use super::auxpow::{
    difficulty_to_target, extract_coinb1_coinb2, hex_plain, target_to_hex, POW_KAWPOW, POW_SCRYPT,
    POW_SHA_BCH, POW_SHA_BTC,
};
use crypto_bigint::{Encoding as _, U256};
use quai_consensus::{
    auxiliary::{coinbase, Auxiliary},
    share_rules::{self, Triple},
    verifier::PINNED_SCHEDULE,
};
use quai_primitives::{transaction::Address, work::WorkObject};
use serde_json::{json, Value};

/// Parameters of `quai_getBlockTemplate` (`BlockTemplateRequest`).
#[derive(Clone, Debug, Default)]
pub struct BlockTemplateParams {
    pub pow_id: u8,
    pub extra_nonce1: String,
    pub extra_nonce2: String,
    pub extra_nonce2_len: usize,
    pub extra_data: String,
    pub coinbase: Option<Address>,
}

impl BlockTemplateParams {
    pub fn from_json(v: Option<&Value>) -> Result<Self, String> {
        let mut out = Self {
            pow_id: POW_KAWPOW,
            extra_nonce2_len: 8,
            ..Self::default()
        };
        let Some(obj) = v.and_then(Value::as_object) else {
            return Ok(out);
        };
        if let Some(rules) = obj.get("rules").and_then(Value::as_array) {
            for r in rules {
                let Some(s) = r.as_str() else {
                    return Err("rule must be a string".into());
                };
                match s.to_ascii_lowercase().as_str() {
                    "kawpow" => out.pow_id = POW_KAWPOW,
                    "sha" | "sha256d" => out.pow_id = POW_SHA_BCH,
                    "scrypt" => out.pow_id = POW_SCRYPT,
                    other => return Err(format!("unsupported rule: {other}")),
                }
            }
        }
        if let Some(en1) = obj.get("extranonce1").and_then(Value::as_str) {
            out.extra_nonce1 = en1.to_string();
        }
        if let Some(en2) = obj.get("extranonce2").and_then(Value::as_str) {
            out.extra_nonce2 = en2.to_string();
        }
        if let Some(len) = obj.get("extranonce2len").and_then(Value::as_u64) {
            if len == 4 {
                out.extra_nonce2_len = 4;
            } else if len == 8 || len == 0 {
                out.extra_nonce2_len = 8;
            } else {
                return Err("extranonce2 length must be 4 or 8".into());
            }
        }
        if let Some(ed) = obj.get("extradata").and_then(Value::as_str) {
            out.extra_data = ed.to_string();
        }
        if let Some(cb) = obj.get("coinbase").and_then(Value::as_str) {
            if !cb.is_empty() {
                let bytes = quai_primitives::decode_hex(cb)
                    .map_err(|e| format!("invalid coinbase address: {e}"))?;
                let addr: Address = bytes
                    .try_into()
                    .map_err(|_| "coinbase address must be 20 bytes".to_string())?;
                if addr != [0u8; 20] {
                    out.coinbase = Some(addr);
                }
            }
        }
        Ok(out)
    }
}

/// Format a pending `WorkObject` as go-quai's `MarshalAuxPowTemplate` JSON object.
pub fn marshal_aux_pow_template(
    wo: &WorkObject,
    pow_id: u8,
    extra_nonce1_hex: &str,
    extra_nonce2_hex: &str,
    extra_nonce2_len: usize,
    extra_data_str: &str,
) -> Result<Value, String> {
    if extra_nonce2_len != 4 && extra_nonce2_len != 8 {
        return Err("extranonce2 length must be 4 or 8".into());
    }
    let aux_schedule = quai_primitives::Schedule {
        kawpow_fork: 0,
        ..PINNED_SCHEDULE
    };
    let header = wo
        .auxiliary_work_header(aux_schedule)
        .map_err(|e| format!("pending header decode: {e}"))?;
    let hw = header.wire();
    let aux_bytes = hw
        .aux_pow
        .as_deref()
        .ok_or_else(|| "no AuxPow in pending header".to_string())?;
    let aux = Auxiliary::decode(aux_bytes).map_err(|e| format!("auxpow decode: {e}"))?;
    let donor_hdr = aux.header();
    if donor_hdr.len() < 80 {
        return Err("auxpow donor header truncated".into());
    }

    let tx_bytes = aux.transaction();
    if tx_bytes.is_empty() {
        return Err("empty coinbase tx bytes in AuxPow".into());
    }
    let decoded_cb = coinbase::Coinbase::decode(tx_bytes)
        .map_err(|e| format!("failed to extract height from coinbase scriptSig: {e}"))?;
    let block_height = decoded_cb.height;

    let (coinb1, mut coinb2) = extract_coinb1_coinb2(tx_bytes)?;

    let mut extra_nonce1 = [0u8; 4];
    if !extra_nonce1_hex.is_empty() {
        let stripped = extra_nonce1_hex
            .strip_prefix("0x")
            .unwrap_or(extra_nonce1_hex);
        let bytes = quai_primitives::decode_hex(&format!("0x{stripped}"))
            .map_err(|e| format!("invalid extranonce1: {e}"))?;
        if bytes.len() > 4 {
            return Err("extranonce1 length exceeds 4 bytes".into());
        }
        extra_nonce1[..bytes.len()].copy_from_slice(&bytes);
    }

    let mut extra_nonce2 = vec![0u8; extra_nonce2_len];
    if !extra_nonce2_hex.is_empty() {
        let stripped = extra_nonce2_hex
            .strip_prefix("0x")
            .unwrap_or(extra_nonce2_hex);
        let bytes = quai_primitives::decode_hex(&format!("0x{stripped}"))
            .map_err(|e| format!("invalid extranonce2: {e}"))?;
        if bytes.len() > 8 {
            return Err("extranonce2 length exceeds 8 bytes".into());
        }
        let n = bytes.len().min(extra_nonce2.len());
        extra_nonce2[..n].copy_from_slice(&bytes[..n]);
    }

    if !extra_data_str.is_empty() {
        let extra_bytes = extra_data_str.as_bytes();
        if extra_bytes.len() > 30 {
            return Err("input extraData length exceeds 30 bytes".into());
        }
        if coinb2.len() < 30 {
            return Err("coinb2 too short for extraData".into());
        }
        coinb2[..extra_bytes.len()].copy_from_slice(extra_bytes);
    }
    if extra_nonce2_len == 4 {
        let mut padded = Vec::with_capacity(4 + coinb2.len());
        padded.extend_from_slice(&[0u8; 4]);
        padded.extend_from_slice(&coinb2);
        coinb2 = padded;
    }

    let mut updated_tx = Vec::with_capacity(coinb1.len() + 4 + extra_nonce2.len() + coinb2.len());
    updated_tx.extend_from_slice(&coinb1);
    updated_tx.extend_from_slice(&extra_nonce1);
    updated_tx.extend_from_slice(&extra_nonce2);
    updated_tx.extend_from_slice(&coinb2);

    let mut prev_block = [0u8; 32];
    prev_block.copy_from_slice(&donor_hdr[4..36]);
    prev_block.reverse();
    let prev_block_hex = hex_plain(&prev_block);

    let template = aux
        .template()
        .map_err(|e| format!("auxpow template: {e}"))?;
    let merkle_branch: Vec<String> = template
        .merkle_branch
        .iter()
        .map(|h| {
            let mut rev = h.clone();
            rev.reverse();
            hex_plain(&rev)
        })
        .collect();

    let mut merkle_root = coinbase::merkle_root(&updated_tx, &template.merkle_branch)
        .map_err(|e| format!("merkle root: {e}"))?;
    merkle_root.reverse();

    let version = u32::from_le_bytes([donor_hdr[0], donor_hdr[1], donor_hdr[2], donor_hdr[3]]);
    let ntime = u32::from_le_bytes([donor_hdr[68], donor_hdr[69], donor_hdr[70], donor_hdr[71]]);
    let nbits = u32::from_le_bytes([donor_hdr[72], donor_hdr[73], donor_hdr[74], donor_hdr[75]]);
    let bits_hex = format!("{nbits:08x}");

    let difficulty = share_rules::uint(hw.difficulty.as_deref().unwrap_or_default())
        .map_err(|e| e.to_string())?;
    let target_diff = match pow_id {
        POW_SHA_BCH | POW_SHA_BTC => Triple::from_header(&header, true)
            .map_err(|e| e.to_string())?
            .difficulty
            .max(U256::ONE),
        POW_SCRYPT => Triple::from_header(&header, false)
            .map_err(|e| e.to_string())?
            .difficulty
            .max(U256::ONE),
        POW_KAWPOW => {
            let sha_c = Triple::from_header(&header, true)
                .map_err(|e| e.to_string())?
                .count;
            let scrypt_c = Triple::from_header(&header, false)
                .map_err(|e| e.to_string())?
                .count;
            let sha_t = share_rules::uint(hw.sha_share_target.as_deref().unwrap_or_default())
                .unwrap_or(U256::ZERO);
            let scrypt_t = share_rules::uint(hw.scrypt_share_target.as_deref().unwrap_or_default())
                .unwrap_or(U256::ZERO);
            let kaw_d = share_rules::uint(hw.kawpow_difficulty.as_deref().unwrap_or_default())
                .unwrap_or(U256::ZERO);
            share_rules::kawpow_share_difficulty(
                difficulty.max(U256::ONE),
                sha_c,
                scrypt_c,
                sha_t,
                scrypt_t,
                kaw_d,
            )
            .unwrap_or(difficulty.max(U256::ONE))
            .max(U256::ONE)
        }
        _ => difficulty.max(U256::ONE),
    };
    let target_hex = target_to_hex(difficulty_to_target(target_diff));

    // Quai root: first 6 bytes of seal hash (with time zeroed for stability as in Go).
    let mut zero_time_header = header.wire().clone();
    zero_time_header.time = Some(0);
    let seal_prefix = {
        use prost::Message;
        quai_primitives::Header::decode_auxiliary(&zero_time_header.encode_to_vec(), aux_schedule)
            .map(|h| hex_plain(&h.seal_hash()[..6]))
            .unwrap_or_else(|_| hex_plain(&header.seal_hash()[..6]))
    };

    let quai_height = share_rules::number(hw.number.as_deref().unwrap_or_default()).unwrap_or(0);
    let quai_diff_dec = u256_to_decimal(difficulty);

    Ok(json!({
        "version": version,
        "previousblockhash": prev_block_hex,
        "target": target_hex,
        "mintime": ntime,
        "noncerange": "00000000ffffffff",
        "sigoplimit": 80000,
        "sizelimit": 1000000,
        "curtime": ntime,
        "bits": bits_hex,
        "height": u64::from(block_height),
        "coinb1": hex_plain(&coinb1),
        "extranonce1Length": 4,
        "extranonce2Length": extra_nonce2_len,
        "coinbaseAuxExtraBytesLength": 30,
        "coinb2": hex_plain(&coinb2),
        "merklebranch": merkle_branch,
        "merkleroot": hex_plain(&merkle_root),
        "quairoot": seal_prefix,
        "quaiheight": quai_height,
        "quaidifficulty": quai_diff_dec,
    }))
}

fn u256_to_decimal(v: U256) -> String {
    if v == U256::ZERO {
        return "0".into();
    }
    let mut n = v;
    let ten = U256::from_u64(10);
    let mut digits = Vec::new();
    while n != U256::ZERO {
        let q = n.wrapping_div(&ten);
        let r = n.wrapping_sub(&q.wrapping_mul(&ten));
        let b = r.to_be_bytes()[31];
        digits.push((b'0' + b) as char);
        n = q;
    }
    digits.reverse();
    digits.into_iter().collect()
}
