//! JSON forms of go-quai v0.57.0 (RPC version "v1"):
//! `RPCMarshalBlock` and `rpcMarshalBlock` (internal/quaiapi/quai_api.go),
//! `WorkObject.RPCMarshalHeader` and `WorkObjectHeader.RPCMarshalWorkObjectHeader`
//! (core/types/wo.go), `Header.RPCMarshalHeader` (core/types/block.go),
//! `newRPCTransaction` (internal/quaiapi/api.go), `GetTransactionReceipt` and
//! `types.Log`. Go writes maps with sorted keys and structs in field order;
//! these values compare equal as JSON, member order aside.
use crate::view::{BlockData, LogData, ReceiptData, UncleKind};
use quai_primitives::{
    schema::block::ProtoHeader,
    transaction::{keccak, AccessTuple, Kind, Transaction},
    Hash, Header,
};
use serde_json::{json, Map, Value};

pub fn u64_hex(n: u64) -> Value {
    Value::String(format!("0x{n:x}"))
}
/// `hexutil.Big` of a big-endian magnitude.
pub fn big_hex(bytes: &[u8]) -> Value {
    let bytes = &bytes[bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len())..];
    if bytes.is_empty() {
        return Value::String("0x0".into());
    }
    let mut s = format!("0x{:x}", bytes[0]);
    for b in &bytes[1..] {
        s.push_str(&format!("{b:02x}"));
    }
    Value::String(s)
}
/// `hexutil.Bytes`.
pub fn bytes_hex(bytes: &[u8]) -> Value {
    Value::String(quai_primitives::encode_hex(bytes))
}
pub fn hash_hex(hash: &[u8]) -> Value {
    let mut h = [0u8; 32];
    let take = hash.len().min(32);
    h[32 - take..].copy_from_slice(&hash[hash.len() - take..]);
    bytes_hex(&h)
}
/// The last 20 bytes, left-padded (`common.BytesToAddress`).
pub fn address20(bytes: &[u8]) -> [u8; 20] {
    let mut a = [0u8; 20];
    let take = bytes.len().min(20);
    a[20 - take..].copy_from_slice(&bytes[bytes.len() - take..]);
    a
}
/// `Address.Hex()`: EIP-55 mixed case.
pub fn checksum(address: &[u8; 20]) -> String {
    let lower = quai_primitives::encode_hex(address);
    let digest = keccak(&lower.as_bytes()[2..]);
    let mut out = String::with_capacity(42);
    out.push_str("0x");
    for (i, c) in lower[2..].chars().enumerate() {
        let nib = if i % 2 == 0 {
            digest[i / 2] >> 4
        } else {
            digest[i / 2] & 15
        };
        if c.is_ascii_alphabetic() && nib > 7 {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c);
        }
    }
    out
}
/// `common.Address` in JSON: lower-case hex.
pub fn address_lower(address: &[u8; 20]) -> Value {
    bytes_hex(address)
}

/// `WorkObjectHeader.RPCMarshalWorkObjectHeader("v1")`.
pub fn wo_header(header: &Header) -> Value {
    let w = header.wire();
    let hash32 = |v: &Option<quai_primitives::wire::BytesValue>| {
        hash_hex(v.as_ref().map(|v| v.value.as_slice()).unwrap_or_default())
    };
    let coinbase = w
        .primary_coinbase
        .as_ref()
        .map(|v| checksum(&address20(&v.value)))
        .unwrap_or_default();
    json!({
        "hash": hash_hex(&header.block_hash().unwrap_or_default()),
        "headerHash": hash32(&w.header_hash),
        "parentHash": hash32(&w.parent_hash),
        "number": big_hex(w.number.as_deref().unwrap_or_default()),
        "difficulty": big_hex(w.difficulty.as_deref().unwrap_or_default()),
        "primeTerminusNumber": big_hex(w.prime_terminus_number.as_deref().unwrap_or_default()),
        "nonce": format!("0x{:016x}", w.nonce.unwrap_or_default()),
        "location": bytes_hex(w.location.as_ref().map(|v| v.value.as_slice()).unwrap_or_default()),
        "txHash": hash32(&w.tx_hash),
        "timestamp": u64_hex(w.time.unwrap_or_default()),
        "mixHash": hash32(&w.mix_hash),
        "lock": u64_hex(w.lock.unwrap_or_default() as u64),
        "primaryCoinbase": coinbase,
        "data": bytes_hex(&w.data),
    })
}

