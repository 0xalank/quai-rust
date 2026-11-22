//! The read methods that a default go-quai v0.57.0 zone node serves and the
//! native RPC ported after the inventory of docs/rpc-parity.md, against
//! go-quai's production handlers (fixtures/protocol/rpc-parity.json,
//! scripts/gen_rpc_parity_vectors.py): the `quai_` block, fee, quote,
//! supply, lockup and proof reads, `net_` peer counts and the `eth_`
//! namespace. Go ran each scenario over a test chain with receipts, a side
//! branch, one committed state that every block carries, a prime terminus,
//! UTXO set sizes, an address UTXO index and coinbase lockup records. Here
//! the same data is a view, and every answer of `App::handle` must equal
//! Go's whole response, except the node identity of `*_clientVersion`.
use quai_consensus::verifier::PINNED_SCHEDULE;
use quai_execution::evm::{
    state::{Account, World, WorldProof},
    word,
};
use quai_execution::lockup::Lockup;
use quai_node::rpc_view::decode_receipts;
use quai_primitives::{decode_hex, work::WorkObject, Hash};
use quai_rpc::view::{
    AccountState, BlockData, CallError, CallRequest, CallResult, ChainView, CoinbaseLockup,
    OutpointData, PeerCounts, ReceiptData, TxPosition, UncleKind, UtxoData, ViewError, U256,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

type Address = [u8; 20];

fn corpus() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/rpc-parity.json")).unwrap()
}
fn raw(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
fn fixed<const N: usize>(v: &Value) -> [u8; N] {
    raw(v).try_into().unwrap()
}
fn hashes(v: &Value) -> Vec<Hash> {
    v.as_array().unwrap().iter().map(fixed).collect()
}
fn trimmed(v: &Value) -> Vec<u8> {
    let b = raw(v);
    b[b.iter().position(|x| *x != 0).unwrap_or(b.len())..].to_vec()
}
fn block(v: &Value) -> Arc<BlockData> {
    let bytes = raw(&v["object"]);
    let object = WorkObject::decode_auxiliary(&bytes, PINNED_SCHEDULE).unwrap();
    let header = object.auxiliary_work_header(PINNED_SCHEDULE).unwrap();
    let uncles = object.uncle_headers(PINNED_SCHEDULE).unwrap();
    let empty = json!([]);
    Arc::new(BlockData {
        hash: fixed(&v["hash"]),
        number: v["number"].as_u64().unwrap(),
        object,
        header,
        uncles,
        size: v["size"].as_u64().unwrap(),
        order: 2,
        tx_hashes: hashes(v.get("tx_hashes").unwrap_or(&empty)),
        etx_hashes: hashes(v.get("etx_hashes").unwrap_or(&empty)),
    })
}

/// The fixture's blocks, receipts, state and records.
struct Chain {
    blocks: HashMap<Hash, Arc<BlockData>>,
    names: HashMap<String, Hash>,
    receipts: HashMap<Hash, Arc<Vec<ReceiptData>>>,
    entropy: HashMap<Hash, U256>,
    kinds: HashMap<Hash, UncleKind>,
    world: World,
    lockups: Vec<(Address, Address, CoinbaseLockup)>,
    outpoints: HashMap<Address, Vec<OutpointData>>,
}
fn chain(c: &Value) -> Arc<Chain> {
    let mut out = Chain {
        blocks: HashMap::new(),
        names: HashMap::new(),
        receipts: HashMap::new(),
        entropy: HashMap::new(),
        kinds: HashMap::new(),
        world: World::default(),
        lockups: vec![],
        outpoints: HashMap::new(),
    };
    let ch = &c["chain"];
    for (name, v) in ch["blocks"].as_object().unwrap() {
        let b = block(v);
        if !v["receipts"].is_null() {
            out.receipts.insert(
                b.hash,
                Arc::new(decode_receipts(&raw(&v["receipts"])).unwrap()),
            );
        }
        let mut e = [0u8; 32];
        let t = raw(&v["total_entropy"]);
        e[32 - t.len()..].copy_from_slice(&t);
        out.entropy.insert(b.hash, e);
        out.names.insert(name.clone(), b.hash);
        out.blocks.insert(b.hash, b);
    }
    let prime = block(&ch["prime"]);
    out.blocks.insert(prime.hash, prime);
    for (hash, kind) in ch["uncle_kinds"].as_object().unwrap() {
        let kind = match kind.as_str().unwrap() {
            "workshare" => UncleKind::Workshare,
            "block" => UncleKind::Block,
            _ => UncleKind::Other,
        };
        out.kinds.insert(fixed(&Value::String(hash.clone())), kind);
    }
    for a in ch["world"].as_array().unwrap() {
        let mut storage = BTreeMap::new();
        for s in a["storage"].as_array().unwrap() {
            storage.insert(fixed(&s["key"]), fixed(&s["value"]));
        }
        out.world
            .insert(
                fixed(&a["address"]),
                Account {
                    nonce: a["nonce"].as_u64().unwrap(),
                    balance: word::from_bytes(&raw(&a["balance"])),
                    code: raw(&a["code"]),
                    storage,
                    storage_size: word::from_bytes(&raw(&a["storage_size"])),
                    backing: None,
                },
            )
            .unwrap();
    }
    assert_eq!(
        out.world.root().unwrap(),
        fixed::<32>(&ch["state_root"]),
        "the committed state root"
    );
    for l in c["lockups"].as_array().unwrap() {
        let record = Lockup::decode(&raw(&l["record"])).unwrap();
        out.lockups.push((
            fixed(&l["owner"]),
            fixed(&l["beneficiary"]),
            CoinbaseLockup {
                lockup_byte: l["lockup_byte"].as_u64().unwrap() as u8,
                epoch: l["epoch"].as_u64().unwrap() as u32,
                balance: word::bytes(record.balance),
                unlock_height: record.unlock_height,
                elements: record.elements,
                delegate: record.delegate,
            },
        ));
    }
    for u in c["utxos"].as_array().unwrap() {
        out.outpoints
            .entry(fixed(&u["address"]))
            .or_default()
            .push(OutpointData {
                tx_hash: fixed(&u["tx_hash"]),
                index: u["index"].as_u64().unwrap() as u16,
                denomination: u["denomination"].as_u64().unwrap() as u8,
                lock: trimmed(&u["lock"]),
            });
    }
    Arc::new(out)
}

/// The committed state, which every block of the fixture carries.
struct State(World);
fn read(e: impl std::fmt::Display) -> ViewError {
    ViewError::Unavailable(e.to_string())
}
impl AccountState for State {
    fn balance(&self, a: &Address) -> Result<U256, ViewError> {
        let account = self.0.account(a).map_err(read)?;
        Ok(account.map_or([0; 32], |a| word::bytes(a.balance)))
    }
    fn nonce(&self, a: &Address) -> Result<u64, ViewError> {
        Ok(self.0.account(a).map_err(read)?.map_or(0, |a| a.nonce))
    }
    fn code(&self, a: &Address) -> Result<Vec<u8>, ViewError> {
        let account = self.0.account(a).map_err(read)?;
        Ok(account.map(|a| a.code.clone()).unwrap_or_default())
    }
    fn storage(&self, a: &Address, key: &Hash) -> Result<Hash, ViewError> {
        match self.0.account(a).map_err(read)? {
            None => Ok([0; 32]),
            Some(a) => a.get(*key).map_err(read),
        }
    }
    fn size(&self, a: &Address) -> Result<U256, ViewError> {
        let account = self.0.account(a).map_err(read)?;
        Ok(account.map_or([0; 32], |a| word::bytes(a.storage_size)))
    }
    fn proof(&self, a: &Address, keys: &[Hash]) -> Result<WorldProof, ViewError> {
        self.0.proof(a, keys).map_err(read)
    }
}

struct View {
    chain: Arc<Chain>,
    canonical: BTreeMap<u64, Hash>,
    utxo_set_size: HashMap<Hash, u64>,
    peers: PeerCounts,
    fees: Option<[U256; 3]>,
}
fn unavailable() -> ViewError {
    ViewError::Unavailable("not in the fixture".into())
}
impl ChainView for View {
    fn chain_id(&self) -> u64 {
        9
    }
    fn location(&self) -> [u8; 2] {
        [0, 0]
    }
    fn head(&self) -> u64 {
        *self.canonical.keys().last().unwrap()
    }
    fn block_by_number(&self, number: u64) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok(self
            .canonical
            .get(&number)
            .and_then(|h| self.chain.blocks.get(h).cloned()))
    }
    fn block_by_hash(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        let Some(b) = self.chain.blocks.get(hash) else {
            return Ok(None);
        };
        Ok((self.canonical.get(&b.number) == Some(hash)).then(|| b.clone()))
    }
    fn block_or_candidate(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok(self.chain.blocks.get(hash).cloned())
    }
    fn canonical_hash(&self, number: u64) -> Result<Option<Hash>, ViewError> {
        Ok(self.canonical.get(&number).copied())
    }
    fn uncle_kinds(&self, block: &BlockData) -> Result<Arc<Vec<UncleKind>>, ViewError> {
        Ok(Arc::new(
            block
                .uncles
                .iter()
                .map(|u| self.chain.kinds[&u.block_hash().unwrap()])
                .collect(),
        ))
    }
    fn total_entropy(&self, block: &BlockData) -> Result<U256, ViewError> {
        // The oracle's TotalLogEntropy gives zero for a block it has no
        // figure for (the prime terminus).
        Ok(self
            .chain
            .entropy
            .get(&block.hash)
            .copied()
            .unwrap_or([0; 32]))
    }
    fn transaction(&self, hash: &Hash) -> Result<Option<TxPosition>, ViewError> {
        for h in self.canonical.values() {
            let b = &self.chain.blocks[h];
            if let Some(i) = b.tx_hashes.iter().position(|t| t == hash) {
                return Ok(Some(TxPosition {
                    block_hash: b.hash,
                    block_number: b.number,
                    index: i as u64,
                }));
            }
        }
        Ok(None)
    }
    fn receipts(&self, block: &BlockData) -> Result<Arc<Vec<ReceiptData>>, ViewError> {
        self.chain
            .receipts
            .get(&block.hash)
            .cloned()
            .ok_or_else(unavailable)
    }
    fn state(&self, _: &BlockData) -> Result<Arc<dyn AccountState>, ViewError> {
        Ok(Arc::new(State(self.chain.world.clone())))
    }
    fn utxo(&self, _: &Hash, _: u16) -> Result<Option<UtxoData>, ViewError> {
        Err(unavailable())
    }
    fn outpoints_by_address(&self, a: &Address) -> Result<Vec<OutpointData>, ViewError> {
        Ok(self.chain.outpoints.get(a).cloned().unwrap_or_default())
    }
    fn outpoints_by_address_and_range(
        &self,
        _: &Address,
        _: u32,
        _: u32,
    ) -> Result<Vec<OutpointData>, ViewError> {
        Err(unavailable())
    }
    fn call(&self, _: &BlockData, _: &CallRequest) -> Result<CallResult, CallError> {
        Err(CallError::View(unavailable()))
    }
    fn utxo_set_size(&self, block: &BlockData) -> Result<u64, ViewError> {
        // rawdb.ReadUTXOSetSize: zero for a block without a record.
        Ok(self.utxo_set_size.get(&block.hash).copied().unwrap_or(0))
    }
    fn peer_counts(&self) -> PeerCounts {
        self.peers
    }
    fn rolling_fee_info(&self) -> Option<[U256; 3]> {
        self.fees
    }
    fn coinbase_lockups(
        &self,
        owner: &Address,
        beneficiary: &Address,
    ) -> Result<Vec<CoinbaseLockup>, ViewError> {
        let mut out: Vec<CoinbaseLockup> = self
            .chain
            .lockups
            .iter()
            .filter(|(o, b, _)| o == owner && b == beneficiary)
            .map(|(_, _, l)| l.clone())
            .collect();
        out.sort_by_key(|l| (l.lockup_byte, l.epoch));
        Ok(out)
    }
}

