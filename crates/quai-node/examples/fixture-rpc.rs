//! Serve the test chain of fixtures/protocol/filters.json with the native
//! RPC (HTTP and WebSocket) on a loopback port, and move its head every
//! INTERVAL milliseconds through b3, b4, b7, the reorg to c6, then c7, c8
//! and back to b8 (each move publishes a head event, as `quai-node run`
//! does). For client tests: Quais.js filters and subscriptions against the
//! Rust node (sdk/fixture-subscriptions.cjs). The chain has no state; total
//! entropy is zero and every uncle counts as a block uncle.
//!
//!     cargo run --release -p quai-node --example fixture-rpc -- [ADDR] [INTERVAL_MS]
use quai_consensus::verifier::PINNED_SCHEDULE;
use quai_node::rpc_view::decode_receipts;
use quai_primitives::{decode_hex, work::WorkObject, Hash};
use quai_rpc::view::{
    AccountState, BlockData, CallError, CallRequest, CallResult, ChainView, OutpointData,
    ReceiptData, TxPosition, UncleKind, UtxoData, ViewError, U256,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

struct View {
    blocks: HashMap<Hash, Arc<BlockData>>,
    receipts: HashMap<Hash, Arc<Vec<ReceiptData>>>,
    canonical: Mutex<BTreeMap<u64, Hash>>,
    events: broadcast::Sender<Hash>,
}
fn unavailable() -> ViewError {
    ViewError::Unavailable("not in the fixture".into())
}
impl View {
    fn set_head(&self, head: Hash) {
        let mut canonical = BTreeMap::new();
        let mut cur = self.blocks.get(&head).cloned();
        while let Some(b) = cur {
            canonical.insert(b.number, b.hash);
            let parent: Hash = b
                .header
                .wire()
                .parent_hash
                .as_ref()
                .map(|h| h.value.as_slice().try_into().unwrap())
                .unwrap_or_default();
            cur = if b.number == 0 {
                None
            } else {
                self.blocks.get(&parent).cloned()
            };
        }
        *self.canonical.lock().unwrap() = canonical;
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
        *self.canonical.lock().unwrap().keys().last().unwrap()
    }
    fn block_by_number(&self, number: u64) -> Result<Option<Arc<BlockData>>, ViewError> {
        let h = self.canonical.lock().unwrap().get(&number).copied();
        Ok(h.and_then(|h| self.blocks.get(&h).cloned()))
    }
    fn block_by_hash(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        let Some(b) = self.blocks.get(hash) else {
            return Ok(None);
        };
        let canonical = self.canonical.lock().unwrap().get(&b.number) == Some(hash);
        Ok(canonical.then(|| b.clone()))
    }
    fn block_or_candidate(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok(self.blocks.get(hash).cloned())
    }
    fn uncle_kinds(&self, b: &BlockData) -> Result<Arc<Vec<UncleKind>>, ViewError> {
        Ok(Arc::new(vec![UncleKind::Block; b.uncles.len()]))
    }
    fn total_entropy(&self, _: &BlockData) -> Result<U256, ViewError> {
        Ok([0; 32])
    }
    fn transaction(&self, hash: &Hash) -> Result<Option<TxPosition>, ViewError> {
        let canonical = self.canonical.lock().unwrap().clone();
        for (n, h) in canonical.iter().rev() {
            if let Some(i) = self.blocks[h].tx_hashes.iter().position(|t| t == hash) {
                return Ok(Some(TxPosition {
                    block_hash: *h,
                    block_number: *n,
                    index: i as u64,
                }));
            }
        }
        Ok(None)
    }
    fn receipts(&self, block: &BlockData) -> Result<Arc<Vec<ReceiptData>>, ViewError> {
        self.receipts
            .get(&block.hash)
            .cloned()
            .ok_or_else(unavailable)
    }
    fn state(&self, _: &BlockData) -> Result<Arc<dyn AccountState>, ViewError> {
        Err(unavailable())
    }
    fn utxo(&self, _: &Hash, _: u16) -> Result<Option<UtxoData>, ViewError> {
        Ok(None)
    }
    fn outpoints_by_address(&self, _: &[u8; 20]) -> Result<Vec<OutpointData>, ViewError> {
        Ok(vec![])
    }
    fn outpoints_by_address_and_range(
        &self,
        _: &[u8; 20],
        _: u32,
        _: u32,
    ) -> Result<Vec<OutpointData>, ViewError> {
        Ok(vec![])
    }
    fn call(&self, _: &BlockData, _: &CallRequest) -> Result<CallResult, CallError> {
        Err(CallError::View(unavailable()))
    }
    fn head_events(&self) -> Option<broadcast::Receiver<Hash>> {
        Some(self.events.subscribe())
    }
}

fn raw(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let addr = args.first().cloned().unwrap_or("127.0.0.1:0".into());
    let interval: u64 = args.get(1).map_or(1500, |v| v.parse().unwrap());
    let c: Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/filters.json")).unwrap();
    let mut blocks = HashMap::new();
    let mut receipts = HashMap::new();
    let mut names = HashMap::new();
    for (name, v) in c["chain"]["blocks"].as_object().unwrap() {
        let bytes = raw(&v["object"]);
        let object = WorkObject::decode_auxiliary(&bytes, PINNED_SCHEDULE).unwrap();
        let header = object.auxiliary_work_header(PINNED_SCHEDULE).unwrap();
        let uncles = object.uncle_headers(PINNED_SCHEDULE).unwrap();
        let hash: Hash = raw(&v["hash"]).try_into().unwrap();
        if !v["receipts"].is_null() {
            receipts.insert(
                hash,
                Arc::new(decode_receipts(&raw(&v["receipts"])).unwrap()),
            );
        }
        let hashes = |k: &str| -> Vec<Hash> {
            v[k].as_array()
                .unwrap()
                .iter()
                .map(|h| raw(h).try_into().unwrap())
                .collect()
        };
        blocks.insert(
            hash,
            Arc::new(BlockData {
                hash,
                number: v["number"].as_u64().unwrap(),
                object,
                header,
                uncles,
                size: v["size"].as_u64().unwrap(),
                order: 2,
                tx_hashes: hashes("tx_hashes"),
                etx_hashes: hashes("etx_hashes"),
            }),
        );
        names.insert(name.clone(), hash);
    }
    let (events, _) = broadcast::channel(64);
    let view = Arc::new(View {
        blocks,
        receipts,
        canonical: Mutex::new(BTreeMap::new()),
        events,
    });
    view.set_head(names["b3"]);
    let app = quai_rpc::App::native(
        quai_rpc::Config {
            mode: "native".into(),
            ..quai_rpc::Config::default()
        },
        view.clone(),
    )
    .unwrap();
    app.filter_api();
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    let local = listener.local_addr().unwrap();
    println!("{}", json!({"ready": true, "address": local.to_string()}));
    let server = tokio::spawn(quai_rpc::serve_until(app, listener, async {
        let _ = tokio::signal::ctrl_c().await;
    }));
    for name in ["b4", "b7", "c6", "c7", "c8", "b8"] {
        tokio::time::sleep(Duration::from_millis(interval)).await;
        let h = names[name];
        view.set_head(h);
        let _ = view.events.send(h);
        println!(
            "{}",
            json!({"head": name, "number": view.head(), "hash": quai_primitives::encode_hex(&h)})
        );
    }
    let _ = server.await;
}
