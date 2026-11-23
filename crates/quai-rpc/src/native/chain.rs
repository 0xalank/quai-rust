//! The other read methods that a default go-quai v0.57.0 zone node serves
//! (docs/rpc-parity.md): `quai_clientVersion`, `rpc_modules`,
//! `net_peerCount`, `net_peerCountByDirection`,
//! `quai_getRawTransactionByBlockNumberAndIndex`,
//! `quai_getRawTransactionByBlockHashAndIndex`,
//! `quai_getBlockOrCandidateByHash`, `quai_getLatestUTXOSetSize`,
//! `quai_getSupplyAnalyticsForBlock`, `quai_rewardAnalytics`,
//! `quai_getKQuaiAndUpdateBit`, `quai_getContractSize`,
//! `quai_getLockupsForContractAndMiner`, `quai_getProof` and
//! `txpool_getRollingFeeInfo` (internal/quaiapi/quai_api.go and api.go).
//! The quotes `quai_baseFee`, `quai_suggestFinalityDepth` and
//! `quai_hashesPerQits` live in `qi.rs`.
//!
//! Hash lookups follow `GetBlockByHash`/`HeaderByHash`: any block the node
//! stores, canonical or not (`ChainView::block_or_candidate`).
use super::args::{self, Address, Args, BlockNumber, BlockRef};
use super::{block_json, by_number, head_block, internal, json, qi, quai_internal, RpcError};
use crate::view::{AccountState, BlockData, ChainView};
use quai_primitives::{decode_hex, Hash};
use serde_json::{Map, Value};
use std::sync::Arc;

type R<T> = Result<T, RpcError>;

/// The methods of this module.
pub const METHODS: &[&str] = &[
    "quai_clientVersion",
    "rpc_modules",
    "net_peerCount",
    "net_peerCountByDirection",
    "quai_getRawTransactionByBlockNumberAndIndex",
    "quai_getRawTransactionByBlockHashAndIndex",
    "quai_getBlockOrCandidateByHash",
    "quai_getLatestUTXOSetSize",
    "quai_getSupplyAnalyticsForBlock",
    "quai_rewardAnalytics",
    "quai_getKQuaiAndUpdateBit",
    "quai_getContractSize",
    "quai_getLockupsForContractAndMiner",
    "quai_getProof",
    "txpool_getRollingFeeInfo",
];

/// This node's answer to `quai_clientVersion` (go-quai answers
/// `go-quai/<VERSION>-<commit>`).
pub fn client_version() -> String {
    format!("quai-rust/v{}", crate::VERSION)
}

