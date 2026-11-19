//! The transaction pool against go-quai v0.57.0's production `core.TxPool`
//! and quaiapi pool handlers (fixtures/protocol/txpool.json,
//! scripts/gen_txpool_vectors.py, reference/txpool).
//!
//! Each scenario is a test chain (work objects, committed state per head,
//! UTXO sets) and a list of steps. The test builds the same chain, drives
//! `quai_node::txpool` through the same steps (RPC submits through the
//! native handlers, gossip batches, runs, head moves with resets, aging and
//! eviction, RPC reads) and compares every answer and, after each step, the
//! pool: pending and queued transactions per account in nonce order, the Qi
//! pool in LRU order, locals, counts, slots, heartbeats and pending nonces.
//! Sharing clients are local mock servers that answer as the Go oracle's
//! did; their requests are compared too.
use quai_consensus::verifier::PINNED_SCHEDULE;
use quai_node::txpool::{self, service::Service, tx::PoolTx, Config, PoolChain};
use quai_primitives::{decode_hex, encode_hex, work::WorkObject, Hash};
use quai_rpc::native::dispatch;
use quai_rpc::view::{
    AccountState, BlockData, CallError, CallRequest, CallResult, ChainView, OutpointData,
    PoolContent, PoolTransaction, ReceiptData, TxPosition, UncleKind, UtxoData, ViewError, U256,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn corpus() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/txpool.json")).unwrap()
}
fn raw(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
fn fixed<const N: usize>(v: &Value) -> [u8; N] {
    raw(v).try_into().unwrap()
}

// --- chain -------------------------------------------------------------------

struct State(BTreeMap<[u8; 20], (u64, U256)>);
impl AccountState for State {
    fn balance(&self, a: &[u8; 20]) -> Result<U256, ViewError> {
        Ok(self.0.get(a).map_or([0; 32], |s| s.1))
    }
    fn nonce(&self, a: &[u8; 20]) -> Result<u64, ViewError> {
        Ok(self.0.get(a).map_or(0, |s| s.0))
    }
    fn code(&self, _: &[u8; 20]) -> Result<Vec<u8>, ViewError> {
        Ok(vec![])
    }
    fn storage(&self, _: &[u8; 20], _: &Hash) -> Result<Hash, ViewError> {
        Ok([0; 32])
    }
}

struct Chain {
    by_name: HashMap<String, Arc<BlockData>>,
    by_hash: HashMap<Hash, (Arc<BlockData>, bool)>,
    states: HashMap<String, Arc<State>>,
    heads: HashMap<Hash, (String, u64)>,
    utxo_sets: HashMap<String, BTreeMap<(Hash, u16), UtxoData>>,
    current: Mutex<(Arc<BlockData>, String)>,
}
impl PoolChain for Chain {
    fn chain_id(&self) -> u64 {
        9
    }
    fn location(&self) -> [u8; 2] {
        [0, 0]
    }
    fn current_block(&self) -> Arc<BlockData> {
        self.current.lock().unwrap().0.clone()
    }
    fn get_block(&self, hash: &Hash, number: u64) -> Option<Arc<BlockData>> {
        self.by_hash
            .get(hash)
            .filter(|(b, stored)| *stored && b.number == number)
            .map(|(b, _)| b.clone())
    }
    fn header_by_hash(&self, hash: &Hash) -> Option<Arc<BlockData>> {
        self.by_hash
            .get(hash)
            .filter(|(_, stored)| *stored)
            .map(|(b, _)| b.clone())
    }
    fn state_at(&self, block: &BlockData) -> Result<Arc<dyn AccountState>, String> {
        let (name, _) = self.heads.get(&block.hash).ok_or("no state")?;
        Ok(self.states[name].clone())
    }
    fn utxo(&self, hash: &Hash, index: u16) -> Option<UtxoData> {
        let set = self.current.lock().unwrap().1.clone();
        self.utxo_sets.get(&set)?.get(&(*hash, index)).cloned()
    }
    fn utxo_set_size(&self, block: &Hash) -> u64 {
        self.heads.get(block).map_or(0, |h| h.1)
    }
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

fn chain(s: &Value) -> Chain {
    let mut by_name = HashMap::new();
    let mut by_hash = HashMap::new();
    for (name, b) in s["blocks"].as_object().unwrap() {
        let data = block(b);
        assert_eq!(data.hash, fixed::<32>(&b["hash"]));
        let stored = b.get("stored").and_then(Value::as_bool).unwrap_or(true);
        by_hash.insert(data.hash, (data.clone(), stored));
        by_name.insert(name.clone(), data);
    }
    let mut states = HashMap::new();
    for (name, list) in s["states"].as_object().unwrap() {
        let mut m = BTreeMap::new();
        for a in list.as_array().unwrap() {
            let mut balance = [0u8; 32];
            let b = raw(&a["balance"]);
            balance[32 - b.len()..].copy_from_slice(&b);
            m.insert(
                fixed(&a["address"]),
                (a["nonce"].as_u64().unwrap(), balance),
            );
        }
        states.insert(name.clone(), Arc::new(State(m)));
    }
    let mut heads = HashMap::new();
    for (name, h) in s["heads"].as_object().unwrap() {
        heads.insert(
            by_name[name].hash,
            (
                h["state"].as_str().unwrap().to_string(),
                h["utxo_set_size"].as_u64().unwrap(),
            ),
        );
    }
    let mut utxo_sets = HashMap::new();
    for (name, list) in s["utxo_sets"].as_object().unwrap() {
        let mut m = BTreeMap::new();
        for u in list.as_array().unwrap() {
            m.insert(
                (fixed(&u["hash"]), u["index"].as_u64().unwrap() as u16),
                UtxoData {
                    denomination: u["denomination"].as_u64().unwrap() as u8,
                    address: fixed(&u["address"]),
                    lock: raw(&u["lock"]),
                },
            );
        }
        utxo_sets.insert(name.clone(), m);
    }
    let start = &s["start"];
    let head = by_name[start["head"].as_str().unwrap()].clone();
    Chain {
        by_name,
        by_hash,
        states,
        heads,
        utxo_sets,
        current: Mutex::new((head, start["utxos"].as_str().unwrap().to_string())),
    }
}

// --- view ----------------------------------------------------------------------

/// The native handlers' view: no blocks, only the pool.
struct View(Arc<Service>);
fn none<T>() -> Result<T, ViewError> {
    Err(ViewError::Unavailable("test view".into()))
}
impl ChainView for View {
    fn chain_id(&self) -> u64 {
        9
    }
    fn location(&self) -> [u8; 2] {
        [0, 0]
    }
    fn head(&self) -> u64 {
        self.0.chain().current_block().number
    }
    fn block_by_number(&self, _: u64) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok(None)
    }
    fn block_by_hash(&self, _: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok(None)
    }
    fn uncle_kinds(&self, _: &BlockData) -> Result<Arc<Vec<UncleKind>>, ViewError> {
        none()
    }
    fn total_entropy(&self, _: &BlockData) -> Result<U256, ViewError> {
        none()
    }
    fn transaction(&self, _: &Hash) -> Result<Option<TxPosition>, ViewError> {
        Ok(None)
    }
    fn receipts(&self, _: &BlockData) -> Result<Arc<Vec<ReceiptData>>, ViewError> {
        none()
    }
    fn state(&self, _: &BlockData) -> Result<Arc<dyn AccountState>, ViewError> {
        none()
    }
    fn utxo(&self, _: &Hash, _: u16) -> Result<Option<UtxoData>, ViewError> {
        none()
    }
    fn outpoints_by_address(&self, _: &[u8; 20]) -> Result<Vec<OutpointData>, ViewError> {
        none()
    }
    fn outpoints_by_address_and_range(
        &self,
        _: &[u8; 20],
        _: u32,
        _: u32,
    ) -> Result<Vec<OutpointData>, ViewError> {
        none()
    }
    fn call(&self, _: &BlockData, _: &CallRequest) -> Result<CallResult, CallError> {
        Err(CallError::View(ViewError::Unavailable("test view".into())))
    }
    fn submit_transaction(&self, raw: &[u8]) -> Result<Hash, ViewError> {
        self.0.submit(raw).map_err(ViewError::Go)
    }
    fn pool_nonce(&self, a: &[u8; 20]) -> Option<u64> {
        Some(self.0.rpc_nonce(a))
    }
    fn pool_transaction(&self, h: &Hash) -> Option<PoolTransaction> {
        self.0.rpc_transaction(h)
    }
    fn pool_content(&self, from: Option<&[u8; 20]>) -> Option<PoolContent> {
        Some(self.0.rpc_content(from))
    }
    fn pool_stats(&self) -> Option<(u64, u64, u64)> {
        Some(self.0.rpc_stats())
    }
}

// --- sharing clients -----------------------------------------------------------

/// A mock sharing client: records requests and answers from a list (the
/// last answer repeats), as the oracle's httptest servers did.
struct Mock {
    url: String,
    got: Arc<Mutex<Vec<Value>>>,
}
fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        500 => "Internal Server Error",
        _ => "Status",
    }
}
fn mock(rt: &tokio::runtime::Runtime, replies: Vec<(u16, String, u64)>) -> Mock {
    let listener = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let got = Arc::new(Mutex::new(vec![]));
    let served = Arc::new(AtomicU64::new(0));
    let g = got.clone();
    rt.spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let (g, served, replies) = (g.clone(), served.clone(), replies.clone());
            tokio::spawn(async move {
                let mut buf = vec![];
                let mut chunk = [0u8; 4096];
                let (head, body) = loop {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..end]).to_string();
                        let len = head
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                (k.eq_ignore_ascii_case("content-length"))
                                    .then(|| v.trim().parse::<usize>().ok())?
                            })
                            .unwrap_or(0);
                        while buf.len() < end + 4 + len {
                            let n = s.read(&mut chunk).await.unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            buf.extend_from_slice(&chunk[..n]);
                        }
                        break (head, buf[end + 4..].to_vec());
                    }
                };
                let header = |name: &str| {
                    head.lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
                        })
                        .unwrap_or_default()
                };
                let first = head.lines().next().unwrap_or_default().to_string();
                let mut parts = first.split(' ');
                let method = parts.next().unwrap_or_default().to_string();
                let path = parts.next().unwrap_or_default().to_string();
                let n = served.fetch_add(1, Ordering::SeqCst) as usize;
                g.lock().unwrap().push(json!({
                    "method": method, "path": path, "content_type": header("content-type"),
                    "accept": header("accept"), "user_agent": header("user-agent"),
                    "body": String::from_utf8_lossy(&body),
                }));
                let (status, text, delay) = replies[n.min(replies.len() - 1)].clone();
                tokio::time::sleep(Duration::from_millis(delay)).await;
                let response = format!(
                    "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{text}",
                    status_text(status),
                    text.len()
                );
                let _ = s.write_all(response.as_bytes()).await;
            });
        }
    });
    Mock { url, got }
}

