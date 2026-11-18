//! `--mode native`: read methods answered from a `ChainView`, with the JSON
//! of go-quai v0.57.0 for the same block (`internal/quaiapi`,
//! `quai/filters`). Each handler names the Go function it follows.
//!
//! Not served here (they need a live head or a transaction pool, Phase 2.3
//! and 3.2): `quai_newFilter`, `quai_newBlockFilter`,
//! `quai_newPendingTransactionFilter`, `quai_getFilterChanges`,
//! `quai_getFilterLogs`, `quai_uninstallFilter`, `quai_subscribe`,
//! `quai_sendRawTransaction`. `ChainView::head_events` and
//! `ChainView::submit_transaction` are their hooks.
pub mod args;
pub mod json;

use crate::view::{BlockData, CallError, CallRequest, ChainView, ViewError};
use args::{Address, Args, BlockNumber, BlockRef};
use quai_primitives::{transaction::AccessTuple, Hash};
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// A JSON-RPC error object (`rpc.jsonError`).
#[derive(Clone, Debug, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}
impl RpcError {
    pub fn params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }
    /// A method error without its own code (`errcodeDefault`).
    pub fn go(message: impl Into<String>) -> Self {
        Self {
            code: -32000,
            message: message.into(),
            data: None,
        }
    }
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("the method {method} does not exist/is not available"),
            data: None,
        }
    }
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("code".into(), Value::from(self.code));
        m.insert("message".into(), Value::from(self.message.clone()));
        if let Some(d) = &self.data {
            m.insert("data".into(), d.clone());
        }
        Value::Object(m)
    }
}
/// Code of answers that go-quai would give but this view cannot.
pub const UNAVAILABLE: i64 = -32004;
impl From<ViewError> for RpcError {
    fn from(e: ViewError) -> Self {
        match e {
            ViewError::Go(m) => Self::go(m),
            ViewError::Unavailable(m) => Self {
                code: UNAVAILABLE,
                message: format!("not available in this native view: {m}"),
                data: None,
            },
        }
    }
}
type R<T> = Result<T, RpcError>;

/// The read methods this mode serves.
pub const METHODS: &[&str] = &[
    "quai_chainId",
    "quai_blockNumber",
    "quai_nodeLocation",
    "quai_getProtocolExpansionNumber",
    "net_version",
    "quai_getBlockByNumber",
    "quai_getBlockByHash",
    "quai_getHeaderByNumber",
    "quai_getHeaderByHash",
    "quai_getBlockTransactionCountByNumber",
    "quai_getBlockTransactionCountByHash",
    "quai_getUncleCountByBlockNumber",
    "quai_getUncleCountByBlockHash",
    "quai_getUncleByBlockNumberAndIndex",
    "quai_getUncleByBlockHashAndIndex",
    "quai_getTransactionByHash",
    "quai_getTransactionByBlockNumberAndIndex",
    "quai_getTransactionByBlockHashAndIndex",
    "quai_getRawTransactionByHash",
    "quai_getTransactionReceipt",
    "quai_getLogs",
    "quai_getBalance",
    "quai_getTransactionCount",
    "quai_getCode",
    "quai_getStorageAt",
    "quai_getUTXO",
    "quai_getOutpointsByAddress",
    "quai_getOutPointsByAddressAndRange",
    "quai_call",
    "quai_estimateGas",
];

/// Answer one call. View calls do disk reads and proof-of-work, so they run
/// on a blocking thread.
pub async fn handle(view: Arc<dyn ChainView>, method: &str, params: &Value) -> R<Value> {
    let method = method.to_string();
    let params = params.clone();
    match tokio::task::spawn_blocking(move || dispatch(view.as_ref(), &method, &params)).await {
        Ok(result) => result,
        Err(_) => Err(RpcError::go("method handler crashed")),
    }
}

