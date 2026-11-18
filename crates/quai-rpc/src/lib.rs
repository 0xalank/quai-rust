use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::{stream, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{io::Write, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[cfg(feature = "reference-gateway")]
mod fresh;
#[cfg(feature = "reference-gateway")]
pub mod ingest;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Serialize)]
pub struct Config {
    pub listen: String,
    pub mode: String,
    #[serde(skip_serializing)]
    pub upstream: Option<String>,
    #[serde(skip_serializing)]
    pub snapshot: Option<String>,
    pub follow: bool,
    pub native_canonical_reads: bool,
    pub poll_ms: u64,
    pub window_blocks: usize,
    pub workers: usize,
    pub batch_concurrency: usize,
    pub encoding: String,
    pub max_inflight: usize,
    pub max_batch: usize,
    pub max_body_bytes: usize,
    pub max_response_bytes: usize,
    pub timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8547".into(),
            mode: "fixture".into(),
            upstream: None,
            snapshot: None,
            follow: false,
            native_canonical_reads: false,
            poll_ms: 1000,
            window_blocks: 128,
            workers: 2,
            batch_concurrency: 1,
            encoding: "value".into(),
            max_inflight: 128,
            max_batch: 128,
            max_body_bytes: 1024 * 1024,
            max_response_bytes: 10 * 1024 * 1024,
            timeout_ms: 5000,
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if !["fixture", "gateway", "hybrid", "snapshot"].contains(&self.mode.as_str()) {
            return Err("mode must be fixture, gateway, hybrid or snapshot".into());
        }
        #[cfg(not(feature = "reference-gateway"))]
        if ["gateway", "hybrid"].contains(&self.mode.as_str())
            || self.upstream.is_some()
            || self.follow
            || self.native_canonical_reads
        {
            return Err("Go-backed modes are excluded from the standalone Rust build; use the isolated reference profile".into());
        }
        if self.native_canonical_reads && !self.follow {
            return Err("native canonical reads require --follow true".into());
        }
        if self.follow
            && (self.mode != "hybrid"
                || !(100..=30_000).contains(&self.poll_ms)
                || !(1..=1024).contains(&self.window_blocks))
        {
            return Err(
                "follow requires hybrid mode, poll 100..30000 ms and window 1..1024".into(),
            );
        }
        #[cfg(feature = "reference-gateway")]
        if self.follow {
            let url =
                reqwest::Url::parse(self.upstream.as_deref().ok_or("follow requires upstream")?)
                    .map_err(|_| "invalid upstream")?;
            if !matches!(url.host_str(), Some("127.0.0.1" | "[::1]")) || url.query().is_some() {
                return Err("follow requires literal loopback upstream without query".into());
            }
        }
        if !["value", "raw"].contains(&self.encoding.as_str()) {
            return Err("encoding must be value or raw".into());
        }
        if !(1..=64).contains(&self.workers)
            || !(1..=128).contains(&self.batch_concurrency)
            || !(1..=4096).contains(&self.max_inflight)
            || !(1..=1024).contains(&self.max_batch)
            || !(1..=60_000).contains(&self.timeout_ms)
            || !(128..=16 * 1024 * 1024).contains(&self.max_body_bytes)
            || !(128..=32 * 1024 * 1024).contains(&self.max_response_bytes)
        {
            return Err("configuration outside supported bounds".into());
        }
        if ["snapshot", "hybrid"].contains(&self.mode.as_str()) && self.snapshot.is_none() {
            return Err("snapshot and hybrid modes require --snapshot".into());
        }
        if self.snapshot.is_some() && !["snapshot", "hybrid"].contains(&self.mode.as_str()) {
            return Err("--snapshot requires snapshot or hybrid mode".into());
        }
        #[cfg(feature = "reference-gateway")]
        if self.mode == "gateway" || self.mode == "hybrid" {
            let url = reqwest::Url::parse(
                self.upstream
                    .as_deref()
                    .ok_or("gateway requires --upstream")?,
            )
            .map_err(|_| "invalid upstream URL")?;
            if url.scheme() != "http"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(
                    "v0.1 upstream must be an HTTP URL without embedded credentials or fragments"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

pub struct App {
    pub config: Config,
    #[cfg(feature = "reference-gateway")]
    client: reqwest::Client,
    store: Option<quai_store::Store>,
    ingestion: std::sync::RwLock<Value>,
    native_hits: AtomicU64,
    native_receipt_hits: AtomicU64,
    native_log_hits: AtomicU64,
    freshness_http_calls: AtomicU64,
    canonical_misses: [AtomicU64; 4],
    canonical_stale: AtomicU64,
    canonical_unavailable: AtomicU64,
    canonical_identity_rejections: AtomicU64,
    reload_failures: AtomicU64,
    upstream_calls: AtomicU64,
    permits: Arc<Semaphore>,
    payload: String,
    hash: String,
    encoded_payload_fields: Vec<u8>,
}

enum Outcome {
    Result(Value),
    Encoded(Vec<u8>),
    Error(Value),
}

fn error(code: i64, message: &str) -> Outcome {
    Outcome::Error(json!({"code":code,"message":message}))
}

fn encode(id: &Value, outcome: Outcome) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(256);
    bytes.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":");
    serde_json::to_writer(&mut bytes, id).expect("JSON Value serializes");
    match outcome {
        Outcome::Result(value) => {
            bytes.extend_from_slice(b",\"result\":");
            serde_json::to_writer(&mut bytes, &value).expect("JSON Value serializes");
        }
        Outcome::Encoded(value) => {
            // Reserve the result marker and closing brace with the payload.
            bytes.reserve(value.len() + 11);
            bytes.extend_from_slice(b",\"result\":");
            bytes.extend_from_slice(&value);
        }
        Outcome::Error(value) => {
            bytes.extend_from_slice(b",\"error\":");
            serde_json::to_writer(&mut bytes, &value).expect("JSON Value serializes");
        }
    }
    bytes.push(b'}');
    bytes
}

impl App {
    pub fn new(config: Config) -> Result<Arc<Self>, String> {
        config.validate()?;
        #[cfg(feature = "reference-gateway")]
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_millis(config.timeout_ms))
            .pool_max_idle_per_host(config.max_inflight)
            .build()
            .map_err(|e| e.to_string())?;
        let payload = "0123456789abcdef".repeat(64);
        let hash = format!("0x{}", "ab".repeat(32));
        let encoded_payload_fields = format!(
            ",\"hash\":{},\"data\":{}}}",
            serde_json::to_string(&hash).unwrap(),
            serde_json::to_string(&payload).unwrap()
        )
        .into_bytes();
        let store = config
            .snapshot
            .as_ref()
            .map(|path| quai_store::Store::open(Path::new(path)))
            .transpose()?;
        Ok(Arc::new(Self {
            permits: Arc::new(Semaphore::new(config.max_inflight)),
            config: config.clone(),
            #[cfg(feature = "reference-gateway")]
            client,
            store,
            ingestion: std::sync::RwLock::new(json!({"enabled":config.follow,"state":"starting"})),
            native_hits: AtomicU64::new(0),
            native_receipt_hits: AtomicU64::new(0),
            native_log_hits: AtomicU64::new(0),
            freshness_http_calls: AtomicU64::new(0),
            canonical_misses: std::array::from_fn(|_| AtomicU64::new(0)),
            canonical_stale: AtomicU64::new(0),
            canonical_unavailable: AtomicU64::new(0),
            canonical_identity_rejections: AtomicU64::new(0),
            reload_failures: AtomicU64::new(0),
            upstream_calls: AtomicU64::new(0),
            payload,
            hash,
            encoded_payload_fields,
        }))
    }

