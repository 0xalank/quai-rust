//! Mining, AuxPoW template, workshare, and donor-chain JSON-RPC methods
//! (`go-quai/quai/api.go`, `internal/quaiapi/quai_api.go`, `internal/quaiapi/api.go`).
use super::args::{self, Args};
use super::{head_block, json, RpcError, R};
use crate::view::ChainView;
use crypto_bigint::{Encoding as _, U256};
use quai_consensus::{
    auxiliary::Auxiliary,
    donor,
    share_rules::{self, Triple},
};
use quai_primitives::{encode_hex, spec::FORKS, transaction::Kind};
use serde_json::{Map, Value};

pub const METHODS: &[&str] = &[
    "eth_mining",
    "eth_primaryCoinbase",
    "eth_coinbase",
    "eth_hashrate",
    "quai_mining",
    "quai_hashrate",
    "miner_start",
    "miner_setExtra",
    "miner_setGasLimit",
    "miner_setGasPrice",
    "miner_setLockupByte",
    "miner_setMinerPreference",
    "miner_setPrimaryCoinbase",
    "miner_setRecommitInterval",
    "quai_calcOrder",
    "quai_getBlockTemplate",
    "quai_getCoinbaseTxForWorkShareHash",
    "quai_getDonorChainInfoForWorkshare",
    "quai_getMiningInfo",
    "quai_getPendingHeader",
    "quai_getPendingWorkShares",
    "quai_getQuaiHeaderForDonorHash",
    "quai_getSubsidyChainHeight",
    "quai_getWorkShareP2PThreshold",
    "quai_receiveMinedHeader",
    "quai_receiveRawWorkShare",
    "quai_receiveWorkShare",
    "quai_setWorkShareP2PThreshold",
    "quai_signAuxTemplate",
    "quai_submitAuxTemplate",
    "quai_submitKawpowBlock",
    "quai_submitScryptBlock",
    "quai_submitShaBlock",
    "workshare_getWorkShareThreshold",
    "workshare_receiveSubWorkshare",
];

