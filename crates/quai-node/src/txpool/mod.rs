//! The transaction pool of go-quai v0.57.0 (`core/tx_pool.go`,
//! `core/tx_list.go`), ported step for step so that it makes the same
//! decisions for the same head state and the same transactions: accept or
//! reject with Go's error text, pending and queued lists per account,
//! replacement by a 5% price bump, per-account and global limits, eviction
//! of the cheapest remotes when full, lifetime eviction, the Qi pool (an
//! LRU without double-spend checks, as in Go) and the reset on a new head
//! with reinjection after a reorg.
//!
//! The pool is a state machine driven by its caller (`service.rs` in the
//! node, the Go oracle vectors in tests): `add_txs` validates and queues,
//! `run_reorg` is one run of Go's `runReorg` (the scheduler starts one
//! every second and on every new head), `evict`, `expire_qi` and `limit`
//! are one pass of Go's eviction, Qi expiry and pool limiter loops. Time is
//! a nanosecond clock that the caller passes in.
//!
//! Where Go iterates a map, the outcome can depend on Go's random order
//! only when two accounts tie (equal pending counts in `truncatePending`,
//! equal heartbeats in `truncateQueue`, equal gas prices in the price heap
//! after a re-heap, the account order of the pool limiter's last step).
//! Rust breaks those ties by address or by insertion order.
pub mod big;
pub mod journal;
pub mod list;
pub mod qi;
pub mod service;
pub mod sharing;
pub mod tx;
pub mod wire;

use list::{num_slots, Lookup, Lru, PricedList, Tx, TxList};
use quai_primitives::Hash;
use quai_rpc::view::{AccountState, BlockData, UtxoData};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tx::{Address, Body, PoolTx};

/// `txMaxSize`: 128 KB.
pub const TX_MAX_SIZE: u64 = 4 * list::TX_SLOT_SIZE;
/// `params.GenesisGasLimit`.
const GENESIS_GAS_LIMIT: u64 = 12_000_000;
/// The nonce cache of `txNoncer`.
const MAX_NONCE_CACHE: usize = 50_000;

pub const ERR_ALREADY_KNOWN: &str = "already known";
pub const ERR_INVALID_SENDER: &str = "invalid sender";
pub const ERR_UNDERPRICED: &str = "transaction underpriced";
pub const ERR_POOL_FULL: &str = "txpool is full";
pub const ERR_REPLACE_UNDERPRICED: &str = "replacement transaction underpriced";

pub fn hex0x(b: &[u8]) -> String {
    quai_primitives::encode_hex(b)
}
/// `common.Address.Hex()`: the mixed-case checksum.
pub fn checksum(a: &[u8; 20]) -> String {
    quai_rpc::native::json::checksum(a)
}

/// `TxPoolConfig` with the node's defaults (`core.DefaultTxPoolConfig`,
/// and `--txpool.sync-tx-with-return`, which defaults to true).
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub locals: Vec<Address>,
    pub no_locals: bool,
    pub sync_tx_with_return: bool,
    pub price_limit: u64,
    pub price_bump: u64,
    pub account_slots: u64,
    pub global_slots: u64,
    pub max_fees_cached: usize,
    pub account_queue: u64,
    pub global_queue: u64,
    pub qi_pool_size: usize,
    /// Nanoseconds.
    pub qi_tx_lifetime: u64,
    /// Nanoseconds.
    pub lifetime: u64,
    /// Nanoseconds between runs.
    pub reorg_frequency: u64,
    pub sharing_clients: Vec<String>,
    /// `--rpc.txfeecap` in QUAI (0: no cap).
    pub rpc_tx_fee_cap: f64,
    /// The journal of local transactions (go-quai: `transactions.rlp` in
    /// the data directory). None: no journal.
    pub journal: Option<std::path::PathBuf>,
}
const SECOND: u64 = 1_000_000_000;
impl Default for Config {
    fn default() -> Self {
        Self {
            locals: vec![],
            no_locals: false,
            sync_tx_with_return: true,
            price_limit: 0,
            price_bump: 5,
            account_slots: 200,
            global_slots: 19000 + 1024,
            max_fees_cached: 50_000,
            account_queue: 200,
            global_queue: 20048,
            qi_pool_size: 10024,
            qi_tx_lifetime: 30 * 60 * SECOND,
            lifetime: 5 * 60 * SECOND,
            reorg_frequency: SECOND,
            sharing_clients: vec![],
            rpc_tx_fee_cap: 10000.0,
            journal: None,
        }
    }
}
impl Config {
    /// `TxPoolConfig.sanitize`.
    pub fn sanitize(mut self) -> Self {
        let d = Self::default();
        if self.price_limit < 1 {
            self.price_limit = d.price_limit;
        }
        if self.price_bump < 1 {
            self.price_bump = d.price_bump;
        }
        if self.account_slots < 1 {
            self.account_slots = d.account_slots;
        }
        if self.global_slots < 1 {
            self.global_slots = d.global_slots;
        }
        if self.account_queue < 1 {
            self.account_queue = d.account_queue;
        }
        if self.global_queue < 1 {
            self.global_queue = d.global_queue;
        }
        if self.qi_pool_size < 1 {
            self.qi_pool_size = d.qi_pool_size;
        }
        if self.qi_tx_lifetime < SECOND {
            self.qi_tx_lifetime = d.qi_tx_lifetime;
        }
        if self.lifetime < 1 {
            self.lifetime = d.lifetime;
        }
        if self.reorg_frequency < 1 {
            self.reorg_frequency = d.reorg_frequency;
        }
        self
    }
}