    #[cfg(feature = "reference-gateway")]
    pub(crate) fn set_ingestion_status(&self, value: Value) {
        *self.ingestion.write().expect("ingestion lock") = value;
    }
    #[cfg(feature = "reference-gateway")]
    pub(crate) fn install_snapshot(&self, snapshot: quai_store::Snapshot) -> Result<(), String> {
        self.store
            .as_ref()
            .ok_or("missing store")?
            .replace(snapshot)
    }

    #[cfg(not(feature = "reference-gateway"))]
    async fn gateway(&self, _request: &Value, _id: &Value, _method: &str) -> Outcome {
        error(
            -32601,
            "upstream gateway is not compiled into this Rust build",
        )
    }
    #[cfg(not(feature = "reference-gateway"))]
    async fn canonical_read(&self, _method: &str, _params: &Value) -> Option<Outcome> {
        None
    }

    pub fn reload_snapshot(&self) -> Result<(), String> {
        let path = self
            .config
            .snapshot
            .as_ref()
            .ok_or("no snapshot configured")?;
        self.store
            .as_ref()
            .ok_or("no store configured")?
            .reload(Path::new(path))
            .inspect_err(|_| {
                self.reload_failures.fetch_add(1, Ordering::Relaxed);
            })
    }

    #[cfg(feature = "reference-gateway")]
    pub async fn verify_store_upstream(&self) -> Result<(), String> {
        if self.config.mode != "hybrid" {
            return Ok(());
        }
        let identity = self
            .store
            .as_ref()
            .ok_or("missing store")?
            .snapshot()
            .identity
            .clone();
        for (method, params, expected) in [
            ("quai_chainId", json!([]), json!(identity.chain_id)),
            (
                "quai_nodeLocation",
                json!([]),
                json!(identity
                    .location
                    .iter()
                    .map(|n| format!("0x{n:x}"))
                    .collect::<Vec<_>>()),
            ),
        ] {
            let request = json!({"jsonrpc":"2.0","id":"startup","method":method,"params":params});
            match self.gateway(&request, &json!("startup"), method).await {
                Outcome::Result(value) if value == expected => (),
                _ => return Err("snapshot/upstream network identity mismatch".into()),
            }
        }
        let request = json!({"jsonrpc":"2.0","id":"startup","method":"quai_getBlockByNumber","params":["0x0",false]});
        match self
            .gateway(&request, &json!("startup"), "quai_getBlockByNumber")
            .await
        {
            Outcome::Result(value) if value.get("hash") == Some(&json!(identity.genesis_hash)) => {
                Ok(())
            }
            _ => Err("snapshot/upstream genesis mismatch".into()),
        }
    }

