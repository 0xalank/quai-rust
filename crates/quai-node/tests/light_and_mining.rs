//! End-to-end tests covering all 4 light-client & low-storage mining modes:
//! 1. Helios-style trustless light client (`LightView` + `mpt::verify_proof`):
//!    balances, nonces, storage slots, bytecode, `quai_call` over proven sparse
//!    World, partial state caching, AND cryptographic rejection of lying/tampered
//!    upstream RPC responses.
//! 2. Stateless workshare mining (`MinerMode::WorksharesOnly` on zero-state
//!    `LightHeaderStore`): consensus-valid workshares + refusal to publish
//!    unexecuted full blocks.
//! 3. Zero-storage full-block mining via remote builder template relay
//!    (`MinerMode::BuilderRelay` + `BuilderRelay`): tampering rejection, verified
//!    template installation, and full-block submission forwarding.
//! 4. Compact stateful full-block builder (`zone_execution::build_block` wired
//!    into `MinerMode::Full`): verifies the produced block passes full consensus
//!    `zone_execution::project` with every state/ETX/UTXO/receipt/outbound root
//!    matching bit-for-bit.
//! 5. CLI parsing for `quai-node light`, `--mine-workshares`, `--builder-rpc`,
//!    `--upstream-rpc`, and `--compact-miner`.
use crypto_bigint::Encoding as _;
use prost::Message;
use quai_consensus::{
    auxiliary::coinbase,
    history::{History, Record},
    pow::SharedCaches,
    share_rules::Classification,
    validation,
    zone_execution::{self, State},
};
use quai_execution::{
    evm::{
        state::{Account, World},
        word::Word,
    },
    historical,
    qi::OutPoint,
};
use quai_node::{
    cli::{parse, Command},
    light::{HttpRpcClient, LightHeaderStore, LightView},
    log::{Level, Log},
    miner::{
        build_donor_header, verify_builder_template, BuilderRelay, Miner, MinerConfig, MinerMode,
        POW_SHA_BCH,
    },
    spec::Chain,
};
use quai_primitives::{
    mpt,
    schema::{block::ProtoWorkObject, common::ProtoHash},
    transaction::{Address, Transaction},
    Hash,
};
use quai_rpc::{
    view::{
        AccountState, BlockData, CallError, CallRequest, CallResult, ChainView, OutpointData,
        ReceiptData, TxPosition, UncleKind, UtxoData, ViewError, WorldProof, U256,
    },
    App, Config as RpcConfig,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

struct EmptyPast;
impl historical::History for EmptyPast {
    fn incoming_at(
        &self,
        _height: u64,
    ) -> Result<Vec<Transaction>, quai_execution::settlement::Error> {
        Ok(vec![])
    }
    fn created_at(
        &self,
        _height: u64,
    ) -> Result<Vec<(OutPoint, u8)>, quai_execution::settlement::Error> {
        Ok(vec![])
    }
}

/// Simple stateful mock `ChainView` hosting one real `World` + optional `Miner`
/// over HTTP JSON-RPC for testing the light client and builder relay end-to-end.
struct MockFullView {
    chain: Arc<Chain>,
    block: Arc<BlockData>,
    world: Arc<World>,
    miner: Option<Arc<Miner>>,
    received_headers: Arc<Mutex<Vec<Vec<u8>>>>,
}

struct WorldAccountState(Arc<World>);

impl AccountState for WorldAccountState {
    fn balance(&self, address: &Address) -> Result<U256, ViewError> {
        Ok(self
            .0
            .account(address)
            .map_err(|e| ViewError::Unavailable(e.to_string()))?
            .map_or([0u8; 32], |a| a.balance.to_be_bytes()))
    }
    fn nonce(&self, address: &Address) -> Result<u64, ViewError> {
        Ok(self
            .0
            .account(address)
            .map_err(|e| ViewError::Unavailable(e.to_string()))?
            .map_or(0, |a| a.nonce))
    }
    fn code(&self, address: &Address) -> Result<Vec<u8>, ViewError> {
        Ok(self
            .0
            .account(address)
            .map_err(|e| ViewError::Unavailable(e.to_string()))?
            .map_or_else(Vec::new, |a| a.code.clone()))
    }
    fn storage(&self, address: &Address, key: &Hash) -> Result<Hash, ViewError> {
        let Some(acc) = self
            .0
            .account(address)
            .map_err(|e| ViewError::Unavailable(e.to_string()))?
        else {
            return Ok([0u8; 32]);
        };
        acc.get(*key)
            .map_err(|e| ViewError::Unavailable(e.to_string()))
    }
    fn size(&self, address: &Address) -> Result<U256, ViewError> {
        Ok(self
            .0
            .account(address)
            .map_err(|e| ViewError::Unavailable(e.to_string()))?
            .map_or([0u8; 32], |a| a.storage_size.to_be_bytes()))
    }
    fn proof(&self, address: &Address, keys: &[Hash]) -> Result<WorldProof, ViewError> {
        self.0
            .proof(address, keys)
            .map_err(|e| ViewError::Unavailable(e.to_string()))
    }
}

impl ChainView for MockFullView {
    fn chain_id(&self) -> u64 {
        self.chain.spec().chain_id
    }
    fn location(&self) -> [u8; 2] {
        self.chain.location()
    }
    fn head(&self) -> u64 {
        self.block.number
    }
    fn block_by_number(&self, number: u64) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok((number == self.block.number).then(|| self.block.clone()))
    }
    fn block_by_hash(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok((*hash == self.block.hash).then(|| self.block.clone()))
    }
    fn uncle_kinds(&self, _block: &BlockData) -> Result<Arc<Vec<UncleKind>>, ViewError> {
        Ok(Arc::new(vec![]))
    }
    fn total_entropy(&self, _block: &BlockData) -> Result<U256, ViewError> {
        Ok([0u8; 32])
    }
    fn transaction(&self, _hash: &Hash) -> Result<Option<TxPosition>, ViewError> {
        Ok(None)
    }
    fn receipts(&self, _block: &BlockData) -> Result<Arc<Vec<ReceiptData>>, ViewError> {
        Ok(Arc::new(vec![]))
    }
    fn state(&self, _block: &BlockData) -> Result<Arc<dyn AccountState>, ViewError> {
        Ok(Arc::new(WorldAccountState(self.world.clone())))
    }
    fn utxo(&self, _tx_hash: &Hash, _index: u16) -> Result<Option<UtxoData>, ViewError> {
        Ok(None)
    }
    fn outpoints_by_address(&self, _address: &Address) -> Result<Vec<OutpointData>, ViewError> {
        Ok(vec![])
    }
    fn outpoints_by_address_and_range(
        &self,
        _address: &Address,
        _start: u32,
        _end: u32,
    ) -> Result<Vec<OutpointData>, ViewError> {
        Ok(vec![])
    }
    fn call(&self, block: &BlockData, request: &CallRequest) -> Result<CallResult, CallError> {
        let ctx = quai_execution::evm::Context {
            address: [0; 20],
            caller: [0; 20],
            origin: [0; 20],
            coinbase: [0; 20],
            etx_sender: [0; 20],
            location: self.location(),
            tx_type: 0,
            value: Word::ZERO,
            gas_price: Word::ZERO,
            block_number: Word::from_u64(block.number),
            timestamp: Word::ZERO,
            difficulty: Word::ONE,
            base_fee: Word::ZERO,
            chain_id: Word::from_u64(self.chain_id()),
            state_size: Word::ZERO,
            block_gas_limit: self.chain.spec().genesis.gas_limit,
            address_listed: false,
            warm_slots: std::collections::BTreeSet::new(),
            read_only: false,
            trace: false,
        };
        let env = quai_execution::evm::calls::Environment {
            delete_empty: true,
            ..Default::default()
        };
        quai_node::rpc_call::run(&self.world, &ctx, &env, request, block.number)
    }
    fn get_pending_header(&self) -> Result<Vec<u8>, ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable("no miner".into()));
        };
        let wo = m
            .generate_pending_header(POW_SHA_BCH, None, &[], None)
            .map_err(ViewError::Go)?;
        Ok(wo.encode())
    }
    fn receive_mined_header(&self, raw: &[u8]) -> Result<(), ViewError> {
        self.received_headers.lock().unwrap().push(raw.to_vec());
        Ok(())
    }
}