/// `reflect.TypeOf(Header{}).Size()` of go-quai v0.57.0 on 64-bit hosts.
const HEADER_STRUCT_SIZE: u64 = 728;
fn bit_len(bytes: &[u8]) -> u64 {
    match bytes.iter().position(|b| *b != 0) {
        None => 0,
        Some(i) => (bytes.len() - i) as u64 * 8 - bytes[i].leading_zeros() as u64,
    }
}
fn proto_hash(v: &Option<quai_primitives::schema::common::ProtoHash>) -> Value {
    hash_hex(v.as_ref().map(|v| v.value.as_slice()).unwrap_or_default())
}
fn big_opt(v: &Option<Vec<u8>>) -> Value {
    big_hex(v.as_deref().unwrap_or_default())
}
/// `Header.RPCMarshalHeader()` of the body header.
pub fn body_header(w: &ProtoHeader) -> Map<String, Value> {
    let extra = w.extra.as_deref().unwrap_or_default();
    let numbers: u64 = w.number.iter().map(|n| bit_len(n)).sum();
    let mut m = Map::new();
    let hashes = |v: &[quai_primitives::schema::common::ProtoHash]| {
        v.iter().map(|h| hash_hex(&h.value)).collect::<Vec<_>>()
    };
    let bigs = |v: &[Vec<u8>]| v.iter().map(|b| big_hex(b)).collect::<Vec<_>>();
    m.insert("parentHash".into(), Value::Array(hashes(&w.parent_hash)));
    m.insert("uncledEntropy".into(), big_opt(&w.uncled_entropy));
    m.insert("uncleHash".into(), proto_hash(&w.uncle_hash));
    m.insert("quaiStateSize".into(), big_opt(&w.quai_state_size));
    m.insert("evmRoot".into(), proto_hash(&w.evm_root));
    m.insert("utxoRoot".into(), proto_hash(&w.utxo_root));
    m.insert("extraData".into(), bytes_hex(extra));
    m.insert(
        "size".into(),
        u64_hex(HEADER_STRUCT_SIZE + extra.len() as u64 + numbers / 8),
    );
    m.insert("transactionsRoot".into(), proto_hash(&w.tx_hash));
    m.insert("receiptsRoot".into(), proto_hash(&w.receipt_hash));
    m.insert("outboundEtxsRoot".into(), proto_hash(&w.outbound_etx_hash));
    m.insert("etxSetRoot".into(), proto_hash(&w.etx_set_root));
    m.insert("etxRollupRoot".into(), proto_hash(&w.etx_rollup_hash));
    m.insert(
        "primeTerminusHash".into(),
        proto_hash(&w.prime_terminus_hash),
    );
    m.insert(
        "interlinkRootHash".into(),
        proto_hash(&w.interlink_root_hash),
    );
    m.insert(
        "manifestHash".into(),
        Value::Array(hashes(&w.manifest_hash)),
    );
    m.insert("gasLimit".into(), u64_hex(w.gas_limit.unwrap_or_default()));
    m.insert("gasUsed".into(), u64_hex(w.gas_used.unwrap_or_default()));
    m.insert(
        "efficiencyScore".into(),
        u64_hex(w.efficiency_score.unwrap_or_default()),
    );
    m.insert(
        "thresholdCount".into(),
        u64_hex(w.threshold_count.unwrap_or_default()),
    );
    m.insert(
        "expansionNumber".into(),
        u64_hex(w.expansion_number.unwrap_or_default()),
    );
    m.insert(
        "etxEligibleSlices".into(),
        proto_hash(&w.etx_eligible_slices),
    );
    m.insert(
        "stateLimit".into(),
        u64_hex(w.state_limit.unwrap_or_default()),
    );
    m.insert(
        "stateUsed".into(),
        u64_hex(w.state_used.unwrap_or_default()),
    );
    m.insert("exchangeRate".into(), big_opt(&w.exchange_rate));
    m.insert("avgTxFees".into(), big_opt(&w.avg_tx_fees));
    m.insert("totalFees".into(), big_opt(&w.total_fees));
    m.insert("kQuaiDiscount".into(), big_opt(&w.k_quai_discount));
    m.insert(
        "conversionFlowAmount".into(),
        big_opt(&w.conversion_flow_amount),
    );
    m.insert("minerDifficulty".into(), big_opt(&w.miner_difficulty));
    m.insert("primeStateRoot".into(), proto_hash(&w.prime_state_root));
    m.insert("regionStateRoot".into(), proto_hash(&w.region_state_root));
    m.insert("number".into(), Value::Array(bigs(&w.number)));
    m.insert(
        "parentEntropy".into(),
        Value::Array(bigs(&w.parent_entropy)),
    );
    m.insert(
        "parentDeltaEntropy".into(),
        Value::Array(bigs(&w.parent_delta_entropy)),
    );
    m.insert(
        "parentUncledDeltaEntropy".into(),
        Value::Array(bigs(&w.parent_uncled_delta_entropy)),
    );
    if w.base_fee.is_some() {
        m.insert("baseFeePerGas".into(), big_opt(&w.base_fee));
    }
    m
}