    async fn fixture(&self, method: &str, params: &Value) -> Outcome {
        let args = match params {
            Value::Null => &[][..],
            Value::Array(args) => args.as_slice(),
            _ => return error(-32602, "invalid params"),
        };
        match method {
            "bench_blockNumber" | "quai_blockNumber" if args.is_empty() => {
                Outcome::Result(json!("0xa7d8c0"))
            }
            "quai_chainId" if args.is_empty() => Outcome::Result(json!("0x9")),
            "bench_null" if args.is_empty() => Outcome::Result(Value::Null),
            "bench_failure" if args.is_empty() => error(-32042, "benchmark failure"),
            "bench_echo" | "bench_payload" | "bench_delay" if args.len() == 1 => {
                let Some(sequence) = args[0].as_u64() else {
                    return error(-32602, "invalid params");
                };
                if method == "bench_delay" {
                    if sequence > 1000 {
                        return error(-32602, "delay exceeds fixture limit");
                    }
                    tokio::time::sleep(Duration::from_millis(sequence)).await;
                    return Outcome::Result(json!(sequence));
                }
                if method == "bench_echo" {
                    return Outcome::Result(json!(sequence));
                }
                if self.config.encoding == "raw" {
                    // Reuse pre-escaped immutable fields; IDs and variable values are
                    // still encoded per request. This is serialization reuse, not caching
                    // blockchain state or returning stale responses.
                    let mut out = Vec::with_capacity(self.encoded_payload_fields.len() + 48);
                    write!(&mut out, "{{\"sequence\":{sequence}").unwrap();
                    out.extend_from_slice(&self.encoded_payload_fields);
                    Outcome::Encoded(out)
                } else {
                    Outcome::Result(
                        json!({"sequence":sequence,"hash":self.hash,"data":self.payload}),
                    )
                }
            }
            "quai_getBalance" if args.len() == 2 && args[0].is_string() && args[1].is_string() => {
                Outcome::Result(json!("0x2a"))
            }
            "bench_blockNumber" | "quai_blockNumber" | "quai_chainId" | "bench_null"
            | "bench_failure" | "bench_echo" | "bench_payload" | "bench_delay"
            | "quai_getBalance" => error(-32602, "invalid params"),
            _ => error(-32601, "method not found"),
        }
    }

