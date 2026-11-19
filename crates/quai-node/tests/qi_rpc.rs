//! The Qi and conversion methods against go-quai v0.57.0's production
//! handlers (fixtures/protocol/qi-rpc.json, scripts/gen_qi_rpc_vectors.py):
//! `quai_qiToQuai`, `quai_quaiToQi`, `quai_calculateConversionAmount`,
//! `quai_estimateFeeForQi`, `quai_getOutpointDeltasForAddressesInRange`,
//! `quai_getLockedBalance`, `quai_getWrappedQiDeposit` and
//! `quai_listRunningChains`. Each scenario gives Go's database: a head, a
//! canonical index, the UTXO set size of the head, the UTXO set, the spent
//! and trimmed journals, the locked-Quai index and the head's storage. The
//! Rust answers come from the native JSON-RPC handler (`App::handle`) over a
//! view of the same data, and must equal Go's whole response.
use quai_consensus::verifier::PINNED_SCHEDULE;
use quai_primitives::{decode_hex, work::WorkObject, Hash};
use quai_rpc::view::{
    AccountState, BlockData, CallError, CallRequest, CallResult, ChainView, OutpointData,
    ReceiptData, SpentUtxo, TxPosition, UncleKind, UtxoData, ViewError, U256,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

type Address = [u8; 20];

fn corpus() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/qi-rpc.json")).unwrap()
}
fn raw(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
fn fixed<const N: usize>(v: &Value) -> [u8; N] {
    raw(v).try_into().unwrap()
}
fn trimmed(v: &Value) -> Vec<u8> {
    let b = raw(v);
    b[b.iter().position(|x| *x != 0).unwrap_or(b.len())..].to_vec()
}
fn hashes(v: &Value) -> Vec<Hash> {
    v.as_array().unwrap().iter().map(fixed).collect()
}
fn block(v: &Value) -> Arc<BlockData> {
    let bytes = raw(&v["object"]);
    let object = WorkObject::decode_auxiliary(&bytes, PINNED_SCHEDULE).unwrap();
    let header = object.auxiliary_work_header(PINNED_SCHEDULE).unwrap();
    let uncles = object.uncle_headers(PINNED_SCHEDULE).unwrap();
    Arc::new(BlockData {
        hash: fixed(&v["hash"]),
        number: v["number"].as_u64().unwrap(),
        object,
        header,
        uncles,
        size: v["size"].as_u64().unwrap(),
        order: 2,
        tx_hashes: hashes(&v["tx_hashes"]),
        etx_hashes: hashes(&v["etx_hashes"]),
    })
}

/// The head's committed storage, or an empty state.
struct Storage(HashMap<(Address, Hash), Hash>);
impl AccountState for Storage {
    fn balance(&self, _: &Address) -> Result<U256, ViewError> {
        Ok([0; 32])
    }
    fn nonce(&self, _: &Address) -> Result<u64, ViewError> {
        Ok(0)
    }
    fn code(&self, _: &Address) -> Result<Vec<u8>, ViewError> {
        Ok(vec![])
    }
    fn storage(&self, a: &Address, k: &Hash) -> Result<Hash, ViewError> {
        Ok(self.0.get(&(*a, *k)).copied().unwrap_or([0; 32]))
    }
}

struct View {
    blocks: Arc<HashMap<Hash, Arc<BlockData>>>,
    canonical: BTreeMap<u64, Hash>,
    head: Hash,
    utxo_set_size: HashMap<Hash, u64>,
    utxos: Vec<(Hash, u16, UtxoData)>,
    spent: HashMap<Hash, Vec<SpentUtxo>>,
    locked: HashMap<Address, Vec<u8>>,
    storage: HashMap<(Address, Hash), Hash>,
}
fn unavailable() -> ViewError {
    ViewError::Unavailable("not in the fixture".into())
}
fn spent_list(v: &Value) -> Vec<SpentUtxo> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| SpentUtxo {
            tx_hash: fixed(&s["tx_hash"]),
            index: s["index"].as_u64().unwrap() as u16,
            denomination: s["denomination"].as_u64().unwrap() as u8,
            address: fixed(&s["address"]),
            lock: trimmed(&s["lock"]),
        })
        .collect()
}
impl View {
    fn new(
        blocks: &Arc<HashMap<Hash, Arc<BlockData>>>,
        names: &HashMap<String, Hash>,
        s: &Value,
    ) -> Self {
        let by_name = |n: &Value| names[n.as_str().unwrap()];
        let mut spent: HashMap<Hash, Vec<SpentUtxo>> = HashMap::new();
        for key in ["spent", "trimmed"] {
            for (name, list) in s[key].as_object().unwrap() {
                spent
                    .entry(names[name])
                    .or_default()
                    .extend(spent_list(list));
            }
        }
        Self {
            blocks: blocks.clone(),
            canonical: s["canonical"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(n, b)| (n.parse().unwrap(), by_name(b)))
                .collect(),
            head: by_name(&s["head"]),
            utxo_set_size: s["utxo_set_size"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(n, v)| (names[n], v.as_u64().unwrap()))
                .collect(),
            utxos: s["utxos"]
                .as_array()
                .unwrap()
                .iter()
                .map(|u| {
                    (
                        fixed(&u["tx_hash"]),
                        u["index"].as_u64().unwrap() as u16,
                        UtxoData {
                            denomination: u["denomination"].as_u64().unwrap() as u8,
                            address: fixed(&u["address"]),
                            lock: trimmed(&u["lock"]),
                        },
                    )
                })
                .collect(),
            spent,
            locked: s["locked_quai"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(a, v)| (fixed(&Value::String(a.clone())), trimmed(v)))
                .collect(),
            storage: s["storage"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| ((fixed(&e["address"]), fixed(&e["key"])), fixed(&e["value"])))
                .collect(),
        }
    }
}
impl ChainView for View {
    fn chain_id(&self) -> u64 {
        9
    }
    fn location(&self) -> [u8; 2] {
        [0, 0]
    }
    fn head(&self) -> u64 {
        self.blocks[&self.head].number
    }
    fn block_by_number(&self, number: u64) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok(self
            .canonical
            .get(&number)
            .and_then(|h| self.blocks.get(h))
            .cloned())
    }
    fn block_by_hash(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok(self
            .blocks
            .get(hash)
            .filter(|b| self.canonical.get(&b.number) == Some(hash))
            .cloned())
    }
    fn block_or_candidate(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok(self.blocks.get(hash).cloned())
    }
    fn uncle_kinds(&self, _: &BlockData) -> Result<Arc<Vec<UncleKind>>, ViewError> {
        Err(unavailable())
    }
    fn total_entropy(&self, _: &BlockData) -> Result<U256, ViewError> {
        Err(unavailable())
    }
    fn transaction(&self, _: &Hash) -> Result<Option<TxPosition>, ViewError> {
        Ok(None)
    }
    fn receipts(&self, _: &BlockData) -> Result<Arc<Vec<ReceiptData>>, ViewError> {
        Err(unavailable())
    }
    fn state(&self, block: &BlockData) -> Result<Arc<dyn AccountState>, ViewError> {
        Ok(Arc::new(Storage(if block.hash == self.head {
            self.storage.clone()
        } else {
            HashMap::new()
        })))
    }
    fn utxo(&self, tx_hash: &Hash, index: u16) -> Result<Option<UtxoData>, ViewError> {
        Ok(self
            .utxos
            .iter()
            .find(|(h, i, _)| h == tx_hash && *i == index)
            .map(|(_, _, u)| u.clone()))
    }
    fn outpoints_by_address(&self, address: &Address) -> Result<Vec<OutpointData>, ViewError> {
        Ok(self
            .utxos
            .iter()
            .filter(|(_, _, u)| u.address == *address)
            .map(|(h, i, u)| OutpointData {
                tx_hash: *h,
                index: *i,
                denomination: u.denomination,
                lock: u.lock.clone(),
            })
            .collect())
    }
    fn outpoints_by_address_and_range(
        &self,
        _: &Address,
        _: u32,
        _: u32,
    ) -> Result<Vec<OutpointData>, ViewError> {
        Ok(vec![])
    }
    fn call(&self, _: &BlockData, _: &CallRequest) -> Result<CallResult, CallError> {
        Err(CallError::View(unavailable()))
    }
    fn utxo_set_size(&self, block: &BlockData) -> Result<u64, ViewError> {
        Ok(self.utxo_set_size.get(&block.hash).copied().unwrap_or(0))
    }
    fn spent_utxos(&self, block: &BlockData) -> Result<Vec<SpentUtxo>, ViewError> {
        Ok(self.spent.get(&block.hash).cloned().unwrap_or_default())
    }
    fn locked_quai(&self, address: &Address) -> Result<U256, ViewError> {
        let v = self.locked.get(address).cloned().unwrap_or_default();
        let mut out = [0u8; 32];
        out[32 - v.len()..].copy_from_slice(&v);
        Ok(out)
    }
}