fn answer(raw: &[u8]) -> Value {
    let v: Value = serde_json::from_slice(raw).unwrap();
    assert_eq!(v["jsonrpc"], "2.0");
    let mut out = serde_json::Map::new();
    for k in ["result", "error"] {
        if let Some(x) = v.get(k) {
            out.insert(k.into(), x.clone());
        }
    }
    Value::Object(out)
}

#[test]
fn new_reads_match_go() {
    let c = corpus();
    let chain = chain(&c);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut equal = 0;
    let mut identity = 0;
    let mut failures = vec![];
    for s in c["scenarios"].as_array().unwrap() {
        let mut canonical = BTreeMap::new();
        for name in s["canonical"].as_array().unwrap() {
            let h = chain.names[name.as_str().unwrap()];
            canonical.insert(chain.blocks[&h].number, h);
        }
        let mut utxo_set_size = HashMap::new();
        for (name, n) in s["utxo_set_size"].as_object().unwrap() {
            utxo_set_size.insert(chain.names[name], n.as_u64().unwrap());
        }
        let peers = s["peers"].as_array().unwrap();
        let (incoming, outgoing) = (peers[0].as_u64().unwrap(), peers[1].as_u64().unwrap());
        let fees: Vec<U256> = s["rolling_fees"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                let b = raw(f);
                let mut w = [0u8; 32];
                w[32 - b.len()..].copy_from_slice(&b);
                w
            })
            .collect();
        let view = Arc::new(View {
            chain: chain.clone(),
            canonical,
            utxo_set_size,
            peers: PeerCounts {
                peers: incoming + outgoing,
                incoming,
                outgoing,
            },
            fees: fees.try_into().ok(),
        });
        let config = quai_rpc::Config {
            mode: "native".into(),
            timeout_ms: 60_000,
            ..quai_rpc::Config::default()
        };
        let app = quai_rpc::App::native(config, view).unwrap();
        for call in s["calls"].as_array().unwrap() {
            let request = &call["request"];
            let want = &call["response"];
            let body = serde_json::to_vec(request).unwrap();
            let got = runtime.block_on(app.handle(&body)).unwrap();
            let got = answer(&got);
            let method = request["method"].as_str().unwrap();
            if method.ends_with("_clientVersion") && want.get("result").is_some() {
                // The node identity: go-quai/<VERSION>-<commit> in Go.
                assert!(want["result"]
                    .as_str()
                    .unwrap()
                    .starts_with("go-quai/v0.57.0-"));
                assert_eq!(
                    got["result"].as_str(),
                    Some(quai_rpc::native::chain::client_version().as_str())
                );
                identity += 1;
                continue;
            }
            if &got == want {
                equal += 1;
            } else {
                failures.push(format!(
                    "{} {} {}\n  go:   {}\n  rust: {}",
                    s["name"], method, request["params"], want, got
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} differ:\n{}",
        failures.len(),
        failures.len() + equal + identity,
        failures.join("\n")
    );
    eprintln!("rpc parity: {equal} answers equal to go-quai, {identity} identity answers");
    assert_eq!(equal + identity, 353);
}

/// `WorkObject.RPCMarshalWorkObject("v1")` and `("v2")` (the payloads of
/// "newHeads", "newHeadsV2", "newChainBlocks" and "newChainBlocksV2") of a
/// block after the KawPow fork with an AuxPow, share counts, targets and a
/// KawPow uncle, and of two blocks before it, as go-quai reads them from
/// its database.
#[test]
fn work_object_json_matches_go() {
    let c = corpus();
    let mut n = 0;
    for m in c["marshal"].as_array().unwrap() {
        let b = block(m);
        assert_eq!(
            quai_rpc::native::json::work_object(&b),
            m["v1"],
            "{} v1",
            m["name"]
        );
        assert_eq!(
            quai_rpc::native::json::work_object_v2(&b),
            m["v2"],
            "{} v2",
            m["name"]
        );
        n += 2;
    }
    assert_eq!(n, 6);
}