pub fn dispatch(view: &dyn ChainView, method: &str, params: &Value) -> Option<R<Value>> {
    Some(match method {
        "eth_mining" | "quai_mining" => Args::new(params, 0).map(|_| Value::Bool(view.is_mining())),
        "eth_primaryCoinbase" | "eth_coinbase" => (|| {
            Args::new(params, 0)?;
            let addr = view.primary_coinbase()?;
            Ok(Value::String(json::checksum(&addr)))
        })(),
        "eth_hashrate" | "quai_hashrate" => {
            Args::new(params, 0).map(|_| json::u64_hex(view.hashrate()))
        }
        "miner_start" => (|| {
            let _ = Args::new(params, 1)?;
            Ok(Value::Bool(view.miner_start()?))
        })(),
        "miner_setExtra" => (|| {
            let a = Args::new(params, 1)?;
            let extra = a.get(0, args::string)?;
            Ok(Value::Bool(view.miner_set_extra(extra.as_bytes())?))
        })(),
        "miner_setGasLimit" => (|| {
            let a = Args::new(params, 1)?;
            let limit = a.get(0, args::uint64)?;
            Ok(Value::Bool(view.miner_set_gas_limit(limit)?))
        })(),
        "miner_setGasPrice" => (|| {
            let a = Args::new(params, 1)?;
            let price = a.get(0, parse_u256_arg)?;
            Ok(Value::Bool(view.miner_set_gas_price(price)?))
        })(),
        "miner_setLockupByte" => (|| {
            let a = Args::new(params, 1)?;
            let lock = a.get(0, args::uint64)?;
            if lock > 3 {
                return Err(RpcError::go(format!("lockup byte {lock} out of range")));
            }
            Ok(Value::Bool(view.miner_set_lockup_byte(lock as u8)?))
        })(),
        "miner_setMinerPreference" => (|| {
            let a = Args::new(params, 1)?;
            let pref_str = a.get(0, args::string)?;
            let pref: f64 = pref_str
                .parse()
                .map_err(|_| RpcError::go(format!("invalid miner preference: {pref_str}")))?;
            if !(0.0..=1.0).contains(&pref) || pref.is_nan() {
                return Err(RpcError::go("miner preference must be between 0 and 1"));
            }
            Ok(Value::Bool(view.miner_set_miner_preference(pref)?))
        })(),
        "miner_setPrimaryCoinbase" => (|| {
            let a = Args::new(params, 1)?;
            let addr = a.get(0, args::address)?;
            Ok(Value::Bool(view.miner_set_primary_coinbase(addr)?))
        })(),
        "miner_setRecommitInterval" => (|| {
            let a = Args::new(params, 1)?;
            let ms = a.get(0, int_arg)?;
            view.miner_set_recommit_interval(ms)?;
            Ok(Value::Null)
        })(),
        "quai_calcOrder" => (|| {
            let a = Args::new(params, 1)?;
            let raw = a.get(0, args::bytes)?;
            Ok(json::u64_hex(u64::from(view.calc_order(&raw)?)))
        })(),
        "quai_getBlockTemplate" => (|| {
            let a = Args::new(params, 1)?;
            let req = match params {
                Value::Array(list) => list.first().filter(|v| !v.is_null()),
                _ => None,
            };
            let _ = a;
            Ok(view.get_block_template(req)?)
        })(),
        "quai_getPendingHeader" => (|| {
            Args::new(params, 0)?;
            Ok(json::bytes_hex(&view.get_pending_header()?))
        })(),
        "quai_getPendingWorkShares" => (|| {
            Args::new(params, 0)?;
            Ok(view.get_pending_workshares()?)
        })(),
        "quai_submitKawpowBlock" => (|| {
            let a = Args::new(params, 1)?;
            let raw = a.get(0, args::bytes)?;
            Ok(view.submit_block(&raw, 1)?)
        })(),
        "quai_submitShaBlock" => (|| {
            let a = Args::new(params, 1)?;
            let raw = a.get(0, args::bytes)?;
            Ok(view.submit_block(&raw, 3)?)
        })(),
        "quai_submitScryptBlock" => (|| {
            let a = Args::new(params, 1)?;
            let raw = a.get(0, args::bytes)?;
            Ok(view.submit_block(&raw, 4)?)
        })(),
        "quai_receiveMinedHeader" => (|| {
            let a = Args::new(params, 1)?;
            let raw = a.get(0, args::bytes)?;
            view.receive_mined_header(&raw)?;
            Ok(Value::Null)
        })(),
        "quai_receiveRawWorkShare" => (|| {
            let a = Args::new(params, 1)?;
            let raw = a.get(0, args::bytes)?;
            view.receive_raw_workshare(&raw)?;
            Ok(Value::Null)
        })(),
        "quai_receiveWorkShare" => (|| {
            let _ = Args::new(params, 1)?;
            let ws = params
                .as_array()
                .and_then(|a| a.first())
                .ok_or_else(|| RpcError::params("missing workshare argument"))?;
            view.receive_workshare(ws)?;
            Ok(Value::Null)
        })(),
        "quai_getWorkShareP2PThreshold" => {
            Args::new(params, 0).map(|_| json!(view.workshare_p2p_threshold()))
        }
        "quai_setWorkShareP2PThreshold" => (|| {
            let a = Args::new(params, 1)?;
            let t = a.get(0, int_arg)?;
            if t > 64 {
                return Err(RpcError::go("workshare p2p threshold out of range"));
            }
            view.set_workshare_p2p_threshold(t as u16)?;
            Ok(Value::Null)
        })(),
        "quai_submitAuxTemplate" => (|| {
            let a = Args::new(params, 1)?;
            let raw = a.get(0, args::bytes)?;
            view.submit_aux_template(&raw)?;
            Ok(Value::Null)
        })(),
        "quai_signAuxTemplate" => (|| {
            let a = Args::new(params, 3)?;
            let tpl = a.get(0, args::bytes)?;
            let other_pk = a.get(1, args::bytes)?;
            let other_nonce = a.get_optional(2, args::bytes)?.unwrap_or_default();
            Ok(view.sign_aux_template(&tpl, &other_pk, &other_nonce)?)
        })(),
        "quai_getSubsidyChainHeight" => (|| {
            Args::new(params, 0)?;
            Ok(view.subsidy_chain_height()?)
        })(),
        "quai_getQuaiHeaderForDonorHash" => (|| {
            let a = Args::new(params, 1)?;
            let donor_hash = a.get(0, args::hash)?;
            let Some(block) = view.quai_header_for_donor(&donor_hash)? else {
                return Err(RpcError::go("no quai header found for donor hash"));
            };
            Ok(json::work_object_v2(&block))
        })(),
        "quai_getCoinbaseTxForWorkShareHash" => coinbase_tx_for_workshare(view, params),
        "quai_getDonorChainInfoForWorkshare" => donor_chain_info_for_workshare(view, params),
        "quai_getMiningInfo" => mining_info(view, params),
        "workshare_getWorkShareThreshold" => {
            Args::new(params, 0).map(|_| json!(view.workshare_threshold()))
        }
        "workshare_receiveSubWorkshare" => (|| {
            let a = Args::new(params, 1)?;
            let raw = a.get(0, args::bytes)?;
            view.receive_sub_workshare(&raw)?;
            Ok(Value::Null)
        })(),
        _ => return None,
    })
}

