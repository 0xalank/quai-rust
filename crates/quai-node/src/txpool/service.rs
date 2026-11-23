//! The running pool: go-quai's `SubmitTransaction` (the fee cap, then
//! `SendTxToSharingClients`), the remote queue that gossiped workshare
//! transactions enter (`core.Core.AddRemotes`, drained every 2 seconds),
//! and the pool's loops: a run every second (`ReorgFrequency`), a reset on
//! every new head, eviction every minute, Qi expiry every 10 minutes and
//! the pool limiter every 30 seconds.
use super::journal::{self, Journal};
use super::list::Lru;
use super::sharing;
use super::tx::{Body, PoolTx};
use super::{big, Config, Pool, PoolChain, Tx};
use crate::log::Log;
use quai_primitives::Hash;
use quai_rpc::view::BlockData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// `c_maxRemoteTxQueue`.
const MAX_REMOTE_QUEUE: usize = 50_000;
/// `c_remoteTxProcPeriod`.
const REMOTE_PERIOD: Duration = Duration::from_secs(2);
const EVICTION_INTERVAL: Duration = Duration::from_secs(60);
const QI_EXPIRATION_INTERVAL: Duration = Duration::from_secs(600);
const LIMITER_INTERVAL: Duration = Duration::from_secs(30);

pub type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

pub struct Service {
    pool: Mutex<Pool>,
    chain: Arc<dyn PoolChain>,
    clock: Clock,
    clients: Vec<Arc<sharing::Client>>,
    sync_tx_with_return: bool,
    v0570_compat: bool,
    fee_cap: f64,
    chain_id: u64,
    remote: Mutex<Lru<Hash, Tx>>,
    runtime: Option<tokio::runtime::Handle>,
    log: Option<Log>,
    journal: Option<Journal>,
}

/// `checkTxFee`: gas price times gas above the cap (in QUAI) is refused.
pub fn check_tx_fee(tx: &PoolTx, cap: f64) -> Result<(), String> {
    if cap == 0.0 {
        return Ok(());
    }
    let fee = big::mul_u64(tx.gas_price(), tx.gas());
    let quai = big::fee_in_ether(&fee);
    if quai > cap {
        return Err(format!(
            "tx fee ({} ether) exceeds the configured cap ({} ether)",
            big::format_2f(quai),
            big::format_2f(cap)
        ));
    }
    Ok(())
}

/// A monotonic nanosecond clock.
pub fn monotonic() -> Clock {
    let start = Instant::now();
    // Start well above zero so that "older than Lifetime" never underflows.
    Box::new(move || 1_000_000 * 1_000_000_000 + start.elapsed().as_nanos() as u64)
}

impl Service {
    pub fn new(
        config: Config,
        chain: Arc<dyn PoolChain>,
        clock: Clock,
        runtime: Option<tokio::runtime::Handle>,
        log: Option<Log>,
    ) -> Result<Self, String> {
        let clients = config
            .sharing_clients
            .iter()
            .map(|u| sharing::Client::new(u).map(Arc::new))
            .collect::<Result<Vec<_>, _>>()?;
        let now = clock();
        let mut pool = Pool::new(config.clone(), chain.clone(), now);
        // NewTxPool: load the journal of local transactions (without
        // journaling them again), then rewrite it with what the pool kept.
        let journal = config.journal.as_deref().map(Journal::new);
        if let Some(j) = &journal {
            let records = j.read();
            let local = !config.no_locals;
            let mut loaded = 0;
            for batch in records.chunks(journal::LOAD_BATCH) {
                let txs: Vec<Tx> = batch
                    .iter()
                    .filter_map(|r| PoolTx::decode(r, now).ok().map(Arc::new))
                    .collect();
                loaded += pool
                    .add_txs(txs, local, now)
                    .iter()
                    .filter(|r| r.is_ok())
                    .count();
            }
            if let Err(e) = j.rotate(&pool.local()) {
                if let Some(l) = &log {
                    l.warn(&format!("txpool: journal {}: {e}", j.path().display()));
                }
            }
            if let Some(l) = &log {
                l.info(&format!(
                    "txpool: journal {}: {} transactions, {loaded} added",
                    j.path().display(),
                    records.len()
                ));
            }
            pool.journaling = true;
        }
        Ok(Self {
            sync_tx_with_return: config.sync_tx_with_return,
            v0570_compat: config.v0570_compat,
            fee_cap: config.rpc_tx_fee_cap,
            chain_id: chain.chain_id(),
            pool: Mutex::new(pool),
            chain,
            clock,
            clients,
            remote: Mutex::new(Lru::new(MAX_REMOTE_QUEUE)),
            runtime,
            log,
            journal,
        })
    }
    /// Append what the pool journaled since the last call.
    fn flush_journal(&self, pool: &mut Pool) {
        let txs = pool.take_journal();
        if let (Some(j), false) = (&self.journal, txs.is_empty()) {
            if let Err(e) = j.insert(&txs) {
                self.warn(&format!("txpool: journal {}: {e}", j.path().display()));
            }
        }
    }
    /// `journal.rotate(pool.local())`.
    pub fn rotate_journal(&self) {
        if let Some(j) = &self.journal {
            let local = self.pool().local();
            if let Err(e) = j.rotate(&local) {
                self.warn(&format!("txpool: journal {}: {e}", j.path().display()));
            }
        }
    }
    pub fn now(&self) -> u64 {
        (self.clock)()
    }
    pub fn pool(&self) -> MutexGuard<'_, Pool> {
        self.pool.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn chain(&self) -> &Arc<dyn PoolChain> {
        &self.chain
    }

