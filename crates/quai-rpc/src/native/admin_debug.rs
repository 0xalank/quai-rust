//! Admin, Debug, Web3, and standard Ethereum/Quai wallet compatibility RPC
//! methods (`go-quai/quai/api.go`, `internal/quaiapi/api.go`).
use super::args::{self, Args, BlockNumber, BlockRef};
use super::{by_number, head_block, json, RpcError, R};
use crate::view::ChainView;
use quai_primitives::{encode_hex, transaction::keccak};
use serde_json::Value;

pub const METHODS: &[&str] = &[
    // Admin namespace
    "admin_exportChain",
    "admin_importChain",
    "admin_nodeInfo",
    "admin_peers",
    // Debug namespace
    "debug_accountRange",
    "debug_chaindbCompact",
    "debug_chaindbProperty",
    "debug_dumpBlock",
    "debug_getBlockRlp",
    "debug_getModifiedAccountsByHash",
    "debug_getModifiedAccountsByNumber",
    "debug_getWorkshareLRUDump",
    "debug_preimage",
    "debug_printBlock",
    "debug_seedHash",
    "debug_storageRangeAt",
    // Standard wallet / Web3 / Eth / Quai utility methods
    "web3_clientVersion",
    "web3_sha3",
    "eth_syncing",
    "quai_syncing",
    "eth_feeHistory",
    "quai_feeHistory",
    "eth_maxPriorityFeePerGas",
    "quai_maxPriorityFeePerGas",
    "eth_getBlockReceipts",
    "quai_getBlockReceipts",
    "eth_protocolVersion",
    "quai_protocolVersion",
    "txpool_getRollingFeeInfoV2",
    "quai_getRollingFeeInfo",
];