fn int_arg(v: &Value) -> Result<u64, String> {
    match v {
        Value::Number(n) => n
            .as_u64()
            .ok_or_else(|| "expected unsigned integer".to_string()),
        Value::String(_) => args::uint64(v),
        _ => Err("expected integer or hex quantity".into()),
    }
}

fn coinbase_tx_for_workshare(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 1)?;
    let ws_hash = a.get(0, args::hash)?;
    let Some(block) = view.block_for_workshare(&ws_hash)? else {
        return Err(RpcError::go("no block found for workshare hash"));
    };
    let loc = Some(view.location());
    for (index, tx) in block.object.transactions().iter().enumerate() {
        if let Kind::External(ext) = tx.kind() {
            if ext.etx_type == 1 && ext.data.len() >= 32 {
                let tail = &ext.data[ext.data.len() - 32..];
                if tail == ws_hash.as_slice() {
                    let hash = block
                        .tx_hashes
                        .get(index)
                        .copied()
                        .or_else(|| tx.id(loc).ok())
                        .unwrap_or_default();
                    return Ok(json::transaction(
                        tx,
                        &hash,
                        Some(json::Position {
                            block_hash: block.hash,
                            block_number: block.number,
                            index: index as u64,
                        }),
                    ));
                }
            }
        }
    }
    Err(RpcError::go(
        "no coinbase transaction found for workshare hash",
    ))
}

fn parse_u256_arg(v: &Value) -> Result<[u8; 32], String> {
    let raw = args::bytes(v).or_else(|_| {
        let s = v
            .as_str()
            .ok_or_else(|| "expected hex quantity".to_string())?;
        let t = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .ok_or_else(|| "hex string without 0x prefix".to_string())?;
        if t.is_empty() {
            return Ok(vec![]);
        }
        let padded = if t.len() % 2 == 1 {
            format!("0{t}")
        } else {
            t.to_string()
        };
        quai_primitives::decode_hex(&format!("0x{padded}"))
    })?;
    if raw.len() > 32 {
        return Err("hex quantity exceeds 256 bits".into());
    }
    let mut out = [0u8; 32];
    out[32 - raw.len()..].copy_from_slice(&raw);
    Ok(out)
}

fn donor_chain_info_for_workshare(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 1)?;
    let ws_hash = a.get(0, args::hash)?;
    let Some(block) = view.block_for_workshare(&ws_hash)? else {
        return Err(RpcError::go("block not found for work share hash"));
    };
    let mut workshare = if block.hash == ws_hash {
        Some(block.header.clone())
    } else {
        block
            .uncles
            .iter()
            .find(|u| u.block_hash().ok() == Some(ws_hash))
            .cloned()
    };
    if workshare.is_none() {
        let start = block.number.saturating_sub(20);
        for n in (start..=block.number).rev() {
            let Ok(Some(b)) = view.block_by_number(n) else {
                continue;
            };
            if b.hash == ws_hash {
                workshare = Some(b.header.clone());
                break;
            }
            if let Some(u) = b
                .uncles
                .iter()
                .find(|u| u.block_hash().ok() == Some(ws_hash))
            {
                workshare = Some(u.clone());
                break;
            }
        }
    }
    let Some(ws) = workshare else {
        return Err(RpcError::go("workshare not found in recent blocks"));
    };
    let Some(aux_bytes) = ws.wire().aux_pow.as_deref() else {
        return Err(RpcError::go("workshare has no AuxPoW (Progpow workshare)"));
    };
    let aux = Auxiliary::decode(aux_bytes).map_err(|e| RpcError::go(e.to_string()))?;
    let pow_id = aux.id();
    let pow_name = match pow_id {
        0 => "Progpow",
        1 => "Kawpow",
        2 => "SHA_BTC",
        3 => "SHA_BCH",
        4 => "Scrypt",
        _ => "Unknown",
    };
    let dh = aux.header();
    let bits = if dh.len() >= 76 {
        u32::from_le_bytes([dh[72], dh[73], dh[74], dh[75]])
    } else {
        0
    };
    let pow_hash = match pow_id {
        1 => view.compute_kawpow_pow_hash(&ws).unwrap_or([0u8; 32]),
        2 | 3 => donor::sha256d(dh).unwrap_or([0u8; 32]),
        4 => donor::scrypt(dh).unwrap_or([0u8; 32]),
        _ => [0u8; 32],
    };

    let mut result = Map::new();
    result.insert("powHash".into(), Value::String(encode_hex(&pow_hash)));
    result.insert("powId".into(), json!(pow_id));
    result.insert("powIdName".into(), Value::String(pow_name.into()));
    result.insert("bits".into(), json!(bits));
    if bits > 0 {
        let target = compact_to_target(bits);
        let pow_int = U256::from_be_bytes(pow_hash);
        result.insert("blockTarget".into(), json::big_hex(&target.to_be_bytes()));
        if pow_int > U256::ZERO && target > U256::ZERO {
            result.insert(
                "meetsBlockDifficulty".into(),
                Value::Bool(pow_int <= target),
            );
            let pct = (u256_to_f64(target) / u256_to_f64(pow_int)) * 100.0;
            if pct.is_finite() {
                result.insert("difficultyPct".into(), json!(pct));
            }
        }
    }
    Ok(Value::Object(result))
}

