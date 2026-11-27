//! Async Tokio Stratum v1 server supporting SHA-256d (ASICBoost / BIP 310),
//! Scrypt, and KAWPOW with VarDiff, password options, PROXY protocol v1/v2,
//! duplicate share detection, and live `Miner` workshare/block submission
//! (`go-quai/stratum/server.go`, `go-quai/stratum/kawpow.go`).
use super::stats::PoolStats;
use crate::log::Log;
use crate::miner::{
    auxpow::{
        build_donor_header, extract_coinb1_coinb2, hex_plain, kawpow_seed_hash, target_to_hex,
        word_swap_prev_hash, POW_KAWPOW, POW_SCRYPT, POW_SHA_BCH,
    },
    Miner,
};
use crypto_bigint::{Encoding as _, U256, U512};
use quai_consensus::{
    auxiliary::{coinbase, Auxiliary},
    donor, share_rules, validation,
};
use quai_primitives::{encode_hex, transaction::Address, work::WorkObject, Hash};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Semaphore};

/// BIP 310 ASICBoost version rolling mask (SHA-256 only).
pub const VERSION_ROLLING_MASK: u32 = 0x1fff_e000;
/// Default connection cap (`go-quai/stratum/server.go`).
pub const DEFAULT_MAX_CONNECTIONS: usize = 10_000;
/// Default max single Stratum JSON line bytes (16 KiB).
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 16_384;
/// Default max `mining.authorize` attempts per connection.
pub const DEFAULT_MAX_AUTHORIZATIONS: usize = 8;

const VARDIFF_TARGET_SECS: f64 = 30.0;
const VARDIFF_WINDOW: usize = 60;
const VARDIFF_FIRST_ADJUST: u64 = 10;
const VARDIFF_RETARGET_INTERVAL: u64 = 30;
const VARDIFF_INITIAL_TIMEOUT: Duration = Duration::from_secs(60);
const VARDIFF_ESTABLISHED_SHARES: u64 = 30;
const VARDIFF_ESTABLISHED_TIMEOUT: Duration = Duration::from_secs(100);
const VARDIFF_DECAY_SHA: f64 = 0.25;
const VARDIFF_DECAY_OTHER: f64 = 0.5;
const LIVENESS_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_JOBS_PER_SESSION: usize = 16;
const MAX_DUP_SHARES_PER_SESSION: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Algo {
    Sha256,
    Scrypt,
    Kawpow,
}

impl Algo {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Scrypt => "scrypt",
            Self::Kawpow => "kawpow",
        }
    }
    pub fn pow_id(self) -> u8 {
        match self {
            Self::Sha256 => POW_SHA_BCH,
            Self::Scrypt => POW_SCRYPT,
            Self::Kawpow => POW_KAWPOW,
        }
    }
    /// `(initial_difficulty, minimum_difficulty)` matching `go-quai/stratum/server.go`.
    pub fn vardiff_defaults(self) -> (f64, f64) {
        match self {
            Self::Sha256 => (4096.0, 256.0),
            Self::Scrypt => (500_000.0, 200_000.0),
            Self::Kawpow => (50.0, 0.1),
        }
    }
    /// Compute the 256-bit share target corresponding to Stratum `difficulty`.
    pub fn diff_to_target(self, diff: f64) -> U256 {
        let diff = if !diff.is_finite() || diff <= 0.0 {
            1e-9
        } else {
            diff
        };
        // Diff-1 targets matching go-quai:
        // SHA256: 0x00000000ffff0000... (approx 2^224)
        // Scrypt: 0x0000ffff0000...     (approx 2^240)
        // KAWPOW: 0x00000000ff000000... (2^224 * 255)
        let diff1_u512: U512 = match self {
            Self::Sha256 => U512::from_u64(0xffff).shl_vartime(208),
            Self::Scrypt => U512::from_u64(0xffff).shl_vartime(224),
            Self::Kawpow => U512::from_u64(0xff).shl_vartime(224),
        };
        // Scale by 10^9 so fractional difficulties (e.g. KAWPOW 0.1 or test 1e-6) stay exact.
        const SCALE: f64 = 1_000_000_000.0;
        let scaled = (diff * SCALE).round();
        if scaled <= 1.0 {
            return U256::MAX;
        }
        let denom = if scaled >= u128::MAX as f64 {
            U512::from_u128(u128::MAX)
        } else {
            U512::from_u128(scaled as u128)
        };
        let q = diff1_u512
            .wrapping_mul(&U512::from_u64(SCALE as u64))
            .wrapping_div(&denom);
        if q.bits_vartime() > 256 {
            U256::MAX
        } else {
            q.resize()
        }
    }
}

/// Configuration for the Stratum v1 server and its HTTP/WS dashboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StratumConfig {
    pub enabled: bool,
    pub sha_addr: Option<String>,
    pub scrypt_addr: Option<String>,
    pub kawpow_addr: Option<String>,
    pub api_addr: Option<String>,
    pub pool_name: String,
    pub pool_tag: String,
    pub vardiff_enabled: bool,
    pub proxy_protocol: bool,
    pub max_connections: usize,
    pub max_message_bytes: usize,
    pub max_authorizations: usize,
    pub ws_origins: Vec<String>,
    /// Optional shared secret required in Stratum `mining.authorize` (`auth=<token>`).
    pub auth_token: Option<String>,
    /// Propagate D/128 KawPoW sub-shares onto workshare gossip so solo/home miners
    /// can broadcast local RPC transactions without waiting for a full workshare.
    pub subshares: bool,
}

impl Default for StratumConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sha_addr: Some("0.0.0.0:3333".into()),
            scrypt_addr: Some("0.0.0.0:3334".into()),
            kawpow_addr: Some("0.0.0.0:3335".into()),
            api_addr: Some("0.0.0.0:3336".into()),
            pool_name: "quai-rust Stratum".into(),
            pool_tag: "quai-rust".into(),
            vardiff_enabled: true,
            proxy_protocol: false,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            max_authorizations: DEFAULT_MAX_AUTHORIZATIONS,
            ws_origins: vec![],
            auth_token: None,
            subshares: false,
        }
    }
}

#[derive(Clone)]
struct StratumJob {
    id: String,
    algo: Algo,
    version: u32,
    prev_hash_le: Hash,
    coinb1: Vec<u8>,
    /// Raw coinb2 (starting at the 30-byte extraData, WITHOUT the 4-byte Stratum padding).
    coinb2_raw: Vec<u8>,
    /// Full coinbase tx (used by KAWPOW where extranonce isn't modified by miner).
    coinbase_tx: Vec<u8>,
    merkle_branch: Vec<Vec<u8>>,
    ntime: u32,
    nbits: u32,
    donor_height: u32,
    /// KAWPOW seal hash (`double_sha(&header_120[..80])`), natural byte order.
    kawpow_seal: Hash,
    /// KAWPOW reversed seal hash hex (what miners submit as `headerHash`).
    kawpow_seal_rev_hex: String,
    /// Workshare target for the job (so we know whether to record/submit).
    _quai_height: u64,
    /// Target difficulty in effect when job was dispatched.
    share_difficulty: f64,
}