// --- replay ----------------------------------------------------------------------

fn h(v: &[u8]) -> String {
    encode_hex(v)
}
fn answer(r: Result<Value, quai_rpc::native::RpcError>) -> Value {
    match r {
        Ok(v) => json!({ "result": v }),
        Err(e) => json!({ "error": e.to_value() }),
    }
}
fn normalize(mut v: Value, mocks: &[Mock]) -> Value {
    if let Some(m) = v.pointer_mut("/error/message") {
        if let Some(s) = m.as_str() {
            let mut s = s.to_string();
            for (i, k) in mocks.iter().enumerate() {
                s = s.replace(&k.url, &format!("http://sharing-client-{i}"));
            }
            *m = Value::String(s);
        }
    }
    v
}

/// The pool as the oracle records it.
fn snapshot(svc: &Service, want: &Value) -> Value {
    let mut pool = svc.pool();
    let s = pool.snapshot();
    let lists = |m: &BTreeMap<[u8; 20], Vec<Hash>>| {
        Value::Object(
            m.iter()
                .map(|(a, hs)| {
                    (
                        h(a),
                        Value::Array(hs.iter().map(|x| Value::String(h(x))).collect()),
                    )
                })
                .collect(),
        )
    };
    let mut nonces = serde_json::Map::new();
    for k in want["nonces"].as_object().unwrap().keys() {
        let a: [u8; 20] = decode_hex(k).unwrap().try_into().unwrap();
        nonces.insert(k.clone(), json!(pool.nonce(&a)));
    }
    json!({
        "pending": lists(&s.pending),
        "queued": lists(&s.queued),
        "qi": s.qi.iter().map(|x| h(x)).collect::<Vec<_>>(),
        "locals": s.locals.iter().map(|x| h(x)).collect::<Vec<_>>(),
        "local_count": s.local_count,
        "remote_count": s.remote_count,
        "slots": s.slots,
        "beats": s.beats.iter().map(|x| h(x)).collect::<Vec<_>>(),
        "nonces": nonces,
    })
}