    /// `SubmitTransaction` after `SendRawTransaction`'s decoding: the hash,
    /// or go-quai's error text.
    pub fn submit(&self, raw: &[u8]) -> Result<Hash, String> {
        let now = self.now();
        let tx = PoolTx::decode(raw, now)?;
        if !matches!(tx.body, Body::Qi(_)) {
            check_tx_fee(&tx, self.fee_cap)?;
        }
        self.send_to_sharing_clients(Arc::new(tx.clone()), now)?;
        if let Body::Quai(_) = &tx.body {
            if tx.pool_sender(self.chain_id).is_err() {
                return Err(tx.sender_error(self.chain_id));
            }
        }
        Ok(tx.hash)
    }

    /// `TxPool.SendTxToSharingClients`.
    fn send_to_sharing_clients(&self, tx: Tx, now: u64) -> Result<(), String> {
        if !self.sync_tx_with_return || self.clients.is_empty() {
            {
                let mut pool = self.pool();
                let added = pool.add_local(tx.clone(), now);
                self.flush_journal(&mut pool);
                added?;
            }
            self.forward(&self.clients, &tx);
            return Ok(());
        }
        if !self.v0570_compat {
            self.pool().validate_for_forwarding(&tx)?;
        }
        let first = self.clients[0].clone();
        let proto = tx.proto.clone();
        let result = match &self.runtime {
            Some(rt) => rt.block_on(async move { first.send(&proto).await }),
            None => Err("no runtime for the sharing clients".into()),
        };
        if let Err(e) = &result {
            self.warn(&format!(
                "txpool: sharing client {}: {e}",
                self.clients[0].url
            ));
        }
        self.forward(&self.clients[1..], &tx);
        result
    }

    /// Send to each client in the background; errors are only logged.
    fn forward(&self, clients: &[Arc<sharing::Client>], tx: &Tx) {
        let Some(rt) = &self.runtime else {
            return;
        };
        for c in clients {
            let c = c.clone();
            let proto = tx.proto.clone();
            let log = self.log;
            rt.spawn(async move {
                if let Err(e) = c.send(&proto).await {
                    if let Some(l) = log {
                        l.warn(&format!("txpool: sharing client {}: {e}", c.url));
                    }
                }
            });
        }
    }
    fn warn(&self, m: &str) {
        if let Some(l) = &self.log {
            l.warn(m);
        }
    }

    /// `core.Core.AddRemotes`: workshare transactions from gossip wait in
    /// a 50,000-entry LRU by hash until the next drain.
    pub fn add_remotes(&self, txs: Vec<Tx>) {
        let mut q = self.remote.lock().unwrap_or_else(|e| e.into_inner());
        for tx in txs {
            q.add(tx.hash, tx);
        }
    }
    /// `startRemoteTxQueue`'s tick: everything queued, oldest first, as one
    /// `AddRemotes` batch.
    pub fn drain_remotes(&self) -> Vec<Result<(), String>> {
        let txs: Vec<Tx> = {
            let mut q = self.remote.lock().unwrap_or_else(|e| e.into_inner());
            let keys = q.keys();
            keys.iter().filter_map(|k| q.remove(k)).collect()
        };
        let now = self.now();
        let mut pool = self.pool();
        let results = pool.add_remotes(txs, now);
        self.flush_journal(&mut pool);
        results
    }

    /// One scheduled run.
    pub fn run(&self) {
        let now = self.now();
        let mut pool = self.pool();
        pool.run_reorg(None, now);
        self.flush_journal(&mut pool);
    }
    /// The reset of a new head.
    pub fn reset(&self, old: Option<Arc<BlockData>>, new: Arc<BlockData>) {
        let now = self.now();
        let mut pool = self.pool();
        pool.run_reorg(Some((old, new)), now);
        self.flush_journal(&mut pool);
    }

