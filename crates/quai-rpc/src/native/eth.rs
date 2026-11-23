//! The `eth` namespace of a go-quai v0.57.0 zone node (quaiapi.GetAPIs and
//! Quai.APIs). A default go-quai node serves it over WebSocket only
//! (docs/rpc-parity.md); this node serves it over HTTP as well, as go-quai
//! does with `--rpc.http-api eth`.
//!
//! go-quai registers `PublicQuaiAPI`, `PublicTransactionPoolAPI` and the
//! filter API under both `eth` and `quai`: those `eth_` names run the same
//! Go function as their `quai_` twins, and `alias` maps them. The others
//! are `PublicBlockChainAPI` (internal/quaiapi/api.go), with its own
//! argument types, errors and Ethereum-style JSON (`RPCMarshalETHHeader`,
//! `RPCMarshalETHBlock`), and `quai/api.go` (`eth_mining`,
//! `eth_primaryCoinbase`: mining, not served).
use super::args::{self, Args, BlockNumber, BlockRef};
use super::chain::{state_ref, StateRef};
use super::{by_number, count, head_block, json, qi, quai_internal, uncle, RpcError, R};
use crate::view::{BlockData, ChainView};
use serde_json::{Map, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// `eth_` names that run the same Go function as a `quai_` name.
pub const ALIASES: &[(&str, &str)] = &[
    ("eth_clientVersion", "quai_clientVersion"),
    ("eth_gasPrice", "quai_gasPrice"),
    (
        "eth_getBlockTransactionCountByHash",
        "quai_getBlockTransactionCountByHash",
    ),
    (
        "eth_getBlockTransactionCountByNumber",
        "quai_getBlockTransactionCountByNumber",
    ),
    (
        "eth_getRawTransactionByBlockHashAndIndex",
        "quai_getRawTransactionByBlockHashAndIndex",
    ),
    (
        "eth_getRawTransactionByBlockNumberAndIndex",
        "quai_getRawTransactionByBlockNumberAndIndex",
    ),
    (
        "eth_getRawTransactionByHash",
        "quai_getRawTransactionByHash",
    ),
    (
        "eth_getTransactionByBlockHashAndIndex",
        "quai_getTransactionByBlockHashAndIndex",
    ),
    (
        "eth_getTransactionByBlockNumberAndIndex",
        "quai_getTransactionByBlockNumberAndIndex",
    ),
    ("eth_getTransactionByHash", "quai_getTransactionByHash"),
    ("eth_getTransactionCount", "quai_getTransactionCount"),
    ("eth_sendRawTransaction", "quai_sendRawTransaction"),
    (
        "eth_receiveTxFromPoolSharingClient",
        "quai_receiveTxFromPoolSharingClient",
    ),
    (
        "eth_newPendingTransactionFilter",
        "quai_newPendingTransactionFilter",
    ),
    ("eth_newBlockFilter", "quai_newBlockFilter"),
    ("eth_newFilter", "quai_newFilter"),
    ("eth_uninstallFilter", "quai_uninstallFilter"),
    ("eth_getFilterChanges", "quai_getFilterChanges"),
    ("eth_getFilterLogs", "quai_getFilterLogs"),
    ("eth_getLogs", "quai_getLogs"),
];
/// The `quai_` twin of an aliased `eth_` name, else the name.
pub fn alias(method: &str) -> &str {
    ALIASES
        .iter()
        .find(|(eth, _)| *eth == method)
        .map_or(method, |(_, quai)| quai)
}

/// The `PublicBlockChainAPI` methods of this module.
pub const METHODS: &[&str] = &[
    "eth_chainId",
    "eth_blockNumber",
    "eth_getBalance",
    "eth_getProof",
    "eth_getHeaderByNumber",
    "eth_getHeaderByHash",
    "eth_getBlockByNumber",
    "eth_getBlockByHash",
    "eth_getUncleByBlockNumberAndIndex",
    "eth_getUncleByBlockHashAndIndex",
    "eth_getUncleCountByBlockNumber",
    "eth_getUncleCountByBlockHash",
    "eth_getCode",
    "eth_getStorageAt",
    "eth_getTransactionReceipt",
    "eth_call",
    "eth_estimateGas",
    "eth_createAccessList",
];

pub fn dispatch(view: &dyn ChainView, method: &str, params: &Value) -> Option<R<Value>> {
    dispatch_cancellable(view, method, params, None)
}
pub fn dispatch_cancellable(
    view: &dyn ChainView,
    method: &str,
    params: &Value,
    cancel: Option<&Arc<AtomicBool>>,
) -> Option<R<Value>> {
    Some(match method {
        // The same bodies as the `quai_` methods.
        "eth_chainId" => super::dispatch(view, "quai_chainId", params),
        "eth_blockNumber" => super::dispatch(view, "quai_blockNumber", params),
        "eth_getProof" => super::chain::dispatch(view, "quai_getProof", params)?,
        "eth_getBalance" => balance(view, params),
        "eth_getHeaderByNumber" => (|| {
            let a = Args::new(params, 1)?;
            let number = a.get(0, args::block_number)?;
            Ok(by_number(view, number)?.map_or(Value::Null, |b| header(&b)))
        })(),
        "eth_getHeaderByHash" => (|| {
            let a = Args::new(params, 1)?;
            let hash = a.get(0, args::hash)?;
            Ok(view
                .block_or_candidate(&hash)?
                .map_or(Value::Null, |b| header(&b)))
        })(),
        "eth_getBlockByNumber" => (|| {
            let a = Args::new(params, 2)?;
            let number = a.get(0, args::block_number)?;
            let full = a.get(1, args::boolean)?;
            Ok(by_number(view, number)?.map_or(Value::Null, |b| block(&b, full)))
        })(),
        "eth_getBlockByHash" => (|| {
            let a = Args::new(params, 2)?;
            let hash = a.get(0, args::hash)?;
            let full = a.get(1, args::boolean)?;
            Ok(view
                .block_or_candidate(&hash)?
                .map_or(Value::Null, |b| block(&b, full)))
        })(),
        "eth_getUncleByBlockNumberAndIndex" => (|| {
            let a = Args::new(params, 2)?;
            let number = a.get(0, args::block_number)?;
            let index = a.get(1, args::uint)?;
            Ok(by_number(view, number)?.map_or(Value::Null, |b| uncle(&b, index)))
        })(),
        "eth_getUncleByBlockHashAndIndex" => (|| {
            let a = Args::new(params, 2)?;
            let hash = a.get(0, args::hash)?;
            let index = a.get(1, args::uint)?;
            Ok(view
                .block_or_candidate(&hash)?
                .map_or(Value::Null, |b| uncle(&b, index)))
        })(),
        "eth_getUncleCountByBlockNumber" => (|| {
            let a = Args::new(params, 1)?;
            let number = a.get(0, args::block_number)?;
            Ok(by_number(view, number)?.map_or(Value::Null, |b| count(&b, method)))
        })(),
        "eth_getUncleCountByBlockHash" => (|| {
            let a = Args::new(params, 1)?;
            let hash = a.get(0, args::hash)?;
            Ok(view
                .block_or_candidate(&hash)?
                .map_or(Value::Null, |b| count(&b, method)))
        })(),
        "eth_getCode" => (|| {
            let a = Args::new(params, 2)?;
            let address = a.get(0, args::address)?;
            let at = a.get(1, args::block_ref)?;
            let state = state_ref(view, at)?;
            quai_internal(view, &address)?;
            Ok(json::bytes_hex(&state.state()?.code(&address)?))
        })(),
        "eth_getStorageAt" => (|| {
            let a = Args::new(params, 3)?;
            let address = a.get(0, args::address)?;
            let key = a.get(1, args::string)?;
            let at = a.get(2, args::block_ref)?;
            let state = state_ref(view, at)?;
            quai_internal(view, &address)?;
            let key = args::lenient_hash(&key);
            Ok(json::bytes_hex(&state.state()?.storage(&address, &key)?))
        })(),
        "eth_getTransactionReceipt" => receipt(view, params),
        // The same body as quai_call on a zone node that processes state.
        "eth_call" => super::dispatch_cancellable(view, "quai_call", params, cancel),
        "eth_estimateGas" => (|| {
            let a = Args::new(params, 2)?;
            let call = a.get(0, super::call_args)?;
            let at = a
                .get_optional(1, args::block_ref)?
                .unwrap_or(BlockRef::Number(BlockNumber::Latest));
            super::do_estimate_gas_cancellable(view, call, at, cancel)
        })(),
        "eth_createAccessList" => (|| {
            // The default block is the pending one (quai_: latest).
            let a = Args::new(params, 2)?;
            let call = a.get(0, super::call_args)?;
            let at = a
                .get_optional(1, args::block_ref)?
                .unwrap_or(BlockRef::Number(BlockNumber::Pending));
            super::create_access_list_cancellable(view, &call, at, cancel)
        })(),
        _ => return None,
    })
}

/// `PublicBlockChainAPI.GetBalance` (`common.AddressBytes`, no checksum):
/// a Qi address sums its unlocked outputs at the head in any zone; a Quai
/// address reads the state.
fn balance(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 2)?;
    let address = a.get(0, args::address_bytes)?;
    let at = a.get(1, args::block_ref)?;
    let state = state_ref(view, at)?;
    if qi(&address) {
        let StateRef::At(block, _) = state else {
            return Err(RpcError::go("block header not found"));
        };
        let head = head_block(view)?;
        if block.hash != head.hash {
            return Err(RpcError::go(
                "qi balance query is only supported for the current block",
            ));
        }
        let mut total: u128 = 0;
        for o in view.outpoints_by_address(&address)? {
            let lock = o.lock.iter().fold(0u128, |a, b| {
                a.saturating_mul(256).saturating_add(*b as u128)
            });
            if !o.lock.is_empty() && (head.number as u128) < lock {
                continue;
            }
            total += *super::DENOMINATIONS
                .get(o.denomination as usize)
                .unwrap_or(&0) as u128;
        }
        return Ok(json::big_hex(&total.to_be_bytes()));
    }
    quai_internal(view, &address)?;
    Ok(json::big_hex(&state.state()?.balance(&address)?))
}