fn mining_info(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 1)?;
    let decimal = a.get_optional(0, args::boolean)?.unwrap_or(false);
    let head = head_block(view)?;
    let hw = head.header.wire();
    let prime =
        share_rules::number(hw.prime_terminus_number.as_deref().unwrap_or_default()).unwrap_or(0);
    if prime < FORKS.kawpow {
        return Err(RpcError::go(
            "getMiningInfo is only available after the KawPow fork",
        ));
    }
    let fmt_u256 = |v: U256| -> Value {
        if decimal {
            Value::String(u256_decimal(v))
        } else {
            json::big_hex(&v.to_be_bytes())
        }
    };

    let block_diff =
        share_rules::uint(hw.difficulty.as_deref().unwrap_or_default()).unwrap_or(U256::ONE);
    let sha_triple = Triple::from_header(&head.header, true).unwrap_or_default();
    let scrypt_triple = Triple::from_header(&head.header, false).unwrap_or_default();
    let sha_target =
        share_rules::uint(hw.sha_share_target.as_deref().unwrap_or_default()).unwrap_or(U256::ZERO);
    let scrypt_target = share_rules::uint(hw.scrypt_share_target.as_deref().unwrap_or_default())
        .unwrap_or(U256::ZERO);
    let kaw_ema = share_rules::uint(hw.kawpow_difficulty.as_deref().unwrap_or_default())
        .unwrap_or(U256::ZERO);
    let kawpow_diff = share_rules::kawpow_share_difficulty(
        block_diff.max(U256::ONE),
        sha_triple.count,
        scrypt_triple.count,
        sha_target,
        scrypt_target,
        kaw_ema,
    )
    .unwrap_or(block_diff);

    // Walk up to 15 minutes (capped at 360 blocks) to compute avgBlockTime and share rates
    let current_time = hw.time.unwrap_or(0);
    let cutoff = current_time.saturating_sub(15 * 60);
    let mut block_count = 0u64;
    let mut oldest_time = current_time;
    let mut kaw_shares = 0u64;
    let mut sha_shares = 0u64;
    let mut scrypt_shares = 0u64;
    let mut n = head.number;
    for _ in 0..360 {
        let Ok(Some(b)) = view.block_by_number(n) else {
            break;
        };
        let bt = b.header.wire().time.unwrap_or(0);
        if block_count > 0 && bt <= cutoff {
            break;
        }
        block_count += 1;
        oldest_time = bt;
        kaw_shares += 1;
        for u in &b.uncles {
            if let Some(aux_bytes) = u.wire().aux_pow.as_deref() {
                if let Ok(aux) = Auxiliary::decode(aux_bytes) {
                    match aux.id() {
                        1 => kaw_shares += 1,
                        2 | 3 => sha_shares += 1,
                        4 => scrypt_shares += 1,
                        _ => {}
                    }
                }
            }
        }
        if n == 0 {
            break;
        }
        n -= 1;
    }
    let span = current_time.saturating_sub(oldest_time);
    let avg_block_time = if block_count > 1 && span > 0 {
        ((span as f64 / (block_count - 1) as f64) * 1000.0).round() / 1000.0
    } else {
        5.0
    };
    let avg_tx_fees = share_rules::uint(
        head.object
            .header()
            .wire()
            .avg_tx_fees
            .as_deref()
            .unwrap_or_default(),
    )
    .unwrap_or(U256::ZERO);
    let exchange = share_rules::uint(
        head.object
            .header()
            .wire()
            .exchange_rate
            .as_deref()
            .unwrap_or_default(),
    )
    .unwrap_or(U256::from_u128(
        quai_primitives::spec::ECONOMICS.exchange_rate,
    ));
    let quote = quai_consensus::economics::reward_quote(&head.header, exchange, avg_tx_fees).ok();
    let base_reward = quote.as_ref().map_or(U256::ZERO, |q| q.quai);
    let est_reward = base_reward.saturating_add(&avg_tx_fees.saturating_mul(&U256::from_u64(2)));
    let ws_reward = est_reward.wrapping_div(&U256::from_u64(9));

    let calc_hr = |diff: U256, count: u64| -> String {
        if count == 0 || span == 0 || diff == U256::ZERO {
            return "0".into();
        }
        let num =
            crypto_bigint::U512::from(&diff).wrapping_mul(&crypto_bigint::U512::from_u64(count));
        let q = num.wrapping_div(&crypto_bigint::U512::from_u64(span));
        if q.bits_vartime() > 256 {
            u256_decimal(U256::MAX)
        } else {
            u256_decimal(q.resize())
        }
    };

    let mut fields = Map::new();
    fields.insert("kawpowDifficulty".into(), fmt_u256(kawpow_diff));
    fields.insert("shaDifficulty".into(), fmt_u256(sha_triple.difficulty));
    fields.insert(
        "scryptDifficulty".into(),
        fmt_u256(scrypt_triple.difficulty),
    );
    fields.insert("avgBlockTime".into(), json!(avg_block_time));
    fields.insert("avgTxFees".into(), fmt_u256(avg_tx_fees));
    fields.insert("blocksAnalyzed".into(), json!(block_count));
    fields.insert("baseBlockReward".into(), fmt_u256(base_reward));
    fields.insert("estimatedBlockReward".into(), fmt_u256(est_reward));
    fields.insert("workshareReward".into(), fmt_u256(ws_reward));
    if prime >= FORKS.conversion_lock_change {
        if let Some(q) = &quote {
            fields.insert(
                "qiWorkshareReward".into(),
                fmt_u256(q.qi.wrapping_div(&U256::from_u64(9))),
            );
        }
    }
    let q_scale = (1u64 << 32) as f64;
    let sha_spb = u256_to_f64(sha_triple.count) / q_scale;
    let scrypt_spb = u256_to_f64(scrypt_triple.count) / q_scale;
    fields.insert(
        "avgShaShareTime".into(),
        json!(if sha_spb > 0.0 {
            ((avg_block_time / sha_spb) * 1000.0).round() / 1000.0
        } else {
            0.0
        }),
    );
    fields.insert(
        "avgScryptShareTime".into(),
        json!(if scrypt_spb > 0.0 {
            ((avg_block_time / scrypt_spb) * 1000.0).round() / 1000.0
        } else {
            0.0
        }),
    );
    fields.insert("avgKawpowShareTime".into(), json!(avg_block_time));
    fields.insert(
        "kawpowHashRate".into(),
        Value::String(calc_hr(kawpow_diff, kaw_shares)),
    );
    fields.insert(
        "shaHashRate".into(),
        Value::String(calc_hr(sha_triple.difficulty, sha_shares)),
    );
    fields.insert(
        "scryptHashRate".into(),
        Value::String(calc_hr(scrypt_triple.difficulty, scrypt_shares)),
    );
    fields.insert(
        "blockNumber".into(),
        if decimal {
            json!(head.number)
        } else {
            json::u64_hex(head.number)
        },
    );
    fields.insert("blockHash".into(), json::hash_hex(&head.hash));
    if let Ok(Some(supply)) = view.supply_analytics(&head.hash) {
        fields.insert(
            "quaiSupplyTotal".into(),
            fmt_u256(U256::from_be_bytes(supply.quai_total)),
        );
    }
    Ok(Value::Object(fields))
}

fn compact_to_target(bits: u32) -> U256 {
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

fn u256_to_f64(v: U256) -> f64 {
    let bytes = v.to_be_bytes();
    let mut acc = 0.0f64;
    for b in bytes {
        acc = acc * 256.0 + f64::from(b);
    }
    acc
}

fn u256_decimal(v: U256) -> String {
    if v == U256::ZERO {
        return "0".into();
    }
    let mut n = v;
    let ten = U256::from_u64(10);
    let mut digits = Vec::new();
    while n != U256::ZERO {
        let q = n.wrapping_div(&ten);
        let r = n.wrapping_sub(&q.wrapping_mul(&ten));
        digits.push((b'0' + r.to_be_bytes()[31]) as char);
        n = q;
    }
    digits.reverse();
    digits.into_iter().collect()
}
