//! Helios-style trustless Merkle-proof RPC client and [`LightView`].
//!
//! Every balance, nonce, storage slot, bytecode, `quai_call` / `eth_call`, and
//! block transaction / receipt read served by [`LightView`] is cryptographically
//! verified against the locally PoW-validated header's `evmRoot`, `txHash`, or
//! `receiptHash`. Any tampered response from an untrusted upstream RPC node is
//! detected and rejected.
use super::store::{LightHeaderStore, VerifiedAccount, VerifiedBlockPayload, VerifiedSlot};
use crate::miner::Miner;
use crate::rpc_call;
use crypto_bigint::Encoding as _;
use quai_consensus::pow::SharedCaches;
use quai_execution::evm::{
    self,
    calls::Environment,
    state::{Account, StorageProof, World, WorldProof},
    word::{from_bytes, Word},
};
use quai_primitives::{
    decode_hex, encode_hex, mpt,
    receipt::{receipts_root, Log, Receipt, BLOOM_BYTES},
    transaction::{keccak, Address, Transaction},
    work, Hash,
};
use quai_rpc::view::{
    AccountState, BlockData, CallError, CallRequest, CallResult, ChainView, LogData, OutpointData,
    PeerCounts, ReceiptData, TxPosition, UncleKind, UtxoData, ViewError, U256,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, RwLock,
};
use std::time::Duration;

const EMPTY_CODE_HASH: Hash = [
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
];

const RPC_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HTTP_BODY: usize = 8 * 1024 * 1024;

/// Synchronous zero-dependency HTTP/1.1 JSON-RPC client (safe inside blocking
/// RPC handlers without nested Tokio `block_on`).
#[derive(Clone, Debug)]
pub struct HttpRpcClient {
    url: String,
    authority: String,
    path: String,
    next_id: Arc<AtomicU64>,
}

impl HttpRpcClient {
    pub fn new(url: &str) -> Result<Self, String> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("upstream RPC {url:?}: only http:// URLs are supported"))?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return Err(format!("upstream RPC {url:?}: missing host"));
        }
        let authority = if authority.contains(':') {
            authority.to_string()
        } else {
            format!("{authority}:80")
        };
        Ok(Self {
            url: url.to_string(),
            authority,
            path: path.to_string(),
            next_id: Arc::new(AtomicU64::new(1)),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let body = serde_json::to_vec(&payload).map_err(|e| format!("encode rpc: {e}"))?;
        let mut stream = TcpStream::connect(&self.authority)
            .map_err(|e| format!("connect {}: {e}", self.authority))?;
        let _ = stream.set_read_timeout(Some(RPC_TIMEOUT));
        let _ = stream.set_write_timeout(Some(RPC_TIMEOUT));
        let req = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.path,
            self.authority,
            body.len()
        );
        stream
            .write_all(req.as_bytes())
            .and_then(|_| stream.write_all(&body))
            .map_err(|e| format!("write {}: {e}", self.authority))?;

        let mut raw = Vec::new();
        let mut limited = stream.take((MAX_HTTP_BODY + 16_384) as u64);
        limited
            .read_to_end(&mut raw)
            .map_err(|e| format!("read {}: {e}", self.authority))?;
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or_else(|| "malformed HTTP response from upstream RPC".to_string())?;
        let head = std::str::from_utf8(&raw[..split])
            .map_err(|_| "non-UTF8 HTTP header from upstream RPC".to_string())?;
        let body_bytes = &raw[split + 4..];
        let decoded_body = if head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            decode_chunked(body_bytes)?
        } else {
            body_bytes.to_vec()
        };
        let env: Value = serde_json::from_slice(&decoded_body)
            .map_err(|e| format!("upstream RPC JSON decode: {e}"))?;
        if let Some(err) = env.get("error") {
            if !err.is_null() {
                let msg = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("upstream RPC error");
                return Err(msg.to_string());
            }
        }
        Ok(env.get("result").cloned().unwrap_or(Value::Null))
    }
}

fn decode_chunked(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    while let Some(crlf) = data.windows(2).position(|w| w == b"\r\n") {
        let size_str = std::str::from_utf8(&data[..crlf])
            .map_err(|_| "invalid chunk header".to_string())?
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let len = usize::from_str_radix(size_str, 16)
            .map_err(|_| format!("invalid chunk size {size_str:?}"))?;
        data = &data[crlf + 2..];
        if len == 0 {
            break;
        }
        if data.len() < len || out.len() + len > MAX_HTTP_BODY {
            return Err("truncated or oversized chunked HTTP body".into());
        }
        out.extend_from_slice(&data[..len]);
        data = data.get(len + 2..).unwrap_or(&[]);
    }
    Ok(out)
}

fn parse_u256_hex(s: &str) -> Result<U256, String> {
    let raw = decode_hex_lenient(s)?;
    if raw.len() > 32 {
        return Err(format!("u256 hex overflow: {s:?}"));
    }
    let mut out = [0u8; 32];
    out[32 - raw.len()..].copy_from_slice(&raw);
    Ok(out)
}

fn parse_u64_hex(s: &str) -> Result<u64, String> {
    let stripped = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if stripped.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(stripped, 16).map_err(|_| format!("invalid u64 hex {s:?}"))
}

fn parse_hash_hex(s: &str) -> Result<Hash, String> {
    let raw = decode_hex_lenient(s)?;
    if raw.len() > 32 {
        return Err(format!("hash hex overflow: {s:?}"));
    }
    let mut out = [0u8; 32];
    out[32 - raw.len()..].copy_from_slice(&raw);
    Ok(out)
}

fn decode_hex_lenient(s: &str) -> Result<Vec<u8>, String> {
    let stripped = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if stripped.is_empty() {
        return Ok(Vec::new());
    }
    let padded = if stripped.len() % 2 == 1 {
        format!("0x0{stripped}")
    } else {
        format!("0x{stripped}")
    };
    decode_hex(&padded).map_err(|e| format!("hex decode {s:?}: {e}"))
}

fn bytes_to_u64(b: &[u8]) -> Result<u64, String> {
    if b.len() > 8 {
        return Err("RLP integer overflows u64".into());
    }
    let mut arr = [0u8; 8];
    arr[8 - b.len()..].copy_from_slice(b);
    Ok(u64::from_be_bytes(arr))
}

fn bytes_to_u256(b: &[u8]) -> Result<U256, String> {
    if b.len() > 32 {
        return Err("RLP integer overflows u256".into());
    }
    let mut arr = [0u8; 32];
    arr[32 - b.len()..].copy_from_slice(b);
    Ok(arr)
}