struct Session {
    id: u64,
    algo: Algo,
    remote_addr: String,
    extranonce1: Vec<u8>,
    subscribed: bool,
    authorized: bool,
    auth_attempts: usize,
    user: String,
    coinbase: Address,
    worker_name: String,
    version_rolling: bool,
    version_mask: u32,
    miner_difficulty: Option<f64>,
    vardiff_enabled: bool,
    vardiff: f64,
    last_sent_diff: f64,
    vardiff_shares: VecDeque<Instant>,
    vardiff_total_shares: u64,
    last_share_time: Instant,
    last_vardiff_retarget: Instant,
    last_job_sent: Instant,
    job_frequency: Duration,
    skip_quai_updates: bool,
    skip_counter: u64,
    lockup_byte: Option<u8>,
    parent_update_only: bool,
    jobs: HashMap<String, StratumJob>,
    job_order: VecDeque<String>,
    submitted_shares: HashSet<Hash>,
    submitted_order: VecDeque<Hash>,
    out: mpsc::Sender<String>,
}

impl Session {
    fn new(
        id: u64,
        algo: Algo,
        remote_addr: String,
        vardiff_enabled: bool,
        out: mpsc::Sender<String>,
    ) -> Self {
        let extranonce1 = match algo {
            Algo::Kawpow => vec![(id & 0xff) as u8],
            Algo::Sha256 | Algo::Scrypt => (id as u32).to_be_bytes().to_vec(),
        };
        let (init_diff, _) = algo.vardiff_defaults();
        let now = Instant::now();
        Self {
            id,
            algo,
            remote_addr,
            extranonce1,
            subscribed: false,
            authorized: false,
            auth_attempts: 0,
            user: String::new(),
            coinbase: [0u8; 20],
            worker_name: "default".into(),
            version_rolling: false,
            version_mask: 0,
            miner_difficulty: None,
            vardiff_enabled,
            vardiff: init_diff,
            last_sent_diff: 0.0,
            vardiff_shares: VecDeque::new(),
            vardiff_total_shares: 0,
            last_share_time: now,
            last_vardiff_retarget: now,
            last_job_sent: now - Duration::from_secs(60),
            job_frequency: Duration::from_secs(5),
            skip_quai_updates: false,
            skip_counter: 0,
            lockup_byte: None,
            parent_update_only: false,
            jobs: HashMap::new(),
            job_order: VecDeque::new(),
            submitted_shares: HashSet::new(),
            submitted_order: VecDeque::new(),
            out,
        }
    }

    fn current_difficulty(&self) -> f64 {
        if let Some(d) = self.miner_difficulty {
            return d;
        }
        if self.vardiff_enabled {
            return self.vardiff;
        }
        self.algo.vardiff_defaults().0
    }

    fn send_json(&self, v: Value) {
        let mut line = v.to_string();
        line.push('\n');
        let _ = self.out.try_send(line);
    }

    fn note_job(&mut self, job: StratumJob) {
        let id = job.id.clone();
        if self.jobs.insert(id.clone(), job).is_none() {
            self.job_order.push_back(id);
            while self.job_order.len() > MAX_JOBS_PER_SESSION {
                if let Some(old) = self.job_order.pop_front() {
                    self.jobs.remove(&old);
                }
            }
        }
    }

    fn note_share_key(&mut self, key: Hash) -> bool {
        if !self.submitted_shares.insert(key) {
            return false;
        }
        self.submitted_order.push_back(key);
        while self.submitted_order.len() > MAX_DUP_SHARES_PER_SESSION {
            if let Some(old) = self.submitted_order.pop_front() {
                self.submitted_shares.remove(&old);
            }
        }
        true
    }

    fn maybe_adjust_vardiff(&mut self, now: Instant) -> bool {
        if !self.vardiff_enabled || self.miner_difficulty.is_some() {
            return false;
        }
        self.vardiff_shares.push_back(now);
        while self.vardiff_shares.len() > VARDIFF_WINDOW {
            self.vardiff_shares.pop_front();
        }
        self.vardiff_total_shares += 1;
        let should_adjust = self.vardiff_total_shares == VARDIFF_FIRST_ADJUST
            || (self.vardiff_total_shares > VARDIFF_FIRST_ADJUST
                && self
                    .vardiff_total_shares
                    .is_multiple_of(VARDIFF_RETARGET_INTERVAL));
        if !should_adjust || self.vardiff_shares.len() < 2 {
            return false;
        }
        let oldest = self.vardiff_shares.front().copied().unwrap_or(now);
        let elapsed = now.saturating_duration_since(oldest).as_secs_f64();
        let avg = elapsed / (self.vardiff_shares.len() - 1) as f64;
        if avg <= 0.0 {
            return false;
        }
        let ratio = (VARDIFF_TARGET_SECS / avg).clamp(0.25, 4.0);
        if (ratio - 1.0).abs() < 0.15 {
            return false;
        }
        let (_, min_diff) = self.algo.vardiff_defaults();
        let next = (self.vardiff * ratio).max(min_diff);
        if (next - self.vardiff).abs() > f64::EPSILON {
            self.vardiff = next;
            self.last_vardiff_retarget = now;
            return true;
        }
        false
    }
}

/// Handle to a running Stratum v1 server (bound addresses, stats, stop signal).
pub struct StratumHandle {
    pub sha_addr: Option<SocketAddr>,
    pub scrypt_addr: Option<SocketAddr>,
    pub kawpow_addr: Option<SocketAddr>,
    pub api_addr: Option<SocketAddr>,
    pub stats: Arc<PoolStats>,
    stop: Arc<AtomicBool>,
}

impl StratumHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Running Stratum v1 server shared across algorithm listeners and sessions.
pub struct StratumServer {
    config: StratumConfig,
    miner: Arc<Miner>,
    stats: Arc<PoolStats>,
    sessions: Mutex<HashMap<u64, Arc<Mutex<Session>>>>,
    next_session_id: AtomicU64,
    next_job_id: AtomicU64,
    last_kawpow_epoch: AtomicU64,
    conn_permits: Arc<Semaphore>,
    stop: Arc<AtomicBool>,
    log: Log,
}