fn config(c: &Value, endpoints: Vec<String>) -> Config {
    Config {
        locals: c["locals"].as_array().unwrap().iter().map(fixed).collect(),
        no_locals: c["no_locals"].as_bool().unwrap(),
        sync_tx_with_return: c["sync_tx_with_return"].as_bool().unwrap(),
        price_limit: c["price_limit"].as_u64().unwrap(),
        price_bump: c["price_bump"].as_u64().unwrap(),
        account_slots: c["account_slots"].as_u64().unwrap(),
        global_slots: c["global_slots"].as_u64().unwrap(),
        account_queue: c["account_queue"].as_u64().unwrap(),
        global_queue: c["global_queue"].as_u64().unwrap(),
        qi_pool_size: c["qi_pool_size"].as_u64().unwrap() as usize,
        qi_tx_lifetime: c["qi_tx_lifetime_s"].as_u64().unwrap() * 1_000_000_000,
        lifetime: c["lifetime_s"].as_u64().unwrap() * 1_000_000_000,
        sharing_clients: endpoints,
        ..Config::default()
    }
}

/// Per scenario family and step kind: (steps, equal).
type Tally = BTreeMap<(String, String), (usize, usize)>;

fn replay(s: &Value, rt: &tokio::runtime::Runtime, tally: &mut Tally, failures: &mut Vec<String>) {
    let id = s["id"].as_str().unwrap();
    let family = id.split('-').next().unwrap().to_string();
    let chain = Arc::new(chain(s));
    let c = &s["config"];
    let mocks: Vec<Mock> = c["sharing_replies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|rs| {
            mock(
                rt,
                rs.as_array()
                    .unwrap()
                    .iter()
                    .map(|r| {
                        (
                            r["status"].as_u64().unwrap() as u16,
                            r["body"].as_str().unwrap().to_string(),
                            r["delay_ms"].as_u64().unwrap(),
                        )
                    })
                    .collect(),
            )
        })
        .collect();
    let endpoints = c["sharing_endpoints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let e = e.as_str().unwrap();
            match e.strip_prefix("http://sharing-client-") {
                Some(i) => mocks[i.parse::<usize>().unwrap()].url.clone(),
                None => e.to_string(),
            }
        })
        .collect();
    let clock = Arc::new(AtomicU64::new(1_000_000 * 1_000_000_000));
    let ck = clock.clone();
    let svc = Arc::new(
        Service::new(
            config(c, endpoints),
            chain.clone(),
            Box::new(move || ck.load(Ordering::SeqCst)),
            Some(rt.handle().clone()),
            None,
        )
        .unwrap(),
    );
    let view = View(svc.clone());
    for (i, step) in s["steps"].as_array().unwrap().iter().enumerate() {
        let now = clock.fetch_add(1_000_000, Ordering::SeqCst) + 1_000_000;
        let op = step["op"].as_str().unwrap();
        let mut problems: Vec<String> = vec![];
        let mut check = |what: &str, got: Value, want: &Value| {
            if &got != want {
                problems.push(format!("{what}: got {got} want {want}"));
            }
        };
        match op {
            "start" => {}
            "send" | "receive" => {
                let method = if op == "send" {
                    "quai_sendRawTransaction"
                } else {
                    "quai_receiveTxFromPoolSharingClient"
                };
                let got = answer(dispatch(&view, method, &json!([step["raw"]])));
                check("answer", normalize(got, &mocks), &step["answer"]);
                if let Some(size) = step.get("size") {
                    let tx = PoolTx::decode(&raw(&step["raw"]), now).unwrap();
                    check("size", json!(tx.size), size);
                }
            }
            "remotes" => {
                let txs = step["raw"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| Arc::new(PoolTx::decode(&raw(r), now).unwrap()))
                    .collect();
                let errs = svc.pool().add_remotes(txs, now);
                let got: Vec<Value> = errs
                    .into_iter()
                    .map(|e| e.err().map_or(Value::Null, Value::String))
                    .collect();
                check("errors", Value::Array(got), &step["errors"]);
            }
            "run" => svc.pool().run_reorg(None, now),
            "head" => {
                let new = chain.by_name[step["head"].as_str().unwrap()].clone();
                let old = match step.get("old").and_then(Value::as_str) {
                    Some(ghost) => chain.by_name[ghost].clone(),
                    None => chain.current_block(),
                };
                *chain.current.lock().unwrap() =
                    (new.clone(), step["utxos"].as_str().unwrap().to_string());
                svc.pool().run_reorg(Some((Some(old), new)), now);
            }
            "age_beat" | "age_tx" | "age_qi" => {
                let ago = step["seconds"].as_u64().unwrap() * 1_000_000_000;
                let mut pool = svc.pool();
                let found = match op {
                    "age_beat" => pool.age_beat(&fixed(&step["account"]), ago, now),
                    "age_tx" => pool.age_tx(&fixed(&step["hash"]), ago, now),
                    _ => pool.age_qi(&fixed(&step["hash"]), ago, now),
                };
                check("found", json!(found), &step["found"]);
            }
            "evict" => svc.pool().evict(now),
            "expire_qi" => {
                // The oracle waits for the 2 ms loop to settle.
                let mut pool = svc.pool();
                loop {
                    let before = pool.stats().2;
                    pool.expire_qi(now);
                    if pool.stats().2 == before {
                        break;
                    }
                }
            }
            "rpc" => {
                let got = answer(dispatch(
                    &view,
                    step["method"].as_str().unwrap(),
                    &step["params"],
                ));
                check("answer", got, &step["answer"]);
            }
            other => panic!("unknown step {other}"),
        }
        if let Some(want) = step.get("shared").and_then(Value::as_array) {
            // Asynchronous forwards: wait until each client has its count.
            let expect = |k: usize| want.iter().filter(|r| r["client"] == json!(k)).count();
            for _ in 0..200 {
                if mocks
                    .iter()
                    .enumerate()
                    .all(|(k, m)| m.got.lock().unwrap().len() >= expect(k))
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let mut got = vec![];
            for (k, m) in mocks.iter().enumerate() {
                for mut r in m.got.lock().unwrap().drain(..) {
                    r["client"] = json!(k);
                    got.push(r);
                }
            }
            let key = |v: &Value| (v["client"].to_string(), v["body"].to_string());
            let mut want = want.clone();
            got.sort_by_key(key);
            want.sort_by_key(key);
            check("shared", Value::Array(got), &Value::Array(want));
        }
        // Aging steps record no pool (Go's 2 ms loops may act at once).
        if let Some(want) = step.get("pool") {
            let pool = snapshot(&svc, want);
            check("pool", pool, want);
        }
        let entry = tally
            .entry((family.clone(), op.to_string()))
            .or_insert((0, 0));
        entry.0 += 1;
        if problems.is_empty() {
            entry.1 += 1;
        } else {
            failures.push(format!(
                "{id} step {i} ({op}, {}): {}",
                step["note"].as_str().unwrap_or(""),
                problems.join("; ")
            ));
        }
    }
}