pub fn dispatch(view: &dyn ChainView, method: &str, params: &Value) -> Option<R<Value>> {
    Some(match method {
        // --- Web3 / Wallet Compatibility ---
        "web3_clientVersion" => {
            Args::new(params, 0).map(|_| Value::String(super::chain::client_version()))
        }
        "web3_sha3" => (|| {
            let a = Args::new(params, 1)?;
            let data = a.get(0, args::bytes)?;
            Ok(json::hash_hex(&keccak(&data)))
        })(),
        "eth_syncing" | "quai_syncing" => Args::new(params, 0).map(|_| view.syncing()),
        "eth_protocolVersion" | "quai_protocolVersion" => {
            Args::new(params, 0).map(|_| Value::String("0x1".into()))
        }
        "eth_maxPriorityFeePerGas" | "quai_maxPriorityFeePerGas" => {
            // Quai uses EIP-1559-style base fee with priority tip; report 1 gwei default
            // or the latest rolling minimum fee if available.
            Args::new(params, 0).map(|_| {
                view.rolling_fee_info()
                    .map(|f| json::big_hex(&f[0]))
                    .unwrap_or_else(|| json::u64_hex(1_000_000_000))
            })
        }
        "eth_getBlockReceipts" | "quai_getBlockReceipts" => block_receipts(view, params),
        "eth_feeHistory" | "quai_feeHistory" => fee_history(view, params),
        "txpool_getRollingFeeInfoV2" | "quai_getRollingFeeInfo" => {
            Args::new(params, 0).and_then(|_| {
                let Some(fees) = view.rolling_fee_info() else {
                    return Err(RpcError::go(
                        "no transactions processed to calculate min, max, or avg",
                    ));
                };
                Ok(json!({
                    "min": json::big_hex(&fees[0]),
                    "max": json::big_hex(&fees[1]),
                    "avg": json::big_hex(&fees[2]),
                }))
            })
        }

        // --- Admin ---
        "admin_nodeInfo" => Args::new(params, 0).and_then(|_| Ok(view.admin_node_info()?)),
        "admin_peers" => Args::new(params, 0).and_then(|_| Ok(view.admin_peers()?)),
        "admin_exportChain" => (|| {
            let a = Args::new(params, 3)?;
            let file = a.get(0, args::string)?;
            let first = a.get_optional(1, uint_or_hex)?;
            let last = a.get_optional(2, uint_or_hex)?;
            if first.is_none() && last.is_some() {
                return Err(RpcError::go("last cannot be specified without first"));
            }
            Ok(Value::Bool(view.admin_export_chain(&file, first, last)?))
        })(),
        "admin_importChain" => (|| {
            let a = Args::new(params, 1)?;
            let file = a.get(0, args::string)?;
            Ok(Value::Bool(view.admin_import_chain(&file)?))
        })(),

        // --- Debug ---
        "debug_getBlockRlp" => (|| {
            let a = Args::new(params, 1)?;
            let number = a.get(0, uint_or_hex)?;
            let Some(block) = view.block_by_number(number)? else {
                return Err(RpcError::go(format!("block #{number} not found")));
            };
            let raw = block.object.encode();
            let mut hex = String::with_capacity(raw.len() * 2);
            for b in raw {
                hex.push_str(&format!("{b:02x}"));
            }
            Ok(Value::String(hex))
        })(),
        "debug_printBlock" => (|| {
            let a = Args::new(params, 1)?;
            let number = a.get(0, uint_or_hex)?;
            let Some(block) = view.block_by_number(number)? else {
                return Err(RpcError::go(format!("block #{number} not found")));
            };
            let pretty = serde_json::to_string_pretty(&super::block_json(view, &block, true)?)
                .unwrap_or_default();
            Ok(Value::String(pretty))
        })(),
        "debug_seedHash" => (|| {
            let a = Args::new(params, 1)?;
            let number = a.get(0, uint_or_hex)?;
            if view.block_by_number(number)?.is_none() {
                return Err(RpcError::go(format!("block #{number} not found")));
            }
            let epoch = number / 388_800;
            let mut seed = [0u8; 32];
            for _ in 0..epoch {
                seed = keccak(&seed);
            }
            Ok(Value::String(encode_hex(&seed)))
        })(),
        "debug_getWorkshareLRUDump" => (|| {
            let a = Args::new(params, 1)?;
            let limit = a.get_optional(0, uint_or_hex)?.unwrap_or(0) as usize;
            Ok(view.workshare_lru_dump(limit)?)
        })(),
        "debug_chaindbProperty" => (|| {
            let a = Args::new(params, 1)?;
            let prop = a.get_optional(0, args::string)?.unwrap_or_default();
            Ok(Value::String(view.debug_chaindb_property(&prop)?))
        })(),
        "debug_chaindbCompact" => (|| {
            Args::new(params, 0)?;
            view.debug_chaindb_compact()?;
            Ok(Value::Null)
        })(),
        "debug_preimage" => (|| {
            let a = Args::new(params, 1)?;
            let hash = a.get(0, args::hash)?;
            match view.debug_preimage(&hash)? {
                Some(bytes) => Ok(json::bytes_hex(&bytes)),
                None => Err(RpcError::go("unknown preimage")),
            }
        })(),
        "debug_dumpBlock" => (|| {
            let a = Args::new(params, 1)?;
            let num = a.get(0, args::block_number)?;
            if num == BlockNumber::Pending {
                return Ok(json!({"root": "", "accounts": {}}));
            }
            let Some(block) = by_number(view, num)? else {
                return Err(RpcError::go("block not found"));
            };
            Ok(view.debug_dump_block(&block, &[], 256, false, false, false)?)
        })(),
        "debug_accountRange" => (|| {
            let a = Args::new(params, 6)?;
            let at = a.get(0, args::block_ref)?;
            let start = a.get_optional(1, args::bytes)?.unwrap_or_default();
            let max_results = a.get_optional(2, uint_or_hex)?.unwrap_or(256).clamp(1, 256) as usize;
            let no_code = a.get_optional(3, args::boolean)?.unwrap_or(false);
            let no_storage = a.get_optional(4, args::boolean)?.unwrap_or(false);
            let incompletes = a.get_optional(5, args::boolean)?.unwrap_or(false);
            let block = match at {
                BlockRef::Number(BlockNumber::Pending) => {
                    return Err(RpcError::go(
                        "pending state is not available for accountRange",
                    ))
                }
                BlockRef::Number(n) => {
                    by_number(view, n)?.ok_or_else(|| RpcError::go("block not found"))?
                }
                BlockRef::Hash { hash, .. } => view
                    .block_or_candidate(&hash)?
                    .ok_or_else(|| RpcError::go("block not found"))?,
                BlockRef::Neither => {
                    return Err(RpcError::go(
                        "either block number or block hash must be specified",
                    ))
                }
            };
            Ok(view.debug_dump_block(
                &block,
                &start,
                max_results,
                no_code,
                no_storage,
                incompletes,
            )?)
        })(),
        "debug_storageRangeAt" => (|| {
            let a = Args::new(params, 5)?;
            let block_hash = a.get(0, args::hash)?;
            let tx_index = a.get(1, uint_or_hex)? as usize;
            let contract = a.get(2, args::address)?;
            let key_start = a.get_optional(3, args::bytes)?.unwrap_or_default();
            let max_result = a
                .get_optional(4, uint_or_hex)?
                .unwrap_or(256)
                .clamp(1, 1024) as usize;
            let Some(block) = view.block_or_candidate(&block_hash)? else {
                return Err(RpcError::go(format!(
                    "block {} not found",
                    encode_hex(&block_hash)
                )));
            };
            Ok(view.debug_storage_range_at(&block, tx_index, &contract, &key_start, max_result)?)
        })(),
        "debug_getModifiedAccountsByNumber" => (|| {
            let a = Args::new(params, 2)?;
            let start_num = a.get(0, uint_or_hex)?;
            let end_num = a.get_optional(1, uint_or_hex)?;
            let Some(start_blk) = view.block_by_number(start_num)? else {
                return Err(RpcError::go(format!("start block {start_num:x} not found")));
            };
            let (from_blk, to_blk) = match end_num {
                None => {
                    if start_num == 0 {
                        return Err(RpcError::go("block 0 has no parent"));
                    }
                    let parent = view
                        .block_by_number(start_num - 1)?
                        .ok_or_else(|| RpcError::go("block has no parent"))?;
                    (parent, start_blk)
                }
                Some(e) => {
                    let end_blk = view
                        .block_by_number(e)?
                        .ok_or_else(|| RpcError::go(format!("end block {e} not found")))?;
                    (start_blk, end_blk)
                }
            };
            let addrs = view.debug_modified_accounts(&from_blk, &to_blk)?;
            Ok(Value::Array(
                addrs
                    .iter()
                    .map(|addr| Value::String(json::checksum(addr)))
                    .collect(),
            ))
        })(),
        "debug_getModifiedAccountsByHash" => (|| {
            let a = Args::new(params, 2)?;
            let start_hash = a.get(0, args::hash)?;
            let end_hash = a.get_optional(1, args::hash)?;
            let Some(start_blk) = view.block_or_candidate(&start_hash)? else {
                return Err(RpcError::go("start block not found"));
            };
            let (from_blk, to_blk) = match end_hash {
                None => {
                    if start_blk.number == 0 {
                        return Err(RpcError::go("block 0 has no parent"));
                    }
                    let parent = view
                        .block_by_number(start_blk.number - 1)?
                        .ok_or_else(|| RpcError::go("block has no parent"))?;
                    (parent, start_blk)
                }
                Some(eh) => {
                    let end_blk = view
                        .block_or_candidate(&eh)?
                        .ok_or_else(|| RpcError::go("end block not found"))?;
                    (start_blk, end_blk)
                }
            };
            let addrs = view.debug_modified_accounts(&from_blk, &to_blk)?;
            Ok(Value::Array(
                addrs
                    .iter()
                    .map(|addr| Value::String(json::checksum(addr)))
                    .collect(),
            ))
        })(),
        _ => return None,
    })
}