    #[cfg(feature = "reference-gateway")]
    async fn gateway(&self, request: &Value, id: &Value, method: &str) -> Outcome {
        if !is_read_method(method) {
            return error(-32601, "method not available in read-only gateway");
        }
        if self.config.follow
            && self.ingestion.read().expect("ingestion lock")["network_verified"] != true
        {
            return error(-32098, "upstream identity not verified by follower");
        }
        let mut forwarded = request.clone();
        // A caller notification still needs an upstream response to verify success;
        // its result is suppressed again before writing the downstream response.
        forwarded["id"] = id.clone();
        self.upstream_calls.fetch_add(1, Ordering::Relaxed);
        let response = match self
            .client
            .post(self.config.upstream.as_ref().unwrap())
            .json(&forwarded)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => response,
            _ => return error(-32098, "upstream request failed"),
        };
        if response
            .content_length()
            .is_some_and(|n| n > self.config.max_response_bytes as u64)
        {
            return error(-32005, "upstream response too large");
        }
        let mut response = response;
        let mut bytes = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if bytes.len() + chunk.len() > self.config.max_response_bytes {
                        return error(-32005, "upstream response too large");
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(_) => return error(-32098, "upstream body failed"),
            }
        }
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => return error(-32098, "invalid upstream JSON"),
        };
        if value.get("jsonrpc") != Some(&json!("2.0"))
            || value.get("id") != Some(id)
            || (value.get("result").is_some() == value.get("error").is_some())
        {
            return error(-32098, "invalid upstream response envelope");
        }
        if let Some(upstream_error) = value.get("error") {
            if upstream_error.get("code").and_then(Value::as_i64).is_none()
                || upstream_error
                    .get("message")
                    .and_then(Value::as_str)
                    .is_none()
            {
                return error(-32098, "invalid upstream error");
            }
            return Outcome::Error(upstream_error.clone());
        }
        Outcome::Result(value["result"].clone())
    }

    async fn call(&self, request: Value) -> Option<Vec<u8>> {
        let invalid = || Some(encode(&Value::Null, error(-32600, "invalid request")));
        let Some(object) = request.as_object() else {
            return invalid();
        };
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return invalid();
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return invalid();
        }
        let notification = !object.contains_key("id");
        let id = object.get("id").unwrap_or(&Value::Null);
        if !matches!(id, Value::Null | Value::String(_) | Value::Number(_)) {
            return invalid();
        }
        let params = object.get("params").unwrap_or(&Value::Null);
        let outcome = if !matches!(params, Value::Null | Value::Array(_) | Value::Object(_)) {
            error(-32602, "invalid params")
        } else if let Ok(_permit) = self.permits.try_acquire() {
            match tokio::time::timeout(Duration::from_millis(self.config.timeout_ms), async {
                if self.config.mode == "fixture" {
                    self.fixture(method, params).await
                } else {
                    if !is_read_method(method) {
                        error(-32601, "method not available in read-only gateway")
                    } else if let Some(value) = self.store.as_ref().and_then(|store| {
                        store
                            .snapshot()
                            .lookup(method, params, self.config.mode == "snapshot")
                    }) {
                        self.native_hits.fetch_add(1, Ordering::Relaxed);
                        Outcome::Encoded(value)
                    } else if self.config.mode == "snapshot" {
                        error(-32004, "read unavailable in this frozen snapshot")
                    } else if let Some(result) = self.canonical_read(method, params).await {
                        result
                    } else {
                        self.gateway(&request, id, method).await
                    }
                }
            })
            .await
            {
                Ok(outcome) => outcome,
                Err(_) => error(-32002, "request timed out"),
            }
        } else {
            error(-32005, "server busy")
        };
        if notification {
            None
        } else {
            Some(encode(id, outcome))
        }
    }

    pub async fn handle(&self, body: &[u8]) -> Option<Vec<u8>> {
        let value: Value = match serde_json::from_slice(body) {
            Ok(value) => value,
            Err(_) => return Some(encode(&Value::Null, error(-32700, "parse error"))),
        };
        let result = if let Value::Array(batch) = value {
            if batch.is_empty() {
                return Some(encode(&Value::Null, error(-32600, "empty batch")));
            }
            if batch.len() > self.config.max_batch {
                return Some(encode(&Value::Null, error(-32600, "batch too large")));
            }
            let mut replies = stream::iter(batch.into_iter().map(|request| self.call(request)))
                .buffered(self.config.batch_concurrency);
            let mut out = vec![b'['];
            let mut count = 0;
            while let Some(reply) = replies.next().await {
                if let Some(reply) = reply {
                    if out.len() + reply.len() + 2 > self.config.max_response_bytes {
                        return Some(encode(&Value::Null, error(-32005, "response too large")));
                    }
                    if count > 0 {
                        out.push(b',')
                    }
                    out.extend_from_slice(&reply);
                    count += 1;
                }
            }
            if count == 0 {
                None
            } else {
                out.push(b']');
                Some(out)
            }
        } else {
            self.call(value).await
        };
        if result
            .as_ref()
            .is_some_and(|body| body.len() > self.config.max_response_bytes)
        {
            Some(encode(&Value::Null, error(-32005, "response too large")))
        } else {
            result
        }
    }
}