pub fn dispatch(view: &dyn ChainView, method: &str, params: &Value) -> R<Value> {
    match method {
        "quai_chainId" => {
            Args::new(params, 0)?;
            Ok(json::u64_hex(view.chain_id()))
        }
        "quai_blockNumber" => {
            Args::new(params, 0)?;
            Ok(json::u64_hex(view.head()))
        }
        "quai_nodeLocation" => {
            Args::new(params, 0)?;
            let [r, z] = view.location();
            Ok(json!([json::u64_hex(r as u64), json::u64_hex(z as u64)]))
        }
        "quai_getProtocolExpansionNumber" => {
            Args::new(params, 0)?;
            let head = head_block(view)?;
            Ok(json::u64_hex(
                head.object
                    .header()
                    .wire()
                    .expansion_number
                    .unwrap_or_default(),
            ))
        }
        "net_version" => {
            Args::new(params, 0)?;
            Ok(Value::String(view.chain_id().to_string()))
        }
        "quai_getBlockByNumber" => {
            let a = Args::new(params, 2)?;
            let number = a.get(0, args::block_number)?;
            let full = a.get(1, args::boolean)?;
            match by_number(view, number)? {
                Some(b) => block_json(view, &b, full),
                None => Ok(Value::Null),
            }
        }
        "quai_getBlockByHash" => {
            let a = Args::new(params, 2)?;
            let hash = a.get(0, args::hash)?;
            let full = a.get(1, args::boolean)?;
            match view.block_by_hash(&hash)? {
                Some(b) => block_json(view, &b, full),
                None => Err(RpcError::go("block not found")),
            }
        }
        "quai_getHeaderByNumber" => {
            let a = Args::new(params, 1)?;
            let number = a.get(0, args::block_number)?;
            Ok(by_number(view, number)?.map_or(Value::Null, |b| json::header(&b)))
        }
        "quai_getHeaderByHash" => {
            let a = Args::new(params, 1)?;
            let hash = a.get(0, args::hash)?;
            Ok(view
                .block_by_hash(&hash)?
                .map_or(Value::Null, |b| json::header(&b)))
        }
        "quai_getBlockTransactionCountByNumber" | "quai_getUncleCountByBlockNumber" => {
            let a = Args::new(params, 1)?;
            let number = a.get(0, args::block_number)?;
            Ok(by_number(view, number)?.map_or(Value::Null, |b| count(&b, method)))
        }
        "quai_getBlockTransactionCountByHash" | "quai_getUncleCountByBlockHash" => {
            let a = Args::new(params, 1)?;
            let hash = a.get(0, args::hash)?;
            Ok(view
                .block_by_hash(&hash)?
                .map_or(Value::Null, |b| count(&b, method)))
        }
        "quai_getUncleByBlockNumberAndIndex" => {
            let a = Args::new(params, 2)?;
            let number = a.get(0, args::block_number)?;
            let index = a.get(1, args::uint)?;
            Ok(by_number(view, number)?.map_or(Value::Null, |b| uncle(&b, index)))
        }
        "quai_getUncleByBlockHashAndIndex" => {
            let a = Args::new(params, 2)?;
            let hash = a.get(0, args::hash)?;
            let index = a.get(1, args::uint)?;
            Ok(view
                .block_by_hash(&hash)?
                .map_or(Value::Null, |b| uncle(&b, index)))
        }
        "quai_getTransactionByHash" => {
            let a = Args::new(params, 1)?;
            let hash = a.get(0, args::hash)?;
            let Some((block, index)) = locate(view, &hash)? else {
                return Ok(Value::Null);
            };
            Ok(tx_at(&block, index))
        }
        "quai_getTransactionByBlockNumberAndIndex" => {
            let a = Args::new(params, 2)?;
            let number = a.get(0, args::block_number)?;
            let index = a.get(1, args::uint)?;
            Ok(by_number(view, number)?.map_or(Value::Null, |b| tx_at(&b, index)))
        }
        "quai_getTransactionByBlockHashAndIndex" => {
            // The Go method takes the node location as a third parameter.
            let a = Args::new(params, 3)?;
            let hash = a.get(0, args::hash)?;
            let index = a.get(1, args::uint)?;
            let location = a.get(2, location)?;
            let Some(block) = view.block_by_hash(&hash)? else {
                return Ok(Value::Null);
            };
            if location.len() != 2 {
                return Ok(Value::Null);
            }
            Ok(tx_at(&block, index))
        }
        "quai_getRawTransactionByHash" => {
            let a = Args::new(params, 1)?;
            let hash = a.get(0, args::hash)?;
            // `tx.MarshalBinary()`: the typed RLP form. Unlike the other
            // transaction reads, an unknown hash is an error here.
            match locate(view, &hash)? {
                Some((b, i)) => Ok(json::bytes_hex(
                    &b.object.transactions()[i as usize].typed_rlp(),
                )),
                None => Err(RpcError::go("transaction not found")),
            }
        }
        "quai_getTransactionReceipt" => {
            let a = Args::new(params, 1)?;
            let hash = a.get(0, args::hash)?;
            let Some((block, index)) = locate(view, &hash)? else {
                return Ok(Value::Null);
            };
            match view.receipts(&block) {
                Ok(receipts) => Ok(json::receipt(&block, &receipts, index as usize)),
                // A view without stored receipts answers as go-quai does for
                // a transaction it cannot find.
                Err(ViewError::Unavailable(_)) => Ok(Value::Null),
                Err(e) => Err(e.into()),
            }
        }
        "quai_getLogs" => {
            let a = Args::new(params, 1)?;
            let filter = a.get(0, filter)?;
            logs(view, &filter)
        }
        "quai_getBalance" => {
            let a = Args::new(params, 2)?;
            let (address, original) = a.get(0, args::mixed_address)?;
            let at = a.get(1, args::block_ref)?;
            balance(view, &address, &original, at)
        }
        "quai_getTransactionCount" => {
            let a = Args::new(params, 2)?;
            let (address, original) = a.get(0, args::mixed_address)?;
            let at = a.get(1, args::block_ref)?;
            check_checksum(&address, &original)?;
            if at == BlockRef::Number(BlockNumber::Pending) {
                // GetPoolNonce: without a transaction pool, the head state nonce.
                let head = head_block(view)?;
                if !internal(view, &address) || qi(&address) {
                    return Ok(json::u64_hex(0));
                }
                return Ok(json::u64_hex(view.state(&head)?.nonce(&address)?));
            }
            let Some((_, state)) = state_at(view, at)? else {
                return Ok(Value::Null);
            };
            quai_internal(view, &address)?;
            Ok(json::u64_hex(state.nonce(&address)?))
        }
        "quai_getCode" => {
            let a = Args::new(params, 2)?;
            let (address, original) = a.get(0, args::mixed_address)?;
            let at = a.get(1, args::block_ref)?;
            check_checksum(&address, &original)?;
            let Some((_, state)) = state_at(view, at)? else {
                return Ok(Value::Null);
            };
            if !internal(view, &address) {
                return Err(RpcError::go("Address belongs to other zone"));
            }
            Ok(json::bytes_hex(&state.code(&address)?))
        }
        "quai_getStorageAt" => {
            let a = Args::new(params, 3)?;
            let (address, original) = a.get(0, args::mixed_address)?;
            let key = a.get(1, args::string)?;
            let at = a.get(2, args::block_ref)?;
            check_checksum(&address, &original)?;
            let Some((_, state)) = state_at(view, at)? else {
                return Ok(Value::Null);
            };
            if !internal(view, &address) {
                return Err(RpcError::go("Address belongs to other zone"));
            }
            let key = args::lenient_hash(&key);
            Ok(json::bytes_hex(&state.storage(&address, &key)?))
        }
        "quai_getUTXO" => {
            let a = Args::new(params, 2)?;
            let hash = a.get(0, args::hash)?;
            let index = a.get(1, args::uint64)?;
            Ok(match view.utxo(&hash, index as u16)? {
                None => Value::Null,
                Some(u) => json!({
                    "address": json::checksum(&u.address),
                    "denomination": json::u64_hex(u.denomination as u64),
                    "lock": json::big_hex(&u.lock),
                }),
            })
        }
        "quai_getOutpointsByAddress" => {
            let a = Args::new(params, 1)?;
            let address = a.get(0, args::address)?;
            if !qi(&address) {
                return Err(RpcError::go(format!(
                    "address {} is in Quai ledger scope",
                    json::checksum(&address)
                )));
            }
            Ok(Value::Array(
                view.outpoints_by_address(&address)?
                    .iter()
                    .map(|o| {
                        json!({
                            "txHash": json::hash_hex(&o.tx_hash),
                            "index": json::u64_hex(o.index as u64),
                            "denomination": json::u64_hex(o.denomination as u64),
                            "lock": json::big_hex(&o.lock),
                        })
                    })
                    .collect(),
            ))
        }
        "quai_getOutPointsByAddressAndRange" => {
            let a = Args::new(params, 3)?;
            let address = a.get(0, args::address)?;
            let start = a.get(1, args::uint64)?;
            let end = a.get(2, args::uint64)?;
            if start > end {
                return Err(RpcError::go("start is greater than end"));
            }
            if end > u32::MAX as u64 {
                return Err(RpcError::go(format!(
                    "end exceeds maximum block number {}",
                    u32::MAX
                )));
            }
            if end - start > 1000 {
                return Err(RpcError::go("range is too large, max range is 1000"));
            }
            if !qi(&address) {
                return Err(RpcError::go(format!(
                    "address {} is in Quai ledger scope",
                    json::checksum(&address)
                )));
            }
            let mut out: Map<String, Value> = Map::new();
            for o in view.outpoints_by_address_and_range(&address, start as u32, end as u32)? {
                let entry = out
                    .entry(quai_primitives::encode_hex(&o.tx_hash))
                    .or_insert_with(|| Value::Array(vec![]));
                if let Value::Array(list) = entry {
                    list.push(json!({
                        "index": json::u64_hex(o.index as u64),
                        "denomination": json::u64_hex(o.denomination as u64),
                        "lock": json::big_hex(&o.lock),
                    }));
                }
            }
            Ok(Value::Object(out))
        }
        "quai_call" => {
            let a = Args::new(params, 3)?;
            let call = a.get(0, call_args)?;
            let at = a.get(1, args::block_ref)?;
            if a.optional(2).is_some() {
                // go-quai v0.57.0 cannot decode any StateOverride object.
                return Err(args::invalid(
                    2,
                    args::type_error("object", "quaiapi.StateOverride"),
                ));
            }
            do_call_rpc(view, &call, at)
        }
        "quai_estimateGas" => {
            let a = Args::new(params, 2)?;
            let call = a.get(0, call_args)?;
            let at = a
                .get_optional(1, args::block_ref)?
                .unwrap_or(BlockRef::Number(BlockNumber::Latest));
            estimate_gas(view, call, at)
        }
        _ => Err(RpcError::method_not_found(method)),
    }
}