fn bytes_to_hash(b: &[u8]) -> Result<Hash, String> {
    if b.len() != 32 {
        return Err(format!(
            "expected 32-byte hash in RLP leaf, got {}",
            b.len()
        ));
    }
    let mut h = [0u8; 32];
    h.copy_from_slice(b);
    Ok(h)
}

/// Cryptographically decode a proven Quai account RLP leaf:
/// `[nonce, balance, storage_root, code_hash, storage_size]`.
fn decode_verified_account(
    rlp_leaf: &[u8],
    account_proof: Vec<Vec<u8>>,
) -> Result<VerifiedAccount, String> {
    let items = mpt::rlp_list_items(rlp_leaf)
        .map_err(|e| format!("verified account RLP decode failed: {e}"))?;
    if items.len() != 5 && items.len() != 4 {
        return Err(format!(
            "verified account RLP expected 5 items (or 4), got {}",
            items.len()
        ));
    }
    let nonce = bytes_to_u64(&items[0])?;
    let balance = bytes_to_u256(&items[1])?;
    let storage_root = bytes_to_hash(&items[2])?;
    let code_hash = bytes_to_hash(&items[3])?;
    let storage_size = if items.len() == 5 {
        bytes_to_u256(&items[4])?
    } else {
        [0u8; 32]
    };
    Ok(VerifiedAccount {
        exists: true,
        nonce,
        balance,
        storage_root,
        code_hash,
        storage_size,
        account_proof,
    })
}

/// Cryptographic proof verifier backed by an untrusted upstream JSON-RPC node
/// and the light node's locally verified [`LightHeaderStore`].
#[derive(Clone)]
pub struct ProofVerifier {
    store: Arc<LightHeaderStore>,
    upstream: HttpRpcClient,
}

impl ProofVerifier {
    pub fn new(store: Arc<LightHeaderStore>, upstream: HttpRpcClient) -> Self {
        Self { store, upstream }
    }

    pub fn upstream(&self) -> &HttpRpcClient {
        &self.upstream
    }

    /// Fetch and cryptographically verify an account (and optional storage keys)
    /// against `evm_root` of `block_number`.
    pub fn verify_account_and_slots(
        &self,
        block_number: u64,
        evm_root: Hash,
        address: &Address,
        keys: &[Hash],
    ) -> Result<(VerifiedAccount, Vec<VerifiedSlot>), ViewError> {
        // Fast path: check verified partial state cache first.
        if let Some(acc) = self.store.get_verified_account(evm_root, address) {
            let mut cached_slots = Vec::with_capacity(keys.len());
            let mut all_cached = true;
            for k in keys {
                if !acc.exists || acc.storage_root == mpt::empty_root() {
                    cached_slots.push(VerifiedSlot {
                        value: [0u8; 32],
                        proof: vec![],
                    });
                } else if let Some(s) = self.store.get_verified_slot(acc.storage_root, k) {
                    cached_slots.push(s);
                } else {
                    all_cached = false;
                    break;
                }
            }
            if all_cached {
                return Ok((acc, cached_slots));
            }
        }

        let key_hexes: Vec<Value> = keys.iter().map(|k| Value::String(encode_hex(k))).collect();
        let res = self
            .upstream
            .call(
                "quai_getProof",
                json!([
                    encode_hex(address),
                    key_hexes,
                    format!("0x{block_number:x}")
                ]),
            )
            .map_err(|e| ViewError::Unavailable(format!("upstream quai_getProof: {e}")))?;

        let account_proof_raw = res
            .get("accountProof")
            .and_then(Value::as_array)
            .ok_or_else(|| ViewError::Unavailable("upstream proof missing accountProof".into()))?;
        let mut account_proof = Vec::with_capacity(account_proof_raw.len());
        for node in account_proof_raw {
            let s = node
                .as_str()
                .ok_or_else(|| ViewError::Unavailable("non-string accountProof node".into()))?;
            account_proof.push(
                decode_hex_lenient(s)
                    .map_err(|e| ViewError::Unavailable(format!("bad accountProof hex: {e}")))?,
            );
        }

        // CRYPTOGRAPHIC CHECK 1: Verify account MPT proof against our PoW-validated `evm_root`.
        let proven_leaf = mpt::verify_proof(evm_root, address, &account_proof).map_err(|e| {
            ViewError::Unavailable(format!(
                "PROOF FRAUD / MISMATCH: account proof failed against evmRoot {}: {e}",
                encode_hex(&evm_root)
            ))
        })?;

        let claimed_balance =
            parse_u256_hex(res.get("balance").and_then(Value::as_str).unwrap_or("0x0"))
                .map_err(ViewError::Unavailable)?;
        let claimed_nonce =
            parse_u64_hex(res.get("nonce").and_then(Value::as_str).unwrap_or("0x0"))
                .map_err(ViewError::Unavailable)?;
        let claimed_storage = parse_hash_hex(
            res.get("storageHash")
                .and_then(Value::as_str)
                .unwrap_or("0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421"),
        )
        .map_err(ViewError::Unavailable)?;
        let claimed_code_hash = parse_hash_hex(
            res.get("codeHash")
                .and_then(Value::as_str)
                .unwrap_or("0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"),
        )
        .map_err(ViewError::Unavailable)?;

        let acc = match proven_leaf {
            None => {
                if claimed_balance != [0u8; 32]
                    || claimed_nonce != 0
                    || (claimed_storage != mpt::empty_root() && claimed_storage != [0u8; 32])
                    || (claimed_code_hash != EMPTY_CODE_HASH && claimed_code_hash != [0u8; 32])
                {
                    return Err(ViewError::Unavailable(
                        "PROOF FRAUD: upstream RPC claimed non-empty fields for cryptographically absent account".into(),
                    ));
                }
                VerifiedAccount {
                    exists: false,
                    nonce: 0,
                    balance: [0u8; 32],
                    storage_root: mpt::empty_root(),
                    code_hash: EMPTY_CODE_HASH,
                    storage_size: [0u8; 32],
                    account_proof,
                }
            }
            Some(leaf_bytes) => {
                let decoded = decode_verified_account(&leaf_bytes, account_proof)
                    .map_err(ViewError::Unavailable)?;
                if decoded.balance != claimed_balance
                    || decoded.nonce != claimed_nonce
                    || decoded.storage_root != claimed_storage
                    || decoded.code_hash != claimed_code_hash
                {
                    return Err(ViewError::Unavailable(
                        "PROOF FRAUD: upstream RPC account fields do not match proven MPT leaf"
                            .into(),
                    ));
                }
                decoded
            }
        };

        // CRYPTOGRAPHIC CHECK 2: Verify every storage slot proof against proven `acc.storage_root`.
        let storage_arr = res
            .get("storageProof")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if storage_arr.len() != keys.len() {
            return Err(ViewError::Unavailable(format!(
                "PROOF FRAUD: requested {} storage proofs, upstream returned {}",
                keys.len(),
                storage_arr.len()
            )));
        }

        let mut verified_slots = Vec::with_capacity(keys.len());
        for (key, sp) in keys.iter().zip(storage_arr.iter()) {
            let claimed_val =
                parse_u256_hex(sp.get("value").and_then(Value::as_str).unwrap_or("0x0"))
                    .map_err(ViewError::Unavailable)?;
            let proof_arr = sp
                .get("proof")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut slot_proof = Vec::with_capacity(proof_arr.len());
            for node in proof_arr {
                let s = node
                    .as_str()
                    .ok_or_else(|| ViewError::Unavailable("non-string storageProof node".into()))?;
                slot_proof.push(
                    decode_hex_lenient(s).map_err(|e| {
                        ViewError::Unavailable(format!("bad storageProof hex: {e}"))
                    })?,
                );
            }

            let proven_val = if !acc.exists || acc.storage_root == mpt::empty_root() {
                [0u8; 32]
            } else {
                match mpt::verify_proof(acc.storage_root, key, &slot_proof).map_err(|e| {
                    ViewError::Unavailable(format!(
                        "PROOF FRAUD / MISMATCH: storage proof failed against storageRoot {}: {e}",
                        encode_hex(&acc.storage_root)
                    ))
                })? {
                    None => [0u8; 32],
                    Some(raw_slot) => {
                        let inner = mpt::rlp_bytes_value(&raw_slot).map_err(|e| {
                            ViewError::Unavailable(format!("storage leaf RLP decode: {e}"))
                        })?;
                        bytes_to_u256(&inner).map_err(ViewError::Unavailable)?
                    }
                }
            };
            if proven_val != claimed_val {
                return Err(ViewError::Unavailable(
                    "PROOF FRAUD: upstream storage slot value does not match proven MPT leaf"
                        .into(),
                ));
            }
            let vs = VerifiedSlot {
                value: proven_val,
                proof: slot_proof,
            };
            if acc.exists && acc.storage_root != mpt::empty_root() {
                self.store
                    .put_verified_slot(acc.storage_root, *key, vs.clone());
            }
            verified_slots.push(vs);
        }

        self.store
            .put_verified_account(evm_root, *address, acc.clone());
        Ok((acc, verified_slots))
    }