/// What the pool reads from the chain (go-quai's `blockChain` and the
/// database it reads UTXOs from).
pub trait PoolChain: Send + Sync {
    fn chain_id(&self) -> u64;
    fn location(&self) -> [u8; 2];
    /// `CurrentBlock()`: the chain's head, which can be ahead of the pool's
    /// last reset.
    fn current_block(&self) -> Arc<BlockData>;
    /// `GetBlock(hash, number)`: canonical or side-chain blocks.
    fn get_block(&self, hash: &Hash, number: u64) -> Option<Arc<BlockData>>;
    /// `GetHeaderByHash` (the prime terminus).
    fn header_by_hash(&self, hash: &Hash) -> Option<Arc<BlockData>>;
    /// `StateAt` the block's roots (the empty state for the genesis).
    fn state_at(&self, block: &BlockData) -> Result<Arc<dyn AccountState>, String>;
    /// `rawdb.GetUTXO` from the current UTXO set.
    fn utxo(&self, hash: &Hash, index: u16) -> Option<UtxoData>;
    /// `rawdb.ReadUTXOSetSize(block)`.
    fn utxo_set_size(&self, block: &Hash) -> u64;
    /// The block of a head event, which the chain may remember for a later
    /// reorg walk (go-quai reads side chains from its database).
    fn head_block(&self, hash: &Hash) -> Option<Arc<BlockData>> {
        self.header_by_hash(hash)
    }
}

/// Header fields of a work object as go-quai's accessors return them.
pub mod block {
    use super::{big, PoolTx, Tx};
    use quai_primitives::Hash;
    use quai_rpc::view::BlockData;
    use std::sync::Arc;
    fn h32(v: Option<&[u8]>) -> Hash {
        let mut out = [0u8; 32];
        if let Some(b) = v {
            let n = b.len().min(32);
            out[32 - n..].copy_from_slice(&b[b.len() - n..]);
        }
        out
    }
    /// `ParentHash(ZONE_CTX)`.
    pub fn parent(b: &BlockData) -> Hash {
        h32(b
            .header
            .wire()
            .parent_hash
            .as_ref()
            .map(|h| h.value.as_slice()))
    }
    pub fn base_fee(b: &BlockData) -> Vec<u8> {
        big::trim(
            b.object
                .header()
                .wire()
                .base_fee
                .as_deref()
                .unwrap_or_default(),
        )
    }
    pub fn gas_limit(b: &BlockData) -> u64 {
        b.object.header().wire().gas_limit.unwrap_or(0)
    }
    pub fn prime_terminus_number(b: &BlockData) -> u64 {
        big::low_u64(
            b.header
                .wire()
                .prime_terminus_number
                .as_deref()
                .unwrap_or_default(),
        )
    }
    pub fn prime_terminus_hash(b: &BlockData) -> Hash {
        h32(b
            .object
            .header()
            .wire()
            .prime_terminus_hash
            .as_ref()
            .map(|h| h.value.as_slice()))
    }
    pub fn expansion_number(b: &BlockData) -> u8 {
        b.object.header().wire().expansion_number.unwrap_or(0) as u8
    }
    pub fn exchange_rate(b: &BlockData) -> Vec<u8> {
        big::trim(
            b.object
                .header()
                .wire()
                .exchange_rate
                .as_deref()
                .unwrap_or_default(),
        )
    }
    pub fn eligible_slices(b: &BlockData) -> Hash {
        h32(b
            .object
            .header()
            .wire()
            .etx_eligible_slices
            .as_ref()
            .map(|h| h.value.as_slice()))
    }
    /// The body transactions as the pool sees them.
    pub fn transactions(b: &BlockData, now: u64) -> Vec<Tx> {
        b.object
            .transactions()
            .iter()
            .filter_map(|t| PoolTx::from_primitive(t, now).ok().map(Arc::new))
            .collect()
    }
}

/// `common.Address.InternalAndQuaiAddress()` for the pool's zone.
pub fn internal_and_quai(a: &Address, location: [u8; 2]) -> Result<Address, String> {
    if a[1] > 127 {
        return Err(format!(
            "expected Quai address, but found Qi address: {}",
            checksum(a)
        ));
    }
    if a[0] != (location[0] << 4) | location[1] {
        return Err("Address belongs to other zone".into());
    }
    Ok(*a)
}

/// A Qi pool entry (`types.TxWithMinerFee`).
#[derive(Clone)]
pub struct QiEntry {
    pub tx: Tx,
    /// The fee in qits.
    pub fee: Vec<u8>,
    /// Nanoseconds of the pool clock.
    pub received: u64,
}

/// `txNoncer`: pending nonces with the reset state as fallback.
struct Noncer {
    fallback: Arc<dyn AccountState>,
    nonces: Lru<Address, u64>,
}
impl Noncer {
    fn new(fallback: Arc<dyn AccountState>) -> Self {
        Self {
            fallback,
            nonces: Lru::new(MAX_NONCE_CACHE),
        }
    }
    fn get(&mut self, a: Address) -> u64 {
        let f = self.fallback.nonce(&a).unwrap_or(0);
        self.nonces.contains_or_add(a, f);
        self.nonces.get(&a).copied().unwrap_or(f)
    }
    fn set(&mut self, a: Address, n: u64) {
        self.nonces.add(a, n);
    }
    fn set_if_lower(&mut self, a: Address, n: u64) {
        let f = self.fallback.nonce(&a).unwrap_or(0);
        self.nonces.contains_or_add(a, f);
        let current = self.nonces.peek(&a).copied().unwrap_or(f);
        if current <= n {
            return;
        }
        self.nonces.add(a, n);
    }
}

/// The empty state before the first reset.
struct NoState;
impl AccountState for NoState {
    fn balance(&self, _: &Address) -> Result<[u8; 32], quai_rpc::view::ViewError> {
        Ok([0; 32])
    }
    fn nonce(&self, _: &Address) -> Result<u64, quai_rpc::view::ViewError> {
        Ok(0)
    }
    fn code(&self, _: &Address) -> Result<Vec<u8>, quai_rpc::view::ViewError> {
        Ok(vec![])
    }
    fn storage(&self, _: &Address, _: &Hash) -> Result<Hash, quai_rpc::view::ViewError> {
        Ok([0; 32])
    }
}