fn block_json(view: &dyn ChainView, block: &BlockData, full: bool) -> R<Value> {
    let kinds = view.uncle_kinds(block)?;
    let entropy = view.total_entropy(block)?;
    Ok(json::block(block, full, &kinds, &entropy))
}
fn head_block(view: &dyn ChainView) -> R<Arc<BlockData>> {
    view.block_by_number(view.head())?
        .ok_or_else(|| RpcError::go("head block missing"))
}
/// `BlockByNumber`/`HeaderByNumber`: no pending block without a miner.
fn by_number(view: &dyn ChainView, n: BlockNumber) -> R<Option<Arc<BlockData>>> {
    match n {
        BlockNumber::Pending => Ok(None),
        BlockNumber::Latest => Ok(Some(head_block(view)?)),
        BlockNumber::Number(n) if n > view.head() => Ok(None),
        BlockNumber::Number(n) => Ok(view.block_by_number(n)?),
    }
}
fn count(block: &BlockData, method: &str) -> Value {
    json::u64_hex(if method.contains("Uncle") {
        block.uncles.len() as u64
    } else {
        block.object.transactions().len() as u64
    })
}
fn uncle(block: &BlockData, index: u64) -> Value {
    block
        .uncles
        .get(index as usize)
        .map_or(Value::Null, json::wo_header)
}
fn tx_at(block: &BlockData, index: u64) -> Value {
    let Some(tx) = block.object.transactions().get(index as usize) else {
        return Value::Null;
    };
    json::transaction(
        tx,
        &block.tx_hashes[index as usize],
        Some(json::Position {
            block_hash: block.hash,
            block_number: block.number,
            index,
        }),
    )
}
/// `rawdb.ReadTransaction`.
fn locate(view: &dyn ChainView, hash: &Hash) -> R<Option<(Arc<BlockData>, u64)>> {
    let Some(p) = view.transaction(hash)? else {
        return Ok(None);
    };
    let Some(block) = view.block_by_number(p.block_number)? else {
        return Ok(None);
    };
    if block.hash != p.block_hash {
        return Ok(None);
    }
    Ok(Some((block, p.index)))
}
/// A `common.Location` parameter: a byte array or base64 text.
fn location(v: &Value) -> Result<Vec<u8>, String> {
    match v {
        Value::Null => Ok(vec![]),
        Value::Array(items) => items
            .iter()
            .map(|i| {
                i.as_u64()
                    .filter(|n| *n < 256)
                    .map(|n| n as u8)
                    .ok_or_else(|| args::type_error("number", "uint8"))
            })
            .collect(),
        Value::String(s) => base64(s),
        Value::Bool(_) => Err(args::type_error("bool", "common.Location")),
        Value::Number(_) => Err(args::type_error("number", "common.Location")),
        Value::Object(_) => Err(args::type_error("object", "common.Location")),
    }
}
/// `encoding/base64.StdEncoding.DecodeString` with Go's error offsets.
fn base64(s: &str) -> Result<Vec<u8>, String> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        let mut quantum = [0u32; 4];
        let mut n = 0;
        while n < 4 {
            if i + n >= bytes.len() {
                return Err(format!("illegal base64 data at input byte {}", i));
            }
            let c = bytes[i + n];
            if c == b'=' {
                if n < 2 || (n == 2 && bytes.get(i + 3) != Some(&b'=')) {
                    return Err(format!("illegal base64 data at input byte {}", i + n));
                }
                break;
            }
            quantum[n] =
                value(c).ok_or_else(|| format!("illegal base64 data at input byte {}", i + n))?;
            n += 1;
        }
        let v = quantum[0] << 18 | quantum[1] << 12 | quantum[2] << 6 | quantum[3];
        out.push((v >> 16) as u8);
        if n > 2 {
            out.push((v >> 8) as u8);
        }
        if n > 3 {
            out.push(v as u8);
        }
        i += 4;
        if n < 4 {
            if i < bytes.len() {
                return Err(format!("illegal base64 data at input byte {}", i));
            }
            break;
        }
    }
    Ok(out)
}