    /// Fetch contract bytecode and cryptographically verify `keccak(code) == code_hash`.
    pub fn verify_code(
        &self,
        block_number: u64,
        address: &Address,
        code_hash: Hash,
    ) -> Result<Vec<u8>, ViewError> {
        if code_hash == EMPTY_CODE_HASH || code_hash == [0u8; 32] {
            return Ok(Vec::new());
        }
        if let Some(cached) = self.store.get_verified_code(&code_hash) {
            return Ok(cached);
        }
        let res = self
            .upstream
            .call(
                "quai_getCode",
                json!([encode_hex(address), format!("0x{block_number:x}")]),
            )
            .map_err(|e| ViewError::Unavailable(format!("upstream quai_getCode: {e}")))?;
        let hex = res
            .as_str()
            .ok_or_else(|| ViewError::Unavailable("upstream quai_getCode non-string".into()))?;
        let code = decode_hex_lenient(hex)
            .map_err(|e| ViewError::Unavailable(format!("bad code hex: {e}")))?;
        let actual_hash = keccak(&code);
        if actual_hash != code_hash {
            return Err(ViewError::Unavailable(format!(
                "PROOF FRAUD: bytecode hash {} does not match proven codeHash {}",
                encode_hex(&actual_hash),
                encode_hex(&code_hash)
            )));
        }
        self.store.put_verified_code(code_hash, code.clone());
        Ok(code)
    }

    /// Fetch and cryptographically verify the full block transaction list
    /// against the locally validated header's `tx_hash`.
    pub fn verify_block_txs(
        &self,
        block: &BlockData,
    ) -> Result<Arc<VerifiedBlockPayload>, ViewError> {
        if let Some(cached) = self.store.get_verified_payload(&block.hash) {
            return Ok(cached);
        }
        let expected_tx_root = hash_bytes(
            block
                .header
                .wire()
                .tx_hash
                .as_ref()
                .map(|h| h.value.as_slice()),
        );
        if expected_tx_root == mpt::empty_root() {
            let payload = Arc::new(VerifiedBlockPayload {
                transactions: vec![],
                tx_hashes: vec![],
                receipts: None,
            });
            self.store.put_verified_payload(block.hash, payload.clone());
            return Ok(payload);
        }
        // If our local BlockData already carries the body transactions, verify them directly.
        if !block.object.transactions().is_empty() {
            let txs = block.object.transactions().to_vec();
            let computed = work::transactions_root(&txs)
                .map_err(|e| ViewError::Unavailable(format!("tx root: {e}")))?;
            if computed != expected_tx_root {
                return Err(ViewError::Unavailable(
                    "PROOF FRAUD: block transactions do not match header txHash".into(),
                ));
            }
            let payload = Arc::new(VerifiedBlockPayload {
                transactions: txs,
                tx_hashes: block.tx_hashes.clone(),
                receipts: None,
            });
            self.store.put_verified_payload(block.hash, payload.clone());
            return Ok(payload);
        }
        // Fetch raw transactions by index from upstream RPC and verify root.
        let count_val = self
            .upstream
            .call(
                "quai_getBlockTransactionCountByHash",
                json!([encode_hex(&block.hash)]),
            )
            .map_err(|e| ViewError::Unavailable(format!("upstream tx count: {e}")))?;
        let count = count_val
            .as_str()
            .ok_or_else(|| ViewError::Unavailable("missing tx count".into()))
            .and_then(|s| parse_u64_hex(s).map_err(ViewError::Unavailable))?;
        let mut txs = Vec::with_capacity(count as usize);
        let mut tx_hashes = Vec::with_capacity(count as usize);
        for i in 0..count {
            let raw_hex = self
                .upstream
                .call(
                    "quai_getRawTransactionByBlockHashAndIndex",
                    json!([encode_hex(&block.hash), format!("0x{i:x}")]),
                )
                .map_err(|e| ViewError::Unavailable(format!("upstream raw tx #{i}: {e}")))?;
            let bytes = raw_hex
                .as_str()
                .ok_or_else(|| ViewError::Unavailable("non-string raw tx".into()))
                .and_then(|s| decode_hex_lenient(s).map_err(ViewError::Unavailable))?;
            let tx = Transaction::decode(&bytes)
                .map_err(|e| ViewError::Unavailable(format!("decode upstream tx #{i}: {e}")))?;
            let id = tx
                .id(None)
                .map_err(|e| ViewError::Unavailable(format!("hash upstream tx #{i}: {e}")))?;
            tx_hashes.push(id);
            txs.push(tx);
        }
        let computed = work::transactions_root(&txs)
            .map_err(|e| ViewError::Unavailable(format!("tx root: {e}")))?;
        if computed != expected_tx_root {
            return Err(ViewError::Unavailable(format!(
                "PROOF FRAUD: fetched transactions root {} != header txHash {}",
                encode_hex(&computed),
                encode_hex(&expected_tx_root)
            )));
        }
        let payload = Arc::new(VerifiedBlockPayload {
            transactions: txs,
            tx_hashes,
            receipts: None,
        });
        self.store.put_verified_payload(block.hash, payload.clone());
        Ok(payload)
    }

