//! Versioned materialized Quai reads. Go supplies validated source data.
//! Import validates index consistency, not consensus or cryptographic commitments.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::Path,
    sync::{Arc, RwLock},
};

mod logs;
pub(crate) mod slice;
pub use logs::ReadMiss;

const MAX_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Identity {
    pub chain_id: String,
    pub genesis_hash: String,
    pub location: Vec<u8>,
}

#[derive(Deserialize)]
struct Envelope {
    sha256: String,
    payload: Box<serde_json::value::RawValue>,
}

#[derive(Deserialize)]
struct Export {
    schema: u32,
    identity: Identity,
    #[serde(default)]
    base_number: u64,
    #[serde(default)]
    anchor: Option<Anchor>,
    canonical: Vec<String>,
    blocks: Vec<Value>,
    receipts: Vec<Value>,
    receipts_complete: bool,
}

#[derive(Deserialize)]
struct Anchor {
    number: u64,
    hash: String,
}

struct Block {
    number: u64,
    parent: String,
    full: Vec<u8>,
    hashes: Vec<u8>,
    transactions: Vec<String>,
    transaction_types: Vec<Value>,
}

struct Receipt {
    logs: Vec<Value>,
    block: String,
    encoded: Vec<u8>,
}

pub struct Snapshot {
    pub identity: Identity,
    blocks: HashMap<String, Block>,
    canonical: BTreeMap<u64, String>,
    receipts: HashMap<(String, String), Receipt>,
    transaction_blocks: HashMap<String, String>,
    complete: bool,
    history_complete: bool,
    log_index: logs::Index,
}

fn hash(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value.as_bytes()[2..]
            .iter()
            .all(|v| v.is_ascii_hexdigit() && !v.is_ascii_uppercase())
}