// --- addresses -------------------------------------------------------------

fn qi(address: &Address) -> bool {
    address[1] > 127
}
/// `common.IsInChainScope`.
fn internal(view: &dyn ChainView, address: &Address) -> bool {
    let [r, z] = view.location();
    *address == [0; 20] || address[0] == (r << 4 | z)
}
/// `MixedcaseAddress.ValidChecksum`.
fn check_checksum(address: &Address, original: &str) -> R<()> {
    if json::checksum(address) != original {
        return Err(RpcError::go("address has invalid checksum"));
    }
    Ok(())
}
/// `Address.InternalAndQuaiAddress`.
fn quai_internal(view: &dyn ChainView, address: &Address) -> R<()> {
    if qi(address) {
        return Err(RpcError::go(format!(
            "expected Quai address, but found Qi address: {}",
            json::checksum(address)
        )));
    }
    if !internal(view, address) {
        return Err(RpcError::go("Address belongs to other zone"));
    }
    Ok(())
}

// --- state -----------------------------------------------------------------

/// A block and the account state after it.
type BlockState = (Arc<BlockData>, Arc<dyn crate::view::AccountState>);
/// `StateAndHeaderByNumberOrHash`. `Ok(None)` is Go's (nil, nil).
fn state_at(view: &dyn ChainView, at: BlockRef) -> R<Option<BlockState>> {
    let block = resolve(view, at)?;
    let state = view.state(&block)?;
    Ok(Some((block, state)))
}
/// The header of `HeaderByNumberOrHash`-style lookups for state reads.
fn resolve(view: &dyn ChainView, at: BlockRef) -> R<Arc<BlockData>> {
    match at {
        BlockRef::Number(BlockNumber::Pending) => {
            // `state.StateDB{}` of the pending block: Go's reads crash.
            Err(RpcError::go("method handler crashed"))
        }
        BlockRef::Number(n) => by_number(view, n)?.ok_or_else(|| RpcError::go("header not found")),
        BlockRef::Hash { hash, .. } => view
            .block_by_hash(&hash)?
            .ok_or_else(|| RpcError::go("header for hash not found")),
        BlockRef::Neither => Err(RpcError::go(
            "invalid arguments; neither block nor hash specified",
        )),
    }
}
/// `types.Denominations` in Qits.
const DENOMINATIONS: [u64; 15] = [
    1, 5, 10, 50, 100, 500, 1000, 5000, 10000, 20000, 100000, 1000000, 10000000, 100000000,
    1000000000,
];
/// `GetBalance` (quai_api.go).
fn balance(view: &dyn ChainView, address: &Address, original: &str, at: BlockRef) -> R<Value> {
    check_checksum(address, original)?;
    let Some((block, state)) = state_at(view, at)? else {
        return Ok(Value::Null);
    };
    if qi(address) {
        if !internal(view, address) {
            return Err(RpcError::go("Address belongs to other zone"));
        }
        let head = head_block(view)?;
        if block.hash != head.hash {
            return Err(RpcError::go(
                "qi balance query is only supported for the current block",
            ));
        }
        let mut total: u128 = 0;
        for o in view.outpoints_by_address(address)? {
            let lock = o
                .lock
                .iter()
                .fold(0u128, |a, b| a.saturating_mul(256) + *b as u128);
            if !o.lock.is_empty() && (head.number as u128) < lock {
                continue;
            }
            total += *DENOMINATIONS.get(o.denomination as usize).unwrap_or(&0) as u128;
        }
        return Ok(json::big_hex(&total.to_be_bytes()));
    }
    if !internal(view, address) {
        return Err(RpcError::go("Address belongs to other zone"));
    }
    Ok(json::big_hex(&state.balance(address)?))
}