pub fn is_read_method(method: &str) -> bool {
    matches!(
        method,
        "quai_chainId"
            | "quai_nodeLocation"
            | "quai_listRunningChains"
            | "quai_getProtocolExpansionNumber"
            | "quai_clientVersion"
            | "quai_getTransactionCount"
            | "quai_createAccessList"
            | "quai_estimateFeeForQi"
            | "quai_getLockedBalance"
            | "quai_getOutpointDeltasForAddressesInRange"
            | "quai_calculateConversionAmount"
            | "quai_qiToQuai"
            | "quai_quaiToQi"
            | "quai_blockNumber"
            | "quai_getBalance"
            | "quai_getCode"
            | "quai_getStorageAt"
            | "quai_getBlockByNumber"
            | "quai_getBlockByHash"
            | "quai_getTransactionByHash"
            | "quai_getTransactionReceipt"
            | "quai_getLogs"
            | "quai_getUTXO"
            | "quai_getOutpointsByAddress"
            | "quai_getOutPointsByAddressAndRange"
            | "quai_call"
            | "quai_estimateGas"
            | "quai_gasPrice"
            | "eth_clientVersion"
            | "eth_getTransactionCount"
            | "eth_createAccessList"
            | "eth_chainId"
            | "eth_blockNumber"
            | "eth_getBalance"
            | "eth_getCode"
            | "eth_getStorageAt"
            | "eth_getBlockByNumber"
            | "eth_getBlockByHash"
            | "eth_getTransactionByHash"
            | "eth_getTransactionReceipt"
            | "eth_getLogs"
            | "eth_call"
            | "eth_estimateGas"
            | "eth_gasPrice"
            | "net_version"
            | "web3_clientVersion"
    )
}