/// The pool (`core.TxPool`).
pub struct Pool {
    pub config: Config,
    chain: Arc<dyn PoolChain>,
    chain_id: u64,
    location: [u8; 2],
    gas_price: Vec<u8>,
    current_state: Arc<dyn AccountState>,
    qi_scaling: f64,
    pending_nonces: Noncer,
    current_max_gas: u64,
    locals: BTreeSet<Address>,
    qi_pool: Lru<Hash, QiEntry>,
    qi_fees: Lru<[u8; 16], Vec<u8>>,
    pending: BTreeMap<Address, TxList>,
    queue: BTreeMap<Address, TxList>,
    beats: BTreeMap<Address, u64>,
    all: Lookup,
    priced: PricedList,
    /// The accounts the adds since the last run reported for promotion.
    dirty: Option<BTreeSet<Address>>,
    /// Local transactions to append to the journal (`journalTx`), when
    /// journaling.
    pub journaling: bool,
    journal_pending: Vec<Tx>,
    /// Resets that reached the new head, and resets that stopped early
    /// (missing old head, unrooted walk, no state), as go-quai's do.
    resets: u64,
    reset_stops: u64,
    reset_head: Option<Hash>,
}

/// The pool's content for snapshots and the RPC.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub pending: BTreeMap<Address, Vec<Hash>>,
    pub queued: BTreeMap<Address, Vec<Hash>>,
    pub qi: Vec<Hash>,
    pub locals: Vec<Address>,
    pub local_count: usize,
    pub remote_count: usize,
    pub slots: usize,
    pub beats: Vec<Address>,
}

impl Pool {
    /// `NewTxPool`: a pool reset to the chain's current block.
    pub fn new(config: Config, chain: Arc<dyn PoolChain>, now: u64) -> Self {
        let config = config.sanitize();
        let state: Arc<dyn AccountState> = Arc::new(NoState);
        let mut pool = Self {
            chain_id: chain.chain_id(),
            location: chain.location(),
            gas_price: big::from_u64(config.price_limit),
            current_state: state.clone(),
            qi_scaling: 0.0,
            pending_nonces: Noncer::new(state),
            current_max_gas: 0,
            locals: config.locals.iter().copied().collect(),
            qi_pool: Lru::new(config.qi_pool_size),
            qi_fees: Lru::new(config.max_fees_cached),
            pending: BTreeMap::new(),
            queue: BTreeMap::new(),
            beats: BTreeMap::new(),
            all: Lookup::default(),
            priced: PricedList::default(),
            dirty: None,
            journaling: false,
            journal_pending: vec![],
            resets: 0,
            reset_stops: 0,
            reset_head: None,
            chain: chain.clone(),
            config,
        };
        let head = chain.current_block();
        pool.reset(None, head, now);
        pool
    }

    fn sender_of(&self, tx: &PoolTx) -> Option<Address> {
        let from = tx.pool_sender(self.chain_id).ok()?;
        internal_and_quai(&from, self.location).ok()
    }
    fn locals_contain(&self, tx: &PoolTx) -> bool {
        self.sender_of(tx).is_some_and(|a| self.locals.contains(&a))
    }
    fn state_nonce(&self, a: &Address) -> u64 {
        self.current_state.nonce(a).unwrap_or(0)
    }
    fn state_balance(&self, a: &Address) -> Vec<u8> {
        big::trim(&self.current_state.balance(a).unwrap_or([0; 32]))
    }

    // --- reads ------------------------------------------------------------

    /// `Nonce`: the next nonce with every pending transaction applied.
    pub fn nonce(&mut self, a: &Address) -> u64 {
        self.pending_nonces.get(*a)
    }
    /// `Stats`: pending, queued and Qi transactions.
    pub fn stats(&self) -> (usize, usize, usize) {
        (
            self.pending.values().map(|l| l.len()).sum(),
            self.queue.values().map(|l| l.len()).sum(),
            self.qi_pool.len(),
        )
    }
    /// `Content`: pending and queued transactions by account, nonce order.
    pub fn content(&self) -> (BTreeMap<Address, Vec<Tx>>, BTreeMap<Address, Vec<Tx>>) {
        (
            self.pending
                .iter()
                .map(|(a, l)| (*a, l.flatten()))
                .collect(),
            self.queue.iter().map(|(a, l)| (*a, l.flatten())).collect(),
        )
    }
    /// `ContentFrom`.
    pub fn content_from(&self, a: &Address) -> (Vec<Tx>, Vec<Tx>) {
        (
            self.pending.get(a).map(|l| l.flatten()).unwrap_or_default(),
            self.queue.get(a).map(|l| l.flatten()).unwrap_or_default(),
        )
    }
    /// `Get`: a Quai transaction, or a Qi one (which Go's LRU `Get`
    /// refreshes).
    pub fn get(&mut self, hash: &Hash) -> Option<Tx> {
        if let Some(t) = self.all.get(hash) {
            return Some(t.clone());
        }
        self.qi_pool.get(hash).map(|e| e.tx.clone())
    }
    /// The head of the last completed reset, completed resets, and resets
    /// that stopped early.
    pub fn reset_state(&self) -> (Option<Hash>, u64, u64) {
        (self.reset_head, self.resets, self.reset_stops)
    }
    /// The Qi pool from the oldest to the newest entry.
    pub fn qi_pending(&self) -> Vec<QiEntry> {
        self.qi_pool.values().into_iter().cloned().collect()
    }
    pub fn snapshot(&self) -> Snapshot {
        let hashes = |l: &TxList| l.flatten().iter().map(|t| t.hash).collect::<Vec<_>>();
        Snapshot {
            pending: self.pending.iter().map(|(a, l)| (*a, hashes(l))).collect(),
            queued: self.queue.iter().map(|(a, l)| (*a, hashes(l))).collect(),
            qi: self.qi_pool.keys(),
            locals: self.locals.iter().copied().collect(),
            local_count: self.all.locals.len(),
            remote_count: self.all.remotes.len(),
            slots: self.all.slots,
            beats: self.beats.keys().copied().collect(),
        }
    }