    /// Fetch and cryptographically verify a block's receipts against the locally
    /// validated header's `receiptHash`.
    pub fn verify_block_receipts(
        &self,
        block: &BlockData,
    ) -> Result<Arc<Vec<ReceiptData>>, ViewError> {
        if let Some(cached) = self.store.get_verified_payload(&block.hash) {
            if let Some(r) = &cached.receipts {
                return Ok(r.clone());
            }
        }
        let expected_receipt_root = hash_opt(&block.object.header().wire().receipt_hash);
        if expected_receipt_root == mpt::empty_root() {
            let empty = Arc::new(Vec::new());
            let mut base = self
                .store
                .get_verified_payload(&block.hash)
                .map(|p| (*p).clone())
                .unwrap_or(VerifiedBlockPayload {
                    transactions: block.object.transactions().to_vec(),
                    tx_hashes: block.tx_hashes.clone(),
                    receipts: None,
                });
            base.receipts = Some(empty.clone());
            self.store.put_verified_payload(block.hash, Arc::new(base));
            return Ok(empty);
        }
        let payload = self.verify_block_txs(block)?;
        let mut consensus_receipts = Vec::new();
        let mut rpc_receipts = Vec::new();
        let mut prev_cum_gas = 0u64;
        for (idx, tx_hash) in payload.tx_hashes.iter().enumerate() {
            let res = self
                .upstream
                .call("quai_getTransactionReceipt", json!([encode_hex(tx_hash)]))
                .map_err(|e| ViewError::Unavailable(format!("upstream receipt: {e}")))?;
            if res.is_null() {
                // External transactions that produce no receipt are skipped in receipt trie.
                continue;
            }
            let status = parse_u64_hex(res.get("status").and_then(Value::as_str).unwrap_or("0x0"))
                .map_err(ViewError::Unavailable)?;
            let cum_gas = parse_u64_hex(
                res.get("cumulativeGasUsed")
                    .and_then(Value::as_str)
                    .unwrap_or("0x0"),
            )
            .map_err(ViewError::Unavailable)?;
            let gas_used =
                parse_u64_hex(res.get("gasUsed").and_then(Value::as_str).unwrap_or("0x0"))
                    .unwrap_or_else(|_| cum_gas.saturating_sub(prev_cum_gas));
            prev_cum_gas = cum_gas;
            let bloom_bytes = res
                .get("logsBloom")
                .and_then(Value::as_str)
                .and_then(|s| decode_hex_lenient(s).ok())
                .filter(|b| b.len() == BLOOM_BYTES)
                .unwrap_or_else(|| vec![0u8; BLOOM_BYTES]);
            let mut logs = Vec::new();
            let mut rpc_logs = Vec::new();
            if let Some(arr) = res.get("logs").and_then(Value::as_array) {
                for l in arr {
                    let addr_b = decode_hex_lenient(
                        l.get("address").and_then(Value::as_str).unwrap_or("0x"),
                    )
                    .map_err(ViewError::Unavailable)?;
                    let mut addr = [0u8; 20];
                    if addr_b.len() == 20 {
                        addr.copy_from_slice(&addr_b);
                    }
                    let mut topics = Vec::new();
                    if let Some(ts) = l.get("topics").and_then(Value::as_array) {
                        for t in ts {
                            if let Some(s) = t.as_str() {
                                topics.push(parse_hash_hex(s).map_err(ViewError::Unavailable)?);
                            }
                        }
                    }
                    let data =
                        decode_hex_lenient(l.get("data").and_then(Value::as_str).unwrap_or("0x"))
                            .map_err(ViewError::Unavailable)?;
                    logs.push(Log {
                        address: addr,
                        topics: topics.clone(),
                        data: data.clone(),
                    });
                    rpc_logs.push(LogData {
                        address: addr,
                        topics,
                        data,
                    });
                }
            }
            let tx_type = payload
                .transactions
                .get(idx)
                .map(|t| match t.kind() {
                    quai_primitives::transaction::Kind::Quai(_) => 0u8,
                    quai_primitives::transaction::Kind::External(_) => 1u8,
                    quai_primitives::transaction::Kind::Qi(_) => 2u8,
                })
                .unwrap_or(0u8);
            let contract_address = res
                .get("contractAddress")
                .and_then(Value::as_str)
                .and_then(|s| decode_hex_lenient(s).ok())
                .and_then(|b| <[u8; 20]>::try_from(b).ok());
            consensus_receipts.push(Receipt {
                transaction_type: tx_type,
                post_state: vec![],
                status,
                cumulative_gas_used: cum_gas,
                bloom: bloom_bytes,
                logs,
                outbound_etxs: vec![],
            });
            rpc_receipts.push(ReceiptData {
                post_state: vec![],
                status,
                cumulative_gas_used: cum_gas,
                gas_used,
                tx_hash: *tx_hash,
                contract_address,
                logs: rpc_logs,
                outbound_etxs: vec![],
            });
        }
        let computed_root = receipts_root(&consensus_receipts)
            .map_err(|e| ViewError::Unavailable(format!("receipts root: {e}")))?;
        if computed_root != expected_receipt_root {
            return Err(ViewError::Unavailable(format!(
                "PROOF FRAUD: fetched receipts root {} != header receiptHash {}",
                encode_hex(&computed_root),
                encode_hex(&expected_receipt_root)
            )));
        }
        let arc_receipts = Arc::new(rpc_receipts);
        let mut updated = (*payload).clone();
        updated.receipts = Some(arc_receipts.clone());
        self.store
            .put_verified_payload(block.hash, Arc::new(updated));
        Ok(arc_receipts)
    }
}