impl StratumServer {
    pub fn new(
        config: StratumConfig,
        miner: Arc<Miner>,
        stop: Arc<AtomicBool>,
        log: Log,
    ) -> Arc<Self> {
        let stats = Arc::new(PoolStats::new(config.pool_name.clone()));
        let max_conns = config.max_connections.max(1);
        Arc::new(Self {
            config,
            miner,
            stats,
            sessions: Mutex::new(HashMap::new()),
            next_session_id: AtomicU64::new(1),
            next_job_id: AtomicU64::new(1),
            last_kawpow_epoch: AtomicU64::new(u64::MAX),
            conn_permits: Arc::new(Semaphore::new(max_conns)),
            stop,
            log,
        })
    }

    pub fn stats(&self) -> Arc<PoolStats> {
        self.stats.clone()
    }

    pub fn miner(&self) -> Arc<Miner> {
        self.miner.clone()
    }

    /// Bind configured algorithm listeners & dashboard API and launch background
    /// loops on `runtime`.
    pub async fn start(self: Arc<Self>) -> Result<StratumHandle, String> {
        let sha_addr = match &self.config.sha_addr {
            Some(addr) if !addr.is_empty() && addr != "off" => {
                let ln = TcpListener::bind(addr)
                    .await
                    .map_err(|e| format!("stratum sha listen {addr}: {e}"))?;
                let local = ln.local_addr().map_err(|e| e.to_string())?;
                self.log
                    .info(&format!("stratum: SHA-256 listening on {local}"));
                let srv = self.clone();
                tokio::spawn(async move { srv.accept_loop(ln, Algo::Sha256).await });
                Some(local)
            }
            _ => None,
        };
        let scrypt_addr = match &self.config.scrypt_addr {
            Some(addr) if !addr.is_empty() && addr != "off" => {
                let ln = TcpListener::bind(addr)
                    .await
                    .map_err(|e| format!("stratum scrypt listen {addr}: {e}"))?;
                let local = ln.local_addr().map_err(|e| e.to_string())?;
                self.log
                    .info(&format!("stratum: Scrypt listening on {local}"));
                let srv = self.clone();
                tokio::spawn(async move { srv.accept_loop(ln, Algo::Scrypt).await });
                Some(local)
            }
            _ => None,
        };
        let kawpow_addr = match &self.config.kawpow_addr {
            Some(addr) if !addr.is_empty() && addr != "off" => {
                let ln = TcpListener::bind(addr)
                    .await
                    .map_err(|e| format!("stratum kawpow listen {addr}: {e}"))?;
                let local = ln.local_addr().map_err(|e| e.to_string())?;
                self.log
                    .info(&format!("stratum: KAWPOW listening on {local}"));
                let srv = self.clone();
                tokio::spawn(async move { srv.accept_loop(ln, Algo::Kawpow).await });
                Some(local)
            }
            _ => None,
        };
        let api_addr = match &self.config.api_addr {
            Some(addr) if !addr.is_empty() && addr != "off" => {
                let ln = TcpListener::bind(addr)
                    .await
                    .map_err(|e| format!("stratum api listen {addr}: {e}"))?;
                let local = ln.local_addr().map_err(|e| e.to_string())?;
                self.log.info(&format!(
                    "stratum: dashboard API listening on http://{local}"
                ));
                let stats = self.stats.clone();
                let miner = self.miner.clone();
                let origins = self.config.ws_origins.clone();
                let stop = self.stop.clone();
                tokio::spawn(async move {
                    let _ = super::api::serve_dashboard(ln, stats, miner, origins, stop).await;
                });
                Some(local)
            }
            _ => None,
        };

        // Template polling & maintenance loop (1s poll, 5s force broadcast / VarDiff timeout / 10m liveness)
        let srv = self.clone();
        tokio::spawn(async move { srv.maintenance_loop().await });

        Ok(StratumHandle {
            sha_addr,
            scrypt_addr,
            kawpow_addr,
            api_addr,
            stats: self.stats.clone(),
            stop: self.stop.clone(),
        })
    }

    async fn accept_loop(self: Arc<Self>, listener: TcpListener, algo: Algo) {
        while !self.stop.load(Ordering::Relaxed) {
            let accept = tokio::time::timeout(Duration::from_secs(1), listener.accept()).await;
            let Ok(Ok((stream, peer_addr))) = accept else {
                continue;
            };
            let Ok(permit) = self.conn_permits.clone().try_acquire_owned() else {
                drop(stream);
                continue;
            };
            let srv = self.clone();
            tokio::spawn(async move {
                let _permit = permit;
                srv.handle_conn(stream, peer_addr, algo).await;
            });
        }
    }