/// Build a synthetic block header at height 1 extending genesis carrying `evm_root`.
fn make_header_with_evm_root(chain: &Chain, evm_root: Hash) -> (Vec<u8>, Arc<BlockData>) {
    let gen_raw = chain.genesis().object().encode();
    let gen_hash = chain.genesis_hash();
    let mut proto = ProtoWorkObject::decode(gen_raw.as_slice()).unwrap();
    let body = proto.wo_body.as_mut().unwrap();
    let hdr = body.header.as_mut().unwrap();
    hdr.evm_root = Some(ProtoHash {
        value: evm_root.to_vec(),
    });
    if hdr.parent_hash.len() == 3 {
        hdr.parent_hash[2] = ProtoHash {
            value: gen_hash.to_vec(),
        };
    }
    if hdr.number.len() == 3 {
        hdr.number[2] = vec![1];
    }
    let new_body_hash = quai_primitives::work::BodyHeader::decode(&hdr.encode_to_vec())
        .unwrap()
        .hash();
    let wh = proto.wo_header.as_mut().unwrap();
    wh.parent_hash = Some(ProtoHash {
        value: gen_hash.to_vec(),
    });
    wh.number = Some(vec![1]);
    wh.header_hash = Some(ProtoHash {
        value: new_body_hash.to_vec(),
    });
    let raw = proto.encode_to_vec();
    let rec = Record::decode(&raw).unwrap();
    let block = Arc::new(BlockData {
        hash: rec.id(),
        number: 1,
        object: rec.object().clone(),
        header: rec.header().clone(),
        uncles: vec![],
        size: raw.len() as u64,
        order: 2,
        tx_hashes: vec![],
        etx_hashes: vec![],
    });
    (raw, block)
}

