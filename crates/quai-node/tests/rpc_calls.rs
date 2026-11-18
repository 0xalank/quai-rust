//! `quai_call`, `quai_estimateGas`, `quai_createAccessList` and
//! `quai_gasPrice` against go-quai v0.57.0's production handlers
//! (fixtures/protocol/rpc-calls.json, scripts/gen_rpc_call_vectors.py).
//! Each scenario is a committed state, the block context that
//! `core.NewEVMBlockContext` made, the head, parent and prime terminus work
//! objects, and Go's JSON answers. The Rust answers come from the native
//! handlers (`quai_rpc::native::dispatch`) over a view whose calls run in
//! `quai_node::rpc_call::run`, as the export view's calls do.
use quai_consensus::verifier::PINNED_SCHEDULE;
use quai_execution::evm::{
    self,
    calls::Environment,
    state::{Account, World},
    word::{self, Word},
};
use quai_primitives::{decode_hex, work::WorkObject, Hash};
use quai_rpc::view::{
    AccountState, BlockData, CallError, CallRequest, CallResult, ChainView, OutpointData,
    ReceiptData, TxPosition, UncleKind, UtxoData, ViewError, U256,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

fn corpus() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/rpc-calls.json")).unwrap()
}
fn raw(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
fn fixed<const N: usize>(v: &Value) -> [u8; N] {
    raw(v).try_into().unwrap()
}
fn word(v: &Value) -> Word {
    word::from_bytes(&raw(v))
}

fn block(v: &Value) -> Arc<BlockData> {
    let bytes = raw(&v["object"]);
    let object = WorkObject::decode_auxiliary(&bytes, PINNED_SCHEDULE).unwrap();
    let header = object.auxiliary_work_header(PINNED_SCHEDULE).unwrap();
    Arc::new(BlockData {
        hash: fixed(&v["hash"]),
        number: v["number"].as_u64().unwrap(),
        object,
        header,
        uncles: vec![],
        size: bytes.len() as u64,
        order: 2,
        tx_hashes: vec![],
        etx_hashes: vec![],
    })
}

fn world(v: &Value) -> World {
    let mut world = World::default();
    for a in v.as_array().unwrap() {
        let mut storage = BTreeMap::new();
        for s in a["storage"].as_array().unwrap() {
            storage.insert(fixed(&s["key"]), fixed(&s["value"]));
        }
        world
            .insert(
                fixed(&a["address"]),
                Account {
                    nonce: a["nonce"].as_u64().unwrap(),
                    balance: word(&a["balance"]),
                    code: raw(&a["code"]),
                    storage,
                    storage_size: word(&a["storage_size"]),
                    backing: None,
                },
            )
            .unwrap();
    }
    world
}

struct State(World);
fn read(e: impl std::fmt::Display) -> ViewError {
    ViewError::Unavailable(e.to_string())
}
impl AccountState for State {
    fn balance(&self, a: &[u8; 20]) -> Result<U256, ViewError> {
        Ok(self
            .0
            .account(a)
            .map_err(read)?
            .map_or([0; 32], |a| word::bytes(a.balance)))
    }
    fn nonce(&self, a: &[u8; 20]) -> Result<u64, ViewError> {
        Ok(self.0.account(a).map_err(read)?.map_or(0, |a| a.nonce))
    }
    fn code(&self, a: &[u8; 20]) -> Result<Vec<u8>, ViewError> {
        Ok(self
            .0
            .account(a)
            .map_err(read)?
            .map(|a| a.code.clone())
            .unwrap_or_default())
    }
    fn storage(&self, a: &[u8; 20], key: &Hash) -> Result<Hash, ViewError> {
        match self.0.account(a).map_err(read)? {
            None => Ok([0; 32]),
            Some(a) => a.get(*key).map_err(read),
        }
    }
}

/// One scenario: every block shares the committed state.
struct VectorView {
    head: Arc<BlockData>,
    blocks: Vec<Arc<BlockData>>,
    world: World,
    context: evm::Context,
    environment: Environment,
}
impl VectorView {
    fn new(s: &Value) -> Self {
        let c = &s["context"];
        let head = block(&s["head"]);
        let mut blocks = vec![head.clone(), block(&s["parent"]), block(&s["prime"])];
        if !s["grandparent"].is_null() {
            blocks.push(block(&s["grandparent"]));
        }
        let context = evm::Context {
            address: [0; 20],
            caller: [0; 20],
            origin: [0; 20],
            coinbase: fixed(&c["coinbase"]),
            etx_sender: [0; 20],
            location: [0, 0],
            tx_type: 0,
            value: Word::ZERO,
            gas_price: Word::ZERO,
            block_number: word(&c["block_number"]),
            timestamp: word(&c["timestamp"]),
            difficulty: word(&c["difficulty"]),
            base_fee: word(&c["base_fee"]),
            chain_id: word(&c["chain_id"]),
            state_size: word(&c["state_size"]),
            block_gas_limit: c["block_gas_limit"].as_u64().unwrap(),
            address_listed: false,
            warm_slots: BTreeSet::new(),
            read_only: false,
            trace: false,
        };
        let environment = Environment {
            prime_number: Some(c["prime_number"].as_u64().unwrap()),
            block_hashes: s["block_hashes"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(n, h)| (n.parse().unwrap(), fixed(h)))
                .collect(),
            transaction_hash: Some([0; 32]),
            eligible_slices: Some(fixed(&c["eligible_slices"])),
            delete_empty: true,
        };
        Self {
            head,
            blocks,
            world: world(&s["world"]),
            context,
            environment,
        }
    }
}
impl ChainView for VectorView {
    fn chain_id(&self) -> u64 {
        9
    }
    fn location(&self) -> [u8; 2] {
        [0, 0]
    }
    fn head(&self) -> u64 {
        self.head.number
    }
    fn block_by_number(&self, n: u64) -> Result<Option<Arc<BlockData>>, ViewError> {
        // The prime terminus is not a canonical zone block of the scenario.
        Ok(self.blocks[..self.blocks.len().min(2)]
            .iter()
            .chain(self.blocks.get(3))
            .find(|b| b.number == n)
            .cloned())
    }
    fn block_by_hash(&self, h: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok(self.blocks.iter().find(|b| &b.hash == h).cloned())
    }
    fn uncle_kinds(&self, _: &BlockData) -> Result<Arc<Vec<UncleKind>>, ViewError> {
        Ok(Arc::new(vec![]))
    }
    fn total_entropy(&self, _: &BlockData) -> Result<U256, ViewError> {
        Ok([0; 32])
    }
    fn transaction(&self, _: &Hash) -> Result<Option<TxPosition>, ViewError> {
        Ok(None)
    }
    fn receipts(&self, _: &BlockData) -> Result<Arc<Vec<ReceiptData>>, ViewError> {
        Ok(Arc::new(vec![]))
    }
    fn state(&self, _: &BlockData) -> Result<Arc<dyn AccountState>, ViewError> {
        Ok(Arc::new(State(self.world.clone())))
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
    fn call(&self, block: &BlockData, request: &CallRequest) -> Result<CallResult, CallError> {
        assert_eq!(block.hash, self.head.hash, "vector calls run at the head");
        quai_node::rpc_call::run(
            &self.world,
            &self.context,
            &self.environment,
            request,
            self.head.number,
        )
    }
}

/// Another zone's form of precompile 1. go-quai's `EVM.precompile`
/// (core/vm/evm.go) replaces the first byte of any address with the zone's
/// prefix before it looks for a precompile, so a top-level call to it runs
/// the zone's ecrecover. Rust's consensus code (`evm::precompile::translate`)
/// accepts only the zone prefix or 0x00 there and sends an ETX instead. This
/// is a consensus difference, which the RPC work leaves alone: the answers
/// for it must keep differing until the consensus code changes.
const OTHER_ZONE_PRECOMPILE: &str = "0x1000000000000000000000000000000000000001";

#[test]
fn rpc_calls_equal_go_quai() {
    let doc = corpus();
    assert_eq!(
        doc["reference_revision"],
        quai_primitives::REFERENCE_REVISION
    );
    let mut per: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut failures = vec![];
    let mut known = 0;
    for s in doc["scenarios"].as_array().unwrap() {
        let view = VectorView::new(s);
        for v in s["vectors"].as_array().unwrap() {
            let method = v["method"].as_str().unwrap();
            let got = match quai_rpc::native::dispatch(&view, method, &v["params"]) {
                Ok(result) => json!({ "result": result }),
                Err(e) => json!({ "error": e.to_value() }),
            };
            if v["params"][0]["to"] == OTHER_ZONE_PRECOMPILE {
                assert_ne!(got, v["go"], "{} {method} {}", s["id"], v["params"]);
                known += 1;
                continue;
            }
            let row = per.entry(method.to_string()).or_default();
            row.0 += 1;
            if got == v["go"] {
                row.1 += 1;
            } else {
                failures.push(format!(
                    "{} {method} {}\n  go   {}\n  rust {got}",
                    s["id"], v["params"], v["go"]
                ));
            }
        }
    }
    for (m, (n, ok)) in &per {
        eprintln!("{m}: {ok}/{n}");
    }
    eprintln!("documented differences (other zone's precompile): {known}");
    assert_eq!(known, 12);
    assert!(
        failures.is_empty(),
        "{} answers differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