    async fn handle_conn(self: Arc<Self>, stream: TcpStream, peer_addr: SocketAddr, algo: Algo) {
        let _ = stream.set_nodelay(true);
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let remote_addr = if self.config.proxy_protocol {
            parse_proxy_header(&mut reader)
                .await
                .unwrap_or_else(|| peer_addr.to_string())
        } else {
            peer_addr.to_string()
        };

        let (tx, mut rx) = mpsc::channel::<String>(256);
        let sid = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let sess = Arc::new(Mutex::new(Session::new(
            sid,
            algo,
            remote_addr,
            self.config.vardiff_enabled,
            tx,
        )));
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(sid, sess.clone());

        let writer = tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if write_half.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });

        let max_len = self.config.max_message_bytes;
        let mut line = Vec::with_capacity(512);
        while !self.stop.load(Ordering::Relaxed) {
            line.clear();
            let read_res = tokio::time::timeout(
                Duration::from_secs(30),
                read_bounded_line(&mut reader, &mut line, max_len),
            )
            .await;
            match read_res {
                Ok(Ok(0)) => break, // EOF
                Ok(Ok(_)) => {
                    let trimmed = std::str::from_utf8(&line).unwrap_or("").trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    self.handle_message(&sess, trimmed).await;
                }
                Ok(Err(_)) => break,
                Err(_) => continue, // Idle read timeout: loop and recheck stop
            }
        }

        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&sid);
        let (user, worker, authorized) = {
            let s = sess.lock().unwrap_or_else(|e| e.into_inner());
            (s.user.clone(), s.worker_name.clone(), s.authorized)
        };
        if authorized {
            self.stats
                .worker_disconnected(&user, &worker, algo.as_str());
        }
        drop(sess);
        writer.abort();
    }

    async fn handle_message(&self, sess: &Arc<Mutex<Session>>, text: &str) {
        let Ok(req) = serde_json::from_str::<Value>(text) else {
            return;
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req
            .get("params")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        match method {
            "mining.subscribe" => self.on_subscribe(sess, id),
            "mining.configure" => self.on_configure(sess, id, &params),
            "mining.extranonce.subscribe" => {
                sess.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .send_json(json!({"id": id, "result": true, "error": null}));
            }
            "mining.get_transactions" => {
                sess.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .send_json(json!({"id": id, "result": [], "error": null}));
            }
            "eth_submitHashrate" => self.on_submit_hashrate(sess, id, &params),
            "mining.authorize" => self.on_authorize(sess, id, &params).await,
            "mining.submit" => self.on_submit(sess, id, &params).await,
            _ => {
                sess.lock().unwrap_or_else(|e| e.into_inner()).send_json(
                    json!({"id": id, "result": null, "error": [20, format!("Unknown method: {method}"), null]}),
                );
            }
        }
    }

    fn on_subscribe(&self, sess: &Arc<Mutex<Session>>, id: Value) {
        let mut s = sess.lock().unwrap_or_else(|e| e.into_inner());
        s.subscribed = true;
        let en1_hex = hex_plain(&s.extranonce1);
        let result = match s.algo {
            Algo::Kawpow => json!([null, en1_hex]),
            Algo::Sha256 | Algo::Scrypt => json!([
                [["mining.set_difficulty", "1"], ["mining.notify", "1"]],
                en1_hex,
                4
            ]),
        };
        s.send_json(json!({"id": id, "result": result, "error": null}));
    }

    fn on_configure(&self, sess: &Arc<Mutex<Session>>, id: Value, params: &[Value]) {
        let mut s = sess.lock().unwrap_or_else(|e| e.into_inner());
        let wants_vr = params
            .first()
            .and_then(Value::as_array)
            .is_some_and(|exts| exts.iter().any(|e| e.as_str() == Some("version-rolling")));
        if s.algo == Algo::Sha256 && wants_vr {
            let requested_mask = params
                .get(1)
                .and_then(|v| v.get("version-rolling.mask"))
                .and_then(Value::as_str)
                .and_then(|h| u32::from_str_radix(h.trim_start_matches("0x"), 16).ok())
                .unwrap_or(VERSION_ROLLING_MASK);
            let mask = requested_mask & VERSION_ROLLING_MASK;
            s.version_rolling = true;
            s.version_mask = mask;
            let mask_hex = format!("{mask:08x}");
            s.send_json(json!({
                "id": id,
                "result": {
                    "version-rolling": true,
                    "version-rolling.mask": mask_hex,
                },
                "error": null
            }));
            s.send_json(json!({
                "id": null,
                "method": "mining.set_version_mask",
                "params": [mask_hex]
            }));
        } else {
            s.send_json(json!({
                "id": id,
                "result": { "version-rolling": false },
                "error": null
            }));
        }
    }

    fn on_submit_hashrate(&self, sess: &Arc<Mutex<Session>>, id: Value, params: &[Value]) {
        let s = sess.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hex_rate) = params.first().and_then(Value::as_str) {
            if let Ok(rate) = u64::from_str_radix(hex_rate.trim_start_matches("0x"), 16) {
                if s.authorized {
                    self.stats.record_reported_hashrate(
                        &s.user,
                        &s.worker_name,
                        s.algo.as_str(),
                        rate as f64,
                    );
                }
            }
        }
        s.send_json(json!({"id": id, "result": true, "error": null}));
    }

    async fn on_authorize(&self, sess: &Arc<Mutex<Session>>, id: Value, params: &[Value]) {
        let username = params.first().and_then(Value::as_str).unwrap_or("");
        let password = params.get(1).and_then(Value::as_str).unwrap_or("");

        let auth_result = {
            let mut s = sess.lock().unwrap_or_else(|e| e.into_inner());
            s.auth_attempts += 1;
            if s.auth_attempts > self.config.max_authorizations {
                s.send_json(json!({"id": id, "result": false, "error": [24, "Too many authorization attempts", null]}));
                return;
            }
            parse_auth_identity(username, password, &self.miner.location())
        };

        let parsed = match auth_result {
            Ok(p) => p,
            Err(msg) => {
                sess.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .send_json(json!({"id": id, "result": false, "error": [24, msg, null]}));
                return;
            }
        };

        if let Some(expected) = self.config.auth_token.as_deref().filter(|t| !t.is_empty()) {
            let valid = parsed
                .auth_token
                .as_deref()
                .is_some_and(|got| constant_time_eq(got.as_bytes(), expected.as_bytes()));
            if !valid {
                sess.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .send_json(json!({"id": id, "result": false, "error": [24, "Invalid auth token", null]}));
                return;
            }
        }

        let (algo, remote, diff) = {
            let mut s = sess.lock().unwrap_or_else(|e| e.into_inner());
            s.authorized = true;
            s.user = parsed.address_hex.clone();
            s.coinbase = parsed.coinbase;
            s.worker_name = parsed.worker_name.clone();
            if let Some(d) = parsed.difficulty {
                s.miner_difficulty = Some(d);
                s.vardiff_enabled = false;
            }
            if let Some(freq) = parsed.frequency {
                s.job_frequency = freq;
            }
            if let Some(skip) = parsed.skip {
                s.skip_quai_updates = skip;
            }
            if let Some(lock) = parsed.lock {
                s.lockup_byte = Some(lock);
            }
            if let Some(pu) = parsed.parent_update {
                s.parent_update_only = pu;
            }
            s.send_json(json!({"id": id, "result": true, "error": null}));
            (s.algo, s.remote_addr.clone(), s.current_difficulty())
        };

        self.stats.worker_connected(
            &parsed.address_hex,
            &parsed.worker_name,
            algo.as_str(),
            diff,
            &remote,
        );
        // Dispatch the initial job immediately after authorization
        self.send_job_to_session(sess, true).await;
    }

    async fn on_submit(&self, sess: &Arc<Mutex<Session>>, id: Value, params: &[Value]) {
        let (algo, authorized, user, worker_name, job_opt, en1, vr, vmask) = {
            let s = sess.lock().unwrap_or_else(|e| e.into_inner());
            let job_id = params.get(1).and_then(Value::as_str).unwrap_or("");
            (
                s.algo,
                s.authorized,
                s.user.clone(),
                s.worker_name.clone(),
                s.jobs.get(job_id).cloned(),
                s.extranonce1.clone(),
                s.version_rolling,
                s.version_mask,
            )
        };

        if !authorized {
            sess.lock().unwrap_or_else(|e| e.into_inner()).send_json(
                json!({"id": id, "result": false, "error": [24, "Unauthorized worker", null]}),
            );
            return;
        }
        let Some(job) = job_opt else {
            self.stats.record_share(
                &user,
                &worker_name,
                algo.as_str(),
                0.0,
                false,
                true,
                false,
                false,
                None,
            );
            sess.lock().unwrap_or_else(|e| e.into_inner()).send_json(
                json!({"id": id, "result": false, "error": [21, "Job not found", null]}),
            );
            return;
        };

        // Dedup key over submitted share tuple
        let dup_key =
            quai_primitives::transaction::keccak(format!("{}:{:?}", job.id, params).as_bytes());
        let is_new = sess
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .note_share_key(dup_key);
        if !is_new {
            self.stats.record_share(
                &user,
                &worker_name,
                algo.as_str(),
                job.share_difficulty,
                false,
                false,
                false,
                false,
                None,
            );
            sess.lock().unwrap_or_else(|e| e.into_inner()).send_json(
                json!({"id": id, "result": false, "error": [22, "Duplicate share", null]}),
            );
            return;
        }

        let verify_res = match algo {
            Algo::Sha256 | Algo::Scrypt => {
                self.verify_sha_scrypt_submit(&job, &en1, vr, vmask, params)
            }
            Algo::Kawpow => self.verify_kawpow_submit(&job, params),
        };

        match verify_res {
            Ok((pow_hash, raw_donor_block)) => {
                let share_target = algo.diff_to_target(job.share_difficulty);
                let pow_int = U256::from_be_bytes(pow_hash);
                if pow_int > share_target {
                    self.stats.record_share(
                        &user,
                        &worker_name,
                        algo.as_str(),
                        job.share_difficulty,
                        false,
                        false,
                        false,
                        false,
                        None,
                    );
                    sess.lock().unwrap_or_else(|e| e.into_inner()).send_json(
                        json!({"id": id, "result": false, "error": [23, "Low difficulty share", null]}),
                    );
                    return;
                }

                // Attempt submission to Miner to see if it meets Quai workshare / block target
                let miner = self.miner.clone();
                let pow_id = algo.pow_id();
                let broadcast_subshares = self.config.subshares && algo == Algo::Kawpow;
                let submit_outcome = tokio::task::spawn_blocking(move || {
                    miner.submit_block_with_options(&raw_donor_block, pow_id, broadcast_subshares)
                })
                .await
                .ok()
                .and_then(Result::ok);

                let (is_ws, is_block, block_info) = match submit_outcome {
                    Some(out) => (
                        out.status >= 1,
                        out.status == 2,
                        Some((encode_hex(&out.hash), out.number)),
                    ),
                    None => (false, false, None),
                };

                self.stats.record_share(
                    &user,
                    &worker_name,
                    algo.as_str(),
                    job.share_difficulty,
                    true,
                    false,
                    is_ws,
                    is_block,
                    block_info,
                );

                let (retargeted, new_diff) = {
                    let mut s = sess.lock().unwrap_or_else(|e| e.into_inner());
                    let now = Instant::now();
                    s.last_share_time = now;
                    s.send_json(json!({"id": id, "result": true, "error": null}));
                    let changed = s.maybe_adjust_vardiff(now);
                    (changed, s.current_difficulty())
                };
                if retargeted {
                    self.stats.update_worker_difficulty(
                        &user,
                        &worker_name,
                        algo.as_str(),
                        new_diff,
                    );
                    self.send_job_to_session(sess, false).await;
                }
            }
            Err(msg) => {
                self.stats.record_share(
                    &user,
                    &worker_name,
                    algo.as_str(),
                    job.share_difficulty,
                    false,
                    false,
                    false,
                    false,
                    None,
                );
                sess.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .send_json(json!({"id": id, "result": false, "error": [20, msg, null]}));
            }
        }
    }

    fn verify_sha_scrypt_submit(
        &self,
        job: &StratumJob,
        en1: &[u8],
        version_rolling: bool,
        version_mask: u32,
        params: &[Value],
    ) -> Result<(Hash, Vec<u8>), String> {
        let ex2_hex = params
            .get(2)
            .and_then(Value::as_str)
            .ok_or_else(|| "missing extranonce2".to_string())?;
        let ntime_hex = params
            .get(3)
            .and_then(Value::as_str)
            .ok_or_else(|| "missing ntime".to_string())?;
        let nonce_hex = params
            .get(4)
            .and_then(Value::as_str)
            .ok_or_else(|| "missing nonce".to_string())?;

        let ex2 = decode_plain_hex(ex2_hex)?;
        if ex2.len() != 4 && ex2.len() != 8 {
            return Err(format!("invalid extranonce2 length: {}", ex2.len()));
        }
        let ntime = u32::from_str_radix(ntime_hex.trim_start_matches("0x"), 16)
            .map_err(|_| "invalid ntime hex".to_string())?;
        let nonce = u32::from_str_radix(nonce_hex.trim_start_matches("0x"), 16)
            .map_err(|_| "invalid nonce hex".to_string())?;

        let mut version = job.version;
        if job.algo == Algo::Sha256 && version_rolling {
            if let Some(vb_hex) = params.get(5).and_then(Value::as_str) {
                let vb = u32::from_str_radix(vb_hex.trim_start_matches("0x"), 16)
                    .map_err(|_| "invalid version_bits hex".to_string())?;
                version = (job.version & !version_mask) | (vb & version_mask);
            }
        }

        // Rebuild full coinbase tx: coinb1 || en1(4B) || ex2(4B) || 00000000 (if 4B) || coinb2_raw
        let mut coinbase_tx = Vec::with_capacity(job.coinb1.len() + 12 + job.coinb2_raw.len());
        coinbase_tx.extend_from_slice(&job.coinb1);
        coinbase_tx.extend_from_slice(en1);
        coinbase_tx.extend_from_slice(&ex2);
        if ex2.len() == 4 {
            coinbase_tx.extend_from_slice(&[0u8; 4]);
        }
        coinbase_tx.extend_from_slice(&job.coinb2_raw);

        let merkle_root = coinbase::merkle_root(&coinbase_tx, &job.merkle_branch)
            .map_err(|e| format!("merkle root: {e}"))?;
        let header = build_donor_header(
            job.algo.pow_id(),
            version,
            job.prev_hash_le,
            merkle_root,
            ntime,
            job.nbits,
            u64::from(nonce),
            job.donor_height,
            [0u8; 32],
        );
        let pow_hash = match job.algo {
            Algo::Sha256 => donor::sha256d(&header).map_err(|e| e.to_string())?,
            Algo::Scrypt => donor::scrypt(&header).map_err(|e| e.to_string())?,
            Algo::Kawpow => return Err("unexpected algo".into()),
        };

        let mut raw_block = Vec::with_capacity(header.len() + 1 + coinbase_tx.len());
        raw_block.extend_from_slice(&header);
        raw_block.push(1); // 1 transaction (coinbase)
        raw_block.extend_from_slice(&coinbase_tx);
        Ok((pow_hash, raw_block))
    }

    fn verify_kawpow_submit(
        &self,
        job: &StratumJob,
        params: &[Value],
    ) -> Result<(Hash, Vec<u8>), String> {
        let nonce_hex = params
            .get(2)
            .and_then(Value::as_str)
            .ok_or_else(|| "missing kawpow nonce".to_string())?;
        let header_hash_hex = params
            .get(3)
            .and_then(Value::as_str)
            .ok_or_else(|| "missing kawpow headerHash".to_string())?;
        let mix_hex = params
            .get(4)
            .and_then(Value::as_str)
            .ok_or_else(|| "missing kawpow mixHash".to_string())?;

        let submitted_seal = header_hash_hex
            .trim_start_matches("0x")
            .to_ascii_lowercase();
        if submitted_seal != job.kawpow_seal_rev_hex {
            return Err("KAWPOW header hash mismatch".into());
        }
        let nonce = u64::from_str_radix(nonce_hex.trim_start_matches("0x"), 16)
            .map_err(|_| "invalid kawpow nonce hex".to_string())?;
        let mut mix_bytes = decode_plain_hex(mix_hex)?;
        if mix_bytes.len() != 32 {
            return Err("KAWPOW mixHash must be 32 bytes".into());
        }
        // Stratum KAWPOW miners send mixHash in big-endian; donor header expects little-endian.
        mix_bytes.reverse();
        let mix_le: Hash = mix_bytes
            .try_into()
            .map_err(|_| "mixHash length".to_string())?;

        let merkle_root = coinbase::merkle_root(&job.coinbase_tx, &job.merkle_branch)
            .map_err(|e| format!("kawpow merkle root: {e}"))?;
        let header_120 = build_donor_header(
            POW_KAWPOW,
            job.version,
            job.prev_hash_le,
            merkle_root,
            job.ntime,
            job.nbits,
            nonce,
            job.donor_height,
            mix_le,
        );
        // Double-check seal consistency
        let actual_seal = coinbase::double_sha(&header_120[..80]);
        if actual_seal != job.kawpow_seal {
            return Err("reconstructed KAWPOW seal hash mismatch".into());
        }
        let pow_hash = self.miner.compute_aux_pow_hash(POW_KAWPOW, &header_120)?;

        let mut raw_block = Vec::with_capacity(header_120.len() + 1 + job.coinbase_tx.len());
        raw_block.extend_from_slice(&header_120);
        raw_block.push(1);
        raw_block.extend_from_slice(&job.coinbase_tx);
        Ok((pow_hash, raw_block))
    }

    async fn send_job_to_session(&self, sess: &Arc<Mutex<Session>>, clean: bool) {
        let (algo, coinbase, lock, diff, last_sent_diff) = {
            let s = sess.lock().unwrap_or_else(|e| e.into_inner());
            if !s.authorized {
                return;
            }
            (
                s.algo,
                s.coinbase,
                s.lockup_byte,
                s.current_difficulty(),
                s.last_sent_diff,
            )
        };
        let miner = self.miner.clone();
        let tag = self.config.pool_tag.clone();
        let wo_res = tokio::task::spawn_blocking(move || {
            miner.generate_pending_header(algo.pow_id(), Some(coinbase), tag.as_bytes(), lock)
        })
        .await;
        let Ok(Ok(wo)) = wo_res else {
            return;
        };
        let Ok(job) = self.build_stratum_job(&wo, algo, diff) else {
            return;
        };
        // Pre-warm the KawPoW verification light cache on the first job of each
        // epoch (7500 donor blocks) so the first GPU share verifies immediately.
        if algo == Algo::Kawpow {
            let donor_height = u64::from(job.donor_height);
            let epoch = donor_height / 7500;
            if self.last_kawpow_epoch.swap(epoch, Ordering::Relaxed) != epoch {
                let miner = self.miner.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = miner.prewarm_kawpow_cache(donor_height);
                });
            }
        }
        let mut s = sess.lock().unwrap_or_else(|e| e.into_inner());
        let diff_changed = (diff - last_sent_diff).abs() > f64::EPSILON || last_sent_diff == 0.0;
        match algo {
            Algo::Sha256 | Algo::Scrypt => {
                if diff_changed {
                    s.send_json(json!({
                        "id": null,
                        "method": "mining.set_difficulty",
                        "params": [diff]
                    }));
                    s.last_sent_diff = diff;
                }
                let mut coinb2_padded = Vec::with_capacity(4 + job.coinb2_raw.len());
                coinb2_padded.extend_from_slice(&[0u8; 4]);
                coinb2_padded.extend_from_slice(&job.coinb2_raw);
                let branch_hex: Vec<String> =
                    job.merkle_branch.iter().map(|b| hex_plain(b)).collect();
                s.send_json(json!({
                    "id": null,
                    "method": "mining.notify",
                    "params": [
                        job.id,
                        word_swap_prev_hash(job.prev_hash_le),
                        hex_plain(&job.coinb1),
                        hex_plain(&coinb2_padded),
                        branch_hex,
                        format!("{:08x}", job.version),
                        format!("{:08x}", job.nbits),
                        format!("{:08x}", job.ntime),
                        clean
                    ]
                }));
            }
            Algo::Kawpow => {
                let target_hex = target_to_hex(algo.diff_to_target(diff));
                if diff_changed {
                    s.send_json(json!({
                        "id": null,
                        "method": "mining.set_target",
                        "params": [target_hex]
                    }));
                    s.last_sent_diff = diff;
                }
                let seed_hex = hex_plain(&kawpow_seed_hash(u64::from(job.donor_height)));
                s.send_json(json!({
                    "id": null,
                    "method": "mining.notify",
                    "params": [
                        job.id,
                        job.kawpow_seal_rev_hex,
                        seed_hex,
                        target_hex,
                        clean,
                        u64::from(job.donor_height),
                        format!("{:08x}", job.nbits)
                    ]
                }));
            }
        }
        s.last_job_sent = Instant::now();
        s.note_job(job);
    }

    fn build_stratum_job(
        &self,
        wo: &WorkObject,
        algo: Algo,
        share_difficulty: f64,
    ) -> Result<StratumJob, String> {
        let header = wo
            .auxiliary_work_header(self.miner.aux_schedule())
            .map_err(|e| e.to_string())?;
        let hw = header.wire();
        let aux_bytes = hw
            .aux_pow
            .as_deref()
            .ok_or_else(|| "pending work object has no AuxPoW".to_string())?;
        let aux = Auxiliary::decode(aux_bytes).map_err(|e| e.to_string())?;
        let dh = aux.header();
        if dh.len() < 80 {
            return Err("donor header too short".into());
        }
        let version = u32::from_le_bytes([dh[0], dh[1], dh[2], dh[3]]);
        let mut prev_hash_le = [0u8; 32];
        prev_hash_le.copy_from_slice(&dh[4..36]);
        let ntime = u32::from_le_bytes([dh[68], dh[69], dh[70], dh[71]]);
        let nbits = u32::from_le_bytes([dh[72], dh[73], dh[74], dh[75]]);
        let tx_bytes = aux.transaction().to_vec();
        let (coinb1, coinb2_raw) = extract_coinb1_coinb2(&tx_bytes)?;
        let decoded_cb = coinbase::Coinbase::decode(&tx_bytes).map_err(|e| e.to_string())?;
        let donor_height = if algo == Algo::Kawpow && dh.len() >= 80 {
            u32::from_le_bytes([dh[76], dh[77], dh[78], dh[79]])
        } else {
            decoded_cb.height
        };
        let template = aux.template().map_err(|e| e.to_string())?;
        let merkle_branch = template.merkle_branch;
        let kawpow_seal = if algo == Algo::Kawpow {
            coinbase::double_sha(&dh[..80])
        } else {
            [0u8; 32]
        };
        let mut seal_rev = kawpow_seal;
        seal_rev.reverse();
        let quai_height =
            share_rules::number(hw.number.as_deref().unwrap_or_default()).unwrap_or(0);
        let jid = format!("{:x}", self.next_job_id.fetch_add(1, Ordering::Relaxed));

        Ok(StratumJob {
            id: jid,
            algo,
            version,
            prev_hash_le,
            coinb1,
            coinb2_raw,
            coinbase_tx: tx_bytes,
            merkle_branch,
            ntime,
            nbits,
            donor_height,
            kawpow_seal,
            kawpow_seal_rev_hex: hex_plain(&seal_rev),
            _quai_height: quai_height,
            share_difficulty,
        })
    }

    async fn maintenance_loop(self: Arc<Self>) {
        let mut last_head = self.miner.view().head();
        let mut tpl_rx = self.miner.subscribe_template_updates();
        let mut poll_tick = tokio::time::interval(Duration::from_secs(1));
        let mut maint_tick = tokio::time::interval(Duration::from_secs(5));

        while !self.stop.load(Ordering::Relaxed) {
            tokio::select! {
                _ = poll_tick.tick() => {
                    let head = self.miner.view().head();
                    if head != last_head {
                        last_head = head;
                        self.broadcast_all(None, true, true).await;
                    }
                }
                Ok(pow_id) = tpl_rx.recv() => {
                    let algo = match pow_id {
                        POW_SCRYPT => Some(Algo::Scrypt),
                        POW_KAWPOW => Some(Algo::Kawpow),
                        _ => Some(Algo::Sha256),
                    };
                    self.broadcast_all(algo, true, false).await;
                }
                _ = maint_tick.tick() => {
                    self.check_stale_and_vardiff().await;
                }
            }
        }
    }

    async fn broadcast_all(&self, filter_algo: Option<Algo>, clean: bool, is_quai_change: bool) {
        let list: Vec<Arc<Mutex<Session>>> = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for sess in list {
            let send = {
                let mut s = sess.lock().unwrap_or_else(|e| e.into_inner());
                if !s.authorized
                    || filter_algo.is_some_and(|a| a != s.algo)
                    || (is_quai_change && s.parent_update_only)
                {
                    false
                } else if is_quai_change && s.skip_quai_updates {
                    s.skip_counter += 1;
                    s.skip_counter.is_multiple_of(2)
                } else {
                    true
                }
            };
            if send {
                self.send_job_to_session(&sess, clean).await;
            }
        }
    }

    async fn check_stale_and_vardiff(&self) {
        let now = Instant::now();
        let list: Vec<Arc<Mutex<Session>>> = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for sess in list {
            let mut needs_job = false;
            let mut drop_session = false;
            {
                let mut s = sess.lock().unwrap_or_else(|e| e.into_inner());
                if !s.authorized {
                    continue;
                }
                let has_liveness = s.vardiff_enabled || s.miner_difficulty.is_some();
                if has_liveness
                    && now.saturating_duration_since(s.last_share_time) >= LIVENESS_TIMEOUT
                {
                    drop_session = true;
                } else {
                    if s.vardiff_enabled && s.miner_difficulty.is_none() {
                        let threshold = if s.vardiff_total_shares >= VARDIFF_ESTABLISHED_SHARES {
                            VARDIFF_ESTABLISHED_TIMEOUT
                        } else {
                            VARDIFF_INITIAL_TIMEOUT
                        };
                        let last_activity = s.last_share_time.max(s.last_vardiff_retarget);
                        if now.saturating_duration_since(last_activity) >= threshold {
                            let decay = if s.algo == Algo::Sha256 {
                                VARDIFF_DECAY_SHA
                            } else {
                                VARDIFF_DECAY_OTHER
                            };
                            let (_, min_diff) = s.algo.vardiff_defaults();
                            let base = if s.last_sent_diff > 0.0 {
                                s.last_sent_diff
                            } else {
                                s.vardiff
                            };
                            let next = (base * decay).max(min_diff);
                            if (next - s.vardiff).abs() > f64::EPSILON {
                                s.vardiff = next;
                                s.last_vardiff_retarget = now;
                                needs_job = true;
                                self.stats.update_worker_difficulty(
                                    &s.user,
                                    &s.worker_name,
                                    s.algo.as_str(),
                                    next,
                                );
                            }
                        }
                    }
                    if now.saturating_duration_since(s.last_job_sent) >= s.job_frequency {
                        needs_job = true;
                    }
                }
            }
            if drop_session {
                // Dropping the sender closes the TCP writer on idle liveness timeout
                let sid = sess.lock().unwrap_or_else(|e| e.into_inner()).id;
                self.sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&sid);
            } else if needs_job {
                self.send_job_to_session(&sess, true).await;
            }
        }
    }
}