fn hash_opt(h: &Option<quai_primitives::schema::common::ProtoHash>) -> Hash {
    hash_bytes(h.as_ref().map(|h| h.value.as_slice()))
}

fn hash_bytes(b: Option<&[u8]>) -> Hash {
    let mut out = [0u8; 32];
    if let Some(h) = b {
        let take = h.len().min(32);
        out[32 - take..].copy_from_slice(&h[h.len() - take..]);
    }
    out
}

/// Proof-verified [`AccountState`] anchored to one locally validated header's
/// `evmRoot`.
pub struct LightAccountState {
    block_number: u64,
    evm_root: Hash,
    verifier: Option<ProofVerifier>,
}

impl LightAccountState {
    fn verifier(&self) -> Result<&ProofVerifier, ViewError> {
        self.verifier.as_ref().ok_or_else(|| {
            ViewError::Unavailable(
                "light client requires --upstream-rpc <URL> to fetch and verify MPT state proofs"
                    .into(),
            )
        })
    }
}

impl AccountState for LightAccountState {
    fn balance(&self, address: &Address) -> Result<U256, ViewError> {
        if self.evm_root == mpt::empty_root() {
            return Ok([0u8; 32]);
        }
        let (acc, _) = self.verifier()?.verify_account_and_slots(
            self.block_number,
            self.evm_root,
            address,
            &[],
        )?;
        Ok(acc.balance)
    }

    fn nonce(&self, address: &Address) -> Result<u64, ViewError> {
        if self.evm_root == mpt::empty_root() {
            return Ok(0);
        }
        let (acc, _) = self.verifier()?.verify_account_and_slots(
            self.block_number,
            self.evm_root,
            address,
            &[],
        )?;
        Ok(acc.nonce)
    }

    fn code(&self, address: &Address) -> Result<Vec<u8>, ViewError> {
        if self.evm_root == mpt::empty_root() {
            return Ok(Vec::new());
        }
        let v = self.verifier()?;
        let (acc, _) =
            v.verify_account_and_slots(self.block_number, self.evm_root, address, &[])?;
        if !acc.exists {
            return Ok(Vec::new());
        }
        v.verify_code(self.block_number, address, acc.code_hash)
    }

    fn storage(&self, address: &Address, key: &Hash) -> Result<Hash, ViewError> {
        if self.evm_root == mpt::empty_root() {
            return Ok([0u8; 32]);
        }
        let (_, slots) = self.verifier()?.verify_account_and_slots(
            self.block_number,
            self.evm_root,
            address,
            &[*key],
        )?;
        Ok(slots.first().map_or([0u8; 32], |s| s.value))
    }

    fn size(&self, address: &Address) -> Result<U256, ViewError> {
        if self.evm_root == mpt::empty_root() {
            return Ok([0u8; 32]);
        }
        let (acc, _) = self.verifier()?.verify_account_and_slots(
            self.block_number,
            self.evm_root,
            address,
            &[],
        )?;
        Ok(acc.storage_size)
    }

    fn proof(&self, address: &Address, keys: &[Hash]) -> Result<WorldProof, ViewError> {
        if self.evm_root == mpt::empty_root() {
            return Ok(WorldProof {
                account: vec![],
                storage: None,
            });
        }
        let (acc, slots) = self.verifier()?.verify_account_and_slots(
            self.block_number,
            self.evm_root,
            address,
            keys,
        )?;
        let storage = if acc.exists {
            Some(StorageProof {
                root: acc.storage_root,
                code_hash: acc.code_hash,
                keys: slots.into_iter().map(|s| (s.value, s.proof)).collect(),
            })
        } else {
            None
        };
        Ok(WorldProof {
            account: acc.account_proof,
            storage,
        })
    }
}

/// Trustless light-client [`ChainView`] over [`LightHeaderStore`] + optional
/// upstream [`ProofVerifier`] + optional [`Miner`].
pub struct LightView {
    store: Arc<LightHeaderStore>,
    caches: Arc<SharedCaches>,
    verifier: Option<ProofVerifier>,
    miner: RwLock<Option<Arc<Miner>>>,
    peers_cb: RwLock<Option<Box<dyn Fn() -> PeerCounts + Send + Sync>>>,
    node_info_cb: RwLock<Option<Box<dyn Fn() -> Value + Send + Sync>>>,
    peers_info_cb: RwLock<Option<Box<dyn Fn() -> Value + Send + Sync>>>,
    add_peer_cb: RwLock<Option<crate::rpc_view::PeerDialCallback>>,
    syncing_cb: RwLock<Option<Box<dyn Fn() -> Value + Send + Sync>>>,
}

impl LightView {
    pub fn new(
        store: Arc<LightHeaderStore>,
        caches: Arc<SharedCaches>,
        upstream: Option<HttpRpcClient>,
    ) -> Arc<Self> {
        let verifier = upstream.map(|u| ProofVerifier::new(store.clone(), u));
        Arc::new(Self {
            store,
            caches,
            verifier,
            miner: RwLock::new(None),
            peers_cb: RwLock::new(None),
            node_info_cb: RwLock::new(None),
            peers_info_cb: RwLock::new(None),
            add_peer_cb: RwLock::new(None),
            syncing_cb: RwLock::new(None),
        })
    }

    pub fn store(&self) -> &Arc<LightHeaderStore> {
        &self.store
    }

    pub fn caches(&self) -> &Arc<SharedCaches> {
        &self.caches
    }

    pub fn set_miner(&self, miner: Arc<Miner>) {
        *self.miner.write().unwrap_or_else(|e| e.into_inner()) = Some(miner);
    }

    pub fn set_peer_counter(&self, cb: Box<dyn Fn() -> PeerCounts + Send + Sync>) {
        *self.peers_cb.write().unwrap_or_else(|e| e.into_inner()) = Some(cb);
    }

    pub fn set_p2p_node_info(&self, cb: Box<dyn Fn() -> Value + Send + Sync>) {
        *self.node_info_cb.write().unwrap_or_else(|e| e.into_inner()) = Some(cb);
    }

    pub fn set_p2p_peers_info(&self, cb: Box<dyn Fn() -> Value + Send + Sync>) {
        *self
            .peers_info_cb
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(cb);
    }

    pub fn set_p2p_add_peer(&self, cb: crate::rpc_view::PeerDialCallback) {
        *self.add_peer_cb.write().unwrap_or_else(|e| e.into_inner()) = Some(cb);
    }