fn uint_or_hex(v: &Value) -> Result<u64, String> {
    match v {
        Value::Number(n) => n
            .as_u64()
            .ok_or_else(|| "expected unsigned integer".to_string()),
        Value::String(_) => args::uint64(v),
        _ => Err("expected integer or hex quantity".into()),
    }
}

fn block_receipts(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 1)?;
    let at = a.get(0, args::block_ref)?;
    let block = match at {
        BlockRef::Number(n) => by_number(view, n)?,
        BlockRef::Hash { hash, .. } => view.block_by_hash(&hash)?,
        BlockRef::Neither => {
            return Err(RpcError::go(
                "invalid arguments; neither block nor hash specified",
            ))
        }
    };
    let Some(block) = block else {
        return Ok(Value::Null);
    };
    let receipts = view.receipts(&block)?;
    let mut out = Vec::with_capacity(receipts.len());
    for i in 0..receipts.len() {
        out.push(json::receipt(&block, &receipts, i));
    }
    Ok(Value::Array(out))
}

fn fee_history(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 3)?;
    let block_count = a.get(0, uint_or_hex)?.min(1024);
    let newest = a.get(1, args::block_number)?;
    let percentiles: Vec<f64> = match params.as_array().and_then(|arr| arr.get(2)) {
        Some(Value::Array(list)) => list
            .iter()
            .map(|v| {
                v.as_f64()
                    .filter(|p| (0.0..=100.0).contains(p))
                    .ok_or_else(|| RpcError::params("invalid reward percentile"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(Value::Null) | None => vec![],
        _ => return Err(RpcError::params("rewardPercentiles must be an array")),
    };
    if block_count == 0 {
        return Ok(json!({
            "oldestBlock": "0x0",
            "baseFeePerGas": [],
            "gasUsedRatio": [],
        }));
    }
    let last_block = by_number(view, newest)?.unwrap_or(head_block(view)?);
    let end = last_block.number;
    let start = end.saturating_sub(block_count.saturating_sub(1));

    let mut base_fees = Vec::new();
    let mut ratios = Vec::new();
    let mut rewards = Vec::new();
    for n in start..=end {
        let Ok(Some(b)) = view.block_by_number(n) else {
            break;
        };
        let hdr = b.object.header().wire();
        let bf = hdr.base_fee.as_deref().unwrap_or_default();
        base_fees.push(json::big_hex(bf));
        let limit = hdr.gas_limit.unwrap_or(0);
        let used = hdr.gas_used.unwrap_or(0);
        let ratio = if limit > 0 {
            used as f64 / limit as f64
        } else {
            0.0
        };
        ratios.push(json!(ratio));
        if !percentiles.is_empty() {
            let mut tips: Vec<u128> = b
                .object
                .transactions()
                .iter()
                .filter_map(|tx| match tx.kind() {
                    quai_primitives::transaction::Kind::Quai(q) => {
                        let mut buf = [0u8; 16];
                        let n = q.gas_price.len().min(16);
                        buf[16 - n..].copy_from_slice(&q.gas_price[q.gas_price.len() - n..]);
                        Some(u128::from_be_bytes(buf))
                    }
                    _ => None,
                })
                .collect();
            tips.sort_unstable();
            let mut row = Vec::with_capacity(percentiles.len());
            for p in &percentiles {
                let val = if tips.is_empty() {
                    0u128
                } else {
                    let idx = ((*p / 100.0) * (tips.len() - 1) as f64).round() as usize;
                    tips[idx.min(tips.len() - 1)]
                };
                row.push(json::big_hex(&val.to_be_bytes()));
            }
            rewards.push(Value::Array(row));
        }
    }
    // Next block's predicted base fee (standard EIP-1559 feeHistory includes N+1 entries)
    let next_bf = view.calc_base_fee(&last_block).unwrap_or([0u8; 32]);
    base_fees.push(json::big_hex(&next_bf));

    let mut out = serde_json::Map::new();
    out.insert("oldestBlock".into(), json::u64_hex(start));
    out.insert("baseFeePerGas".into(), Value::Array(base_fees));
    out.insert("gasUsedRatio".into(), Value::Array(ratios));
    if !percentiles.is_empty() {
        out.insert("reward".into(), Value::Array(rewards));
    }
    Ok(Value::Object(out))
}