/// `WorkObject.RPCMarshalHeader("v1")`: the body header and "woHeader".
pub fn header(block: &BlockData) -> Value {
    let mut m = body_header(block.object.header().wire());
    m.insert("woHeader".into(), wo_header(&block.header));
    Value::Object(m)
}

fn access_list(list: &[AccessTuple]) -> Value {
    Value::Array(
        list.iter()
            .map(|t| {
                json!({
                    "address": address_lower(&t.address),
                    "storageKeys": t.keys.iter().map(|k| hash_hex(k)).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

/// The block position of a transaction in `newRPCTransaction`.
#[derive(Clone, Copy)]
pub struct Position {
    pub block_hash: Hash,
    pub block_number: u64,
    pub index: u64,
}

/// `newRPCTransaction` in a zone. `hash` is `tx.Hash()`.
pub fn transaction(tx: &Transaction, hash: &Hash, at: Option<Position>) -> Value {
    let mut m = Map::new();
    let mut set = |k: &str, v: Value| {
        m.insert(k.to_string(), v);
    };
    set("type", u64_hex(tx.type_id() as u64));
    set("hash", hash_hex(hash));
    match tx.kind() {
        Kind::Qi(q) => {
            set("chainId", big_hex(&q.chain_id));
            set("utxoSignature", bytes_hex(&q.signature));
            set("input", bytes_hex(&q.data));
            set("gas", u64_hex(0));
            set("nonce", u64_hex(0));
            if !q.inputs.is_empty() {
                set(
                    "inputs",
                    Value::Array(
                        q.inputs
                            .iter()
                            .map(|i| {
                                json!({
                                    "previousOutPoint": {
                                        "txHash": hash_hex(&i.hash),
                                        "index": u64_hex(i.index as u64),
                                    },
                                    "pubKey": bytes_hex(&i.public_key),
                                })
                            })
                            .collect(),
                    ),
                );
            }
            if !q.outputs.is_empty() {
                set(
                    "outputs",
                    Value::Array(
                        q.outputs
                            .iter()
                            .map(|o| {
                                json!({
                                    "denomination": u64_hex(o.denomination as u64),
                                    "address": checksum(&address20(o.address.as_deref().unwrap_or_default())),
                                    "lock": big_hex(&o.lock),
                                })
                            })
                            .collect(),
                    ),
                );
            }
        }
        Kind::Quai(q) => {
            let from = tx.sender().unwrap_or([0; 20]);
            set("from", Value::String(checksum(&from)));
            set("gas", u64_hex(q.gas));
            set("input", bytes_hex(&q.data));
            set("nonce", u64_hex(q.nonce));
            set("value", big_hex(&q.value));
            set("chainId", big_hex(&q.chain_id));
            set("gasPrice", big_hex(&q.gas_price));
            if let Some(p) = &q.work.parent {
                set("parentHash", hash_hex(p));
            }
            if let Some(p) = &q.work.mix {
                set("mixHash", hash_hex(p));
            }
            if let Some(n) = q.work.nonce {
                set("workNonce", u64_hex(n));
            }
            if let Some(to) = &q.to {
                set("to", Value::String(checksum(to)));
            }
            set("v", big_hex(&q.v));
            set("r", big_hex(&q.r));
            set("s", big_hex(&q.s));
            set("accessList", access_list(&q.access_list));
        }
        Kind::External(e) => {
            set("gas", u64_hex(e.gas));
            set("input", bytes_hex(&e.data));
            set("nonce", u64_hex(0));
            set("to", Value::String(checksum(&e.to)));
            set("value", big_hex(&e.value));
            set("originatingTxHash", hash_hex(&e.originating_hash));
            set("from", Value::String(checksum(&e.sender)));
            set("etxIndex", u64_hex(e.index as u64));
            set("etxType", u64_hex(e.etx_type));
            set("accessList", access_list(&e.access_list));
        }
    }
    match at {
        Some(p) if p.block_hash != [0; 32] => {
            set("blockHash", hash_hex(&p.block_hash));
            set("blockNumber", u64_hex(p.block_number));
            set("transactionIndex", u64_hex(p.index));
        }
        _ => {
            set("blockHash", Value::Null);
            set("blockNumber", Value::Null);
            set("transactionIndex", Value::Null);
        }
    }
    Value::Object(m)
}

/// `RPCMarshalBlock` plus "totalEntropy".
pub fn block(block: &BlockData, full: bool, kinds: &[UncleKind], total_entropy: &[u8]) -> Value {
    let mut m = Map::new();
    m.insert("hash".into(), hash_hex(&block.hash));
    m.insert("woHeader".into(), wo_header(&block.header));
    m.insert(
        "header".into(),
        Value::Object(body_header(block.object.header().wire())),
    );
    m.insert("size".into(), u64_hex(block.size));
    m.insert("order".into(), Value::from(block.order));
    let at = |index: usize| Position {
        block_hash: block.hash,
        block_number: block.number,
        index: index as u64,
    };
    let txs = block.object.transactions();
    let etxs = block.object.outbound_etxs();
    let list = |list: &[Transaction], hashes: &[Hash]| -> Value {
        Value::Array(
            list.iter()
                .zip(hashes)
                .enumerate()
                .map(|(i, (tx, h))| {
                    if full {
                        transaction(tx, h, Some(at(i)))
                    } else {
                        hash_hex(h)
                    }
                })
                .collect(),
        )
    };
    m.insert("transactions".into(), list(txs, &block.tx_hashes));
    m.insert("outboundEtxs".into(), list(etxs, &block.etx_hashes));
    let uncles = |kind: UncleKind| -> Value {
        Value::Array(
            block
                .uncles
                .iter()
                .zip(kinds)
                .filter(|(_, k)| **k == kind)
                .map(|(h, _)| wo_header(h))
                .collect(),
        )
    };
    m.insert("uncles".into(), uncles(UncleKind::Block));
    m.insert("workshares".into(), uncles(UncleKind::Workshare));
    m.insert(
        "subManifest".into(),
        Value::Array(
            block
                .object
                .manifest_hashes()
                .iter()
                .map(|h| hash_hex(h))
                .collect(),
        ),
    );
    m.insert(
        "interlinkHashes".into(),
        Value::Array(
            block
                .object
                .interlink_hashes()
                .iter()
                .map(|h| hash_hex(h))
                .collect(),
        ),
    );
    m.insert("totalEntropy".into(), big_hex(total_entropy));
    Value::Object(m)
}

/// `Transaction.MarshalJSON` (core/types/transaction_marshalling.go): the
/// form of transactions inside a marshalled work object. `hash` is
/// `tx.Hash()`. Fields without "omitempty" are null when the type has no
/// value for them.
pub fn tx_json(tx: &Transaction, hash: &Hash) -> Value {
    let mut m = Map::new();
    m.insert("type".into(), u64_hex(tx.type_id() as u64));
    m.insert("hash".into(), hash_hex(hash));
    for k in [
        "nonce",
        "gasPrice",
        "gas",
        "value",
        "input",
        "to",
        "accessList",
    ] {
        m.insert(k.into(), Value::Null);
    }
    let work = |m: &mut Map<String, Value>, w: &quai_primitives::transaction::Work| {
        if let Some(p) = &w.parent {
            m.insert("parentHash".into(), hash_hex(p));
        }
        if let Some(p) = &w.mix {
            m.insert("mixHash".into(), hash_hex(p));
        }
    };
    match tx.kind() {
        Kind::Quai(q) => {
            m.insert("chainId".into(), big_hex(&q.chain_id));
            m.insert("accessList".into(), access_list(&q.access_list));
            m.insert("nonce".into(), u64_hex(q.nonce));
            m.insert("gas".into(), u64_hex(q.gas));
            m.insert("gasPrice".into(), big_hex(&q.gas_price));
            m.insert("value".into(), big_hex(&q.value));
            m.insert("input".into(), bytes_hex(&q.data));
            if let Some(to) = &q.to {
                m.insert("to".into(), Value::String(checksum(to)));
            }
            m.insert("v".into(), big_hex(&q.v));
            m.insert("r".into(), big_hex(&q.r));
            m.insert("s".into(), big_hex(&q.s));
            work(&mut m, &q.work);
            if let Some(n) = q.work.nonce {
                m.insert("workNonce".into(), u64_hex(n));
            }
        }
        Kind::External(e) => {
            m.insert("accessList".into(), access_list(&e.access_list));
            m.insert("originatingTxHash".into(), hash_hex(&e.originating_hash));
            m.insert("etxIndex".into(), u64_hex(e.index as u64));
            m.insert("gas".into(), u64_hex(e.gas));
            m.insert("value".into(), big_hex(&e.value));
            m.insert("input".into(), bytes_hex(&e.data));
            m.insert("to".into(), Value::String(checksum(&e.to)));
            m.insert("from".into(), Value::String(checksum(&e.sender)));
            m.insert("etxType".into(), u64_hex(e.etx_type));
        }
        Kind::Qi(q) => {
            m.insert("chainId".into(), big_hex(&q.chain_id));
            if !q.inputs.is_empty() {
                m.insert(
                    "inputs".into(),
                    Value::Array(
                        q.inputs
                            .iter()
                            .map(|i| {
                                json!({
                                    "previousOutPoint": {
                                        "txHash": hash_hex(&i.hash),
                                        "index": u64_hex(i.index as u64),
                                    },
                                    "pubkey": bytes_hex(&i.public_key),
                                })
                            })
                            .collect(),
                    ),
                );
            }
            // A decoded Qi transaction always has data (`ProtoEncode` writes
            // an empty slice for none).
            m.insert("input".into(), bytes_hex(&q.data));
            if !q.outputs.is_empty() {
                m.insert(
                    "outputs".into(),
                    Value::Array(
                        q.outputs
                            .iter()
                            .map(|o| {
                                json!({
                                    "address": bytes_hex(o.address.as_deref().unwrap_or_default()),
                                    "denomination": u64_hex(o.denomination as u64),
                                    "lock": big_hex(&o.lock),
                                })
                            })
                            .collect(),
                    ),
                );
            }
            m.insert("utxoSignature".into(), bytes_hex(&q.signature));
        }
    }
    Value::Object(m)
}

/// `WorkObject.RPCMarshalWorkObject("v1")` of a block read from the
/// database (`ProtoDecode` gives every body list, so none is null): the
/// payload of a "newHeads" notification.
pub fn work_object(block: &BlockData) -> Value {
    let list = |txs: &[Transaction], hashes: &[Hash]| -> Value {
        Value::Array(
            txs.iter()
                .zip(hashes)
                .map(|(tx, h)| tx_json(tx, h))
                .collect(),
        )
    };
    let hashes = |v: Vec<Hash>| -> Value { Value::Array(v.iter().map(|h| hash_hex(h)).collect()) };
    json!({
        "woHeader": wo_header(&block.header),
        "woBody": {
            "header": Value::Object(body_header(block.object.header().wire())),
            "transactions": list(block.object.transactions(), &block.tx_hashes),
            "outboundEtxs": list(block.object.outbound_etxs(), &block.etx_hashes),
            "manifest": hashes(block.object.manifest_hashes()),
            "interlinkHashes": hashes(block.object.interlink_hashes()),
            "uncles": Value::Array(block.uncles.iter().map(wo_header).collect()),
        },
    })
}

/// A `types.Log` with its derived fields.
pub fn log(log: &LogData, block: &BlockData, tx_index: usize, log_index: usize) -> Value {
    json!({
        "address": address_lower(&log.address),
        "topics": log.topics.iter().map(|t| hash_hex(t)).collect::<Vec<_>>(),
        "data": bytes_hex(&log.data),
        "blockNumber": u64_hex(block.number),
        "transactionHash": hash_hex(&block.tx_hashes[tx_index]),
        "transactionIndex": u64_hex(tx_index as u64),
        "blockHash": hash_hex(&block.hash),
        "logIndex": u64_hex(log_index as u64),
        "removed": false,
    })
}

/// `GetTransactionReceipt` for transaction `index` of `block`.
pub fn receipt(block: &BlockData, receipts: &[ReceiptData], index: usize) -> Value {
    let tx = &block.object.transactions()[index];
    let tx_hash = block.tx_hashes[index];
    // Go keeps the last receipt with this hash, or an empty receipt.
    let empty = ReceiptData {
        post_state: vec![],
        status: 0,
        cumulative_gas_used: 0,
        gas_used: 0,
        tx_hash: [0; 32],
        contract_address: None,
        logs: vec![],
        outbound_etxs: vec![],
    };
    // `DeriveFields` gives receipt i the hash of transaction i; Go then keeps
    // the last receipt with the requested hash.
    let position = (0..receipts.len().min(block.tx_hashes.len()))
        .rev()
        .find(|i| block.tx_hashes[*i] == tx_hash);
    let r = position.map_or(&empty, |i| &receipts[i]);
    // Log indices count from the start of the block.
    let first_log: usize = position.map_or(0, |i| receipts[..i].iter().map(|r| r.logs.len()).sum());
    let log_tx = position.unwrap_or(index);
    let logs: Vec<Value> = r
        .logs
        .iter()
        .enumerate()
        .map(|(j, l)| log(l, block, log_tx, first_log + j))
        .collect();
    // `CreateBloom` of this receipt: Quai's 10,240-byte bloom.
    let bloom = quai_primitives::receipt::logs_bloom(
        &r.logs
            .iter()
            .map(|l| quai_primitives::receipt::Log {
                address: l.address,
                topics: l.topics.clone(),
                data: l.data.clone(),
            })
            .collect::<Vec<_>>(),
    );
    let mut m = Map::new();
    m.insert("blockHash".into(), hash_hex(&block.hash));
    m.insert("blockNumber".into(), u64_hex(block.number));
    m.insert("transactionHash".into(), hash_hex(&tx_hash));
    m.insert("transactionIndex".into(), u64_hex(index as u64));
    m.insert("gasUsed".into(), u64_hex(r.gas_used));
    m.insert("cumulativeGasUsed".into(), u64_hex(r.cumulative_gas_used));
    m.insert("contractAddress".into(), Value::Null);
    m.insert("logs".into(), Value::Array(logs));
    m.insert("logsBloom".into(), bytes_hex(&bloom));
    m.insert("type".into(), u64_hex(tx.type_id() as u64));
    match tx.kind() {
        Kind::Quai(q) => {
            if let Some(to) = &q.to {
                m.insert("to".into(), Value::String(checksum(to)));
            }
            let from = tx.sender().unwrap_or([0; 20]);
            m.insert("from".into(), Value::String(checksum(&from)));
            // hexutil.Uint64(gasPrice.Uint64()): the low 64 bits.
            let p = &q.gas_price;
            let low = &p[p.len().saturating_sub(8)..];
            let mut b = [0u8; 8];
            b[8 - low.len()..].copy_from_slice(low);
            m.insert("effectiveGasPrice".into(), u64_hex(u64::from_be_bytes(b)));
        }
        Kind::External(e) => {
            m.insert("originatingTxHash".into(), hash_hex(&e.originating_hash));
            m.insert("etxType".into(), u64_hex(e.etx_type));
        }
        Kind::Qi(_) => {}
    }
    if !matches!(tx.kind(), Kind::Quai(_)) {
        let fee = block
            .object
            .header()
            .wire()
            .base_fee
            .clone()
            .unwrap_or_default();
        let low = &fee[fee.len().saturating_sub(8)..];
        let mut b = [0u8; 8];
        b[8 - low.len()..].copy_from_slice(low);
        m.insert("effectiveGasPrice".into(), u64_hex(u64::from_be_bytes(b)));
    }
    if !r.outbound_etxs.is_empty() {
        let at = Position {
            block_hash: block.hash,
            block_number: block.number,
            index: index as u64,
        };
        m.insert(
            "outboundEtxs".into(),
            Value::Array(
                r.outbound_etxs
                    .iter()
                    .map(|etx| {
                        let h = etx.id(None).unwrap_or_default();
                        transaction(etx, &h, Some(at))
                    })
                    .collect(),
            ),
        );
    }
    if r.post_state.is_empty() {
        m.insert("status".into(), u64_hex(r.status));
    } else {
        m.insert("root".into(), bytes_hex(&r.post_state));
    }
    // `DeriveFields` derives the address of a Quai contract creation whose
    // stored address is zero (`crypto.CreateAddress`).
    let mut contract = r.contract_address.filter(|a| *a != [0; 20]);
    if let (None, Kind::Quai(q), Some(_)) = (contract, tx.kind(), position) {
        if q.to.is_none() {
            let from = tx.sender().unwrap_or([0; 20]);
            let mut pre = from.to_vec();
            pre.extend(q.nonce.to_be_bytes());
            pre.extend(&q.data);
            contract = Some(address20(&keccak(&pre)[12..])).filter(|a| *a != [0; 20]);
        }
    }
    if let Some(a) = contract {
        m.insert("contractAddress".into(), Value::String(checksum(&a)));
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hex_forms() {
        assert_eq!(big_hex(&[]), "0x0");
        assert_eq!(big_hex(&[0, 0, 1, 2]), "0x102");
        assert_eq!(u64_hex(0), "0x0");
        assert_eq!(bytes_hex(&[]), "0x");
        let a = quai_primitives::decode_hex("0x0016c1ad78818d97d27ff41ca8f32ee99119d9c4").unwrap();
        assert_eq!(
            checksum(&address20(&a)),
            "0x0016c1AD78818D97d27FF41cA8F32eE99119d9C4"
        );
        assert_eq!(bit_len(&[0, 1]), 1);
        assert_eq!(bit_len(&[1, 0xf9]), 9);
    }
}