    /// Start the loops on `runtime`. `heads` are new canonical head
    /// hashes; the pool resets to the chain's current block on each.
    pub fn start(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        heads: Option<tokio::sync::broadcast::Receiver<Hash>>,
        stop: Arc<AtomicBool>,
        reorg_frequency: Duration,
    ) {
        let every = |period: Duration, f: Box<dyn Fn(&Service) + Send + Sync>| {
            let svc = self.clone();
            let stop = stop.clone();
            runtime.spawn(async move {
                let mut tick = tokio::time::interval(period);
                tick.tick().await;
                while !stop.load(Ordering::Relaxed) {
                    tick.tick().await;
                    let s = svc.clone();
                    let f = &f;
                    // Pool work holds a mutex and reads state: keep it off
                    // the async workers.
                    tokio::task::block_in_place(|| f(&s));
                }
            });
        };
        every(reorg_frequency, Box::new(|s| s.run()));
        every(
            REMOTE_PERIOD,
            Box::new(|s| {
                s.drain_remotes();
            }),
        );
        every(
            EVICTION_INTERVAL,
            Box::new(|s| {
                let now = s.now();
                s.pool().evict(now);
            }),
        );
        every(
            QI_EXPIRATION_INTERVAL,
            Box::new(|s| {
                let now = s.now();
                s.pool().expire_qi(now);
            }),
        );
        every(
            LIMITER_INTERVAL,
            Box::new(|s| {
                let now = s.now();
                s.pool().limit(now);
            }),
        );
        every(journal::REJOURNAL, Box::new(|s| s.rotate_journal()));
        if let Some(mut heads) = heads {
            let svc = self.clone();
            runtime.spawn(async move {
                let mut head = svc.chain.current_block();
                while !stop.load(Ordering::Relaxed) {
                    let event = match heads.recv().await {
                        Ok(hash) => Some(hash),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
                        Err(_) => break,
                    };
                    // The event's block and its ancestors are remembered for
                    // a later reorg walk. The reset goes to the current head:
                    // go-quai also merges the heads that arrive before its
                    // next reorg run into one reset, and the view has state
                    // at the head only.
                    let s = svc.clone();
                    let new = tokio::task::block_in_place(|| {
                        if let Some(hash) = event {
                            let _ = s.chain.head_block(&hash);
                        }
                        s.chain.current_block()
                    });
                    if new.hash == head.hash {
                        continue;
                    }
                    let old = head.clone();
                    let n = new.clone();
                    tokio::task::block_in_place(|| s.reset(Some(old), n));
                    head = new;
                }
            });
        }
    }
}

/// The answers of the RPC pool hooks (`ChainView::pool_*`).
impl Service {
    /// The head of the last completed reset, completed resets, and resets
    /// that stopped early (`Pool::reset_state`).
    pub fn reset_state(&self) -> (Option<Hash>, u64, u64) {
        self.pool().reset_state()
    }
    pub fn rpc_nonce(&self, a: &[u8; 20]) -> u64 {
        self.pool().nonce(a)
    }
    pub fn rpc_transaction(&self, hash: &Hash) -> Option<quai_rpc::view::PoolTransaction> {
        let tx = self.pool().get(hash)?;
        Some(quai_rpc::view::PoolTransaction {
            hash: tx.hash,
            tx: tx.primitive()?,
        })
    }
    /// `Content`, or `ContentFrom` (empty outside the zone's Quai ledger).
    pub fn rpc_content(&self, from: Option<&[u8; 20]>) -> quai_rpc::view::PoolContent {
        let convert = |txs: Vec<Tx>| {
            txs.iter()
                .filter_map(|t| {
                    Some(quai_rpc::view::PoolTransaction {
                        hash: t.hash,
                        tx: t.primitive()?,
                    })
                })
                .collect::<Vec<_>>()
        };
        let pool = self.pool();
        match from {
            None => {
                let (pending, queued) = pool.content();
                quai_rpc::view::PoolContent {
                    pending: pending.into_iter().map(|(a, t)| (a, convert(t))).collect(),
                    queued: queued.into_iter().map(|(a, t)| (a, convert(t))).collect(),
                }
            }
            Some(a) => {
                if super::internal_and_quai(a, self.chain.location()).is_err() {
                    return quai_rpc::view::PoolContent::default();
                }
                let (pending, queued) = pool.content_from(a);
                quai_rpc::view::PoolContent {
                    pending: vec![(*a, convert(pending))],
                    queued: vec![(*a, convert(queued))],
                }
            }
        }
    }
    pub fn rpc_stats(&self) -> (u64, u64, u64) {
        let (p, q, qi) = self.pool().stats();
        (p as u64, q as u64, qi as u64)
    }
}