    // --- validation ---------------------------------------------------------

    /// `validateTx`: the sender of a Quai transaction that passes.
    fn validate_tx(&self, tx: &PoolTx) -> Result<Address, String> {
        let q = tx.quai().ok_or(ERR_INVALID_SENDER)?;
        if tx.size > TX_MAX_SIZE {
            return Err("oversized data".into());
        }
        if self.current_max_gas < q.gas {
            return Err(format!(
                "exceeds block gas limit, tx: {}, current limit: {}",
                q.gas, self.current_max_gas
            ));
        }
        if big::bit_len(&q.gas_price) > 256 {
            return Err("max fee per gas higher than 2^256-1".into());
        }
        let internal = match tx.sender {
            Some(s) => internal_and_quai(&s, self.location)?,
            None => return Err(ERR_INVALID_SENDER.into()),
        };
        let min = block::base_fee(&self.chain.current_block());
        if big::cmp(&min, &q.gas_price).is_gt() {
            return Err(format!(
                "tx has incorrect or low gas price, have {}, want {}",
                big::decimal(&q.gas_price),
                big::decimal(&min)
            ));
        }
        if big::cmp(&q.gas_price, &self.gas_price).is_lt() {
            return Err(ERR_UNDERPRICED.into());
        }
        if self.state_nonce(&internal) > q.nonce {
            return Err("nonce too low".into());
        }
        if big::cmp(&self.state_balance(&internal), &tx.cost()).is_lt() {
            return Err("insufficient funds for gas * price + value".into());
        }
        let intrinsic = quai_execution::gas::intrinsic_gas(&q.data, &q.access_list, q.to.is_none())
            .map_err(|_| "gas uint64 overflow".to_string())?;
        if q.gas < intrinsic {
            return Err("intrinsic gas too low".into());
        }
        Ok(internal)
    }

    // --- adding -------------------------------------------------------------

    /// `add`: validate and queue (or replace a pending transaction).
    /// Returns whether it replaced one.
    fn add(&mut self, tx: Tx, local: bool, now: u64) -> Result<bool, String> {
        let hash = tx.hash;
        if self.all.get(&hash).is_some() {
            return Err(ERR_ALREADY_KNOWN.into());
        }
        let is_local = local || self.locals_contain(&tx);
        self.validate_tx(&tx)?;
        let capacity = self.config.global_slots + self.config.global_queue;
        if (self.all.slots + num_slots(&tx)) as u64 > capacity {
            if self.priced.underpriced(&tx, &self.all) {
                return Err(ERR_UNDERPRICED.into());
            }
            let need = self.all.slots as i64 - capacity as i64 + num_slots(&tx) as i64;
            let (drop, ok) = self.priced.discard(need, false, &self.all);
            if !ok {
                return Err(ERR_POOL_FULL.into());
            }
            for d in drop {
                self.remove_tx(&d.hash, false, now);
            }
        }
        let from = tx.pool_sender(self.chain_id).map_err(String::from)?;
        let internal = internal_and_quai(&from, self.location)?;
        if self.pending.get(&internal).is_some_and(|l| l.overlaps(&tx)) {
            let list = self.pending.get_mut(&internal).ok_or(ERR_INVALID_SENDER)?;
            let (inserted, old) = list.add(tx.clone(), self.config.price_bump);
            if !inserted {
                return Err(ERR_REPLACE_UNDERPRICED.into());
            }
            if let Some(old) = &old {
                self.all.remove(&old.hash);
                self.priced.removed(1, &self.all);
            }
            self.all.add(tx.clone(), is_local);
            self.priced.put(tx.clone(), is_local);
            self.journal_tx(internal, &tx);
            self.beats.insert(internal, now);
            return Ok(old.is_some());
        }
        let replaced = self.enqueue_tx(tx.clone(), is_local, true, now)?;
        if local && !self.locals.contains(&internal) {
            self.locals.insert(internal);
            let locals = self.locals.clone();
            let (chain_id, location) = (self.chain_id, self.location);
            let migrated = self.all.remote_to_locals(|t| {
                t.pool_sender(chain_id)
                    .ok()
                    .and_then(|f| internal_and_quai(&f, location).ok())
                    .is_some_and(|a| locals.contains(&a))
            });
            self.priced.removed(migrated, &self.all);
        }
        self.journal_tx(internal, &tx);
        Ok(replaced)
    }

    /// `journalTx`: a local account's transaction goes to the journal.
    fn journal_tx(&mut self, from: Address, tx: &Tx) {
        if self.journaling && self.locals.contains(&from) {
            self.journal_pending.push(tx.clone());
        }
    }
    /// The transactions to append to the journal since the last call.
    pub fn take_journal(&mut self) -> Vec<Tx> {
        std::mem::take(&mut self.journal_pending)
    }
    /// `local`: the transactions of local accounts, pending then queued.
    pub fn local(&self) -> Vec<Tx> {
        let mut out = vec![];
        for a in &self.locals {
            if let Some(l) = self.pending.get(a) {
                out.extend(l.flatten());
            }
            if let Some(l) = self.queue.get(a) {
                out.extend(l.flatten());
            }
        }
        out
    }

    /// `enqueueTx`.
    fn enqueue_tx(&mut self, tx: Tx, local: bool, add_all: bool, now: u64) -> Result<bool, String> {
        let from = tx.pool_sender(self.chain_id).map_err(String::from)?;
        let internal = internal_and_quai(&from, self.location)?;
        let list = self
            .queue
            .entry(internal)
            .or_insert_with(|| TxList::new(false));
        let (inserted, old) = list.add(tx.clone(), self.config.price_bump);
        if !inserted {
            return Err(ERR_REPLACE_UNDERPRICED.into());
        }
        if let Some(old) = &old {
            self.all.remove(&old.hash);
            self.priced.removed(1, &self.all);
        }
        if add_all {
            self.all.add(tx.clone(), local);
            self.priced.put(tx, local);
        }
        self.beats.entry(internal).or_insert(now);
        Ok(old.is_some())
    }