// --- logs ------------------------------------------------------------------

/// `filters.FilterCriteria`.
pub struct Filter {
    block_hash: Option<Hash>,
    from: Option<BlockNumber>,
    to: Option<BlockNumber>,
    addresses: Vec<Address>,
    topics: Vec<Vec<Hash>>,
}
/// `hexutil.Decode` then a length check (`decodeAddress`, `decodeTopic`).
fn decode_fixed(s: &str, want: usize, what: &str) -> Result<Vec<u8>, String> {
    if s.is_empty() {
        return Err("empty hex string".into());
    }
    if !(s.starts_with("0x") || s.starts_with("0X")) {
        return Err("hex string without 0x prefix".into());
    }
    let raw = &s[2..];
    if !raw.len().is_multiple_of(2) {
        return Err("hex string of odd length".into());
    }
    let b = quai_primitives::decode_hex(&format!("0x{raw}")).map_err(|_| "invalid hex string")?;
    if b.len() != want {
        return Err(format!(
            "hex has invalid length {} after decoding; expected {want} for {what}",
            b.len()
        ));
    }
    Ok(b)
}
fn filter(v: &Value) -> Result<Filter, String> {
    let Value::Object(m) = v else {
        return Err(args::type_error(
            match v {
                Value::String(_) => "string",
                Value::Number(_) => "number",
                Value::Bool(_) => "bool",
                Value::Array(_) => "array",
                _ => "null",
            },
            "filters.input",
        ));
    };
    let get = |k: &str| m.get(k).filter(|v| !v.is_null());
    let block_hash = get("blockHash")
        .map(|v| {
            args::hash(v).map_err(|e| {
                e.replace(
                    "into Go value of type",
                    "into Go struct field input.blockHash of type",
                )
            })
        })
        .transpose()?;
    let from = get("fromBlock").map(args::block_number).transpose()?;
    let to = get("toBlock").map(args::block_number).transpose()?;
    if block_hash.is_some() && (from.is_some() || to.is_some()) {
        return Err(
            "cannot specify both BlockHash and FromBlock/ToBlock, choose one or the other".into(),
        );
    }
    let mut addresses = vec![];
    match get("address") {
        None => {}
        Some(Value::Array(list)) => {
            for (i, a) in list.iter().enumerate() {
                let Value::String(s) = a else {
                    return Err(format!("non-string address at index {i}"));
                };
                let b = decode_fixed(s, 20, "address")
                    .map_err(|e| format!("invalid address at index {i}: {e}"))?;
                addresses.push(json::address20(&b));
            }
        }
        Some(Value::String(s)) => {
            let b = decode_fixed(s, 20, "address").map_err(|e| format!("invalid address: {e}"))?;
            addresses.push(json::address20(&b));
        }
        Some(_) => return Err("invalid addresses in query".into()),
    }
    let mut topics = vec![];
    match get("topics") {
        None => {}
        Some(Value::Array(list)) => {
            for t in list {
                topics.push(match t {
                    Value::Null => vec![],
                    Value::String(s) => vec![topic(s)?],
                    Value::Array(alternatives) => {
                        let mut set = vec![];
                        for a in alternatives {
                            match a {
                                Value::Null => {
                                    set.clear();
                                    break;
                                }
                                Value::String(s) => set.push(topic(s)?),
                                _ => return Err("invalid topic(s)".into()),
                            }
                        }
                        set
                    }
                    _ => return Err("invalid topic(s)".into()),
                });
            }
        }
        Some(other) => {
            return Err(format!(
            "json: cannot unmarshal {} into Go struct field input.topics of type []interface {{}}",
            match other {
                Value::String(_) => "string",
                Value::Number(_) => "number",
                Value::Bool(_) => "bool",
                _ => "object",
            }
        ))
        }
    }
    Ok(Filter {
        block_hash,
        from,
        to,
        addresses,
        topics,
    })
}
fn topic(s: &str) -> Result<Hash, String> {
    let b = decode_fixed(s, 32, "topic")?;
    Ok(b.try_into().unwrap_or([0; 32]))
}
/// `filterLogs`.
fn matches(f: &Filter, log: &crate::view::LogData) -> bool {
    if !f.addresses.is_empty() && !f.addresses.contains(&log.address) {
        return false;
    }
    if f.topics.len() > log.topics.len() {
        return false;
    }
    f.topics
        .iter()
        .zip(&log.topics)
        .all(|(set, t)| set.is_empty() || set.contains(t))
}
fn block_logs(view: &dyn ChainView, f: &Filter, block: &BlockData, out: &mut Vec<Value>) -> R<()> {
    if block.tx_hashes.is_empty() {
        return Ok(());
    }
    let receipts = view.receipts(block)?;
    let mut index = 0;
    for (i, r) in receipts.iter().enumerate() {
        for l in &r.logs {
            if matches(f, l) {
                out.push(json::log(l, block, i, index));
            }
            index += 1;
        }
    }
    Ok(())
}
const MAX_FILTER_RANGE: i64 = 10_000;
/// `PublicFilterAPI.GetLogs` and `Filter.Logs`, in block order.
fn logs(view: &dyn ChainView, f: &Filter) -> R<Value> {
    let mut out = vec![];
    if let Some(hash) = f.block_hash {
        let Some(block) = view.block_by_hash(&hash)? else {
            return Err(RpcError::go("unknown block"));
        };
        block_logs(view, f, &block, &mut out)?;
        return Ok(Value::Array(out));
    }
    let begin = f.from.map_or(-1, BlockNumber::int64);
    let end = f.to.map_or(-1, BlockNumber::int64);
    if end != -1 && begin > end {
        return Err(RpcError::go(
            "fromBlock must be less than or equal to toBlock",
        ));
    }
    if end != -1 && end - begin > MAX_FILTER_RANGE {
        return Err(RpcError::go(format!(
            "filter range must be less than or equal to {MAX_FILTER_RANGE}"
        )));
    }
    let head = view.head();
    let begin = if begin >= 0 { begin as u64 } else { head };
    let end = if end >= 0 { end as u64 } else { head };
    if begin > end {
        return Ok(Value::Array(out));
    }
    if end - begin > MAX_FILTER_RANGE as u64 {
        return Err(RpcError::go(format!(
            "filter range exceeds maximum limit of {MAX_FILTER_RANGE} blocks"
        )));
    }
    for n in begin..=end.min(head) {
        if !view.may_have_logs(n)? {
            continue;
        }
        if let Some(block) = view.block_by_number(n)? {
            block_logs(view, f, &block, &mut out)?;
        }
    }
    Ok(Value::Array(out))
}