/// `quai_getSupplyAnalyticsForBlock` subtracts this address's balance.
const BURN_ADDRESS: Address = [
    0x00, 0x50, 0xaf, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
/// `params.ExchangeRate`: what `GetKQuaiAndUpdateBit` gives in v0.57.0.
const EXCHANGE_RATE: &str = "221077819000000000";

fn crashed() -> RpcError {
    RpcError::go("method handler crashed")
}

pub fn dispatch(view: &dyn ChainView, method: &str, params: &Value) -> Option<R<Value>> {
    Some(match method {
        "quai_clientVersion" => Args::new(params, 0).map(|_| Value::String(client_version())),
        "rpc_modules" => Args::new(params, 0).map(|_| modules()),
        "net_peerCount" => Args::new(params, 0).map(|_| json::u64_hex(view.peer_counts().peers)),
        "net_peerCountByDirection" => Args::new(params, 0).map(|_| {
            let p = view.peer_counts();
            json!({"incoming": json::u64_hex(p.incoming), "outgoing": json::u64_hex(p.outgoing)})
        }),
        "quai_getRawTransactionByBlockNumberAndIndex" => (|| {
            let a = Args::new(params, 2)?;
            let number = a.get(0, args::block_number)?;
            let index = a.get(1, args::uint)?;
            Ok(raw_tx(by_number(view, number)?.as_deref(), index))
        })(),
        "quai_getRawTransactionByBlockHashAndIndex" => (|| {
            let a = Args::new(params, 2)?;
            let hash = a.get(0, args::hash)?;
            let index = a.get(1, args::uint)?;
            Ok(raw_tx(view.block_or_candidate(&hash)?.as_deref(), index))
        })(),
        "quai_getBlockOrCandidateByHash" => (|| {
            let a = Args::new(params, 2)?;
            let hash = a.get(0, args::hash)?;
            let full = a.get(1, args::boolean)?;
            match view.block_or_candidate(&hash)? {
                Some(b) => block_json(view, &b, full),
                None => Ok(Value::Null),
            }
        })(),
        "quai_getLatestUTXOSetSize" => (|| {
            Args::new(params, 0)?;
            let head = head_block(view)?;
            Ok(json::u64_hex(view.utxo_set_size(&head)?))
        })(),
        "quai_getSupplyAnalyticsForBlock" => supply_analytics(view, params),
        "quai_rewardAnalytics" => Args::new(params, 0).and_then(|_| {
            let head = head_block(view)?;
            if let Some(s) = view.supply_analytics(&head.hash)? {
                use crypto_bigint::Encoding as _;
                let total = crypto_bigint::U256::from_be_bytes(s.quai_total);
                let mut dec = String::new();
                if total == crypto_bigint::U256::ZERO {
                    dec.push('0');
                } else {
                    let mut n = total;
                    let ten = crypto_bigint::U256::from_u64(10);
                    let mut digits = Vec::new();
                    while n != crypto_bigint::U256::ZERO {
                        let q = n.wrapping_div(&ten);
                        let r = n.wrapping_sub(&q.wrapping_mul(&ten));
                        digits.push((b'0' + crypto_bigint::Encoding::to_be_bytes(&r)[31]) as char);
                        n = q;
                    }
                    digits.reverse();
                    dec.extend(digits);
                }
                return Ok(json!({"cumulativeQuaiMined": dec}));
            }
            Ok(json!({"cumulativeQuaiMined": "0"}))
        }),
        "quai_getKQuaiAndUpdateBit" => (|| {
            let a = Args::new(params, 1)?;
            let hash = a.get(0, args::hash)?;
            if view.location() != [0, 0] {
                return Err(RpcError::go(
                    "cannot call GetKQuaiAndUpdateBit in any other chain than cyprus-1",
                ));
            }
            if view.block_or_candidate(&hash)?.is_none() {
                return Err(RpcError::go("sub not synced to dom"));
            }
            Ok(json!({"kQuai": EXCHANGE_RATE, "updateBit": 1}))
        })(),
        "quai_getContractSize" => contract_size(view, params),
        "quai_getLockupsForContractAndMiner" => lockups(view, params),
        "quai_getProof" => proof(view, params),
        "txpool_getRollingFeeInfo" => Args::new(params, 0).and_then(|_| {
            let Some(fees) = view.rolling_fee_info() else {
                return Err(RpcError::go(
                    "no transactions processed to calculate min, max, or avg",
                ));
            };
            Ok(Value::Array(
                fees.iter().map(|f| json::big_hex(f)).collect(),
            ))
        }),
        _ => return None,
    })
}

/// `RPCService.Modules`: every namespace this node serves, at "1.0".
fn modules() -> Value {
    let mut m = Map::new();
    for method in super::METHODS {
        if let Some((namespace, _)) = method.split_once('_') {
            m.insert(namespace.into(), Value::String("1.0".into()));
        }
    }
    m.insert("rpc".into(), Value::String("1.0".into()));
    Value::Object(m)
}

/// `newRPCRawTransactionFromBlockIndex`: `MarshalBinary` of transaction
/// `index`; a nil `hexutil.Bytes` ("0x") without the block or the index.
fn raw_tx(block: Option<&BlockData>, index: u64) -> Value {
    let tx = block.and_then(|b| b.object.transactions().get(index as usize));
    json::bytes_hex(&tx.map(|t| t.typed_rlp()).unwrap_or_default())
}

/// What `StateAndHeaderByNumberOrHash` gives: a block and its state, or the
/// pending block's empty `state.StateDB{}`, whose reads crash.
pub(super) enum StateRef {
    Pending,
    At(Arc<BlockData>, Arc<dyn AccountState>),
}
impl StateRef {
    pub(super) fn state(&self) -> R<&dyn AccountState> {
        match self {
            Self::Pending => Err(crashed()),
            Self::At(_, s) => Ok(s.as_ref()),
        }
    }
}
/// `StateAndHeaderByNumberOrHash` (quai/api_backend.go): a hash finds any
/// stored block.
pub(super) fn state_ref(view: &dyn ChainView, at: BlockRef) -> R<StateRef> {
    let block = match at {
        BlockRef::Number(BlockNumber::Pending) => return Ok(StateRef::Pending),
        BlockRef::Number(n) => {
            by_number(view, n)?.ok_or_else(|| RpcError::go("header not found"))?
        }
        BlockRef::Hash { hash, canonical } => {
            let block = view
                .block_or_candidate(&hash)?
                .ok_or_else(|| RpcError::go("header for hash not found"))?;
            if canonical && view.canonical_hash(block.number)? != Some(hash) {
                return Err(RpcError::go("hash is not currently canonical"));
            }
            block
        }
        BlockRef::Neither => {
            return Err(RpcError::go(
                "invalid arguments; neither block nor hash specified",
            ))
        }
    };
    let state = view.state(&block)?;
    Ok(StateRef::At(block, state))
}

/// `HeaderByNumberOrHash`: None is Go's (nil, nil).
fn header_ref(view: &dyn ChainView, at: BlockRef) -> R<Option<Arc<BlockData>>> {
    match at {
        BlockRef::Number(n) => by_number(view, n),
        BlockRef::Hash { hash, canonical } => {
            let block = view
                .block_or_candidate(&hash)?
                .ok_or_else(|| RpcError::go("header for hash not found"))?;
            if canonical && view.canonical_hash(block.number)? != Some(hash) {
                return Err(RpcError::go("hash is not currently canonical"));
            }
            Ok(Some(block))
        }
        BlockRef::Neither => Err(RpcError::go(
            "invalid arguments; neither block nor hash specified",
        )),
    }
}

/// `GetSupplyAnalyticsForBlock`. go-quai writes the supply index only with
/// `--node.index-address-utxos`, which is off by default: every figure
/// reads zero, and the Quai total is zero less the burn address's balance
/// when `GetBalance` of that block succeeds.
fn supply_analytics(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 1)?;
    let at = a.get(0, args::block_ref)?;
    let Some(block) = header_ref(view, at)? else {
        return Ok(Value::Null);
    };
    // An error of GetBalance leaves the total at zero; one this view cannot
    // answer is not Go's.
    let burned = match state_ref(view, at).and_then(|s| {
        quai_internal(view, &BURN_ADDRESS)?;
        Ok(s.state()?.balance(&BURN_ADDRESS)?)
    }) {
        Ok(b) => b,
        Err(e) if e.code == super::UNAVAILABLE => return Err(e),
        Err(_) => [0; 32],
    };
    if let Some(indexed) = view.supply_analytics(&block.hash)? {
        use crypto_bigint::{Encoding as _, U256};
        let total_u = U256::from_be_bytes(indexed.quai_total);
        let burned_u = U256::from_be_bytes(burned);
        let total = if total_u >= burned_u {
            json::big_hex(&total_u.wrapping_sub(&burned_u).to_be_bytes())
        } else {
            let neg = burned_u.wrapping_sub(&total_u).to_be_bytes();
            match json::big_hex(&neg) {
                Value::String(s) if s != "0x0" => Value::String(format!("-{s}")),
                v => v,
            }
        };
        return Ok(json!({
            "quaiSupplyAdded": json::big_hex(&indexed.quai_added),
            "quaiSupplyRemoved": json::big_hex(&indexed.quai_removed),
            "quaiSupplyTotal": total,
            "qiSupplyAdded": json::big_hex(&indexed.qi_added),
            "qiSupplyRemoved": json::big_hex(&indexed.qi_removed),
            "qiSupplyTotal": json::big_hex(&indexed.qi_total),
        }));
    }
    let total = match json::big_hex(&burned) {
        Value::String(s) if s != "0x0" => Value::String(format!("-{s}")),
        v => v,
    };
    Ok(json!({
        "quaiSupplyAdded": "0x0",
        "quaiSupplyRemoved": "0x0",
        "quaiSupplyTotal": total,
        "qiSupplyAdded": "0x0",
        "qiSupplyRemoved": "0x0",
        "qiSupplyTotal": "0x0",
    }))
}