#[test]
fn qi_and_conversion_methods_match_go() {
    let c = corpus();
    assert_eq!(
        c["reference_revision"],
        "33db82f2891b267ba898ca38bd2dd8bf75cbb072"
    );
    let mut names = HashMap::new();
    let mut blocks = HashMap::new();
    for (name, b) in c["chain"]["blocks"].as_object().unwrap() {
        let b = block(b);
        names.insert(name.clone(), b.hash);
        blocks.insert(b.hash, b);
    }
    let blocks = Arc::new(blocks);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (mut total, mut failed) = (0, vec![]);
    let mut per_method: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for s in c["scenarios"].as_array().unwrap() {
        let view: Arc<dyn ChainView> = Arc::new(View::new(&blocks, &names, s));
        let app = quai_rpc::App::native(
            quai_rpc::Config {
                mode: "native".into(),
                ..quai_rpc::Config::default()
            },
            view,
        )
        .unwrap();
        for call in s["calls"].as_array().unwrap() {
            total += 1;
            let body = serde_json::to_vec(&call["request"]).unwrap();
            let got: Value =
                serde_json::from_slice(&runtime.block_on(app.handle(&body)).expect("an answer"))
                    .unwrap();
            let method = call["request"]["method"]
                .as_str()
                .unwrap_or("?")
                .to_string();
            let entry = per_method.entry(method).or_default();
            entry.0 += 1;
            if got == call["response"] {
                entry.1 += 1;
            } else {
                failed.push(format!(
                    "{} {}: want {} got {}",
                    s["name"], call["request"], call["response"], got
                ));
            }
        }
    }
    for (m, (n, ok)) in &per_method {
        println!("{m}: {ok}/{n}");
    }
    assert!(
        failed.is_empty(),
        "{} of {total} differ:\n{}",
        failed.len(),
        failed.join("\n")
    );
    assert_eq!(total, 385);
}