// --- calls -----------------------------------------------------------------

/// `TransactionArgs`.
#[derive(Clone, Debug, Default)]
pub struct CallArgs {
    from: Option<Address>,
    to: Option<Address>,
    gas: Option<u64>,
    gas_price: Option<[u8; 32]>,
    value: Option<[u8; 32]>,
    data: Option<Vec<u8>>,
    input: Option<Vec<u8>>,
    access_list: Option<Vec<AccessTuple>>,
    tx_type: u8,
}
impl CallArgs {
    fn data(&self) -> Vec<u8> {
        self.input
            .clone()
            .or_else(|| self.data.clone())
            .unwrap_or_default()
    }
    /// `args.from`: the zero address when unset.
    fn from(&self) -> Address {
        self.from.unwrap_or([0; 20])
    }
}
fn in_field<T>(r: Result<T, String>, field: &str) -> Result<T, String> {
    r.map_err(|e| {
        e.replace(
            "into Go value of type",
            &format!("into Go struct field TransactionArgs.{field} of type"),
        )
    })
}
fn call_args(v: &Value) -> Result<CallArgs, String> {
    let Value::Object(m) = v else {
        return Err(args::type_error(
            match v {
                Value::String(_) => "string",
                Value::Number(_) => "number",
                Value::Bool(_) => "bool",
                Value::Array(_) => "array",
                _ => "null",
            },
            "quaiapi.TransactionArgs",
        ));
    };
    let get = |k: &str| m.get(k).filter(|v| !v.is_null());
    let mut a = CallArgs {
        from: get("from")
            .map(|v| in_field(args::address(v), "from"))
            .transpose()?,
        to: get("to")
            .map(|v| in_field(args::address(v), "to"))
            .transpose()?,
        gas: get("gas")
            .map(|v| in_field(args::uint64(v), "gas"))
            .transpose()?,
        gas_price: get("gasPrice")
            .map(|v| in_field(args::big(v), "gasPrice"))
            .transpose()?,
        value: get("value")
            .map(|v| in_field(args::big(v), "value"))
            .transpose()?,
        data: get("data")
            .map(|v| in_field(args::bytes(v), "data"))
            .transpose()?,
        input: get("input")
            .map(|v| in_field(args::bytes(v), "input"))
            .transpose()?,
        access_list: None,
        tx_type: 0,
    };
    if let Some(v) = get("accessList") {
        let Value::Array(list) = v else {
            return Err("json: cannot unmarshal object into Go struct field TransactionArgs.accessList of type types.AccessList".into());
        };
        let mut out = vec![];
        for t in list {
            let address = t
                .get("address")
                .filter(|v| !v.is_null())
                .ok_or("missing required field 'address' for AccessTuple")?;
            let keys = t
                .get("storageKeys")
                .filter(|v| !v.is_null())
                .ok_or("missing required field 'storageKeys' for AccessTuple")?;
            let address = args::address(address)?;
            let keys = keys
                .as_array()
                .ok_or("json: cannot unmarshal object into Go struct field AccessTuple.storageKeys of type []common.Hash")?
                .iter()
                .map(args::hash)
                .collect::<Result<Vec<_>, _>>()?;
            out.push(AccessTuple { address, keys });
        }
        a.access_list = Some(out);
    }
    if let Some(v) = get("txType") {
        a.tx_type = match v.as_u64() {
            Some(n) if n < 256 => n as u8,
            _ => {
                return Err(format!(
                    "json: cannot unmarshal {} into Go struct field TransactionArgs.txType of type uint8",
                    if v.is_number() {
                        format!("number {v}")
                    } else if v.is_string() {
                        "string".into()
                    } else {
                        "object".into()
                    }
                ))
            }
        };
    }
    Ok(a)
}
/// The default and maximum call gas: `params.GasCeil`, go-quai's default
/// `--rpc.gascap`.
pub const GAS_CAP: u64 = 50_000_000;
const TX_GAS: u64 = 21_000;