    /// `promoteTx`.
    fn promote_tx(&mut self, addr: Address, tx: Tx, now: u64) -> bool {
        let list = self
            .pending
            .entry(addr)
            .or_insert_with(|| TxList::new(true));
        let (inserted, old) = list.add(tx.clone(), self.config.price_bump);
        if !inserted {
            self.all.remove(&tx.hash);
            self.priced.removed(1, &self.all);
            return false;
        }
        if let Some(old) = old {
            self.all.remove(&old.hash);
            self.priced.removed(1, &self.all);
        }
        self.pending_nonces.set(addr, tx.nonce() + 1);
        self.beats.insert(addr, now);
        true
    }

    /// `AddLocal`: an RPC submit.
    pub fn add_local(&mut self, tx: Tx, now: u64) -> Result<(), String> {
        let local = !self.config.no_locals;
        self.add_txs(vec![tx], local, now)
            .pop()
            .unwrap_or(Err(ERR_INVALID_SENDER.into()))
    }
    /// `AddRemotes`: a gossip batch.
    pub fn add_remotes(&mut self, txs: Vec<Tx>, now: u64) -> Vec<Result<(), String>> {
        if txs.is_empty() {
            return vec![];
        }
        self.add_txs(txs, false, now)
    }

    /// `addTxs`.
    pub fn add_txs(&mut self, txs: Vec<Tx>, local: bool, now: u64) -> Vec<Result<(), String>> {
        let mut errs: Vec<Result<(), String>> = vec![Ok(()); txs.len()];
        let mut news = vec![];
        let mut news_idx = vec![];
        let mut qi_news = vec![];
        let mut qi_idx = vec![];
        for (i, tx) in txs.into_iter().enumerate() {
            if self.all.get(&tx.hash).is_some() {
                errs[i] = Err(ERR_ALREADY_KNOWN.into());
                continue;
            }
            match &tx.body {
                Body::Qi(_) => {
                    if self.qi_pool.get(&tx.hash).is_some() {
                        errs[i] = Err(ERR_ALREADY_KNOWN.into());
                        continue;
                    }
                    qi_news.push(tx);
                    qi_idx.push(i);
                    continue;
                }
                Body::External(_) => {
                    errs[i] = Err("external tx is not supported in tx pool".into());
                    continue;
                }
                Body::Quai(_) => {}
            }
            match tx.sender {
                Some(s) => {
                    if let Err(e) = internal_and_quai(&s, self.location) {
                        errs[i] = Err(e);
                        continue;
                    }
                }
                None => {
                    errs[i] = Err(ERR_INVALID_SENDER.into());
                    continue;
                }
            }
            news.push(tx);
            news_idx.push(i);
        }
        if !qi_news.is_empty() {
            for (i, e) in self.add_qi_txs(qi_news, now).into_iter().enumerate() {
                errs[qi_idx[i]] = e;
            }
        }
        if news.is_empty() {
            return errs;
        }
        let (new_errs, dirty) = self.add_txs_locked(news, local, now);
        for (i, e) in new_errs.into_iter().enumerate() {
            errs[news_idx[i]] = e;
        }
        self.dirty.get_or_insert_with(BTreeSet::new).extend(dirty);
        errs
    }

    /// `addTxsLocked`: the Quai transactions of a batch; Qi ones are
    /// skipped.
    fn add_txs_locked(
        &mut self,
        txs: Vec<Tx>,
        local: bool,
        now: u64,
    ) -> (Vec<Result<(), String>>, BTreeSet<Address>) {
        let mut dirty = BTreeSet::new();
        let mut errs = vec![];
        for tx in txs {
            if tx.qi().is_some() {
                errs.push(Ok(()));
                continue;
            }
            let sender = self.sender_of(&tx);
            match self.add(tx, local, now) {
                Ok(replaced) => {
                    if !replaced {
                        if let Some(a) = sender {
                            dirty.insert(a);
                        }
                    }
                    errs.push(Ok(()));
                }
                Err(e) => errs.push(Err(e)),
            }
        }
        (errs, dirty)
    }

    /// The ETX limits and context of `addQiTxs`.
    fn qi_context(&self) -> (Arc<BlockData>, u64, u64) {
        let current = self.chain.current_block();
        let (r, p) = qi::etx_limits(&current);
        (current, r, p)
    }

    /// `addQiTxs`: validate and add Qi transactions.
    fn add_qi_txs(&mut self, txs: Vec<Tx>, now: u64) -> Vec<Result<(), String>> {
        let (current, etx_r, etx_p) = self.qi_context();
        let expansion = block::expansion_number(&current);
        let mut errs = Vec::with_capacity(txs.len());
        let mut ok = vec![];
        for tx in txs {
            let Some(q) = tx.qi() else {
                errs.push(Err("not a Qi transaction".to_string()));
                continue;
            };
            if qi::inactive_output(q, expansion) {
                errs.push(Err("Qi TXO emitted to an inactive chain".into()));
                continue;
            }
            let total = match qi::validate_inputs(&tx, self.chain.as_ref(), &current) {
                Ok(t) => t,
                Err(e) => {
                    errs.push(Err(e));
                    continue;
                }
            };
            match qi::validate_outputs_and_signature(
                &tx,
                self.chain.as_ref(),
                total,
                &current,
                self.qi_scaling,
                etx_r,
                etx_p,
            ) {
                Ok(fee) => {
                    errs.push(Ok(()));
                    ok.push((tx, big::from_u64(fee)));
                }
                Err(e) => errs.push(Err(e)),
            }
        }
        for (tx, fee) in ok {
            let hash = tx.hash;
            self.qi_pool.add(
                hash,
                QiEntry {
                    tx,
                    fee: fee.clone(),
                    received: now,
                },
            );
            let mut key = [0u8; 16];
            key.copy_from_slice(&hash[..16]);
            self.qi_fees.contains_or_add(key, fee);
        }
        errs
    }