fn quantity(value: &str) -> Option<u64> {
    let digits = value.strip_prefix("0x")?;
    if digits.is_empty() || digits.len() > 16 || (digits.len() > 1 && digits.starts_with('0')) {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field: {key}"))
}

/// Encode a schema export using an exact-byte SHA-256 envelope.
pub fn seal_export(value: &Value) -> Result<Vec<u8>, String> {
    let payload = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let digest = format!("{:x}", Sha256::digest(&payload));
    Ok(format!(
        "{{\"sha256\":\"{digest}\",\"payload\":{}}}\n",
        String::from_utf8(payload).map_err(|e| e.to_string())?
    )
    .into_bytes())
}

impl Snapshot {
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        use std::io::Read;
        if file.metadata().map_err(|e| e.to_string())?.len() > MAX_BYTES {
            return Err("snapshot exceeds 128 MiB".into());
        }
        (&mut file)
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("snapshot exceeds 128 MiB".into());
        }
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err("snapshot exceeds 128 MiB".into());
        }
        let envelope: Envelope = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let digest = format!("{:x}", Sha256::digest(envelope.payload.get().as_bytes()));
        if digest != envelope.sha256 {
            return Err("snapshot SHA-256 mismatch".into());
        }
        let export: Export =
            serde_json::from_str(envelope.payload.get()).map_err(|e| e.to_string())?;
        if export.schema != 1
            || !hash(&export.identity.genesis_hash)
            || quantity(&export.identity.chain_id).is_none()
            || export.identity.location.len() != 2
            || export.identity.location.iter().any(|v| *v > 15)
            || export.canonical.is_empty()
            || export.blocks.len() > 100_000
            || export.receipts.len() > 1_000_000
        {
            return Err("invalid schema, identity or record limits".into());
        }
        let mut blocks = HashMap::new();
        for value in export.blocks {
            let key = field(&value, "hash")?.to_owned();
            let header = value.get("woHeader").ok_or("missing woHeader")?;
            let parent = field(header, "parentHash")?.to_owned();
            let number = quantity(field(header, "number")?).ok_or("invalid block number")?;
            if !hash(&key) || !hash(&parent) || field(header, "hash")? != key {
                return Err("inconsistent block hash".into());
            }
            let mut hashes = value.clone();
            let mut transactions = Vec::new();
            let mut transaction_types = Vec::new();
            for name in ["transactions", "outboundEtxs"] {
                let entries = value
                    .get(name)
                    .and_then(Value::as_array)
                    .ok_or("missing transaction array")?;
                let mut ids = Vec::new();
                for entry in entries {
                    let tx = field(entry, "hash")?;
                    if !hash(tx) {
                        return Err("invalid transaction hash".into());
                    }
                    ids.push(json!(tx));
                    if name == "transactions" {
                        transactions.push(tx.to_owned());
                        let kind = field(entry, "type")?;
                        if !["0x0", "0x1", "0x2"].contains(&kind) {
                            return Err("unsupported transaction type".into());
                        }
                        transaction_types.push(json!(kind));
                    }
                }
                hashes[name] = Value::Array(ids);
            }
            let block = Block {
                number,
                parent,
                transactions,
                transaction_types,
                full: serde_json::to_vec(&value).map_err(|e| e.to_string())?,
                hashes: serde_json::to_vec(&hashes).map_err(|e| e.to_string())?,
            };
            if blocks.insert(key, block).is_some() {
                return Err("duplicate block hash".into());
            }
        }
        let first = blocks
            .get(&export.canonical[0])
            .ok_or("first canonical block missing")?;
        if export.base_number > 0 {
            let anchor = export
                .anchor
                .as_ref()
                .ok_or("partial export requires parent anchor")?;
            if anchor.number != export.base_number - 1
                || !hash(&anchor.hash)
                || anchor.hash != first.parent
            {
                return Err("invalid partial export anchor".into());
            }
        } else if export.anchor.is_some() {
            return Err("genesis export cannot have an anchor".into());
        }
        let mut canonical = BTreeMap::new();
        let mut previous: Option<String> = None;
        let mut canonical_transactions = HashMap::new();
        for (height, key) in export.canonical.into_iter().enumerate() {
            let block = blocks.get(&key).ok_or("canonical block missing")?;
            let expected_height = export
                .base_number
                .checked_add(height as u64)
                .ok_or("height overflow")?;
            if block.number != expected_height
                || (expected_height == 0 && key != export.identity.genesis_hash)
                || previous.as_ref().is_some_and(|p| *p != block.parent)
            {
                return Err("canonical chain gap, parent mismatch or wrong genesis".into());
            }
            for tx in &block.transactions {
                if canonical_transactions
                    .insert(tx.clone(), key.clone())
                    .is_some()
                {
                    return Err("duplicate canonical transaction".into());
                }
            }
            canonical.insert(expected_height, key.clone());
            previous = Some(key);
        }
        let mut receipts = HashMap::new();
        for value in export.receipts {
            let tx = field(&value, "transactionHash")?.to_owned();
            let key = field(&value, "blockHash")?.to_owned();
            let block = blocks.get(&key).ok_or("receipt block missing")?;
            let position = quantity(field(&value, "transactionIndex")?)
                .ok_or("invalid receipt index")? as usize;
            if !hash(&tx)
                || block.transactions.get(position) != Some(&tx)
                || quantity(field(&value, "blockNumber")?) != Some(block.number)
                || value.get("type") != block.transaction_types.get(position)
            {
                return Err("receipt/transaction index mismatch".into());
            }
            let receipt = Receipt {
                logs: logs::receipt_logs(&value)?,
                block: key,
                encoded: serde_json::to_vec(&value).map_err(|e| e.to_string())?,
            };
            if receipts
                .insert((tx, receipt.block.clone()), receipt)
                .is_some()
            {
                return Err("duplicate receipt transaction".into());
            }
        }
        if export.receipts_complete
            && canonical_transactions
                .iter()
                .any(|(tx, block)| !receipts.contains_key(&(tx.clone(), block.clone())))
        {
            return Err("incomplete canonical receipts".into());
        }
        let log_index = if export.receipts_complete {
            logs::Index::build(&canonical, &blocks, &receipts)?
        } else {
            logs::Index::default()
        };
        Ok(Self {
            log_index,
            identity: export.identity,
            blocks,
            canonical,
            receipts,
            transaction_blocks: canonical_transactions,
            complete: export.receipts_complete,
            history_complete: export.base_number == 0,
        })
    }

    pub fn head(&self) -> (&u64, &String) {
        self.canonical
            .last_key_value()
            .expect("validated canonical chain")
    }

    /// Live hybrid mode serves only immutable block-hash reads. Frozen mode has
    /// an explicit indexed canonical view, including block numbers and receipts.
    pub fn lookup(&self, method: &str, params: &Value, frozen: bool) -> Option<Vec<u8>> {
        let args: &[Value] = match params {
            Value::Null => &[],
            Value::Array(values) => values,
            _ => return None,
        };
        let value = match method {
            "quai_getBlockByHash" if args.len() == 2 => {
                let key = args[0].as_str()?;
                let full = args[1].as_bool()?;
                let block = self.blocks.get(key)?;
                return Some(if full {
                    block.full.clone()
                } else {
                    block.hashes.clone()
                });
            }
            "quai_getBlockByNumber" if frozen && args.len() == 2 => {
                let number = match args[0].as_str()? {
                    "latest" => *self.head().0,
                    "earliest" => 0,
                    n => quantity(n)?,
                };
                let full = args[1].as_bool()?;
                let block = self.blocks.get(self.canonical.get(&number)?)?;
                return Some(if full {
                    block.full.clone()
                } else {
                    block.hashes.clone()
                });
            }
            "quai_getTransactionReceipt" if frozen && args.len() == 1 => {
                let tx = args[0].as_str()?;
                if !hash(tx) {
                    return None;
                }
                if let Some(receipt) = self
                    .transaction_blocks
                    .get(tx)
                    .and_then(|block| self.receipts.get(&(tx.to_owned(), block.clone())))
                {
                    let block = self.blocks.get(&receipt.block)?;
                    if self.canonical.get(&block.number) == Some(&receipt.block) {
                        return Some(receipt.encoded.clone());
                    }
                }
                if self.complete && self.history_complete {
                    Value::Null
                } else {
                    return None;
                }
            }
            "quai_getLogs" if frozen => {
                return self.indexed_read(method, params, 10 * 1024 * 1024).ok()
            }
            "quai_chainId" if frozen && args.is_empty() => json!(self.identity.chain_id),
            "quai_blockNumber" if frozen && args.is_empty() => {
                json!(format!("0x{:x}", self.head().0))
            }
            "quai_nodeLocation" if frozen && args.is_empty() => json!(self
                .identity
                .location
                .iter()
                .map(|v| format!("0x{v:x}"))
                .collect::<Vec<_>>()),
            "quai_listRunningChains" if frozen && args.is_empty() => {
                json!([self.identity.location])
            }
            _ => return None,
        };
        serde_json::to_vec(&value).ok()
    }

    pub fn info(&self) -> Value {
        json!({"identity":self.identity,"head_number":self.head().0,"head_hash":self.head().1,
            "blocks":self.blocks.len(),"indexed_logs":self.log_index.len(),"receipts":self.receipts.len(),"receipts_complete":self.complete,"history_complete":self.history_complete,"first_indexed_number":self.canonical.first_key_value().map(|v| v.0),
            "consensus_verified_by_rust":false})
    }
}