/// What `DoCall` returns: Go's `(*ExecutionResult, error)`.
enum Called {
    Done(crate::view::CallResult),
    Failed(RpcError),
    /// `core.ErrIntrinsicGas` (estimateGas raises the limit).
    Intrinsic(RpcError),
}
/// `DoCall` without overrides.
fn do_call(view: &dyn ChainView, args: &CallArgs, block: &BlockData) -> Called {
    let from = args.from();
    if let Err(e) = quai_internal(view, &from) {
        return Called::Failed(e);
    }
    let gas = args.gas.unwrap_or(GAS_CAP).min(GAS_CAP);
    let request = CallRequest {
        from,
        to: args.to,
        gas,
        value: args.value.unwrap_or([0; 32]),
        data: args.data(),
        access_list: args.access_list.clone().unwrap_or_default(),
    };
    if block.number == 0 {
        return Called::Failed(RpcError::go(format!(
            "parent block not found, parenthash: {}",
            quai_primitives::encode_hex(&[0; 32])
        )));
    }
    match view.call(block, &request) {
        Ok(result) => Called::Done(result),
        Err(CallError::Message {
            text,
            intrinsic_gas,
        }) => {
            let e = RpcError::go(format!("err: {text} (supplied gas {gas})"));
            if intrinsic_gas {
                Called::Intrinsic(e)
            } else {
                Called::Failed(e)
            }
        }
        Err(CallError::View(e)) => Called::Failed(e.into()),
    }
}
/// `abi.UnpackRevert`: the `Error(string)` reason.
fn revert_reason(data: &[u8]) -> Option<String> {
    if data.len() < 4 || data[..4] != [0x08, 0xc3, 0x79, 0xa0] {
        return None;
    }
    let body = &data[4..];
    let word = |at: usize| -> Option<usize> {
        let w = body.get(at..at + 32)?;
        if w[..24].iter().any(|b| *b != 0) {
            return None;
        }
        Some(u64::from_be_bytes(w[24..].try_into().ok()?) as usize)
    };
    let offset = word(0)?;
    let length = word(offset)?;
    let start = offset.checked_add(32)?;
    let bytes = body.get(start..start.checked_add(length)?)?;
    String::from_utf8(bytes.to_vec()).ok()
}
/// `newRevertError`.
fn revert_error(data: &[u8]) -> RpcError {
    RpcError {
        code: 3,
        message: match revert_reason(data) {
            Some(reason) => format!("execution reverted: {reason}"),
            None => "execution reverted".into(),
        },
        data: Some(json::bytes_hex(data)),
    }
}
/// `PublicBlockChainQuaiAPI.Call`.
fn do_call_rpc(view: &dyn ChainView, args: &CallArgs, at: BlockRef) -> R<Value> {
    let block = resolve(view, at)?;
    view.state(&block)?;
    match do_call(view, args, &block) {
        Called::Done(r) => {
            if r.reverted && !r.return_data.is_empty() {
                return Err(revert_error(&r.return_data));
            }
            match r.error {
                Some(e) => Err(RpcError::go(e)),
                None => Ok(json::bytes_hex(&r.return_data)),
            }
        }
        Called::Failed(e) | Called::Intrinsic(e) => Err(e),
    }
}
/// `PublicBlockChainQuaiAPI.EstimateGas` and `DoEstimateGas`.
fn estimate_gas(view: &dyn ChainView, mut args: CallArgs, at: BlockRef) -> R<Value> {
    let from_quai = !qi(&args.from());
    if from_quai && args.to.is_some_and(|t| qi(&t)) {
        return Err(RpcError::from(ViewError::Unavailable(
            "conversion estimates (Quai to Qi)".into(),
        )));
    }
    match args.tx_type {
        0 => {}
        2 => {
            return Err(RpcError::from(ViewError::Unavailable(
                "Qi transaction estimates".into(),
            )))
        }
        _ => return Err(RpcError::go("unsupported tx type")),
    }
    // HeaderByNumberOrHash for the gas ceiling, and the state for the calls.
    let block = match at {
        BlockRef::Number(BlockNumber::Pending) => return Err(RpcError::go("block not found")),
        BlockRef::Number(n) => {
            by_number(view, n)?.ok_or_else(|| RpcError::go("block not found"))?
        }
        BlockRef::Hash { hash, .. } => view
            .block_by_hash(&hash)?
            .ok_or_else(|| RpcError::go("header for hash not found"))?,
        BlockRef::Neither => {
            return Err(RpcError::go(
                "invalid arguments; neither block nor hash specified",
            ))
        }
    };
    let mut hi = match args.gas {
        Some(g) if g >= TX_GAS => g,
        _ => match block.object.header().wire().gas_limit.unwrap_or_default() {
            0 => GAS_CAP,
            limit => limit,
        },
    };
    let fee_cap = args.gas_price.unwrap_or([0; 32]);
    if fee_cap != [0; 32] {
        let state = view.state(&block)?;
        let from = args.from();
        quai_internal(view, &from)?;
        let balance = u256(state.balance(&from)?);
        let value = u256(args.value.unwrap_or([0; 32]));
        let mut available = balance;
        if value != 0 {
            if value >= available {
                return Err(RpcError::go("insufficient funds for transfer"));
            }
            available -= value;
        }
        let allowance = available / u256(fee_cap).max(1);
        if allowance <= u64::MAX as u128 && hi > allowance as u64 {
            hi = allowance as u64;
        }
    }
    hi = hi.min(GAS_CAP);
    let cap = hi;
    // An access list for contract calls with data needs an access-list
    // tracer; a call to an account without code gets the empty list.
    if args.access_list.is_none() && args.to.is_some() && !args.data().is_empty() {
        let to = args.to.unwrap_or_default();
        let has_code = internal(view, &to)
            && view
                .state(&block)
                .ok()
                .and_then(|s| s.code(&to).ok())
                .is_some_and(|c| !c.is_empty());
        if has_code {
            return Err(RpcError::from(ViewError::Unavailable(
                "gas estimates for contract calls without an access list (needs an access-list tracer)"
                    .into(),
            )));
        }
        args.access_list = Some(vec![]);
    }
    let executable = |gas: u64| -> R<(bool, Option<crate::view::CallResult>)> {
        let mut a = args.clone();
        a.gas = Some(gas);
        match do_call(view, &a, &block) {
            Called::Done(r) => Ok((r.error.is_some() || r.reverted, Some(r))),
            Called::Intrinsic(_) => Ok((true, None)),
            Called::Failed(e) => Err(e),
        }
    };
    let mut lo = TX_GAS - 1;
    while lo + 1 < hi {
        let mid = (hi + lo) / 2;
        if executable(mid)?.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    if hi == cap {
        let (failed, result) = executable(hi)?;
        if failed {
            if let Some(r) = result.filter(|r| r.error.as_deref() != Some("out of gas")) {
                if r.reverted && !r.return_data.is_empty() {
                    return Err(revert_error(&r.return_data));
                }
                return Err(RpcError::go(
                    r.error.unwrap_or_else(|| "execution reverted".into()),
                ));
            }
            return Err(RpcError::go(format!(
                "gas required exceeds allowance ({cap})"
            )));
        }
    }
    Ok(json::u64_hex(hi + hi / 3))
}
fn u256(b: [u8; 32]) -> u128 {
    if b[..16].iter().any(|v| *v != 0) {
        return u128::MAX;
    }
    u128::from_be_bytes(b[16..].try_into().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64_errors_match_go() {
        assert_eq!(
            base64("0x0000").unwrap_err(),
            "illegal base64 data at input byte 4"
        );
        assert_eq!(base64("AAA=").unwrap(), vec![0, 0]);
        assert_eq!(base64("AAAA").unwrap(), vec![0, 0, 0]);
        assert!(base64("A!AA").is_err());
    }
    #[test]
    fn revert_reasons() {
        let mut data = vec![0x08, 0xc3, 0x79, 0xa0];
        data.extend([0u8; 31]);
        data.push(32);
        data.extend([0u8; 31]);
        data.push(2);
        data.extend(b"no");
        data.extend([0u8; 30]);
        assert_eq!(revert_reason(&data).as_deref(), Some("no"));
        assert_eq!(revert_error(&data).message, "execution reverted: no");
        assert_eq!(revert_error(&[1, 2]).message, "execution reverted");
    }
}