/// `GetContractSize`: the size of a Quai account in the state of a block.
fn contract_size(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 2)?;
    let address = a.get(0, args::address_bytes)?;
    let at = a.get(1, args::block_ref)?;
    if qi(&address) {
        return Err(RpcError::go(
            "getContractSize cannot be called on a Qi Address",
        ));
    }
    let state = state_ref(view, at)?;
    quai_internal(view, &address)?;
    Ok(json::big_hex(&state.state()?.size(&address)?))
}

/// `GetLockupsForContractAndMiner` (`vm.GetAllLockupData`): the records by
/// epoch, then lockup byte, each a list of one.
fn lockups(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 2)?;
    let owner = a.get(0, args::address)?;
    let beneficiary = a.get(1, args::address)?;
    quai_internal(view, &owner)?;
    if !internal(view, &beneficiary) {
        return Err(RpcError::go("Address belongs to other zone"));
    }
    let mut out = Map::new();
    for l in view.coinbase_lockups(&owner, &beneficiary)? {
        let record = json!({
            "balance": decimal_number(&l.balance),
            "trancheUnlockHeight": l.unlock_height,
            "elements": l.elements,
            "delegate": json::address_lower(&l.delegate),
        });
        let epoch = out
            .entry(l.epoch.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(bytes) = epoch {
            if let Value::Array(list) = bytes
                .entry(l.lockup_byte.to_string())
                .or_insert_with(|| Value::Array(vec![]))
            {
                list.push(record);
            }
        }
    }
    Ok(Value::Object(out))
}