/// `PublicBlockChainAPI.GetTransactionReceipt`: the `quai_` receipt with
/// `Bloom.ToLegacyBloom()`, the first 256 bytes of the bloom.
fn receipt(view: &dyn ChainView, params: &Value) -> R<Value> {
    let mut v = super::dispatch(view, "quai_getTransactionReceipt", params)?;
    if let Some(Value::String(bloom)) = v.get_mut("logsBloom") {
        bloom.truncate(2 + 2 * 256);
    }
    Ok(v)
}

/// `RPCMarshalETHHeader(wo.Header(), wo.WorkObjectHeader())`.
pub fn header(block: &BlockData) -> Value {
    let body = block.object.header().wire();
    let wo = block.header.wire();
    let hash32 = |v: &Option<quai_primitives::wire::BytesValue>| {
        json::hash_hex(v.as_ref().map(|v| v.value.as_slice()).unwrap_or_default())
    };
    let proto = |v: &Option<quai_primitives::schema::common::ProtoHash>| {
        json::hash_hex(v.as_ref().map(|v| v.value.as_slice()).unwrap_or_default())
    };
    let coinbase = wo
        .primary_coinbase
        .as_ref()
        .map(|v| json::address20(&v.value))
        .unwrap_or_default();
    let mut m = Map::new();
    m.insert(
        "number".into(),
        json::big_hex(wo.number.as_deref().unwrap_or_default()),
    );
    m.insert("hash".into(), json::hash_hex(&block.hash));
    m.insert("parentHash".into(), hash32(&wo.parent_hash));
    m.insert(
        "nonce".into(),
        Value::String(format!("0x{:016x}", wo.nonce.unwrap_or_default())),
    );
    m.insert("mixHash".into(), hash32(&wo.mix_hash));
    m.insert("sha3Uncles".into(), proto(&body.uncle_hash));
    m.insert("logsBloom".into(), json::bytes_hex(&[0; 256]));
    m.insert("stateRoot".into(), proto(&body.evm_root));
    m.insert("miner".into(), json::address_lower(&coinbase));
    m.insert(
        "difficulty".into(),
        json::big_hex(wo.difficulty.as_deref().unwrap_or_default()),
    );
    m.insert(
        "extraData".into(),
        json::bytes_hex(body.extra.as_deref().unwrap_or_default()),
    );
    m.insert(
        "gasLimit".into(),
        json::u64_hex(body.gas_limit.unwrap_or_default()),
    );
    m.insert(
        "gasUsed".into(),
        json::u64_hex(body.gas_used.unwrap_or_default()),
    );
    m.insert(
        "timestamp".into(),
        json::u64_hex(wo.time.unwrap_or_default()),
    );
    m.insert("transactionsRoot".into(), proto(&body.tx_hash));
    m.insert("receiptsRoot".into(), proto(&body.receipt_hash));
    m.insert(
        "baseFeePerGas".into(),
        body.base_fee.as_deref().map_or(Value::Null, json::big_hex),
    );
    Value::Object(m)
}

/// `RPCMarshalETHBlock(block, true, fullTx, location)`: the header, the
/// size, the transactions (hashes, or `newRPCTransactionFromBlockHash`,
/// the first transaction with that hash) and the hashes of every uncle.
pub fn block(block: &BlockData, full: bool) -> Value {
    let Value::Object(mut m) = header(block) else {
        return Value::Null;
    };
    m.insert("size".into(), json::u64_hex(block.size));
    let txs = block
        .tx_hashes
        .iter()
        .map(|h| {
            if !full {
                return json::hash_hex(h);
            }
            let first = block.tx_hashes.iter().position(|x| x == h).unwrap_or(0);
            super::tx_at(block, first as u64)
        })
        .collect();
    m.insert("transactions".into(), Value::Array(txs));
    m.insert(
        "uncles".into(),
        Value::Array(
            block
                .uncles
                .iter()
                .map(|u| json::hash_hex(&u.block_hash().unwrap_or_default()))
                .collect(),
        ),
    );
    Value::Object(m)
}