    /// `addQiTxsWithoutValidationLocked`: reinjection after a reorg; a
    /// transaction whose fee is cached is not validated again.
    fn add_qi_txs_without_validation(&mut self, txs: Vec<Tx>, now: u64) {
        for tx in txs {
            let hash = tx.hash;
            if self.qi_pool.get(&hash).is_some() {
                continue;
            }
            let mut key = [0u8; 16];
            key.copy_from_slice(&hash[..16]);
            let fee = match self.qi_fees.get(&key).cloned() {
                Some(f) => f,
                None => {
                    let (current, etx_r, etx_p) = self.qi_context();
                    let Ok(total) = qi::validate_inputs(&tx, self.chain.as_ref(), &current) else {
                        continue;
                    };
                    let Ok(fee) = qi::validate_outputs_and_signature(
                        &tx,
                        self.chain.as_ref(),
                        total,
                        &current,
                        self.qi_scaling,
                        etx_r,
                        etx_p,
                    ) else {
                        continue;
                    };
                    let fee = big::from_u64(fee);
                    self.qi_fees.contains_or_add(key, fee.clone());
                    fee
                }
            };
            self.qi_pool.add(
                hash,
                QiEntry {
                    tx,
                    fee,
                    received: now,
                },
            );
        }
    }

    fn remove_qi_txs(&mut self, txs: &[Tx]) {
        for tx in txs {
            if self.qi_pool.get(&tx.hash).is_some() {
                self.qi_pool.remove(&tx.hash);
            }
        }
    }

    // --- removal ------------------------------------------------------------

    /// `removeTx`: drop one transaction; later pending nonces go back to the
    /// queue.
    fn remove_tx(&mut self, hash: &Hash, outofbound: bool, now: u64) {
        let Some(tx) = self.all.get(hash).cloned() else {
            return;
        };
        let Some(internal) = self.sender_of(&tx) else {
            return;
        };
        self.all.remove(hash);
        if outofbound {
            self.priced.removed(1, &self.all);
        }
        if let Some(pending) = self.pending.get_mut(&internal) {
            let (removed, invalids) = pending.remove(&tx);
            if removed {
                if pending.is_empty() {
                    self.pending.remove(&internal);
                }
                for t in invalids {
                    let _ = self.enqueue_tx(t, false, false, now);
                }
                self.pending_nonces.set_if_lower(internal, tx.nonce());
                return;
            }
        }
        if let Some(future) = self.queue.get_mut(&internal) {
            future.remove(&tx);
            if future.is_empty() {
                self.queue.remove(&internal);
                self.beats.remove(&internal);
            }
        }
    }

    // --- runs -----------------------------------------------------------------

    /// One `runReorg`: a reset to `new_head` from `old_head` when given, then
    /// promotion of the dirty accounts (every queued account after a
    /// reset), demotion after a reset, and the global limits.
    pub fn run_reorg(&mut self, reset: Option<(Option<Arc<BlockData>>, Arc<BlockData>)>, now: u64) {
        let dirty = self.dirty.take();
        let mut promote: Vec<Address> = match (&dirty, &reset) {
            (Some(d), None) => d.iter().copied().collect(),
            _ => vec![],
        };
        let reset_done = reset.is_some();
        if let Some((old, new)) = reset {
            self.reset(old, new, now);
            promote = self.queue.keys().copied().collect();
        }
        self.promote_executables(&promote, now);
        if reset_done {
            self.demote_unexecutables(now);
            self.priced.set_base_fee(&self.all);
        }
        self.truncate_pending();
        self.truncate_queue(now);
        let lasts: Vec<(Address, u64)> = self
            .pending
            .iter()
            .filter_map(|(a, l)| l.txs.last().map(|t| (*a, t.nonce() + 1)))
            .collect();
        for (a, n) in lasts {
            self.pending_nonces.set(a, n);
        }
    }

