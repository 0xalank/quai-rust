//! The native JSON-RPC of `quai-node rpc` against go-quai v0.57.0 answers
//! recorded for mainnet zone 0..=8 (fixtures/native-rpc/zone-0-8.json, made
//! by scripts/gen_native_rpc_vectors.py from a local go-quai node). The view
//! is a state replay of fixtures/mainnet/qex1-0-8 with its checkpoint at 8,
//! so the test needs neither Go nor network access.
use quai_consensus::pow::SharedCaches;
use quai_node::log::{Level, Log};
use quai_node::replay::{self, Options};
use quai_node::rpc_view::ExportView;
use quai_rpc::view::ChainView;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

fn mainnet_slice() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mainnet/qex1-0-8")
}
fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/native-rpc/zone-0-8.json")).unwrap()
}
/// A state replay of zone 0..=8 and the RPC view over it, shared by the tests.
fn view() -> Arc<ExportView> {
    static V: OnceLock<Arc<ExportView>> = OnceLock::new();
    V.get_or_init(|| {
        let data = std::env::temp_dir().join(format!("quai-node-rpc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let mut o = Options::new(mainnet_slice());
        o.jobs = 2;
        o.progress = Duration::from_secs(3600);
        o.state = true;
        o.datadir = Some(data.clone());
        let caches = Arc::new(SharedCaches::new(2).unwrap());
        let summary = replay::run_with(&o, &Log::new(Level::Warn), Some(caches)).unwrap();
        assert_eq!(summary.highest_passed, Some(8), "{}", summary.line());
        Arc::new(ExportView::open(&mainnet_slice(), &data, &Log::new(Level::Warn)).unwrap())
    })
    .clone()
}
fn app() -> Arc<quai_rpc::App> {
    let config = quai_rpc::Config {
        mode: "native".into(),
        timeout_ms: 60_000,
        ..quai_rpc::Config::default()
    };
    quai_rpc::App::native(config, view()).unwrap()
}
/// The answer without the envelope: {"result": ..} or {"error": ..}.
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
fn answers_equal_go_quai_for_zone_0_to_8() {
    let doc = fixture();
    assert_eq!(doc["head"], 8);
    let app = app();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut per: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut failures = vec![];
    for (i, v) in doc["vectors"].as_array().unwrap().iter().enumerate() {
        let method = v["method"].as_str().unwrap();
        let request =
            serde_json::json!({"jsonrpc":"2.0","id":i,"method":method,"params":v["params"]});
        let raw = runtime
            .block_on(app.handle(request.to_string().as_bytes()))
            .unwrap();
        let got = answer(&raw);
        let want = if v["compare"] == "difference" {
            &v["rust"]
        } else {
            &v["go"]
        };
        let row = per.entry(method.to_string()).or_default();
        row.0 += 1;
        if &got == want {
            row.1 += 1;
        } else {
            failures.push(format!(
                "{method} {}\n  want {want}\n  got  {got}",
                v["params"]
            ));
        }
    }
    for (m, (n, ok)) in &per {
        eprintln!("{m}: {ok}/{n}");
    }
    assert!(
        failures.is_empty(),
        "{} of {} answers differ:\n{}",
        failures.len(),
        doc["vectors"].as_array().unwrap().len(),
        failures.join("\n")
    );
}

#[test]
fn head_entropy_is_computed_as_the_child_records_it() {
    // For blocks below the head the view takes the total entropy from the
    // child's zone parent entropy; the head needs the native computation.
    // Both must agree wherever both exist.
    let v = view();
    for h in 1..8u64 {
        let child = v.record(h + 1).unwrap();
        let recorded = child.object().header().wire().parent_entropy[2].clone();
        let native = v.native_entropy(h).unwrap();
        let start = native.iter().position(|b| *b != 0).unwrap_or(32);
        assert_eq!(native[start..], recorded[..], "height {h}");
    }
    assert_eq!(v.head(), 8);
}

/// The view with stored logs added to the receipts of block 7. go-quai has
/// no logs before the first user transaction (zone 259,200), so the shape of
/// a log, its indices and the filter rules are checked against go-quai's
/// code (core/types/gen_log_json.go, quai/filters/filter.go) here.
struct WithLogs(Arc<ExportView>);
impl quai_rpc::view::ChainView for WithLogs {
    fn chain_id(&self) -> u64 {
        self.0.chain_id()
    }
    fn location(&self) -> [u8; 2] {
        self.0.location()
    }
    fn head(&self) -> u64 {
        self.0.head()
    }
    fn block_by_number(&self, n: u64) -> ViewResult<Option<Arc<BlockData>>> {
        self.0.block_by_number(n)
    }
    fn block_by_hash(&self, h: &[u8; 32]) -> ViewResult<Option<Arc<BlockData>>> {
        self.0.block_by_hash(h)
    }
    fn uncle_kinds(&self, b: &BlockData) -> ViewResult<Arc<Vec<quai_rpc::view::UncleKind>>> {
        self.0.uncle_kinds(b)
    }
    fn total_entropy(&self, b: &BlockData) -> ViewResult<[u8; 32]> {
        self.0.total_entropy(b)
    }
    fn transaction(&self, h: &[u8; 32]) -> ViewResult<Option<quai_rpc::view::TxPosition>> {
        self.0.transaction(h)
    }
    fn receipts(&self, b: &BlockData) -> ViewResult<Arc<Vec<ReceiptData>>> {
        let mut r = (*self.0.receipts(b)?).clone();
        if b.number == 7 {
            let log = |address: u8, topics: &[u8]| LogData {
                address: [address; 20],
                topics: topics.iter().map(|t| [*t; 32]).collect(),
                data: vec![address, 0xff],
            };
            r[0].logs = vec![log(1, &[10]), log(2, &[10, 11])];
            r[1].logs = vec![log(1, &[11, 12])];
        }
        Ok(Arc::new(r))
    }
    fn state(&self, b: &BlockData) -> ViewResult<Arc<dyn quai_rpc::view::AccountState>> {
        self.0.state(b)
    }
    fn utxo(&self, h: &[u8; 32], i: u16) -> ViewResult<Option<quai_rpc::view::UtxoData>> {
        self.0.utxo(h, i)
    }
    fn outpoints_by_address(&self, a: &[u8; 20]) -> ViewResult<Vec<quai_rpc::view::OutpointData>> {
        self.0.outpoints_by_address(a)
    }
    fn outpoints_by_address_and_range(
        &self,
        a: &[u8; 20],
        s: u32,
        e: u32,
    ) -> ViewResult<Vec<quai_rpc::view::OutpointData>> {
        self.0.outpoints_by_address_and_range(a, s, e)
    }
    fn call(
        &self,
        b: &BlockData,
        r: &quai_rpc::view::CallRequest,
    ) -> Result<quai_rpc::view::CallResult, quai_rpc::view::CallError> {
        self.0.call(b, r)
    }
}
type ViewResult<T> = Result<T, quai_rpc::view::ViewError>;
use quai_rpc::view::{BlockData, LogData, ReceiptData};

#[test]
fn logs_carry_block_positions_and_follow_go_filter_rules() {
    let v = WithLogs(view());
    let get = |params: Value| quai_rpc::native::dispatch(&v, "quai_getLogs", &params).unwrap();
    let all = get(serde_json::json!([{"fromBlock":"0x0","toBlock":"0x8"}]));
    let all = all.as_array().unwrap();
    assert_eq!(all.len(), 3);
    let block = v.block_by_number(7).unwrap().unwrap();
    let tx0 = quai_primitives::encode_hex(&block.tx_hashes[0]);
    let tx1 = quai_primitives::encode_hex(&block.tx_hashes[1]);
    // gen_log_json.go: address in lower case, positions as hex quantities.
    assert_eq!(
        all[2],
        serde_json::json!({
            "address": format!("0x{}", "01".repeat(20)),
            "topics": [format!("0x{}", "0b".repeat(32)), format!("0x{}", "0c".repeat(32))],
            "data": "0x01ff",
            "blockNumber": "0x7",
            "transactionHash": tx1,
            "transactionIndex": "0x1",
            "blockHash": quai_primitives::encode_hex(&block.hash),
            "logIndex": "0x2",
            "removed": false,
        })
    );
    assert_eq!(all[0]["transactionHash"], tx0);
    assert_eq!(all[1]["logIndex"], "0x1");
    // filterLogs: an address list, positional topics with null wildcards,
    // and fewer log topics than filter positions excluded.
    let addr1 = format!("0x{}", "01".repeat(20));
    let t = |b: &str| format!("0x{}", b.repeat(32));
    assert_eq!(
        get(serde_json::json!([{"fromBlock":"0x7","toBlock":"0x7","address":addr1}]))
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        get(serde_json::json!([{"fromBlock":"0x7","toBlock":"0x7","topics":[t("0a")]}]))
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        get(serde_json::json!([{"fromBlock":"0x7","toBlock":"0x7","topics":[null, t("0b")]}]))
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        get(serde_json::json!([{"fromBlock":"0x7","toBlock":"0x7","topics":[[t("0a"), t("0b")]]}]))
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        get(serde_json::json!([{"fromBlock":"0x7","toBlock":"0x7","topics":[[t("0a"), null]]}]))
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        get(serde_json::json!([{"fromBlock":"0x7","toBlock":"0x7","topics":[null, null, null]}]))
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let by_hash = get(serde_json::json!([{"blockHash": quai_primitives::encode_hex(&block.hash)}]));
    assert_eq!(by_hash.as_array().unwrap().len(), 3);
    // The receipt of transaction 1 has its own log, indexed in the block.
    let receipt =
        quai_rpc::native::dispatch(&v, "quai_getTransactionReceipt", &serde_json::json!([tx1]))
            .unwrap();
    assert_eq!(receipt["logs"].as_array().unwrap().len(), 1);
    assert_eq!(receipt["logs"][0]["logIndex"], "0x2");
    let bloom = quai_primitives::decode_hex(receipt["logsBloom"].as_str().unwrap()).unwrap();
    assert_eq!(bloom.len(), quai_primitives::receipt::BLOOM_BYTES);
    assert!(bloom.iter().any(|b| *b != 0));
}