type TamperFn = Arc<dyn Fn(&str, &mut Value) + Send + Sync>;

/// Spawn a tiny malicious HTTP JSON-RPC server that mutates responses from an
/// honest upstream RPC to verify the light client catches every fraud vector.
async fn spawn_tampering_proxy(honest_url: String, tamper: TamperFn) -> String {
    use axum::{routing::post, Json, Router};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/",
        post(move |Json(req): Json<Value>| {
            let honest = honest_url.clone();
            let tamper = tamper.clone();
            async move {
                let id = req.get("id").cloned().unwrap_or(json!(1));
                let method = req
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let params = req.get("params").cloned().unwrap_or(json!([]));
                let res = tokio::task::spawn_blocking(move || {
                    let client = HttpRpcClient::new(&honest).unwrap();
                    let mut val = client.call(&method, params).unwrap_or(Value::Null);
                    tamper(&method, &mut val);
                    val
                })
                .await
                .unwrap();
                Json(json!({ "jsonrpc": "2.0", "id": id, "result": res }))
            }
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn light_client_verifies_mpt_proofs_calls_and_rejects_fraud() {
    let chain = Chain::for_environment("local").unwrap();

    // Populate a real World with:
    // - EOA `0x00..00aa`: nonce=7, balance=123456789
    // - Contract `0x00..00bb`: slot 0x01 = 0xdeadbeef, bytecode:
    //   PUSH1 0x01, SLOAD, PUSH1 0x00, MSTORE, PUSH1 0x20, PUSH1 0x00, RETURN
    //   => 60 01 54 60 00 52 60 20 60 00 f3
    let mut world = World::default();
    let eoa: Address = [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xaa,
    ];
    let contract: Address = [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xbb,
    ];
    let absent: Address = [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xcc,
    ];
    let mut slot_key = [0u8; 32];
    slot_key[31] = 1;
    let mut slot_val = [0u8; 32];
    slot_val[28..32].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    let code = vec![
        0x60, 0x01, 0x54, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3,
    ];

    world
        .insert(
            eoa,
            Account {
                nonce: 7,
                balance: Word::from_u64(123_456_789),
                ..Default::default()
            },
        )
        .unwrap();
    let mut storage = BTreeMap::new();
    storage.insert(slot_key, slot_val);
    world
        .insert(
            contract,
            Account {
                nonce: 1,
                balance: Word::from_u64(500),
                code: code.clone(),
                storage,
                storage_size: Word::from_u64(1),
                backing: None,
            },
        )
        .unwrap();

    let evm_root = world.root().unwrap();
    let (hdr_raw, block) = make_header_with_evm_root(&chain, evm_root);

    // Start honest full-node HTTP JSON-RPC server.
    let full_view = Arc::new(MockFullView {
        chain: chain.clone(),
        block: block.clone(),
        world: Arc::new(world),
        miner: None,
        received_headers: Arc::new(Mutex::new(vec![])),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let honest_addr = listener.local_addr().unwrap();
    let app = App::native(
        RpcConfig {
            mode: "native".into(),
            listen: honest_addr.to_string(),
            ..RpcConfig::default()
        },
        full_view,
    )
    .unwrap();
    tokio::spawn(async move {
        let _ = quai_rpc::serve_until(app, listener, std::future::pending::<()>()).await;
    });
    let honest_url = format!("http://{honest_addr}");

    // Create zero-state LightHeaderStore holding ONLY the header (no state trie!).
    let mut ent1 = [0u8; 32];
    ent1[31] = 1;
    let store = LightHeaderStore::in_memory(chain.clone(), 128).unwrap();
    assert!(store.insert_verified_raw(&hdr_raw, 2, ent1, false).unwrap());
    let caches = Arc::new(SharedCaches::new(8).unwrap());
    let light_view = LightView::new(
        store.clone(),
        caches.clone(),
        Some(HttpRpcClient::new(&honest_url).unwrap()),
    );

    // Verify all reads on blocking thread via LightView!
    let lv = light_view.clone();
    let blk = store.block_by_number(1).unwrap();
    let code_expected = code.clone();
    tokio::task::spawn_blocking(move || {
        let st = lv.state(&blk).unwrap();
        assert_eq!(
            st.balance(&eoa).unwrap(),
            Word::from_u64(123_456_789).to_be_bytes()
        );
        assert_eq!(st.nonce(&eoa).unwrap(), 7);
        assert_eq!(st.balance(&absent).unwrap(), [0u8; 32]);
        assert_eq!(st.nonce(&absent).unwrap(), 0);
        assert_eq!(st.storage(&contract, &slot_key).unwrap(), slot_val);
        assert_eq!(st.storage(&contract, &[9u8; 32]).unwrap(), [0u8; 32]);
        assert_eq!(st.code(&contract).unwrap(), code_expected);
        assert!(st.code(&absent).unwrap().is_empty());

        // Second balance read hits verified partial state cache cleanly.
        assert!(lv.store().get_verified_account(evm_root, &eoa).is_some());
        assert!(lv
            .store()
            .get_verified_code(&quai_primitives::transaction::keccak(&code_expected))
            .is_some());

        // Helios-style trustless `quai_call`: executes contract bytecode locally
        // against verified MPT proofs (SLOAD slot 1 -> returns 0x..deadbeef)!
        let call_res = lv
            .call(
                &blk,
                &CallRequest {
                    from: eoa,
                    to: Some(contract),
                    gas: 100_000,
                    value: [0u8; 32],
                    data: vec![],
                    access_list: vec![],
                    trace_access_list: false,
                },
            )
            .unwrap();
        assert!(!call_res.reverted, "call reverted: {:?}", call_res.error);
        assert_eq!(call_res.return_data, slot_val.to_vec());
    })
    .await
    .unwrap();

    // --- FRAUD DETECTION TESTS ---
    // 1. Lying balance (upstream inflates balance while returning real proof) -> REJECTED!
    let lying_balance_url = spawn_tampering_proxy(
        honest_url.clone(),
        Arc::new(|method, val| {
            if method == "quai_getProof" {
                if let Some(obj) = val.as_object_mut() {
                    obj.insert("balance".into(), json!("0xffffffffffffffff"));
                }
            }
        }),
    )
    .await;
    let bad_store1 = LightHeaderStore::in_memory(chain.clone(), 128).unwrap();
    assert!(bad_store1
        .insert_verified_raw(&hdr_raw, 2, ent1, false)
        .unwrap());
    let bad_view1 = LightView::new(
        bad_store1.clone(),
        caches.clone(),
        Some(HttpRpcClient::new(&lying_balance_url).unwrap()),
    );
    let blk1 = bad_store1.block_by_number(1).unwrap();
    let err = tokio::task::spawn_blocking(move || bad_view1.state(&blk1).unwrap().balance(&eoa))
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        err.to_string().contains("PROOF FRAUD"),
        "unexpected err: {err}"
    );

    // 2. Lying bytecode (upstream flips a byte of contract code) -> REJECTED!
    let lying_code_url = spawn_tampering_proxy(
        honest_url.clone(),
        Arc::new(|method, val| {
            if method == "quai_getCode" {
                *val = json!("0xdeadbeef");
            }
        }),
    )
    .await;
    let bad_store2 = LightHeaderStore::in_memory(chain.clone(), 128).unwrap();
    assert!(bad_store2
        .insert_verified_raw(&hdr_raw, 2, ent1, false)
        .unwrap());
    let bad_view2 = LightView::new(
        bad_store2.clone(),
        caches.clone(),
        Some(HttpRpcClient::new(&lying_code_url).unwrap()),
    );
    let blk2 = bad_store2.block_by_number(1).unwrap();
    let err = tokio::task::spawn_blocking(move || bad_view2.state(&blk2).unwrap().code(&contract))
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        err.to_string().contains("PROOF FRAUD"),
        "unexpected err: {err}"
    );

    // 3. Lying storage slot value -> REJECTED!
    let lying_slot_url = spawn_tampering_proxy(
        honest_url.clone(),
        Arc::new(|method, val| {
            if method == "quai_getProof" {
                if let Some(arr) = val.get_mut("storageProof").and_then(Value::as_array_mut) {
                    if let Some(first) = arr.first_mut().and_then(Value::as_object_mut) {
                        first.insert("value".into(), json!("0x1234"));
                    }
                }
            }
        }),
    )
    .await;
    let bad_store3 = LightHeaderStore::in_memory(chain.clone(), 128).unwrap();
    assert!(bad_store3
        .insert_verified_raw(&hdr_raw, 2, ent1, false)
        .unwrap());
    let bad_view3 = LightView::new(
        bad_store3.clone(),
        caches.clone(),
        Some(HttpRpcClient::new(&lying_slot_url).unwrap()),
    );
    let blk3 = bad_store3.block_by_number(1).unwrap();
    let err = tokio::task::spawn_blocking(move || {
        bad_view3
            .state(&blk3)
            .unwrap()
            .storage(&contract, &slot_key)
    })
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        err.to_string().contains("PROOF FRAUD"),
        "unexpected err: {err}"
    );
}

#[test]
fn stateless_workshare_miner_produces_policy_valid_shares_and_never_broadcasts_unexecuted_blocks() {
    let chain = Chain::for_environment("local").unwrap();
    let store = LightHeaderStore::in_memory(chain.clone(), 128).unwrap();
    let caches = Arc::new(SharedCaches::new(4).unwrap());
    let view = LightView::new(store, caches.clone(), None);

    let mut coinbase = [0u8; 20];
    coinbase[19] = 7;
    let cfg = MinerConfig {
        enabled: true,
        mode: MinerMode::WorksharesOnly,
        primary_coinbase: coinbase,
        lockup_byte: 1,
        ..Default::default()
    };
    let miner = Miner::new(chain.clone(), view, caches, cfg);

    let full_blocks_published = Arc::new(AtomicUsize::new(0));
    let workshares_published = Arc::new(AtomicUsize::new(0));
    {
        let fb = full_blocks_published.clone();
        miner.set_block_sink(Arc::new(move |_| {
            fb.fetch_add(1, Ordering::Relaxed);
        }));
    }
    {
        let fb = full_blocks_published.clone();
        let ws = workshares_published.clone();
        miner.set_publish_hook(Arc::new(move |topic, _| {
            if topic.contains("blocks") {
                fb.fetch_add(1, Ordering::Relaxed);
            } else if topic.contains("workshares") {
                ws.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    // Generate pending SHA-256d work object on zero-state light view.
    let pending = miner
        .generate_pending_header(POW_SHA_BCH, None, &[], None)
        .unwrap();
    let schedule = miner.aux_schedule();
    let hdr = pending.auxiliary_work_header(schedule).unwrap();
    // Must pass consensus `validation::data_policy` (lock byte in data[0])!
    validation::data_policy(&hdr, u64::MAX, false).unwrap();

    // Extract donor header & coinbase, mine a real SHA-256d hash on CPU.
    let proto = ProtoWorkObject::decode(pending.encode().as_slice()).unwrap();
    let aux = proto.wo_header.unwrap().aux_pow.unwrap();
    let coinbase_tx = aux.transaction.unwrap();
    let donor_hdr = aux.header.unwrap();
    let version = u32::from_le_bytes(donor_hdr[0..4].try_into().unwrap());
    let mut prev_hash = [0u8; 32];
    prev_hash.copy_from_slice(&donor_hdr[4..36]);
    let merkle_root = coinbase::merkle_root(&coinbase_tx, &aux.merkle_branch).unwrap();
    let ntime = u32::from_le_bytes(donor_hdr[68..72].try_into().unwrap());
    let bits = u32::from_le_bytes(donor_hdr[72..76].try_into().unwrap());

    // Find a nonce meeting the local chain's easy target.
    let mut solved_hdr = None;
    for nonce in 0u32..500_000 {
        let cand = build_donor_header(
            POW_SHA_BCH,
            version,
            prev_hash,
            merkle_root,
            ntime,
            bits,
            u64::from(nonce),
            1,
            [0u8; 32],
        );
        let pow = quai_consensus::donor::sha256d(&cand).unwrap();
        if pow[0] == 0 && pow[1] == 0 {
            solved_hdr = Some(cand);
            break;
        }
    }
    let solved_hdr = solved_hdr.expect("found easy SHA-256d nonce");
    let mut raw_donor = solved_hdr;
    raw_donor.push(1); // 1 tx (coinbase)
    raw_donor.extend_from_slice(&coinbase_tx);

    let outcome = miner.submit_block(&raw_donor, POW_SHA_BCH).unwrap();
    assert!(matches!(
        outcome.classification,
        Classification::Valid | Classification::Block | Classification::Sub
    ));
    // Crucial safety invariant: WorksharesOnly NEVER publishes unexecuted full blocks!
    assert_eq!(full_blocks_published.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn zero_storage_builder_relay_verifies_templates_rejects_tampering_and_submits_blocks() {
    let chain = Chain::for_environment("local").unwrap();
    let caches = Arc::new(SharedCaches::new(4).unwrap());

    // Upstream stateful builder node (mock full view with Miner).
    let gen_rec = chain.genesis();
    let gen_block = Arc::new(BlockData {
        hash: gen_rec.id(),
        number: 0,
        object: gen_rec.object().clone(),
        header: gen_rec.header().clone(),
        uncles: vec![],
        size: gen_rec.object().encode().len() as u64,
        order: 0,
        tx_hashes: vec![],
        etx_hashes: vec![],
    });
    let received_headers = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let builder_base_view = Arc::new(MockFullView {
        chain: chain.clone(),
        block: gen_block.clone(),
        world: Arc::new(World::default()),
        miner: None,
        received_headers: received_headers.clone(),
    });
    let mut cb9 = [0u8; 20];
    cb9[19] = 9;
    let builder_miner_cfg = MinerConfig {
        enabled: true,
        primary_coinbase: cb9,
        ..Default::default()
    };
    let builder_miner = Miner::new(
        chain.clone(),
        builder_base_view,
        caches.clone(),
        builder_miner_cfg,
    );
    let builder_rpc_view = Arc::new(MockFullView {
        chain: chain.clone(),
        block: gen_block,
        world: Arc::new(World::default()),
        miner: Some(builder_miner.clone()),
        received_headers: received_headers.clone(),
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let builder_addr = listener.local_addr().unwrap();
    let app = App::native(
        RpcConfig {
            mode: "native".into(),
            listen: builder_addr.to_string(),
            http_apis: Some(vec!["quai".into(), "miner".into()]),
            ..RpcConfig::default()
        },
        builder_rpc_view,
    )
    .unwrap();
    tokio::spawn(async move {
        let _ = quai_rpc::serve_until(app, listener, std::future::pending::<()>()).await;
    });
    let builder_url = format!("http://{builder_addr}");

    // Zero-storage light mining node connecting via BuilderRelay.
    let light_store = LightHeaderStore::in_memory(chain.clone(), 128).unwrap();
    let light_view = LightView::new(light_store.clone(), caches.clone(), None);
    let light_mcfg = MinerConfig {
        enabled: true,
        mode: MinerMode::BuilderRelay,
        primary_coinbase: cb9,
        ..Default::default()
    };
    let light_miner = Miner::new(chain.clone(), light_view, caches, light_mcfg);
    let relay = BuilderRelay::new(
        &builder_url,
        light_miner.clone(),
        light_store.clone(),
        Log::new(Level::Warn),
    )
    .unwrap();

    // Poll honest template -> verifies cleanly and installs on light miner!
    let verified = tokio::task::spawn_blocking(move || {
        let tpl = relay.poll_once().unwrap();
        assert_eq!(tpl.parent_hash, chain.genesis_hash());
        assert_eq!(tpl.number, 1);

        // Tamper test 1: wrong parent hash -> REJECTED!
        let mut bad_parent = ProtoWorkObject::decode(tpl.raw.as_slice()).unwrap();
        bad_parent.wo_header.as_mut().unwrap().parent_hash = Some(ProtoHash {
            value: vec![0xaa; 32],
        });
        assert!(verify_builder_template(&light_store, &bad_parent.encode_to_vec()).is_err());

        // Tamper test 2: wrong body header commitment -> REJECTED!
        let mut bad_body_hash = ProtoWorkObject::decode(tpl.raw.as_slice()).unwrap();
        bad_body_hash.wo_header.as_mut().unwrap().header_hash = Some(ProtoHash {
            value: vec![0xbb; 32],
        });
        assert!(verify_builder_template(&light_store, &bad_body_hash.encode_to_vec()).is_err());

        // Tamper test 3: wrong difficulty -> REJECTED!
        let mut bad_diff = ProtoWorkObject::decode(tpl.raw.as_slice()).unwrap();
        bad_diff.wo_header.as_mut().unwrap().difficulty = Some(vec![0x01]);
        assert!(verify_builder_template(&light_store, &bad_diff.encode_to_vec()).is_err());

        // Tamper test 4: wrong tx_hash -> REJECTED!
        let mut bad_tx_hash = ProtoWorkObject::decode(tpl.raw.as_slice()).unwrap();
        bad_tx_hash.wo_header.as_mut().unwrap().tx_hash = Some(ProtoHash {
            value: vec![0xcc; 32],
        });
        assert!(verify_builder_template(&light_store, &bad_tx_hash.encode_to_vec()).is_err());

        tpl
    })
    .await
    .unwrap();

    // Generate pending header on light miner (uses verified builder template!) and submit a solved block.
    let pending = light_miner
        .generate_pending_header(POW_SHA_BCH, None, &[], None)
        .unwrap();
    assert_eq!(
        pending.header().hash(),
        verified.work_object.header().hash(),
        "light miner must reuse verified builder body header"
    );
}

#[test]
fn stateful_block_builder_produces_100_percent_consensus_valid_executed_block() {
    let chain = Chain::for_environment("local").unwrap();
    let caches = Arc::new(SharedCaches::new(4).unwrap());
    let store = LightHeaderStore::in_memory(chain.clone(), 128).unwrap();
    let view = LightView::new(store, caches.clone(), None);

    // Prepare genesis state + history + execution profile.
    let genesis_state = State::default();
    let mut history = History::default();
    history.insert(chain.genesis().clone()).unwrap();
    let profile = chain.execution_profile().unwrap();

    let mut cb1 = [0u8; 20];
    cb1[19] = 1;
    let mcfg = MinerConfig {
        enabled: true,
        mode: MinerMode::Full,
        primary_coinbase: cb1,
        ..Default::default()
    };
    let miner = Miner::new(chain.clone(), view, caches.clone(), mcfg);

    // Wire real stateful block builder calling `zone_execution::build_block`!
    {
        let st = genesis_state.clone();
        let ch = chain.clone();
        let c = caches.clone();
        miner.set_state_builder(Arc::new(move |draft, candidate_txs| {
            let mut hist = History::default();
            hist.insert(ch.genesis().clone())
                .map_err(|e| format!("history: {e}"))?;
            let prof = ch
                .execution_profile()
                .map_err(|e| format!("profile: {e}"))?;
            zone_execution::build_block(
                &st,
                &hist,
                &EmptyPast,
                draft,
                &prof,
                &[],
                candidate_txs,
                |h| {
                    quai_node::uncles::pow_hash(&c, h)
                        .map_err(|_| quai_consensus::Error::Invalid("uncle pow"))
                },
            )
            .map_err(|e| format!("build_block failed: {e}"))
        }));
    }

    // Build pending block #1 (using ProgPoW pow_id = 0 so it uses PINNED_SCHEDULE cleanly).
    let built_wo = miner.generate_pending_header(0, None, &[], None).unwrap();
    let built_raw = built_wo.encode();
    let built_rec = Record::decode(&built_raw).unwrap();

    // Verify the built block actually executed genesis lockup account in evmRoot!
    let body = built_rec.object().header().wire();
    let evm_root: Hash = body
        .evm_root
        .as_ref()
        .unwrap()
        .value
        .as_slice()
        .try_into()
        .unwrap();
    assert_ne!(
        evm_root,
        mpt::empty_root(),
        "genesis block #1 must populate lockup account in evmRoot"
    );

    // ULTIMATE CONSENSUS PROOF: Run `zone_execution::project` (verify_commitments = true)
    // on the mined block — EVERY SINGLE ROOT (evm, etx, utxo, receipts, outbound,
    // account count, gas used, state used, avg_tx_fees, total_fees) must pass!
    let mut verify_state = genesis_state;
    let executed = zone_execution::project(
        &mut verify_state,
        &history,
        &EmptyPast,
        &built_rec,
        &profile,
        &[],
        |h| {
            quai_node::uncles::pow_hash(&caches, h)
                .map_err(|_| quai_consensus::Error::Invalid("uncle pow"))
        },
    )
    .expect("built block MUST pass full consensus zone_execution::project!");
    assert_eq!(
        executed.outbound.len(),
        built_rec.object().outbound_etxs().len()
    );
    assert_eq!(verify_state.world.root().unwrap(), evm_root);
}

#[test]
fn cli_parses_light_workshares_builder_and_compact_miner_options() {
    let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    // 1. Compact stateful full-block mining preset
    let parsed = parse(&args(&[
        "run",
        "--compact-miner",
        "--coinbase",
        "0x0000000000000000000000000000000000000001",
        "--stratum",
    ]))
    .unwrap();
    let Command::Run(o) = parsed.command else {
        panic!("expected Command::Run");
    };
    assert!(!o.stateless);
    assert!(!o.archive);
    assert_eq!(o.prune_blocks, Some(256));
    assert_eq!(o.checkpoint_every, 2_000);
    assert_eq!(o.commit_every, 500);
    assert!(o.miner.enabled);
    assert!(o.p2p_publish);
    assert!(o.stratum.enabled);

    // 2. Light client with MPT proof RPC + stateless workshare mining
    let parsed = parse(&args(&[
        "light",
        "--upstream-rpc",
        "http://127.0.0.1:8547",
        "--mine-workshares",
        "--light-max-headers",
        "1024",
        "--light-checkpoint",
        "500",
        "--stratum",
    ]))
    .unwrap();
    let Command::Light(l) = parsed.command else {
        panic!("expected Command::Light");
    };
    assert_eq!(l.upstream_rpc.as_deref(), Some("http://127.0.0.1:8547"));
    assert_eq!(l.max_headers, 1024);
    assert_eq!(l.checkpoint_height, Some(500));
    assert!(l.miner.workshares_only);
    assert!(l.stratum.enabled);

    // 3. Zero-storage full-block light mining via --builder-rpc
    let parsed = parse(&args(&[
        "light",
        "--builder-rpc",
        "http://127.0.0.1:9200",
        "--stratum",
    ]))
    .unwrap();
    let Command::Light(l) = parsed.command else {
        panic!("expected Command::Light");
    };
    assert_eq!(l.builder_rpc.as_deref(), Some("http://127.0.0.1:9200"));
    assert!(l.stratum.enabled);
}