pub struct Store {
    current: RwLock<(Arc<Snapshot>, u64)>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        Ok(Self {
            current: RwLock::new((Arc::new(Snapshot::load(path)?), 1)),
        })
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.current.read().expect("store lock").0.clone()
    }
    pub fn reload(&self, path: &Path) -> Result<(), String> {
        self.replace(Snapshot::load(path)?)
    }
    pub fn replace(&self, snapshot: Snapshot) -> Result<(), String> {
        let next = Arc::new(snapshot);
        let mut current = self.current.write().map_err(|_| "store lock")?;
        if current.0.identity != next.identity {
            return Err("snapshot network identity changed".into());
        }
        *current = (next, current.1 + 1);
        Ok(())
    }
    pub fn info(&self) -> Value {
        let current = self.current.read().expect("store lock");
        let mut info = current.0.info();
        info["generation"] = json!(current.1);
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn h(n: u8) -> String {
        format!("0x{n:064x}")
    }
    fn sealed(value: &Value) -> Vec<u8> {
        let payload = serde_json::to_vec(value).unwrap();
        let digest = format!("{:x}", Sha256::digest(&payload));
        format!(
            "{{\"sha256\":\"{digest}\",\"payload\":{}}}",
            String::from_utf8(payload).unwrap()
        )
        .into_bytes()
    }
    fn fixture() -> Value {
        json!({"schema":1,"identity":{"chain_id":"0x539","genesis_hash":h(1),"location":[0,0]},
        "canonical":[h(1),h(2)],"blocks":[
          {"hash":h(1),"woHeader":{"hash":h(1),"number":"0x0","parentHash":h(0)},"transactions":[],"outboundEtxs":[]},
          {"hash":h(2),"woHeader":{"hash":h(2),"number":"0x1","parentHash":h(1)},"transactions":[{"hash":h(3),"type":"0x2","custom":"kept"}],"outboundEtxs":[]}],
        "receipts":[{"transactionHash":h(3),"blockHash":h(2),"blockNumber":"0x1","transactionIndex":"0x0","type":"0x2","logs":[]}],"receipts_complete":true})
    }
    #[test]
    fn query_and_live_policy() {
        let store = Snapshot::from_bytes(&sealed(&fixture())).unwrap();
        let value: Value = serde_json::from_slice(
            &store
                .lookup("quai_getBlockByHash", &json!([h(2), false]), false)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["transactions"], json!([h(3)]));
        assert!(store
            .lookup("quai_getTransactionReceipt", &json!([h(3)]), false)
            .is_none());
        assert!(store
            .lookup("quai_getBlockByNumber", &json!(["latest", true]), false)
            .is_none());
        assert!(store
            .lookup("quai_getTransactionReceipt", &json!([h(3)]), true)
            .is_some());
        let full: Value = serde_json::from_slice(
            &store
                .lookup("quai_getBlockByHash", &json!([h(2), true]), true)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(full["transactions"][0]["custom"], "kept");
    }
    #[test]
    fn anchored_partial_export_does_not_invent_missing_receipts() {
        let mut data = fixture();
        data["base_number"] = json!(1);
        data["anchor"] = json!({"number":0,"hash":h(1)});
        data["canonical"] = json!([h(2)]);
        let store = Snapshot::from_bytes(&sealed(&data)).unwrap();
        assert!(store
            .lookup("quai_getTransactionReceipt", &json!([h(3)]), true)
            .is_some());
        assert!(store
            .lookup("quai_getTransactionReceipt", &json!([h(99)]), true)
            .is_none());
        data["anchor"]["hash"] = json!(h(99));
        assert!(Snapshot::from_bytes(&sealed(&data)).is_err());
    }
    #[test]
    fn same_transaction_can_be_reincluded_after_reorg() {
        let mut data = fixture();
        let mut branch = data["blocks"][1].clone();
        branch["hash"] = json!(h(9));
        branch["woHeader"]["hash"] = json!(h(9));
        data["blocks"].as_array_mut().unwrap().push(branch);
        let mut receipt = data["receipts"][0].clone();
        receipt["blockHash"] = json!(h(9));
        data["receipts"].as_array_mut().unwrap().push(receipt);
        data["canonical"] = json!([h(1), h(9)]);
        let store = Snapshot::from_bytes(&sealed(&data)).unwrap();
        let response: Value = serde_json::from_slice(
            &store
                .lookup("quai_getTransactionReceipt", &json!([h(3)]), true)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["blockHash"], h(9));
    }
    #[test]
    fn rejects_changed_payload_without_new_checksum() {
        let encoded = String::from_utf8(sealed(&fixture())).unwrap();
        let corrupted = encoded.replace("kept", "lost");
        assert!(
            matches!(Snapshot::from_bytes(corrupted.as_bytes()), Err(message) if message.contains("SHA-256"))
        );
    }
    #[test]
    fn rejects_corrupt_exports() {
        let original = fixture();
        let mut variants = Vec::new();
        let mut v = original.clone();
        v["canonical"] = json!([h(2)]);
        variants.push(v);
        let mut v = original.clone();
        v["blocks"][1]["woHeader"]["parentHash"] = json!(h(9));
        variants.push(v);
        let mut v = original.clone();
        v["blocks"][1]["hash"] = json!(h(9));
        variants.push(v);
        let mut v = original.clone();
        v["receipts"] = json!([]);
        variants.push(v);
        let mut v = original.clone();
        v["receipts"][0]["transactionIndex"] = json!("0x1");
        variants.push(v);
        let mut v = original.clone();
        v["receipts"][0]["type"] = json!("0x0");
        variants.push(v);
        for v in variants {
            assert!(Snapshot::from_bytes(&sealed(&v)).is_err());
        }
    }
    fn log_fixture() -> Value {
        let mut data = fixture();
        let logs=(0..3).map(|i|json!({"address":format!("0x{}","12".repeat(20)),"topics":[h(4),h(5)],"data":"0xab","blockHash":h(2),"blockNumber":"0x1","transactionHash":h(3),"transactionIndex":"0x0","logIndex":format!("0x{i:x}"),"removed":false})).collect::<Vec<_>>();
        data["receipts"][0]["logs"] = json!(logs);
        data
    }
    #[test]
    fn indexed_logs_preserve_objects_duplicates_and_filter_semantics() {
        let data = log_fixture();
        let store = Snapshot::from_bytes(&sealed(&data)).unwrap();
        let address = format!("0x{}", "12".repeat(20));
        for filter in [
            json!({"nodeLocation":[0,0]}),
            json!({}),
            json!({"blockHash":h(2),"address":[address.clone(),address.clone()],"topics":[[h(4),h(9)],null]}),
            json!({"fromBlock":"0x0","toBlock":"latest","topics":[[],[null,"ignored invalid topic"]]}),
        ] {
            let logs: Value = serde_json::from_slice(
                &store
                    .indexed_read("quai_getLogs", &json!([filter]), 100000)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(logs, data["receipts"][0]["logs"]);
        }
        for filter in [
            json!({"topics":[h(9)]}),
            json!({"topics":[null,null,null]}),
            json!({"address":format!("0x{}","34".repeat(20))}),
        ] {
            assert_eq!(
                store
                    .indexed_read("quai_getLogs", &json!([filter]), 100000)
                    .unwrap(),
                b"[]"
            );
        }
        assert_eq!(
            store.indexed_read("quai_getLogs", &json!([{"topics":[[[h(4)]]]}]), 100000),
            Err(ReadMiss::Unsupported)
        );
        assert_eq!(
            store.indexed_read("quai_getLogs", &json!([{}]), 10),
            Err(ReadMiss::Budget)
        );
    }
    #[test]
    fn invalid_log_metadata_never_publishes_an_index() {
        let data = log_fixture();
        for (key, value) in [
            ("blockHash", json!(h(9))),
            ("transactionHash", json!(h(9))),
            ("transactionIndex", json!("0x2")),
            ("logIndex", json!("0x3")),
            ("removed", json!(true)),
            ("data", json!("0xa")),
            ("address", json!("0x12")),
            ("topics", json!([h(1), h(2), h(3), h(4), h(5)])),
        ] {
            let mut altered = data.clone();
            altered["receipts"][0]["logs"][0][key] = value;
            assert!(Snapshot::from_bytes(&sealed(&altered)).is_err(), "{key}");
        }
    }
    #[test]
    fn indexed_reads_respect_partial_coverage_and_no_live_nulls() {
        let mut data = log_fixture();
        data["base_number"] = json!(1);
        data["anchor"] = json!({"number":0,"hash":h(1)});
        data["canonical"] = json!([h(2)]);
        let store = Snapshot::from_bytes(&sealed(&data)).unwrap();
        assert!(store
            .indexed_read("quai_getTransactionReceipt", &json!([h(3)]), 100000)
            .is_ok());
        assert_eq!(
            store.indexed_read("quai_getTransactionReceipt", &json!([h(99)]), 100000),
            Err(ReadMiss::Coverage)
        );
        assert_eq!(
            store.indexed_read("quai_getLogs", &json!([{"fromBlock":"earliest"}]), 100000),
            Err(ReadMiss::Coverage)
        );
        data["receipts_complete"] = json!(false);
        data["receipts"] = json!([]);
        let store = Snapshot::from_bytes(&sealed(&data)).unwrap();
        assert_eq!(
            store.indexed_read("quai_getLogs", &json!([{}]), 100000),
            Err(ReadMiss::Coverage)
        );
    }
    #[test]
    fn multi_block_log_order_is_explicitly_delegated() {
        let mut data = log_fixture();
        let block = json!({"hash":h(6),"woHeader":{"hash":h(6),"number":"0x2","parentHash":h(2)},"transactions":[{"hash":h(7),"type":"0x0"}],"outboundEtxs":[]});
        data["canonical"].as_array_mut().unwrap().push(json!(h(6)));
        data["blocks"].as_array_mut().unwrap().push(block);
        let mut receipt = data["receipts"][0].clone();
        receipt["transactionHash"] = json!(h(7));
        receipt["blockHash"] = json!(h(6));
        receipt["blockNumber"] = json!("0x2");
        receipt["type"] = json!("0x0");
        for log in receipt["logs"].as_array_mut().unwrap() {
            log["transactionHash"] = json!(h(7));
            log["blockHash"] = json!(h(6));
            log["blockNumber"] = json!("0x2");
        }
        data["receipts"].as_array_mut().unwrap().push(receipt);
        let store = Snapshot::from_bytes(&sealed(&data)).unwrap();
        assert_eq!(
            store.indexed_read(
                "quai_getLogs",
                &json!([{"fromBlock":"0x1","toBlock":"0x2"}]),
                100000
            ),
            Err(ReadMiss::Order)
        );
        assert!(store
            .indexed_read(
                "quai_getLogs",
                &json!([{"fromBlock":"0x1","toBlock":"0x1"}]),
                100000
            )
            .is_ok());
    }
}
pub mod journal;

pub mod state_db;

pub mod admission;