    pub fn set_syncing(&self, cb: Box<dyn Fn() -> Value + Send + Sync>) {
        *self.syncing_cb.write().unwrap_or_else(|e| e.into_inner()) = Some(cb);
    }

    fn miner_ref(&self) -> Result<Arc<Miner>, ViewError> {
        self.miner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| {
                ViewError::Unavailable("mining is not configured on this light node".into())
            })
    }

    /// Build a cryptographically proven sparse [`World`] for a single `quai_call`
    /// / `eth_call` / `quai_estimateGas` request against `block`'s `evmRoot`.
    fn build_proven_world(
        &self,
        block: &BlockData,
        request: &CallRequest,
    ) -> Result<World, CallError> {
        let evm_root = hash_opt(&block.object.header().wire().evm_root);
        let mut world = World::default();
        if evm_root == mpt::empty_root() {
            return Ok(world);
        }
        let verifier = self.verifier.as_ref().ok_or_else(|| {
            CallError::View(ViewError::Unavailable(
                "light client call requires --upstream-rpc <URL>".into(),
            ))
        })?;

        // Collect addresses & storage keys to cryptographically prove:
        // 1) Always prove `from`, `to`, and explicit `request.access_list`.
        let mut targets: BTreeMap<Address, BTreeSet<Hash>> = BTreeMap::new();
        targets.entry(request.from).or_default();
        if let Some(to) = request.to {
            targets.entry(to).or_default();
        }
        for tuple in &request.access_list {
            let set = targets.entry(tuple.address).or_default();
            for k in &tuple.keys {
                set.insert(*k);
            }
        }

        // 2) Ask upstream `quai_createAccessList` strictly as an UNTRUSTED HINT
        //    for which accounts & storage slots a contract call touches. Every
        //    hinted address/slot/code is cryptographically verified via MPT proofs
        //    against our locally PoW-validated `evmRoot` below!
        if request.to.is_some() {
            let mut call_obj = serde_json::Map::new();
            call_obj.insert("from".into(), Value::String(encode_hex(&request.from)));
            if let Some(to) = request.to {
                call_obj.insert("to".into(), Value::String(encode_hex(&to)));
            }
            if request.gas > 0 {
                call_obj.insert("gas".into(), Value::String(format!("0x{:x}", request.gas)));
            }
            if request.value != [0u8; 32] {
                let v = crypto_bigint::U256::from_be_bytes(request.value);
                call_obj.insert("value".into(), Value::String(format!("0x{v:x}")));
            }
            if !request.data.is_empty() {
                call_obj.insert("data".into(), Value::String(encode_hex(&request.data)));
            }
            if let Ok(al_res) = verifier.upstream().call(
                "quai_createAccessList",
                json!([Value::Object(call_obj), format!("0x{:x}", block.number)]),
            ) {
                if let Some(list) = al_res.get("accessList").and_then(Value::as_array) {
                    for entry in list {
                        if let Some(addr_s) = entry.get("address").and_then(Value::as_str) {
                            if let Ok(addr_b) = decode_hex_lenient(addr_s) {
                                if let Ok(addr) = <[u8; 20]>::try_from(addr_b) {
                                    let set = targets.entry(addr).or_default();
                                    if let Some(keys) =
                                        entry.get("storageKeys").and_then(Value::as_array)
                                    {
                                        for k in keys {
                                            if let Some(ks) = k.as_str() {
                                                if let Ok(kh) = parse_hash_hex(ks) {
                                                    set.insert(kh);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 3) Prove every account, storage slot, and contract bytecode against `evm_root`.
        for (addr, key_set) in targets {
            let keys: Vec<Hash> = key_set.into_iter().collect();
            let (acc, slots) = verifier
                .verify_account_and_slots(block.number, evm_root, &addr, &keys)
                .map_err(CallError::View)?;
            if !acc.exists {
                continue;
            }
            let code = verifier
                .verify_code(block.number, &addr, acc.code_hash)
                .map_err(CallError::View)?;
            let mut storage = BTreeMap::new();
            for (k, vs) in keys.into_iter().zip(slots) {
                if vs.value != [0u8; 32] {
                    storage.insert(k, vs.value);
                }
            }
            world
                .insert(
                    addr,
                    Account {
                        nonce: acc.nonce,
                        balance: from_bytes(&acc.balance),
                        code,
                        storage,
                        storage_size: from_bytes(&acc.storage_size),
                        backing: None,
                    },
                )
                .map_err(|e| CallError::View(ViewError::Unavailable(e.to_string())))?;
        }
        Ok(world)
    }

    fn build_evm_context(&self, block: &BlockData) -> evm::Context {
        let w = block.header.wire();
        let body = block.object.header().wire();
        let mut coinbase = [0u8; 20];
        if let Some(c) = &w.primary_coinbase {
            if c.value.len() == 20 {
                coinbase.copy_from_slice(&c.value);
            }
        }
        let parent_time = if block.number > 0 {
            self.store
                .block_by_number(block.number - 1)
                .and_then(|p| p.header.wire().time)
                .unwrap_or_else(|| w.time.unwrap_or(0))
        } else {
            w.time.unwrap_or(0)
        };
        evm::Context {
            address: [0; 20],
            caller: [0; 20],
            origin: [0; 20],
            coinbase,
            etx_sender: [0; 20],
            location: self.store.chain().location(),
            tx_type: 0,
            value: Word::ZERO,
            gas_price: Word::ZERO,
            block_number: Word::from_u64(block.number),
            timestamp: Word::from_u64(parent_time),
            difficulty: from_bytes(w.difficulty.as_deref().unwrap_or_default()),
            base_fee: from_bytes(body.base_fee.as_deref().unwrap_or_default()),
            chain_id: Word::from_u64(self.store.chain().spec().chain_id),
            state_size: from_bytes(body.quai_state_size.as_deref().unwrap_or_default()),
            block_gas_limit: body
                .gas_limit
                .unwrap_or(self.store.chain().spec().genesis.gas_limit),
            address_listed: false,
            warm_slots: BTreeSet::new(),
            read_only: false,
            trace: false,
        }
    }

    fn build_evm_environment(&self, block: &BlockData) -> Environment {
        let mut block_hashes = BTreeMap::new();
        let start = block.number.saturating_sub(256);
        for n in start..block.number {
            if let Some(h) = self.store.canonical_hash(n) {
                block_hashes.insert(n, h);
            }
        }
        let prime_number = quai_consensus::share_rules::number(
            block
                .header
                .wire()
                .prime_terminus_number
                .as_deref()
                .unwrap_or_default(),
        )
        .ok();
        Environment {
            prime_number,
            eligible_slices: Some(hash_opt(&block.object.header().wire().etx_eligible_slices)),
            block_hashes,
            delete_empty: true,
            ..Default::default()
        }
    }
}

impl ChainView for LightView {
    fn chain_id(&self) -> u64 {
        self.store.chain().spec().chain_id
    }

    fn location(&self) -> [u8; 2] {
        self.store.chain().location()
    }

    fn head(&self) -> u64 {
        self.store.head().0
    }

    fn block_by_number(&self, number: u64) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok(self.store.block_by_number(number))
    }

    fn block_by_hash(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok(self.store.block_by_hash(hash))
    }

    fn canonical_hash(&self, number: u64) -> Result<Option<Hash>, ViewError> {
        Ok(self.store.canonical_hash(number))
    }

    fn uncle_kinds(&self, block: &BlockData) -> Result<Arc<Vec<UncleKind>>, ViewError> {
        Ok(Arc::new(self.store.classify_uncles(block, &self.caches)))
    }

    fn total_entropy(&self, block: &BlockData) -> Result<U256, ViewError> {
        Ok(self.store.total_entropy(&block.hash).unwrap_or([0u8; 32]))
    }

    fn transaction(&self, hash: &Hash) -> Result<Option<TxPosition>, ViewError> {
        if let Some((block_hash, block_number, index)) = self.store.tx_position(hash) {
            return Ok(Some(TxPosition {
                block_hash,
                block_number,
                index,
            }));
        }
        // Ask upstream for tx location, then cryptographically verify the containing
        // block's `transactionsRoot` against our locally validated header!
        let Some(v) = &self.verifier else {
            return Ok(None);
        };
        let Ok(res) = v
            .upstream()
            .call("quai_getTransactionByHash", json!([encode_hex(hash)]))
        else {
            return Ok(None);
        };
        let Some(bh_str) = res.get("blockHash").and_then(Value::as_str) else {
            return Ok(None);
        };
        let Ok(bh) = parse_hash_hex(bh_str) else {
            return Ok(None);
        };
        let Some(block) = self.store.block_by_hash(&bh) else {
            return Ok(None);
        };
        let payload = v.verify_block_txs(&block)?;
        for (i, txh) in payload.tx_hashes.iter().enumerate() {
            if txh == hash {
                return Ok(Some(TxPosition {
                    block_hash: bh,
                    block_number: block.number,
                    index: i as u64,
                }));
            }
        }
        Ok(None)
    }

    fn receipts(&self, block: &BlockData) -> Result<Arc<Vec<ReceiptData>>, ViewError> {
        let Some(v) = &self.verifier else {
            return Err(ViewError::Unavailable(
                "light client requires --upstream-rpc <URL> to fetch and verify receipts".into(),
            ));
        };
        v.verify_block_receipts(block)
    }

    fn state(&self, block: &BlockData) -> Result<Arc<dyn AccountState>, ViewError> {
        let evm_root = hash_opt(&block.object.header().wire().evm_root);
        Ok(Arc::new(LightAccountState {
            block_number: block.number,
            evm_root,
            verifier: self.verifier.clone(),
        }))
    }

    fn utxo(&self, _tx_hash: &Hash, _index: u16) -> Result<Option<UtxoData>, ViewError> {
        Err(ViewError::Unavailable(
            "UTXO proof queries are not yet supported over upstream RPC".into(),
        ))
    }

    fn outpoints_by_address(&self, _address: &Address) -> Result<Vec<OutpointData>, ViewError> {
        Err(ViewError::Unavailable(
            "UTXO index is not kept by a light client".into(),
        ))
    }

    fn outpoints_by_address_and_range(
        &self,
        _address: &Address,
        _start: u32,
        _end: u32,
    ) -> Result<Vec<OutpointData>, ViewError> {
        Err(ViewError::Unavailable(
            "UTXO index is not kept by a light client".into(),
        ))
    }

    fn call(&self, block: &BlockData, request: &CallRequest) -> Result<CallResult, CallError> {
        self.call_cancellable(block, request, &Arc::new(AtomicBool::new(false)))
    }

    fn call_cancellable(
        &self,
        block: &BlockData,
        request: &CallRequest,
        cancel: &Arc<AtomicBool>,
    ) -> Result<CallResult, CallError> {
        let world = self.build_proven_world(block, request)?;
        let ctx = self.build_evm_context(block);
        let env = self.build_evm_environment(block);
        rpc_call::run_cancellable(
            &world,
            &ctx,
            &env,
            request,
            self.head(),
            Some(cancel.clone()),
        )
    }

    fn submit_transaction(&self, raw: &[u8]) -> Result<Hash, ViewError> {
        let tx = Transaction::decode(raw)
            .map_err(|e| ViewError::Go(format!("invalid transaction: {e}")))?;
        let expected_hash = tx
            .id(None)
            .map_err(|e| ViewError::Go(format!("transaction hash: {e}")))?;
        let Some(v) = &self.verifier else {
            return Err(ViewError::Unavailable(
                "light client requires --upstream-rpc <URL> to broadcast transactions".into(),
            ));
        };
        let res = v
            .upstream()
            .call("quai_sendRawTransaction", json!([encode_hex(raw)]))
            .map_err(ViewError::Go)?;
        if let Some(s) = res.as_str() {
            if let Ok(h) = parse_hash_hex(s) {
                return Ok(h);
            }
        }
        Ok(expected_hash)
    }

    fn head_events(&self) -> Option<tokio::sync::broadcast::Receiver<Hash>> {
        Some(self.store.head_events())
    }

    fn chain_events(&self) -> Option<tokio::sync::broadcast::Receiver<Hash>> {
        Some(self.store.chain_events())
    }

    fn peer_counts(&self) -> PeerCounts {
        self.peers_cb
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|f| f())
            .unwrap_or_default()
    }

    fn syncing(&self) -> Value {
        self.syncing_cb
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|f| f())
            .unwrap_or(Value::Bool(false))
    }

    fn admin_node_info(&self) -> Result<Value, ViewError> {
        if let Some(f) = self
            .node_info_cb
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return Ok(f());
        }
        Ok(json!({
            "name": format!("quai-rust-light/{}", env!("CARGO_PKG_VERSION")),
            "protocols": {
                "quai": {
                    "mode": "light",
                    "chainId": self.chain_id(),
                    "location": self.location(),
                    "head": self.head(),
                }
            }
        }))
    }

    fn admin_peers(&self) -> Result<Value, ViewError> {
        if let Some(f) = self
            .peers_info_cb
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return Ok(f());
        }
        Ok(json!([]))
    }

    fn admin_add_peer(&self, multiaddr: &str) -> Result<bool, ViewError> {
        if let Some(f) = self
            .add_peer_cb
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            return f(multiaddr).map_err(ViewError::Go);
        }
        Err(ViewError::Unavailable(
            "peer dialing is not configured on this light view".into(),
        ))
    }

    // --- Miner delegation ---
    fn is_mining(&self) -> bool {
        self.miner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|m| m.is_mining())
    }
    fn hashrate(&self) -> u64 {
        self.miner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(0, |m| m.hashrate())
    }
    fn primary_coinbase(&self) -> Result<Address, ViewError> {
        Ok(self.miner_ref()?.primary_coinbase())
    }
    fn miner_start(&self) -> Result<bool, ViewError> {
        self.miner_ref()?.start_mining();
        Ok(true)
    }
    fn miner_set_extra(&self, extra: &[u8]) -> Result<bool, ViewError> {
        self.miner_ref()?
            .set_extra(extra.to_vec())
            .map_err(ViewError::Go)?;
        Ok(true)
    }
    fn miner_set_gas_limit(&self, limit: u64) -> Result<bool, ViewError> {
        self.miner_ref()?.set_gas_limit(limit);
        Ok(true)
    }
    fn miner_set_gas_price(&self, price: U256) -> Result<bool, ViewError> {
        self.miner_ref()?
            .set_gas_price(crypto_bigint::U256::from_be_bytes(price));
        Ok(true)
    }
    fn miner_set_lockup_byte(&self, lock: u8) -> Result<bool, ViewError> {
        self.miner_ref()?
            .set_lockup_byte(lock)
            .map_err(ViewError::Go)?;
        Ok(true)
    }
    fn miner_set_miner_preference(&self, pref: f64) -> Result<bool, ViewError> {
        self.miner_ref()?
            .set_miner_preference(pref)
            .map_err(ViewError::Go)?;
        Ok(true)
    }
    fn miner_set_primary_coinbase(&self, addr: Address) -> Result<bool, ViewError> {
        self.miner_ref()?.set_primary_coinbase(addr);
        Ok(true)
    }
    fn miner_set_recommit_interval(&self, ms: u64) -> Result<(), ViewError> {
        self.miner_ref()?.set_recommit_interval(ms);
        Ok(())
    }
    fn get_block_template(&self, params: Option<&Value>) -> Result<Value, ViewError> {
        let parsed = crate::miner::BlockTemplateParams::from_json(params).map_err(ViewError::Go)?;
        self.miner_ref()?
            .get_block_template(&parsed)
            .map_err(ViewError::Go)
    }
    fn get_pending_header(&self) -> Result<Vec<u8>, ViewError> {
        let m = self.miner_ref()?;
        let wo = m
            .generate_pending_header(crate::miner::POW_KAWPOW, None, &[], None)
            .map_err(ViewError::Go)?;
        Ok(wo.encode())
    }
    fn get_pending_workshares(&self) -> Result<Value, ViewError> {
        Ok(self
            .miner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or_else(
                || json!({"kawpow": [], "sha": [], "scrypt": []}),
                |m| m.pending_workshares_json(),
            ))
    }
    fn submit_block(&self, raw: &[u8], pow_id: u8) -> Result<Value, ViewError> {
        let out = self
            .miner_ref()?
            .submit_block(raw, pow_id)
            .map_err(ViewError::Go)?;
        Ok(json!({
            "hash": encode_hex(&out.hash),
            "number": format!("0x{:x}", out.number),
            "status": out.status,
        }))
    }
    fn receive_mined_header(&self, raw: &[u8]) -> Result<(), ViewError> {
        self.miner_ref()?
            .receive_mined_header(raw)
            .map_err(ViewError::Go)
    }
    fn receive_raw_workshare(&self, raw: &[u8]) -> Result<(), ViewError> {
        self.miner_ref()?
            .receive_raw_workshare(raw)
            .map(|_| ())
            .map_err(ViewError::Go)
    }
    fn receive_sub_workshare(&self, raw: &[u8]) -> Result<(), ViewError> {
        self.miner_ref()?
            .receive_sub_workshare(raw)
            .map(|_| ())
            .map_err(ViewError::Go)
    }
    fn calc_order(&self, raw: &[u8]) -> Result<u8, ViewError> {
        self.miner_ref()?.calc_order(raw).map_err(ViewError::Go)
    }
    fn workshare_threshold(&self) -> u16 {
        self.miner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(quai_primitives::spec::WORKSHARES_THRESHOLD_DIFF, |m| {
                m.workshare_threshold()
            })
    }
    fn workshare_p2p_threshold(&self) -> u16 {
        self.miner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(quai_primitives::spec::WORKSHARE_P2P_THRESHOLD_DIFF, |m| {
                m.workshare_p2p_threshold()
            })
    }
    fn set_workshare_p2p_threshold(&self, threshold: u16) -> Result<(), ViewError> {
        self.miner_ref()?
            .set_workshare_p2p_threshold(threshold)
            .map_err(ViewError::Go)
    }
    fn submit_aux_template(&self, raw: &[u8]) -> Result<(), ViewError> {
        self.miner_ref()?
            .submit_aux_template(raw)
            .map_err(ViewError::Go)
    }
    fn subsidy_chain_height(&self) -> Result<Value, ViewError> {
        self.miner_ref()?
            .subsidy_chain_height()
            .map_err(ViewError::Go)
    }
    fn workshare_lru_dump(&self, limit: usize) -> Result<Value, ViewError> {
        Ok(self
            .miner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or_else(
                || json!({"receivedCount": 0, "minedCount": 0, "received": [], "mined": []}),
                |m| m.workshare_lru_dump(limit),
            ))
    }
    fn pending_header_events(&self) -> Option<tokio::sync::broadcast::Receiver<Vec<u8>>> {
        self.miner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|m| m.subscribe_pending_header())
    }
    fn template_update_events(&self) -> Option<tokio::sync::broadcast::Receiver<u8>> {
        self.miner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|m| m.subscribe_template_updates())
    }
    fn workshare_events(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<Arc<quai_primitives::work::WorkObject>>> {
        self.miner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|m| m.subscribe_workshares())
    }
    fn compute_kawpow_pow_hash(&self, header: &quai_primitives::Header) -> Result<Hash, ViewError> {
        crate::uncles::pow_hash(&self.caches, header).map_err(|e| ViewError::Go(e.to_string()))
    }
}