/// A `*big.Int` in JSON: a decimal number.
fn decimal_number(v: &[u8; 32]) -> Value {
    serde_json::from_str(&super::fees::decimal(v)).unwrap_or(Value::Null)
}

/// `[]string`.
fn strings(v: &Value) -> Result<Vec<String>, String> {
    match v {
        Value::Null => Ok(vec![]),
        Value::Array(items) => items
            .iter()
            .map(|i| match i {
                Value::String(s) => Ok(s.clone()),
                Value::Null => Ok(String::new()),
                other => Err(args::type_error(json_kind(other), "string")),
            })
            .collect(),
        other => Err(args::type_error(json_kind(other), "[]string")),
    }
}
fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `GetProof` (quai_api.go and api.go): the account proof, and a storage
/// proof of each key (`common.HexToHash`), echoed as given.
fn proof(view: &dyn ChainView, params: &Value) -> R<Value> {
    let a = Args::new(params, 3)?;
    let address = a.get(0, args::address)?;
    let keys = a.get(1, strings)?;
    let at = a.get(2, args::block_ref)?;
    let state = state_ref(view, at)?;
    quai_internal(view, &address)?;
    let state = state.state()?;
    let hashes: Vec<Hash> = keys.iter().map(|k| args::lenient_hash(k)).collect();
    let p = state.proof(&address, &hashes)?;
    let hex_list =
        |nodes: &[Vec<u8>]| Value::Array(nodes.iter().map(|n| json::bytes_hex(n)).collect());
    let (storage_hash, code_hash, storage) = match &p.storage {
        Some(s) => (
            s.root,
            s.code_hash,
            keys.iter()
                .zip(&s.keys)
                .map(|(k, (value, nodes))| json!({"key": k, "value": json::big_hex(value), "proof": hex_list(nodes)}))
                .collect(),
        ),
        None => (
            empty_root(),
            empty_code_hash(),
            keys.iter()
                .map(|k| json!({"key": k, "value": "0x0", "proof": []}))
                .collect(),
        ),
    };
    let mut m = Map::new();
    m.insert("address".into(), Value::String(json::checksum(&address)));
    m.insert("accountProof".into(), hex_list(&p.account));
    m.insert("balance".into(), json::big_hex(&state.balance(&address)?));
    m.insert("codeHash".into(), json::hash_hex(&code_hash));
    m.insert("nonce".into(), json::u64_hex(state.nonce(&address)?));
    m.insert("storageHash".into(), json::hash_hex(&storage_hash));
    m.insert("storageProof".into(), Value::Array(storage));
    Ok(Value::Object(m))
}
fn empty_root() -> Hash {
    quai_primitives::mpt::empty_root()
}
/// keccak256 of no bytes.
fn empty_code_hash() -> Hash {
    decode_hex("0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470")
        .ok()
        .and_then(|b| b.try_into().ok())
        .unwrap_or_default()
}