struct ParsedAuth {
    address_hex: String,
    coinbase: Address,
    worker_name: String,
    difficulty: Option<f64>,
    frequency: Option<Duration>,
    skip: Option<bool>,
    lock: Option<u8>,
    parent_update: Option<bool>,
    auth_token: Option<String>,
}

/// Constant-time byte slice equality for `--stratum-auth-token` checks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    let max_len = a.len().max(b.len());
    for i in 0..max_len {
        let ba = a.get(i).copied().unwrap_or(0);
        let bb = b.get(i).copied().unwrap_or(0);
        diff |= usize::from(ba ^ bb);
    }
    diff == 0
}

/// Public entrypoint for fuzzing Stratum `mining.authorize` username/password parsing.
pub fn parse_auth_for_fuzz(username: &str, password: &str, location: &[u8; 2]) -> Result<(), String> {
    parse_auth_identity(username, password, location).map(|_| ())
}

fn parse_auth_identity(
    username: &str,
    password: &str,
    location: &[u8; 2],
) -> Result<ParsedAuth, String> {
    let trimmed = username.trim();
    if trimmed.is_empty() {
        return Err("empty username".into());
    }
    let mut parts = trimmed.split('.');
    let addr_part = parts.next().unwrap_or("");
    let raw_addr = quai_primitives::decode_hex(addr_part)
        .or_else(|_| quai_primitives::decode_hex(&format!("0x{addr_part}")))
        .map_err(|_| format!("invalid Quai address: {addr_part}"))?;
    let coinbase: Address = raw_addr
        .try_into()
        .map_err(|_| "Quai address must be 20 bytes".to_string())?;
    if !validation::internal_address(&coinbase, location) {
        return Err("address is not in this zone slice".into());
    }
    let mut worker_name = "default".to_string();
    let mut frequency = None;
    let rest: Vec<&str> = parts.collect();
    if let Some(first) = rest.first() {
        if !first.is_empty() && !first.contains('=') {
            worker_name = (*first).to_string();
        }
    }
    for tok in rest {
        if let Some(v) = tok.strip_prefix("frequency=") {
            if let Ok(sec) = v.parse::<f64>() {
                if (1.0..=10.0).contains(&sec) {
                    frequency = Some(Duration::from_secs_f64(sec));
                }
            }
        }
    }

    let mut difficulty = None;
    let mut skip = None;
    let mut lock = None;
    let mut parent_update = None;
    let mut auth_token = None;
    let mut saw_kv = false;

    for token in password.split([',', '_']) {
        let token = token.trim();
        if let Some((k, v)) = token.split_once('=') {
            saw_kv = true;
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            match k.as_str() {
                "d" | "diff" => {
                    if let Ok(d) = v.parse::<f64>() {
                        if d.is_finite() && d > 0.0 {
                            difficulty = Some(d);
                        }
                    }
                }
                "frequency" | "freq" => {
                    if let Ok(sec) = v.parse::<f64>() {
                        if (1.0..=10.0).contains(&sec) {
                            frequency = Some(Duration::from_secs_f64(sec));
                        }
                    }
                }
                "skip" => {
                    if let Ok(b) = v.parse::<bool>() {
                        skip = Some(b);
                    }
                }
                "lock" => {
                    if let Ok(b) = v.parse::<u8>() {
                        if b <= 3 {
                            lock = Some(b);
                        }
                    }
                }
                "parentupdate" => {
                    if let Ok(b) = v.parse::<bool>() {
                        parent_update = Some(b);
                    }
                }
                "auth" | "token" if !v.is_empty() => {
                    auth_token = Some(v.to_string());
                }
                _ => {}
            }
        }
    }
    if auth_token.is_none() && !saw_kv {
        let bare = password.trim();
        if !bare.is_empty() && bare != "x" {
            auth_token = Some(bare.to_string());
        }
    }

    Ok(ParsedAuth {
        address_hex: encode_hex(&coinbase),
        coinbase,
        worker_name,
        difficulty,
        frequency,
        skip,
        lock,
        parent_update,
        auth_token,
    })
}

