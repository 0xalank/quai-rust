//! Filters and WebSocket subscriptions against go-quai v0.57.0's production
//! filter API, event system and JSON-RPC server (fixtures/protocol/
//! filters.json, scripts/gen_filter_vectors.py). Go ran each scenario over
//! a test chain with logs, a side branch and pending transactions, moving
//! the head as its core does (each new canonical block's logs, then one
//! head event). Here the same chain is a view whose head moves; the native
//! RPC serves it on a loopback port, HTTP calls go through `App::handle` and
//! WebSocket calls through real connections. Every answer, every frame and
//! every subscription's notifications must equal Go's. Filter and
//! subscription IDs are random in both: the fixture names them "$F<n>" and
//! "$S<n>", and each Rust ID must have Go's form.
use futures_util::{SinkExt, StreamExt};
use quai_consensus::verifier::PINNED_SCHEDULE;
use quai_node::rpc_view::decode_receipts;
use quai_primitives::{decode_hex, work::WorkObject, Hash};
use quai_rpc::view::{
    AccountState, BlockData, CallError, CallRequest, CallResult, ChainView, OutpointData,
    ReceiptData, TxPosition, UncleKind, UtxoData, ViewError, U256,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;

fn corpus() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/filters.json")).unwrap()
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

struct Chain {
    blocks: HashMap<Hash, Arc<BlockData>>,
    receipts: HashMap<Hash, Arc<Vec<ReceiptData>>>,
    names: HashMap<String, Hash>,
}
fn chain(c: &Value) -> Arc<Chain> {
    let mut out = Chain {
        blocks: HashMap::new(),
        receipts: HashMap::new(),
        names: HashMap::new(),
    };
    for (name, v) in c["chain"]["blocks"].as_object().unwrap() {
        let bytes = raw(&v["object"]);
        let object = WorkObject::decode_auxiliary(&bytes, PINNED_SCHEDULE).unwrap();
        let header = object.auxiliary_work_header(PINNED_SCHEDULE).unwrap();
        let uncles = object.uncle_headers(PINNED_SCHEDULE).unwrap();
        let hash: Hash = fixed(&v["hash"]);
        if !v["receipts"].is_null() {
            out.receipts.insert(
                hash,
                Arc::new(decode_receipts(&raw(&v["receipts"])).unwrap()),
            );
        }
        out.blocks.insert(
            hash,
            Arc::new(BlockData {
                hash,
                number: v["number"].as_u64().unwrap(),
                object,
                header,
                uncles,
                size: v["size"].as_u64().unwrap(),
                order: 2,
                tx_hashes: hashes(&v["tx_hashes"]),
                etx_hashes: hashes(&v["etx_hashes"]),
            }),
        );
        out.names.insert(name.clone(), hash);
    }
    Arc::new(out)
}

/// The fixture chain with a movable head: the canonical chain is the head's
/// ancestry.
struct View {
    chain: Arc<Chain>,
    canonical: Mutex<BTreeMap<u64, Hash>>,
    events: broadcast::Sender<Hash>,
}
fn unavailable() -> ViewError {
    ViewError::Unavailable("not in the fixture".into())
}
impl View {
    fn new(chain: Arc<Chain>, head: Hash) -> Self {
        let (events, _) = broadcast::channel(64);
        let v = Self {
            chain,
            canonical: Mutex::new(BTreeMap::new()),
            events,
        };
        v.set_head(head);
        v
    }
    fn set_head(&self, head: Hash) {
        let mut canonical = BTreeMap::new();
        let mut cur = self.chain.blocks.get(&head).cloned();
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
                self.chain.blocks.get(&parent).cloned()
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
        Ok(h.and_then(|h| self.chain.blocks.get(&h).cloned()))
    }
    fn block_by_hash(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        let Some(b) = self.chain.blocks.get(hash) else {
            return Ok(None);
        };
        let canonical = self.canonical.lock().unwrap().get(&b.number) == Some(hash);
        Ok(canonical.then(|| b.clone()))
    }
    fn block_or_candidate(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok(self.chain.blocks.get(hash).cloned())
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
    fn receipts(&self, block: &BlockData) -> Result<Arc<Vec<ReceiptData>>, ViewError> {
        self.chain
            .receipts
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

/// go-quai's ID form: "0x0" or "0x" and up to 32 hex digits without a
/// leading zero.
fn go_id(id: &str) -> bool {
    let Some(hex) = id.strip_prefix("0x") else {
        return false;
    };
    hex == "0"
        || (!hex.is_empty()
            && hex.len() <= 32
            && !hex.starts_with('0')
            && hex
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
}

/// Placeholders ("$F1", "$S2") and the Rust IDs they stand for.
#[derive(Default)]
struct Ids {
    real: HashMap<String, String>,
    name: HashMap<String, String>,
}
impl Ids {
    fn to_real(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.real.get(s).cloned().unwrap_or(s.clone())),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.to_real(x)).collect()),
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, x)| (k.clone(), self.to_real(x)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    fn to_names(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.name.get(s).cloned().unwrap_or(s.clone())),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.to_names(x)).collect()),
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, x)| (k.clone(), self.to_names(x)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    /// Learn the IDs that `want` names where `got` has a fresh ID.
    fn learn(&mut self, want: &Value, got: &Value) {
        match (want, got) {
            (Value::String(w), Value::String(g))
                if (w.starts_with("$F") || w.starts_with("$S")) && !self.real.contains_key(w) =>
            {
                assert!(go_id(g), "ID {g} has not go-quai's form");
                assert!(!self.name.contains_key(g), "ID {g} reused");
                self.real.insert(w.clone(), g.clone());
                self.name.insert(g.clone(), w.clone());
            }
            (Value::Array(w), Value::Array(g)) => {
                for (a, b) in w.iter().zip(g) {
                    self.learn(a, b);
                }
            }
            (Value::Object(w), Value::Object(g)) => {
                for (k, a) in w {
                    if let Some(b) = g.get(k) {
                        self.learn(a, b);
                    }
                }
            }
            _ => {}
        }
    }
}

/// One WebSocket connection: answers by request ID, notifications and
/// frames in arrival order.
struct Conn {
    send: mpsc::UnboundedSender<Message>,
    frames: mpsc::UnboundedReceiver<String>,
    /// Frames read but not yet consumed by a call (notifications).
    pending: Vec<String>,
    /// The raw text of the latest answer.
    last_answer: String,
    /// Every frame received.
    seen: Vec<String>,
}
async fn connect(addr: std::net::SocketAddr) -> Conn {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/"))
        .await
        .unwrap();
    let (mut sink, mut stream) = ws.split();
    let (send, mut outgoing) = mpsc::unbounded_channel::<Message>();
    let (incoming, frames) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(m) = outgoing.recv().await {
            let close = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || close {
                break;
            }
        }
    });
    tokio::spawn(async move {
        while let Some(Ok(m)) = stream.next().await {
            if let Message::Text(t) = m {
                let _ = incoming.send(t.to_string());
            }
        }
    });
    Conn {
        send,
        frames,
        pending: vec![],
        last_answer: String::new(),
        seen: vec![],
    }
}
fn is_answer(text: &str, want: &Value) -> bool {
    let v: Value = serde_json::from_str(text).unwrap();
    match (v, want) {
        (Value::Array(_), Value::Array(_)) => true,
        (Value::Object(m), Value::Object(w)) => {
            !m.contains_key("method") && m.get("id") == w.get("id")
        }
        _ => false,
    }
}
impl Conn {
    async fn call(&mut self, request: &Value, want: &Value) -> Value {
        self.send
            .send(Message::Text(request.to_string().into()))
            .unwrap();
        loop {
            let text = tokio::time::timeout(Duration::from_secs(10), self.frames.recv())
                .await
                .expect("an answer within 10 s")
                .expect("an open connection");
            self.seen.push(text.clone());
            if is_answer(&text, want) {
                self.last_answer = text.clone();
                return serde_json::from_str(&text).unwrap();
            }
            self.pending.push(text);
        }
    }
    /// Everything that arrived until 400 ms pass without a frame.
    async fn drain(&mut self) -> Vec<String> {
        while let Ok(Some(t)) =
            tokio::time::timeout(Duration::from_millis(400), self.frames.recv()).await
        {
            self.seen.push(t.clone());
            self.pending.push(t);
        }
        std::mem::take(&mut self.pending)
    }
}