#[test]
fn txpool_matches_go_oracle() {
    let v = corpus();
    assert_eq!(v["chain_id"], 9);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut tally = Tally::new();
    let mut failures = vec![];
    for s in v["scenarios"].as_array().unwrap() {
        replay(s, &rt, &mut tally, &mut failures);
    }
    let mut total = (0, 0);
    for ((family, op), (n, ok)) in &tally {
        eprintln!("txpool vectors {family:8} {op:10} {ok}/{n}");
        total.0 += n;
        total.1 += ok;
    }
    eprintln!("txpool vectors total {}/{}", total.1, total.0);
    for f in &failures {
        eprintln!("MISMATCH {f}");
    }
    assert!(
        failures.is_empty(),
        "{} of {} steps differ from Go",
        failures.len(),
        total.0
    );
    let _ = txpool::TX_MAX_SIZE;
}

/// The chain and alice's queued transactions (nonces 2 to 6) of the
/// quai-account-queue scenario.
fn account_queue() -> (Arc<Chain>, Vec<Vec<u8>>) {
    let v = corpus();
    let s = v["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "quai-account-queue")
        .unwrap()
        .clone();
    let raws = s["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|st| st["op"] == "send")
        .take(5)
        .map(|st| raw(&st["raw"]))
        .collect();
    (Arc::new(chain(&s)), raws)
}

/// go-quai's pool limiter (every 30 s) above GlobalQueue: `Cap(len -
/// AccountQueue)` keeps len - AccountQueue transactions of each account
/// (none when an account has no more than AccountQueue), then whole
/// accounts go while the queue is still at or above GlobalQueue. Go runs it
/// on a 30-second ticker that the oracle cannot time, so this pins the
/// port's reading of `poolLimiterGoroutine`.
#[test]
fn pool_limiter_follows_go() {
    let (chain, raws) = account_queue();
    let config = Config {
        global_queue: 2,
        account_queue: 3,
        ..Config::default()
    };
    let now = 1_000_000 * 1_000_000_000u64;
    let svc = Service::new(config, chain, Box::new(move || now), None, None).unwrap();
    let mut pool = svc.pool();
    let txs: Vec<_> = raws
        .iter()
        .map(|r| Arc::new(PoolTx::decode(r, now).unwrap()))
        .collect();
    assert!(pool.add_remotes(txs, now).iter().all(|e| e.is_ok()));
    assert_eq!(pool.stats(), (0, 5, 0));
    // 5 > 2: alice keeps 5 - 3 = 2, then 2 is not below 2 and alice goes.
    pool.limit(now);
    assert_eq!(pool.stats(), (0, 0, 0));
    // Below the limit nothing happens.
    let txs: Vec<_> = raws[..2]
        .iter()
        .map(|r| Arc::new(PoolTx::decode(r, now).unwrap()))
        .collect();
    pool.add_remotes(txs, now);
    pool.limit(now);
    assert_eq!(pool.stats(), (0, 2, 0));
    assert_eq!(pool.snapshot().slots, 2);
}

/// `core.Core.AddRemotes`: gossip transactions wait in an LRU by hash and
/// enter the pool as one `AddRemotes` batch, oldest first; a repeated hash
/// moves to the newest place.
#[test]
fn remote_queue_drains_oldest_first() {
    let (chain, raws) = account_queue();
    let now = 1_000_000 * 1_000_000_000u64;
    let svc = Service::new(Config::default(), chain, Box::new(move || now), None, None).unwrap();
    let txs: Vec<_> = raws
        .iter()
        .map(|r| Arc::new(PoolTx::decode(r, now).unwrap()))
        .collect();
    svc.add_remotes(vec![txs[1].clone(), txs[0].clone()]);
    svc.add_remotes(vec![txs[1].clone()]);
    let results = svc.drain_remotes();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.is_ok()));
    assert!(svc.drain_remotes().is_empty());
    // Both are queued (nonce gap) as remotes.
    let s = svc.pool().snapshot();
    assert_eq!((s.remote_count, s.local_count), (2, 0));
}

