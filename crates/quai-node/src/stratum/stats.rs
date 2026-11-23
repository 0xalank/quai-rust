//! Pool, algorithm, miner, worker, share and block statistics for the Stratum
//! server and HTTP/WebSocket dashboard API (`go-quai/stratum/stats.go`).
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::RwLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const MAX_RECENT_SHARES: usize = 500;
const MAX_RECENT_BLOCKS: usize = 100;
const MAX_OFFLINE_WORKERS: usize = 4096;
const HASHRATE_WINDOW_SECS: f64 = 300.0;

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlgoStats {
    pub algorithm: String,
    pub hashrate: f64,
    pub workers: usize,
    pub valid_shares: u64,
    pub invalid_shares: u64,
    pub stale_shares: u64,
    pub blocks_found: u64,
    pub workshares_found: u64,
    pub difficulty: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerStats {
    pub address: String,
    pub worker_name: String,
    pub algorithm: String,
    pub connected: bool,
    pub connected_at: u64,
    pub last_share_at: u64,
    pub difficulty: f64,
    pub valid_shares: u64,
    pub invalid_shares: u64,
    pub stale_shares: u64,
    pub blocks_found: u64,
    pub workshares_found: u64,
    pub hashrate: f64,
    pub remote_addr: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MinerStats {
    pub address: String,
    pub total_hashrate: f64,
    pub active_workers: usize,
    pub valid_shares: u64,
    pub invalid_shares: u64,
    pub stale_shares: u64,
    pub blocks_found: u64,
    pub workshares_found: u64,
    pub workers: Vec<WorkerStats>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockFound {
    pub hash: String,
    pub height: u64,
    pub algorithm: String,
    pub miner: String,
    pub worker: String,
    pub difficulty: f64,
    pub is_block: bool,
    pub timestamp: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareRecord {
    pub miner: String,
    pub worker: String,
    pub algorithm: String,
    pub difficulty: f64,
    pub valid: bool,
    pub stale: bool,
    pub is_workshare: bool,
    pub is_block: bool,
    pub timestamp: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolOverview {
    pub pool_name: String,
    pub uptime: u64,
    pub total_hashrate: f64,
    pub active_workers: usize,
    pub total_miners: usize,
    pub total_shares: u64,
    pub valid_shares: u64,
    pub invalid_shares: u64,
    pub stale_shares: u64,
    pub blocks_found: u64,
    pub workshares_found: u64,
    pub algorithms: BTreeMap<String, AlgoStats>,
}

struct WorkerInternal {
    stats: WorkerStats,
    /// `(timestamp, work_hashes)` samples for windowed hashrate estimation.
    samples: VecDeque<(Instant, f64)>,
}

struct Inner {
    pool_name: String,
    started: Instant,
    algos: BTreeMap<String, AlgoStats>,
    workers: HashMap<String, WorkerInternal>,
    blocks: VecDeque<BlockFound>,
    shares: VecDeque<ShareRecord>,
    total_valid: u64,
    total_invalid: u64,
    total_stale: u64,
    total_blocks: u64,
    total_workshares: u64,
}

/// Thread-safe Stratum pool statistics registry.
pub struct PoolStats {
    inner: RwLock<Inner>,
}

fn hashes_per_diff(algo: &str) -> f64 {
    match algo {
        "sha256" => 4_294_967_296.0, // 2^32
        "scrypt" => 65_536.0,        // 2^16
        "kawpow" => 16_843_009.0,    // ~2^32 / 255
        _ => 4_294_967_296.0,
    }
}

fn worker_key(address: &str, worker_name: &str, algo: &str) -> String {
    format!("{}:{}:{}", address.to_ascii_lowercase(), worker_name, algo)
}

impl PoolStats {
    pub fn new(pool_name: String) -> Self {
        let mut algos = BTreeMap::new();
        for name in ["sha256", "scrypt", "kawpow"] {
            algos.insert(
                name.to_string(),
                AlgoStats {
                    algorithm: name.to_string(),
                    hashrate: 0.0,
                    workers: 0,
                    valid_shares: 0,
                    invalid_shares: 0,
                    stale_shares: 0,
                    blocks_found: 0,
                    workshares_found: 0,
                    difficulty: 0.0,
                },
            );
        }
        Self {
            inner: RwLock::new(Inner {
                pool_name,
                started: Instant::now(),
                algos,
                workers: HashMap::new(),
                blocks: VecDeque::new(),
                shares: VecDeque::new(),
                total_valid: 0,
                total_invalid: 0,
                total_stale: 0,
                total_blocks: 0,
                total_workshares: 0,
            }),
        }
    }

    pub fn pool_name(&self) -> String {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .pool_name
            .clone()
    }

    pub fn set_algo_difficulty(&self, algo: &str, diff: f64) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(a) = inner.algos.get_mut(algo) {
            a.difficulty = diff;
        }
    }

    pub fn worker_connected(
        &self,
        address: &str,
        worker_name: &str,
        algo: &str,
        difficulty: f64,
        remote_addr: &str,
    ) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let key = worker_key(address, worker_name, algo);
        let now = unix_now();
        let entry = inner.workers.entry(key).or_insert_with(|| WorkerInternal {
            stats: WorkerStats {
                address: address.to_string(),
                worker_name: worker_name.to_string(),
                algorithm: algo.to_string(),
                connected: true,
                connected_at: now,
                last_share_at: 0,
                difficulty,
                valid_shares: 0,
                invalid_shares: 0,
                stale_shares: 0,
                blocks_found: 0,
                workshares_found: 0,
                hashrate: 0.0,
                remote_addr: remote_addr.to_string(),
            },
            samples: VecDeque::new(),
        });
        entry.stats.connected = true;
        entry.stats.connected_at = now;
        entry.stats.difficulty = difficulty;
        entry.stats.remote_addr = remote_addr.to_string();
    }

    pub fn worker_disconnected(&self, address: &str, worker_name: &str, algo: &str) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let key = worker_key(address, worker_name, algo);
        if let Some(w) = inner.workers.get_mut(&key) {
            w.stats.connected = false;
        }
        // Bound offline worker history to MAX_OFFLINE_WORKERS
        let offline: Vec<(String, u64)> = inner
            .workers
            .iter()
            .filter(|(_, w)| !w.stats.connected)
            .map(|(k, w)| (k.clone(), w.stats.last_share_at.max(w.stats.connected_at)))
            .collect();
        if offline.len() > MAX_OFFLINE_WORKERS {
            let mut sorted = offline;
            sorted.sort_by_key(|(_, ts)| *ts);
            let remove_count = sorted.len() - MAX_OFFLINE_WORKERS;
            for (k, _) in sorted.into_iter().take(remove_count) {
                inner.workers.remove(&k);
            }
        }
    }

    pub fn update_worker_difficulty(
        &self,
        address: &str,
        worker_name: &str,
        algo: &str,
        difficulty: f64,
    ) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let key = worker_key(address, worker_name, algo);
        if let Some(w) = inner.workers.get_mut(&key) {
            w.stats.difficulty = difficulty;
        }
    }

    pub fn record_reported_hashrate(
        &self,
        address: &str,
        worker_name: &str,
        algo: &str,
        hashrate: f64,
    ) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let key = worker_key(address, worker_name, algo);
        if let Some(w) = inner.workers.get_mut(&key) {
            if hashrate.is_finite() && hashrate >= 0.0 {
                w.stats.hashrate = hashrate;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_share(
        &self,
        address: &str,
        worker_name: &str,
        algo: &str,
        difficulty: f64,
        valid: bool,
        stale: bool,
        is_workshare: bool,
        is_block: bool,
        block_hash: Option<(String, u64)>,
    ) {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let now_inst = Instant::now();
        let now_ts = unix_now();
        if valid {
            inner.total_valid += 1;
        } else if stale {
            inner.total_stale += 1;
        } else {
            inner.total_invalid += 1;
        }
        if is_workshare {
            inner.total_workshares += 1;
        }
        if is_block {
            inner.total_blocks += 1;
        }
        if let Some(a) = inner.algos.get_mut(algo) {
            if valid {
                a.valid_shares += 1;
            } else if stale {
                a.stale_shares += 1;
            } else {
                a.invalid_shares += 1;
            }
            if is_workshare {
                a.workshares_found += 1;
            }
            if is_block {
                a.blocks_found += 1;
            }
        }
        let key = worker_key(address, worker_name, algo);
        if let Some(w) = inner.workers.get_mut(&key) {
            w.stats.difficulty = difficulty;
            if valid {
                w.stats.valid_shares += 1;
                w.stats.last_share_at = now_ts;
                w.samples
                    .push_back((now_inst, difficulty * hashes_per_diff(algo)));
                while let Some((t, _)) = w.samples.front() {
                    if now_inst.saturating_duration_since(*t).as_secs_f64() > HASHRATE_WINDOW_SECS {
                        w.samples.pop_front();
                    } else {
                        break;
                    }
                }
                w.stats.hashrate = compute_worker_hashrate(&w.samples, now_inst);
            } else if stale {
                w.stats.stale_shares += 1;
            } else {
                w.stats.invalid_shares += 1;
            }
            if is_workshare {
                w.stats.workshares_found += 1;
            }
            if is_block {
                w.stats.blocks_found += 1;
            }
        }
        inner.shares.push_front(ShareRecord {
            miner: address.to_string(),
            worker: worker_name.to_string(),
            algorithm: algo.to_string(),
            difficulty,
            valid,
            stale,
            is_workshare,
            is_block,
            timestamp: now_ts,
        });
        while inner.shares.len() > MAX_RECENT_SHARES {
            inner.shares.pop_back();
        }
        if let Some((hash, height)) = block_hash {
            if is_workshare || is_block {
                inner.blocks.push_front(BlockFound {
                    hash,
                    height,
                    algorithm: algo.to_string(),
                    miner: address.to_string(),
                    worker: worker_name.to_string(),
                    difficulty,
                    is_block,
                    timestamp: now_ts,
                });
                while inner.blocks.len() > MAX_RECENT_BLOCKS {
                    inner.blocks.pop_back();
                }
            }
        }
    }

    pub fn overview(&self) -> PoolOverview {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let now_inst = Instant::now();
        let mut algos = inner.algos.clone();
        for a in algos.values_mut() {
            a.workers = 0;
            a.hashrate = 0.0;
        }
        let mut active_workers = 0usize;
        let mut miners = std::collections::HashSet::new();
        let mut total_hashrate = 0.0f64;
        for w in inner.workers.values() {
            miners.insert(w.stats.address.to_ascii_lowercase());
            let hr = if w.samples.is_empty() {
                w.stats.hashrate
            } else {
                compute_worker_hashrate(&w.samples, now_inst)
            };
            if w.stats.connected {
                active_workers += 1;
                total_hashrate += hr;
                if let Some(a) = algos.get_mut(&w.stats.algorithm) {
                    a.workers += 1;
                    a.hashrate += hr;
                }
            }
        }
        PoolOverview {
            pool_name: inner.pool_name.clone(),
            uptime: inner.started.elapsed().as_secs(),
            total_hashrate,
            active_workers,
            total_miners: miners.len(),
            total_shares: inner.total_valid + inner.total_invalid + inner.total_stale,
            valid_shares: inner.total_valid,
            invalid_shares: inner.total_invalid,
            stale_shares: inner.total_stale,
            blocks_found: inner.total_blocks,
            workshares_found: inner.total_workshares,
            algorithms: algos,
        }
    }

    pub fn workers(&self) -> Vec<WorkerStats> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let now_inst = Instant::now();
        let mut out: Vec<WorkerStats> = inner
            .workers
            .values()
            .map(|w| {
                let mut s = w.stats.clone();
                if !w.samples.is_empty() {
                    s.hashrate = compute_worker_hashrate(&w.samples, now_inst);
                }
                s
            })
            .collect();
        out.sort_by(|a, b| {
            b.connected
                .cmp(&a.connected)
                .then_with(|| a.address.cmp(&b.address))
                .then_with(|| a.worker_name.cmp(&b.worker_name))
        });
        out
    }

    pub fn miner_stats(&self, address: &str) -> MinerStats {
        let want = address.to_ascii_lowercase();
        let all_workers: Vec<WorkerStats> = self
            .workers()
            .into_iter()
            .filter(|w| w.address.to_ascii_lowercase() == want)
            .collect();
        let mut total_hashrate = 0.0;
        let mut active_workers = 0;
        let mut valid_shares = 0;
        let mut invalid_shares = 0;
        let mut stale_shares = 0;
        let mut blocks_found = 0;
        let mut workshares_found = 0;
        for w in &all_workers {
            if w.connected {
                active_workers += 1;
                total_hashrate += w.hashrate;
            }
            valid_shares += w.valid_shares;
            invalid_shares += w.invalid_shares;
            stale_shares += w.stale_shares;
            blocks_found += w.blocks_found;
            workshares_found += w.workshares_found;
        }
        MinerStats {
            address: address.to_string(),
            total_hashrate,
            active_workers,
            valid_shares,
            invalid_shares,
            stale_shares,
            blocks_found,
            workshares_found,
            workers: all_workers,
        }
    }

    pub fn miners(&self) -> Vec<MinerStats> {
        let workers = self.workers();
        let mut by_addr: BTreeMap<String, Vec<WorkerStats>> = BTreeMap::new();
        for w in workers {
            by_addr
                .entry(w.address.to_ascii_lowercase())
                .or_default()
                .push(w);
        }
        by_addr
            .into_values()
            .map(|list| {
                let addr = list.first().map(|w| w.address.clone()).unwrap_or_default();
                let mut m = MinerStats {
                    address: addr,
                    total_hashrate: 0.0,
                    active_workers: 0,
                    valid_shares: 0,
                    invalid_shares: 0,
                    stale_shares: 0,
                    blocks_found: 0,
                    workshares_found: 0,
                    workers: list,
                };
                for w in &m.workers {
                    if w.connected {
                        m.active_workers += 1;
                        m.total_hashrate += w.hashrate;
                    }
                    m.valid_shares += w.valid_shares;
                    m.invalid_shares += w.invalid_shares;
                    m.stale_shares += w.stale_shares;
                    m.blocks_found += w.blocks_found;
                    m.workshares_found += w.workshares_found;
                }
                m
            })
            .collect()
    }

    pub fn recent_blocks(&self) -> Vec<BlockFound> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .blocks
            .iter()
            .cloned()
            .collect()
    }

    pub fn recent_shares(&self) -> Vec<ShareRecord> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .shares
            .iter()
            .cloned()
            .collect()
    }
}

fn compute_worker_hashrate(samples: &VecDeque<(Instant, f64)>, now: Instant) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let total_work: f64 = samples
        .iter()
        .filter(|(t, _)| now.saturating_duration_since(*t).as_secs_f64() <= HASHRATE_WINDOW_SECS)
        .map(|(_, w)| *w)
        .sum();
    let oldest = samples
        .front()
        .map(|(t, _)| now.saturating_duration_since(*t).as_secs_f64())
        .unwrap_or(1.0);
    let span = oldest.clamp(1.0, HASHRATE_WINDOW_SECS);
    total_work / span
}