    /// `reset`, with its outcome counted.
    fn reset(&mut self, old: Option<Arc<BlockData>>, new: Arc<BlockData>, now: u64) {
        let hash = new.hash;
        if self.reset_to(old, new, now) {
            self.resets += 1;
            self.reset_head = Some(hash);
        } else {
            self.reset_stops += 1;
        }
    }
    /// go-quai's `reset`; false where it returns early.
    fn reset_to(&mut self, old: Option<Arc<BlockData>>, new: Arc<BlockData>, now: u64) -> bool {
        let mut reinject: Vec<Tx> = vec![];
        let chain = self.chain.clone();
        match &old {
            Some(old) if old.hash != block::parent(&new) => {
                let (old_num, new_num) = (old.number, new.number);
                if old_num.abs_diff(new_num) <= 64 {
                    let rem = chain.get_block(&old.hash, old_num);
                    let add = chain.get_block(&new.hash, new_num);
                    match rem {
                        None => {
                            if new_num >= old_num {
                                // "Transaction pool reset with missing oldhead".
                                return false;
                            }
                            // A setHead: no reinjection, the state still resets.
                        }
                        Some(mut rem) => {
                            let Some(mut add) = add else {
                                return false;
                            };
                            let mut discarded = vec![];
                            let mut included = vec![];
                            while rem.number > add.number {
                                discarded.extend(block::transactions(&rem, now));
                                match chain.get_block(&block::parent(&rem), rem.number - 1) {
                                    Some(b) => rem = b,
                                    None => return false,
                                }
                            }
                            while add.number > rem.number {
                                included.extend(block::transactions(&add, now));
                                match chain.get_block(&block::parent(&add), add.number - 1) {
                                    Some(b) => add = b,
                                    None => return false,
                                }
                            }
                            while rem.hash != add.hash {
                                discarded.extend(block::transactions(&rem, now));
                                match chain.get_block(&block::parent(&rem), rem.number - 1) {
                                    Some(b) => rem = b,
                                    None => return false,
                                }
                                included.extend(block::transactions(&add, now));
                                match chain.get_block(&block::parent(&add), add.number - 1) {
                                    Some(b) => add = b,
                                    None => return false,
                                }
                            }
                            // TxDifferenceWithoutETXs, with the time reset.
                            let remove: BTreeSet<Hash> = included.iter().map(|t| t.hash).collect();
                            reinject = discarded
                                .into_iter()
                                .filter(|t| !remove.contains(&t.hash) && t.type_id() != 1)
                                .map(|t| {
                                    let t = (*t).clone();
                                    t.set_time(now);
                                    Arc::new(t)
                                })
                                .collect();
                        }
                    }
                }
            }
            _ => {
                if let Some(b) = chain.get_block(&new.hash, new.number) {
                    let qis: Vec<Tx> = block::transactions(&b, now)
                        .into_iter()
                        .filter(|t| t.qi().is_some())
                        .collect();
                    self.remove_qi_txs(&qis);
                }
            }
        }
        let state = match chain.state_at(&new) {
            Ok(s) => s,
            Err(_) => return false,
        };
        self.current_state = state.clone();
        self.qi_scaling = (chain.utxo_set_size(&new.hash) as f64).ln();
        self.pending_nonces = Noncer::new(state);
        self.current_max_gas = block::gas_limit(&new);
        if self.current_max_gas == 0 {
            self.current_max_gas = GENESIS_GAS_LIMIT;
        }
        let qis: Vec<Tx> = reinject
            .iter()
            .filter(|t| t.qi().is_some())
            .cloned()
            .collect();
        if !reinject.is_empty() {
            self.add_txs_locked(reinject, false, now);
        }
        if !qis.is_empty() {
            self.add_qi_txs_without_validation(qis, now);
        }
        true
    }

    /// `promoteExecutables`.
    fn promote_executables(&mut self, accounts: &[Address], now: u64) -> Vec<Tx> {
        let mut promoted = vec![];
        for addr in accounts {
            let Some(mut list) = self.queue.remove(addr) else {
                continue;
            };
            let forwards = list.forward(self.state_nonce(addr));
            for t in &forwards {
                self.all.remove(&t.hash);
            }
            let balance = self.state_balance(addr);
            let (drops, _) = list.filter(&balance, self.current_max_gas);
            for t in &drops {
                self.all.remove(&t.hash);
            }
            let start = self.pending_nonces.get(*addr);
            for t in list.ready(start) {
                if self.promote_tx(*addr, t.clone(), now) {
                    promoted.push(t);
                }
            }
            let caps = list.cap(self.config.account_queue as usize);
            for t in &caps {
                self.all.remove(&t.hash);
            }
            self.priced
                .removed(forwards.len() + drops.len() + caps.len(), &self.all);
            if list.is_empty() {
                self.beats.remove(addr);
            } else {
                self.queue.insert(*addr, list);
            }
        }
        promoted
    }

    /// `truncatePending`: above GlobalSlots, take from the accounts with
    /// the most pending transactions first, down to AccountSlots.
    fn truncate_pending(&mut self) {
        let mut pending: u64 = self.pending.values().map(|l| l.len() as u64).sum();
        if pending <= self.config.global_slots {
            return;
        }
        let account_slots = self.config.account_slots;
        let mut spammers: Vec<(Address, usize)> = self
            .pending
            .iter()
            .filter(|(_, l)| l.len() as u64 > account_slots)
            .map(|(a, l)| (*a, l.len()))
            .collect();
        // prque pops the highest priority first.
        spammers.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut offenders: Vec<Address> = vec![];
        let len = |pool: &Self, a: &Address| pool.pending.get(a).map_or(0, |l| l.len());
        let mut next = 0;
        while pending > self.config.global_slots && next < spammers.len() {
            let offender = spammers[next].0;
            next += 1;
            offenders.push(offender);
            if offenders.len() > 1 {
                let threshold = len(self, &offender);
                while pending > self.config.global_slots
                    && len(self, &offenders[offenders.len() - 2]) > threshold
                {
                    for addr in offenders.iter().take(offenders.len() - 1) {
                        self.drop_last_pending(*addr);
                        pending -= 1;
                    }
                }
            }
        }
        if pending > self.config.global_slots && !offenders.is_empty() {
            while pending > self.config.global_slots
                && len(self, &offenders[offenders.len() - 1]) as u64 > account_slots
            {
                for addr in &offenders {
                    self.drop_last_pending(*addr);
                    pending -= 1;
                }
            }
        }
    }
    fn drop_last_pending(&mut self, addr: Address) {
        let Some(list) = self.pending.get_mut(&addr) else {
            return;
        };
        if list.is_empty() {
            return;
        }
        let caps = list.cap(list.len() - 1);
        for t in &caps {
            self.all.remove(&t.hash);
            self.pending_nonces.set_if_lower(addr, t.nonce());
        }
        self.priced.removed(caps.len(), &self.all);
    }

    /// `truncateQueue`: above GlobalQueue, drop from the accounts with the
    /// newest heartbeat first.
    fn truncate_queue(&mut self, now: u64) {
        let queued: u64 = self.queue.values().map(|l| l.len() as u64).sum();
        if queued <= self.config.global_queue {
            return;
        }
        let mut addresses: Vec<(u64, Address)> = self
            .queue
            .keys()
            .map(|a| (self.beats.get(a).copied().unwrap_or(0), *a))
            .collect();
        addresses.sort();
        let mut drop = queued - self.config.global_queue;
        while drop > 0 {
            let Some((_, addr)) = addresses.pop() else {
                break;
            };
            let txs = self
                .queue
                .get(&addr)
                .map(|l| l.flatten())
                .unwrap_or_default();
            let size = txs.len() as u64;
            if size <= drop {
                for t in &txs {
                    self.remove_tx(&t.hash, true, now);
                }
                drop -= size;
                continue;
            }
            for t in txs.iter().rev() {
                if drop == 0 {
                    break;
                }
                self.remove_tx(&t.hash, true, now);
                drop -= 1;
            }
        }
    }