/// The transactions of a gossiped workshare view reach the pool decoded as
/// go-quai decodes them.
#[test]
fn workshare_transactions_are_extracted() {
    use prost::Message;
    use quai_primitives::schema::block::{
        ProtoTransaction, ProtoTransactions, ProtoWorkObject, ProtoWorkObjectBody,
        ProtoWorkObjectShareView,
    };
    let (_, raws) = account_queue();
    let txs = raws
        .iter()
        .map(|r| ProtoTransaction::decode(r.as_slice()).unwrap())
        .collect();
    let view = ProtoWorkObjectShareView {
        work_object: Some(ProtoWorkObject {
            wo_body: Some(ProtoWorkObjectBody {
                transactions: Some(ProtoTransactions { transactions: txs }),
                ..Default::default()
            }),
            ..Default::default()
        }),
    };
    let got = quai_node::tip::workshare_transactions(&view.encode_to_vec(), 7);
    assert_eq!(got.len(), 5);
    for (tx, r) in got.iter().zip(&raws) {
        assert_eq!(tx.hash, PoolTx::decode(r, 7).unwrap().hash);
    }
    assert!(quai_node::tip::workshare_transactions(&[0xff], 7).is_empty());
}

/// go-quai's journal: local transactions survive a restart (load, rotate,
/// append); remote ones do not.
#[test]
fn journal_keeps_local_transactions_across_restarts() {
    let (chain, raws) = account_queue();
    let dir = std::env::temp_dir().join(format!("quai-txpool-journal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("transactions.journal");
    let config = Config {
        journal: Some(path.clone()),
        ..Config::default()
    };
    let now = 1_000_000 * 1_000_000_000u64;
    let svc = Service::new(
        config.clone(),
        chain.clone(),
        Box::new(move || now),
        None,
        None,
    )
    .unwrap();
    // Two locals over RPC, one remote from gossip.
    assert!(svc.submit(&raws[0]).is_ok());
    assert!(svc.submit(&raws[1]).is_ok());
    let remote = Arc::new(PoolTx::decode(&raws[2], now).unwrap());
    svc.add_remotes(vec![remote]);
    assert!(svc.drain_remotes()[0].is_ok());
    drop(svc);
    let svc = Service::new(config, chain, Box::new(move || now), None, None).unwrap();
    let s = svc.pool().snapshot();
    // alice is local after the restart, so all three of her transactions
    // return as locals: the remote one too was journaled (`journalTx` keys
    // on the sender being local).
    assert_eq!(s.local_count, 3);
    assert_eq!(s.queued.values().map(Vec::len).sum::<usize>(), 3);
    assert_eq!(svc.pool().local().len(), 3);
    std::fs::remove_dir_all(&dir).unwrap();
}