async fn run(chain: Arc<Chain>, s: &Value) -> Result<usize, String> {
    let name = s["name"].as_str().unwrap();
    let view = Arc::new(View::new(
        chain.clone(),
        chain.names[s["head"].as_str().unwrap()],
    ));
    let app = quai_rpc::App::native(
        quai_rpc::Config {
            mode: "native".into(),
            filter_timeout_ms: s["timeout_ms"].as_u64().unwrap(),
            ws_max_subscriptions: s["subscription_limit"].as_u64().unwrap() as usize,
            ..quai_rpc::Config::default()
        },
        view.clone(),
    )
    .unwrap();
    // go-quai makes its filter API when the node starts.
    let api = app.filter_api().unwrap().clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(quai_rpc::serve_until(app.clone(), listener, async {
        let _ = stopped.await;
    }));
    let mut conns: HashMap<u64, Conn> = HashMap::new();
    let mut ids = Ids::default();
    let mut checked = 0;
    for (i, step) in s["steps"].as_array().unwrap().iter().enumerate() {
        let at = format!("{name} step {i} {}", step);
        match step["op"].as_str().unwrap() {
            "call" => {
                let request = ids.to_real(&step["request"]);
                let want = &step["response"];
                let got = if step["via"] == "http" {
                    let body = app.handle(request.to_string().as_bytes()).await.unwrap();
                    serde_json::from_slice(&body).unwrap()
                } else {
                    let k = step["conn"].as_u64().unwrap();
                    if !conns.contains_key(&k) {
                        conns.insert(k, connect(addr).await);
                    }
                    conns.get_mut(&k).unwrap().call(&request, want).await
                };
                ids.learn(want, &got);
                let mut got = ids.to_names(&got);
                if step["sorted"] == true {
                    if let Some(list) = got.get_mut("result").and_then(Value::as_array_mut) {
                        let n = |v: &Value| {
                            u64::from_str_radix(
                                v.as_str().unwrap_or("0x0").trim_start_matches("0x"),
                                16,
                            )
                            .unwrap_or(0)
                        };
                        list.sort_by_key(|l| (n(&l["blockNumber"]), n(&l["logIndex"])));
                    }
                }
                if &got != want {
                    return Err(format!("{at}\n  want {want}\n  got  {got}"));
                }
            }
            "frame" => {
                let k = step["conn"].as_u64().unwrap();
                let want = step["text"].as_str().unwrap();
                let conn = conns.get_mut(&k).unwrap();
                if step.get("kind").is_some() {
                    // A notification: its envelope text and its result.
                    let want_v: Value = serde_json::from_str(want).unwrap();
                    let sub = want_v["params"]["subscription"].as_str().unwrap();
                    let real = &ids.real[sub];
                    let prefix = format!(
                        "{{\"jsonrpc\":\"2.0\",\"method\":\"quai_subscription\",\"params\":{{\"subscription\":\"{real}\",\"result\":"
                    );
                    let matches = |t: &String| {
                        t.starts_with(&prefix)
                            && t.ends_with("}}\n")
                            && ids.to_names(&serde_json::from_str::<Value>(t).unwrap()) == want_v
                    };
                    if !conn.seen.iter().any(matches) {
                        // The frame may still be on its way.
                        let more = conn.drain().await;
                        conn.pending = more;
                        if !conn.seen.iter().any(matches) {
                            return Err(format!("{at}\n  no such notification frame"));
                        }
                    }
                } else {
                    let got = ids
                        .real
                        .iter()
                        .fold(conn.last_answer.clone(), |t, (p, r)| t.replace(r, p));
                    if got != want {
                        return Err(format!("{at}\n  want {want:?}\n  got  {got:?}"));
                    }
                }
            }
            "head" => {
                let head = chain.names[step["head"].as_str().unwrap()];
                let before = api.handled();
                view.set_head(head);
                view.events.send(head).unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                while api.handled() == before {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "{at}: head not handled"
                    );
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
            "notifications" => {
                let k = step["conn"].as_u64().unwrap();
                let conn = conns.get_mut(&k).unwrap();
                let mut by: BTreeMap<String, Vec<Value>> = BTreeMap::new();
                let mut other = vec![];
                for text in conn.drain().await {
                    let v: Value = serde_json::from_str(&text).unwrap();
                    if v["method"] == "quai_subscription" {
                        let sub = v["params"]["subscription"].as_str().unwrap();
                        let sub = ids.name.get(sub).cloned().unwrap_or(sub.to_string());
                        by.entry(sub)
                            .or_default()
                            .push(v["params"]["result"].clone());
                    } else {
                        other.push(ids.to_names(&v));
                    }
                }
                let want: BTreeMap<String, Vec<Value>> = step["by_subscription"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_array().unwrap().clone()))
                    .collect();
                if by != want {
                    let summary = |m: &BTreeMap<String, Vec<Value>>| {
                        m.iter()
                            .map(|(k, v)| format!("{k}:{}", v.len()))
                            .collect::<Vec<_>>()
                            .join(" ")
                    };
                    let detail = want
                        .iter()
                        .find(|(k, v)| by.get(*k) != Some(*v))
                        .map(|(k, v)| {
                            format!(
                                "\n  first difference {k}\n  want {}\n  got  {}",
                                Value::Array(v.clone()),
                                by.get(k).map_or(Value::Null, |g| Value::Array(g.clone()))
                            )
                        })
                        .unwrap_or_default();
                    return Err(format!(
                        "{at}\n  want {}\n  got  {}{detail}",
                        summary(&want),
                        summary(&by)
                    ));
                }
                let want_other = step.get("other").cloned().unwrap_or(Value::Array(vec![]));
                if Value::Array(other.clone()) != want_other {
                    return Err(format!("{at}\n  other frames {other:?}"));
                }
            }
            "sleep" => {
                tokio::time::sleep(Duration::from_millis(step["ms"].as_u64().unwrap())).await;
            }
            "close" => {
                let k = step["conn"].as_u64().unwrap();
                if let Some(conn) = conns.remove(&k) {
                    let _ = conn.send.send(Message::Close(None));
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            op => return Err(format!("{at}: unknown op {op}")),
        }
        checked += 1;
    }
    let _ = stop.send(());
    let _ = server.await;
    Ok(checked)
}

#[test]
fn filters_and_subscriptions_match_go() {
    let c = corpus();
    assert_eq!(
        c["reference_revision"],
        "33db82f2891b267ba898ca38bd2dd8bf75cbb072"
    );
    let chain = chain(&c);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let mut total = 0;
    let mut failures = vec![];
    for s in c["scenarios"].as_array().unwrap() {
        match runtime.block_on(run(chain.clone(), s)) {
            Ok(n) => {
                println!(
                    "{}: {n}/{} steps equal",
                    s["name"],
                    s["steps"].as_array().unwrap().len()
                );
                total += n;
            }
            Err(e) => failures.push(e),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert_eq!(total, 311);
}

/// A head that moves back onto a block that is already canonical (a
/// rollback with no new block): go-quai's `SetCurrentHeader` appends no
/// block, so no logs, and `GeneratePendingHeader` still sends one head
/// event. Not in the Go oracle; from core/headerchain.go and core/slice.go.
#[test]
fn rollback_onto_a_canonical_block_sends_only_the_head() {
    let c = corpus();
    let chain = chain(&c);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let view = Arc::new(View::new(chain.clone(), chain.names["b3"]));
        let app = quai_rpc::App::native(
            quai_rpc::Config {
                mode: "native".into(),
                ..quai_rpc::Config::default()
            },
            view.clone(),
        )
        .unwrap();
        let api = app.filter_api().unwrap().clone();
        let call = |method: &str, params: Value| {
            let app = app.clone();
            let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
            async move {
                let out = app.handle(body.to_string().as_bytes()).await.unwrap();
                serde_json::from_slice::<Value>(&out).unwrap()["result"].clone()
            }
        };
        let blocks = call("quai_newBlockFilter", Value::Array(vec![])).await;
        let logs = call("quai_newFilter", serde_json::json!([{}])).await;
        let mut handled = api.handled();
        for name in ["b5", "b4"] {
            let h = chain.names[name];
            view.set_head(h);
            view.events.send(h).unwrap();
            while api.handled() == handled {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            handled = api.handled();
            let hashes = call("quai_getFilterChanges", serde_json::json!([blocks])).await;
            assert_eq!(
                hashes,
                serde_json::json!([quai_primitives::encode_hex(&h)]),
                "{name}"
            );
            let got = call("quai_getFilterChanges", serde_json::json!([logs])).await;
            let numbers: Vec<&str> = got
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l["blockNumber"].as_str().unwrap())
                .collect();
            if name == "b5" {
                // b4 and b5 became canonical.
                assert!(numbers.iter().all(|n| *n == "0x4" || *n == "0x5"));
                assert!(numbers.contains(&"0x4"), "{numbers:?}");
            } else {
                assert!(numbers.is_empty(), "rollback sent logs {numbers:?}");
            }
        }
    });
}