    /// `demoteUnexecutables`: after a reset, drop included and unpayable
    /// pending transactions and queue the ones behind them.
    fn demote_unexecutables(&mut self, now: u64) {
        let addrs: Vec<Address> = self.pending.keys().copied().collect();
        for addr in addrs {
            let Some(mut list) = self.pending.remove(&addr) else {
                continue;
            };
            let nonce = self.state_nonce(&addr);
            for t in list.forward(nonce) {
                self.all.remove(&t.hash);
            }
            let balance = self.state_balance(&addr);
            let (drops, invalids) = list.filter(&balance, self.current_max_gas);
            for t in &drops {
                self.all.remove(&t.hash);
            }
            for t in invalids {
                let _ = self.enqueue_tx(t, false, false, now);
            }
            if !list.is_empty() && list.txs.get(nonce).is_none() {
                for t in list.cap(0) {
                    let _ = self.enqueue_tx(t, false, false, now);
                }
            }
            if !list.is_empty() {
                self.pending.insert(addr, list);
            }
        }
    }

    // --- aging (the oracle's steps; Go's clock does this by waiting) ---------

    /// Move the heartbeat of `a` back to `now - ago`. False without one.
    pub fn age_beat(&mut self, a: &Address, ago: u64, now: u64) -> bool {
        match self.beats.get_mut(a) {
            Some(b) => {
                *b = now.saturating_sub(ago);
                true
            }
            None => false,
        }
    }
    /// Move the time of a pooled Quai transaction back to `now - ago`.
    pub fn age_tx(&mut self, hash: &Hash, ago: u64, now: u64) -> bool {
        match self.all.get(hash) {
            Some(t) => {
                t.set_time(now.saturating_sub(ago));
                true
            }
            None => false,
        }
    }
    /// Move the receive time of a Qi pool entry back to `now - ago`.
    pub fn age_qi(&mut self, hash: &Hash, ago: u64, now: u64) -> bool {
        match self.qi_pool.peek_mut(hash) {
            Some(e) => {
                e.received = now.saturating_sub(ago);
                true
            }
            None => false,
        }
    }

    // --- periodic loops -------------------------------------------------------

    /// One pass of the eviction ticker (every minute): queued accounts
    /// without a heartbeat within Lifetime, and pending accounts whose
    /// first transaction is older than Lifetime.
    pub fn evict(&mut self, now: u64) {
        let lifetime = self.config.lifetime;
        let since = |t: u64| now.saturating_sub(t);
        let queued: Vec<Address> = self.queue.keys().copied().collect();
        for addr in queued {
            let Some(list) = self.queue.get(&addr) else {
                continue;
            };
            let old = self.beats.get(&addr).is_none_or(|b| since(*b) > lifetime);
            if old {
                for t in list.flatten() {
                    self.remove_tx(&t.hash, true, now);
                }
            }
        }
        let pending: Vec<Address> = self.pending.keys().copied().collect();
        for addr in pending {
            let Some(list) = self.pending.get(&addr) else {
                continue;
            };
            let txs = list.flatten();
            let Some(first) = txs.first() else {
                continue;
            };
            if since(first.time()) > lifetime {
                for t in &txs {
                    self.remove_tx(&t.hash, true, now);
                }
            }
        }
    }

    /// One pass of `qiTxExpirationGoroutine` (every 10 minutes): look at the
    /// oldest entry Len/5 times and drop it when older than QiTxLifetime.
    pub fn expire_qi(&mut self, now: u64) {
        let mut i = 0;
        while i < self.qi_pool.len() / 5 {
            let expired = self.qi_pool.oldest().map(|(h, e)| {
                (
                    *h,
                    now.saturating_sub(e.received) > self.config.qi_tx_lifetime,
                )
            });
            if let Some((h, true)) = expired {
                self.qi_pool.remove(&h);
            }
            i += 1;
        }
    }

    /// One pass of `poolLimiterGoroutine` (every 30 seconds). Above
    /// GlobalQueue it keeps `len - AccountQueue` transactions of each queued
    /// account (all of them go when an account has no more than
    /// AccountQueue), as Go's `Cap(len - AccountQueue)` does, then drops
    /// whole accounts while the queue is still at or above GlobalQueue.
    pub fn limit(&mut self, now: u64) {
        let queued: u64 = self.queue.values().map(|l| l.len() as u64).sum();
        if queued <= self.config.global_queue {
            return;
        }
        let addrs: Vec<Address> = self.queue.keys().copied().collect();
        for addr in &addrs {
            let Some(list) = self.queue.get_mut(addr) else {
                continue;
            };
            let capacity = (list.len() as i64 - self.config.account_queue as i64).max(0) as usize;
            let caps = list.cap(capacity);
            for t in &caps {
                self.all.remove(&t.hash);
            }
            self.priced.removed(caps.len(), &self.all);
        }
        let mut queued: u64 = self.queue.values().map(|l| l.len() as u64).sum();
        if queued < self.config.global_queue {
            return;
        }
        for addr in addrs {
            let txs = self
                .queue
                .get(&addr)
                .map(|l| l.flatten())
                .unwrap_or_default();
            for t in &txs {
                self.remove_tx(&t.hash, true, now);
            }
            if txs.len() as u64 > queued {
                break;
            }
            queued -= txs.len() as u64;
            if queued < self.config.global_queue {
                break;
            }
        }
    }
}