fn decode_plain_hex(s: &str) -> Result<Vec<u8>, String> {
    let stripped = s.trim().strip_prefix("0x").unwrap_or(s.trim());
    quai_primitives::decode_hex(&format!("0x{stripped}")).map_err(|e| format!("hex decode: {e}"))
}

async fn read_bounded_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max_len: usize,
) -> std::io::Result<usize> {
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(total);
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            let take = pos + 1;
            if total + take > max_len {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "stratum message exceeds limit",
                ));
            }
            buf.extend_from_slice(&available[..take]);
            reader.consume(take);
            return Ok(total + take);
        }
        let len = available.len();
        if total + len > max_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stratum message exceeds limit",
            ));
        }
        buf.extend_from_slice(available);
        reader.consume(len);
        total += len;
    }
}

/// Parse optional HAProxy PROXY protocol v1 (text) or v2 (binary) header from
/// the start of a newly accepted TCP connection.
async fn parse_proxy_header<R: tokio::io::AsyncBufRead + AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Option<String> {
    const V2_SIG: &[u8; 12] = b"\r\n\r\n\x00\r\nQUIT\n";
    let peek = tokio::time::timeout(Duration::from_secs(2), reader.fill_buf())
        .await
        .ok()?
        .ok()?;
    if peek.starts_with(b"PROXY ") {
        let mut line = Vec::new();
        read_bounded_line(reader, &mut line, 128).await.ok()?;
        let s = std::str::from_utf8(&line).ok()?;
        let parts: Vec<&str> = s.split_whitespace().collect();
        if parts.len() >= 6 {
            return Some(format!("{}:{}", parts[2], parts[4]));
        }
        return None;
    }
    if peek.len() >= 16 && &peek[..12] == V2_SIG {
        let mut hdr = [0u8; 16];
        reader.read_exact(&mut hdr).await.ok()?;
        let fam = hdr[13];
        let len = u16::from_be_bytes([hdr[14], hdr[15]]) as usize;
        if len > 512 {
            return None;
        }
        let mut payload = vec![0u8; len];
        reader.read_exact(&mut payload).await.ok()?;
        if (fam == 0x11 || fam == 0x12) && payload.len() >= 12 {
            let ip = std::net::Ipv4Addr::new(payload[0], payload[1], payload[2], payload[3]);
            let port = u16::from_be_bytes([payload[8], payload[9]]);
            return Some(format!("{ip}:{port}"));
        }
        if (fam == 0x21 || fam == 0x22) && payload.len() >= 36 {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&payload[..16]);
            let ip = std::net::Ipv6Addr::from(octets);
            let port = u16::from_be_bytes([payload[32], payload[33]]);
            return Some(format!("[{ip}]:{port}"));
        }
    }
    None
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[test]
    fn constant_time_eq_checks_value_and_length() {
        assert!(constant_time_eq(b"secret-token", b"secret-token"));
        assert!(!constant_time_eq(b"secret-token", b"secret-tokeN"));
        assert!(!constant_time_eq(b"secret", b"secret-token"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn parse_auth_identity_extracts_auth_token_modes() {
        let loc = [0u8, 0u8];
        let user = "0x0000000000000000000000000000000000000001.rig1";

        let kv = parse_auth_identity(user, "d=25,auth=my-secret-123,lock=1", &loc).unwrap();
        assert_eq!(kv.worker_name, "rig1");
        assert_eq!(kv.difficulty, Some(25.0));
        assert_eq!(kv.lock, Some(1));
        assert_eq!(kv.auth_token.as_deref(), Some("my-secret-123"));

        let tok = parse_auth_identity(user, "token=alt-secret", &loc).unwrap();
        assert_eq!(tok.auth_token.as_deref(), Some("alt-secret"));

        let bare = parse_auth_identity(user, "bare-secret", &loc).unwrap();
        assert_eq!(bare.auth_token.as_deref(), Some("bare-secret"));

        let default_x = parse_auth_identity(user, "x", &loc).unwrap();
        assert_eq!(default_x.auth_token, None);
    }
}