async fn rpc_handler(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .unwrap_or("")
        .trim();
    if ![
        "application/json",
        "application/json-rpc",
        "application/jsonrequest",
    ]
    .contains(&content_type)
    {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected application/json",
        )
            .into_response();
    }
    match app.handle(&body).await {
        Some(body) => ([(header::CONTENT_TYPE, "application/json")], body).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

async fn health(State(app): State<Arc<App>>) -> Json<Value> {
    Json(
        json!({"service":"quai-rust","version":VERSION,"mode":app.config.mode,
        "chain_verified":false,"runtime_profile":if cfg!(feature="reference-gateway") {"reference"}else{"standalone-rust"},"go_runtime_dependency":cfg!(feature="reference-gateway"),"chain_view":if app.config.mode=="snapshot" {"frozen-unverified-import"}else{"reference-fixture"},"config":app.config,
        "native_hits":app.native_hits.load(Ordering::Relaxed),"reload_failures":app.reload_failures.load(Ordering::Relaxed),"upstream_calls":app.upstream_calls.load(Ordering::Relaxed),
        "read_view":if app.config.mode == "snapshot" {"frozen"} else {"upstream"},
        "canonical_reads":{"enabled":app.config.native_canonical_reads,"native_receipts":app.native_receipt_hits.load(Ordering::Relaxed),"native_logs":app.native_log_hits.load(Ordering::Relaxed),"freshness_http_calls":app.freshness_http_calls.load(Ordering::Relaxed),"freshness_rpc_calls":app.freshness_http_calls.load(Ordering::Relaxed)*4,
        "fallbacks":{"unsupported":app.canonical_misses[0].load(Ordering::Relaxed),"coverage":app.canonical_misses[1].load(Ordering::Relaxed),"order":app.canonical_misses[2].load(Ordering::Relaxed),"budget":app.canonical_misses[3].load(Ordering::Relaxed),"head_mismatch":app.canonical_stale.load(Ordering::Relaxed),"unavailable":app.canonical_unavailable.load(Ordering::Relaxed)},"identity_rejections":app.canonical_identity_rejections.load(Ordering::Relaxed)},
        "ingestion":app.ingestion.read().expect("ingestion lock").clone(),
        "store":app.store.as_ref().map(quai_store::Store::info)}),
    )
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", post(rpc_handler))
        .route("/health", get(health))
        .layer(DefaultBodyLimit::max(app.config.max_body_bytes))
        .with_state(app)
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn run(raw: &str, config: Config) -> Option<Value> {
        App::new(config)
            .unwrap()
            .handle(raw.as_bytes())
            .await
            .map(|x| serde_json::from_slice(&x).unwrap())
    }
    #[tokio::test]
    async fn ids_results_and_notifications() {
        for id in [
            "null",
            "123",
            "9007199254740993123456789",
            "\"quoted\\\"id\"",
        ] {
            let raw =
                format!(r#"{{"jsonrpc":"2.0","method":"bench_echo","params":[42],"id":{id}}}"#);
            let result = run(&raw, Config::default()).await.unwrap();
            assert_eq!(result["id"], serde_json::from_str::<Value>(id).unwrap());
            assert_eq!(result["result"], 42);
        }
        assert!(run(
            r#"{"jsonrpc":"2.0","method":"bench_echo","params":[42]}"#,
            Config::default()
        )
        .await
        .is_none());
        assert!(run(
            r#"[{"jsonrpc":"2.0","method":"bench_echo","params":[42]}]"#,
            Config::default()
        )
        .await
        .is_none());
    }
    #[tokio::test]
    async fn protocol_errors() {
        for (raw, code) in [
            ("{", -32700),
            ("[]", -32600),
            ("null", -32600),
            ("3", -32600),
            (r#"{"jsonrpc":"1.0","method":"bench_null","id":1}"#, -32600),
            (
                r#"{"jsonrpc":"2.0","method":"bench_null","id":true}"#,
                -32600,
            ),
            (r#"{"jsonrpc":"2.0","method":"missing","id":1}"#, -32601),
            (
                r#"{"jsonrpc":"2.0","method":"bench_echo","params":[-1],"id":1}"#,
                -32602,
            ),
        ] {
            assert_eq!(
                run(raw, Config::default()).await.unwrap()["error"]["code"],
                code
            );
        }
    }
    #[tokio::test]
    async fn raw_encoding_and_batch_order_match() {
        let batch = json!([
            {"jsonrpc":"2.0","method":"bench_delay","params":[3],"id":1},
            {"jsonrpc":"2.0","method":"bench_payload","params":[123],"id":"two"},
            {"jsonrpc":"2.0","method":"bench_null","id":3},
            {"jsonrpc":"2.0","method":"bench_failure","id":4},
            {"jsonrpc":"2.0","method":"bench_echo","params":[4]}
        ])
        .to_string();
        let baseline = run(&batch, Config::default()).await;
        let config = Config {
            encoding: "raw".into(),
            batch_concurrency: 8,
            ..Config::default()
        };
        assert_eq!(baseline, run(&batch, config).await);
        assert_eq!(baseline.unwrap().as_array().unwrap().len(), 4);
    }
    #[tokio::test]
    async fn limits_and_timeout() {
        let config = Config {
            max_batch: 1,
            ..Config::default()
        };
        assert_eq!(
            run("[{},{}]", config).await.unwrap()["error"]["code"],
            -32600
        );
        let config = Config {
            max_response_bytes: 128,
            ..Config::default()
        };
        assert_eq!(
            run(
                r#"{"jsonrpc":"2.0","method":"bench_payload","params":[0],"id":1}"#,
                config
            )
            .await
            .unwrap()["error"]["code"],
            -32005
        );
        let config = Config {
            timeout_ms: 1,
            ..Config::default()
        };
        assert_eq!(
            run(
                r#"{"jsonrpc":"2.0","method":"bench_delay","params":[100],"id":1}"#,
                config
            )
            .await
            .unwrap()["error"]["code"],
            -32002
        );
    }
    #[test]
    fn gateway_boundary_and_config() {
        assert!(is_read_method("quai_getBalance"));
        for method in [
            "quai_getOutPointsByAddressAndRange",
            "quai_getTransactionCount",
            "quai_createAccessList",
            "quai_estimateFeeForQi",
        ] {
            assert!(is_read_method(method));
        }
        assert!(!is_read_method("quai_getOutpointsByAddressAndRange"));
        for method in [
            "quai_sendRawTransaction",
            "admin_nodeInfo",
            "debug_traceBlock",
            "personal_unlockAccount",
            "bench_payload",
        ] {
            assert!(!is_read_method(method));
        }
        assert!(Config {
            workers: 0,
            ..Config::default()
        }
        .validate()
        .is_err());
        assert!(Config {
            mode: "gateway".into(),
            ..Config::default()
        }
        .validate()
        .is_err());
    }
}
