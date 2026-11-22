//! The chain processor: one per-height pipeline for every block source.
//!
//! A [`BlockSource`] supplies zone heights in order (a QEX1 export for
//! `replay`, public peers for `run`). For each height the processor:
//! 1. decodes each record and compares its hash and number with the values
//!    the source recorded, and the stored receipts (when the source has them)
//!    with the receipt root;
//! 2. checks the block and workshare proof-of-work on worker threads with
//!    shared epoch caches (`pow.rs`) and hands the results to the verifier
//!    memo, so that it never trusts a value it could not recompute;
//! 3. runs `Verifier::validate_block` (`quai_consensus::block`) against the
//!    ancestry window: coincident binding, parent hash and number in each
//!    context, body commitments, the block proof-of-work, the computed order
//!    against the source's records, header/inclusion/append rules and PCRC in
//!    each coincident context, the uncled entropy, the prime controllers, the
//!    ETX rollup root and the region/zone rollup agreement; with `--state`,
//!    the confirmed ETXs and the execution of the zone block with every state
//!    root;
//! 4. commits the state every `commit_every` heights and writes a checkpoint
//!    every `checkpoint_every` heights and when it stops (`checkpoint.rs`).
//!
//! Both modes keep the region rollups of recent blocks and the prime
//! controller state; checkpoints carry them in their prime section.
use crate::blockstore::{self, Accepted, BlockStore, Created, DominantView, UtxoChanges};
use crate::checkpoint::{self, Checkpoint};
use crate::log::Log;
use crate::pow::{Done, Job, Pool};
use crate::qex::{Context, Height};
use crate::spec::Chain;
use crate::state::{self, StateReplay};
use crate::store::{self, PastDb, StateStore};
use crate::uncles;
use crate::window::Window;
use crypto_bigint::Encoding as _;
use quai_consensus::{
    block, choice_window, confirmation,
    history::{History, Record},
    placement::{self, CandidateIndex, Snapshot},
    pow::SharedCaches,
    prime_controller,
    rules::Uint,
    share_rules,
    verifier::Verifier,
    Error,
};
use quai_execution::qi::{OutPoint, UtxoView};
use quai_primitives::transaction::{Kind, Transaction};
use quai_primitives::{encode_hex, work, Hash};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// What a block source has next.
pub enum Next {
    /// The next zone height, in order from the processor's start height.
    Height(Box<Height>),
    /// Nothing yet: call [`BlockSource::wait`], then ask again.
    Wait,
    /// The source has no more heights.
    End,
}
/// A source that cannot continue: its container or its peers' data is bad.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceError {
    pub context: String,
    pub check: String,
    pub expected: String,
    pub actual: String,
}
/// Zone heights in order. The first height a source returns is the
/// processor's start height ([`Processor::start`]): the genesis, or the block
/// of the checkpoint it resumed from, which must be the checkpoint's block.
pub trait BlockSource {
    /// The next height, without blocking for long.
    fn next(&mut self) -> Result<Next, SourceError>;
    /// Block until a height may be ready, or `stop` is set, or a short time
    /// passed. Only called after `next` returned [`Next::Wait`].
    fn wait(&mut self, stop: &AtomicBool) {
        if !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    /// The highest zone height this source knows of, for progress lines.
    fn target(&self) -> Option<u64>;
    /// The source carries go-quai's stored zone receipts (Go exports).
    fn receipts(&self) -> bool;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub zone_height: Option<u64>,
    pub context: String,
    pub number: Option<u64>,
    pub hash: Option<Hash>,
    pub check: String,
    pub expected: String,
    pub actual: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// Every height in the export passed.
    Complete,
    /// Every height up to `--to` passed.
    ReachedTo,
    Failed,
    /// A signal stopped the run after the last height that passed.
    Stopped,
}
#[derive(Clone, Debug)]
pub struct Summary {
    pub environment: String,
    pub last_zone: u64,
    pub highest_passed: Option<u64>,
    pub heights: u64,
    pub records: [u64; 3],
    pub shares: u64,
    pub elapsed: Duration,
    pub end: End,
    pub failure: Option<Failure>,
    pub timing: Timing,
    /// Zone blocks executed with every state root equal, when `--state` is on.
    pub state_blocks: Option<u64>,
    /// The checkpoint this run continued from.
    pub resumed_from: Option<u64>,
    /// Zone heights checked by this run (the counters above include earlier runs).
    pub run_heights: u64,
    /// The last checkpoint written.
    pub checkpoint: Option<u64>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
    pub read_decode: Duration,
    pub wait_pow: Duration,
    pub ordered_checks: Duration,
}
impl Summary {
    /// Zone heights per second in this run.
    pub fn rate(&self) -> f64 {
        let s = self.elapsed.as_secs_f64();
        if s > 0.0 {
            self.run_heights as f64 / s
        } else {
            0.0
        }
    }
    /// The single result line.
    pub fn line(&self) -> String {
        let passed = self
            .highest_passed
            .map_or("none".to_string(), |h| h.to_string());
        let state = match self.state_blocks {
            Some(n) => format!(", {n} zone blocks executed with all state roots equal"),
            None => String::new(),
        };
        let resumed = match self.resumed_from {
            Some(h) => format!(
                "resumed from checkpoint {h}, {} heights in this run, ",
                self.run_heights
            ),
            None => String::new(),
        };
        let checkpoint = match self.checkpoint {
            Some(h) => format!(", checkpoint {h}"),
            None => String::new(),
        };
        let stats = format!(
            "{resumed}{} heights, {} prime, {} region, {} zone records, {} workshares{state}{checkpoint}, {:.1} blocks/s, {:.1}s",
            self.heights,
            self.records[0],
            self.records[1],
            self.records[2],
            self.shares,
            self.rate(),
            self.elapsed.as_secs_f64()
        );
        match (&self.end, &self.failure) {
            (End::Failed, Some(f)) => format!(
                "replay FAIL: highest zone height with all checks passed: {passed}; first failure: zone_height={} context={} number={} hash={} check=\"{}\" expected={} actual={} ({stats})",
                f.zone_height.map_or("-".into(), |h| h.to_string()),
                f.context,
                f.number.map_or("-".into(), |n| n.to_string()),
                f.hash.map_or("-".into(), |h| encode_hex(&h)),
                f.check,
                quote(&f.expected),
                quote(&f.actual),
            ),
            (End::ReachedTo, _) => format!(
                "replay PASS: highest zone height with all checks passed: {passed} (stopped at --to; export ends at {}; environment {}; {stats})",
                self.last_zone, self.environment
            ),
            (End::Stopped, _) => format!(
                "replay STOPPED: highest zone height with all checks passed: {passed} (stopped by a signal; export ends at {}; environment {}; {stats})",
                self.last_zone, self.environment
            ),
            _ => format!(
                "replay PASS: highest zone height with all checks passed: {passed} (end of export; environment {}; {stats})",
                self.environment
            ),
        }
    }
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "environment": self.environment,
            "reference_revision": quai_primitives::REFERENCE_REVISION,
            "export_last_zone": self.last_zone,
            "highest_passed_zone_height": self.highest_passed,
            "heights_checked": self.heights,
            "records": {"prime": self.records[0], "region": self.records[1], "zone": self.records[2]},
            "workshares": self.shares,
            "state_blocks_executed": self.state_blocks,
            "resumed_from": self.resumed_from,
            "heights_this_run": self.run_heights,
            "last_checkpoint": self.checkpoint,
            "elapsed_seconds": self.elapsed.as_secs_f64(),
            "zone_blocks_per_second": self.rate(),
            "end": format!("{:?}", self.end),
            "timing_seconds": {
                "read_decode": self.timing.read_decode.as_secs_f64(),
                "wait_pow": self.timing.wait_pow.as_secs_f64(),
                "ordered_checks": self.timing.ordered_checks.as_secs_f64(),
            },
            "failure": self.failure.as_ref().map(|f| serde_json::json!({
                "zone_height": f.zone_height,
                "context": f.context,
                "number": f.number,
                "hash": f.hash.map(|h| encode_hex(&h)),
                "check": f.check,
                "expected": f.expected,
                "actual": f.actual,
            })),
        })
    }
}
fn quote(v: &str) -> String {
    if v.is_empty() || v.contains(char::is_whitespace) {
        format!("\"{v}\"")
    } else {
        v.to_string()
    }
}
pub fn describe(e: &Error) -> String {
    block::describe(e)
}
fn hex(h: &[u8]) -> String {
    encode_hex(h)
}

/// A decoded zone height waiting for its proof-of-work result.
struct Slot {
    seq: u64,
    height: Height,
    records: [Option<Result<Record, Error>>; 3],
    submitted: bool,
}

struct Checker {
    chain: Arc<Chain>,
    profiles: [placement::Profile; 3],
    verifier: Verifier,
    window: Arc<Window>,
    histories: [History; 3],
    indexes: [CandidateIndex; 3],
    heads: [Option<(Hash, u64)>; 3],
    orders: HashMap<Hash, u8>,
    order_log: VecDeque<Hash>,
    /// Region rollups of recent region and prime blocks (both modes).
    rollups: HashMap<Hash, Arc<Vec<Transaction>>>,
    rollup_log: VecDeque<Hash>,
    /// The prime controller state after the prime head.
    controller: Option<prime_controller::State>,
    now: u64,
    /// The source carries the stored zone receipts.
    receipts: bool,
    state: Option<StateReplay>,
    /// Build what the block store keeps of each accepted block.
    collect: bool,
    caches: Arc<SharedCaches>,
    /// The zone block a shadow bootstrap trusted (test tool): it and its
    /// ancestry were not validated here. Every checkpoint carries it on.
    trusted: Option<(Hash, u64)>,
    /// The RPC's live view: unlock events and the rolling fee window.
    live: Option<Arc<crate::rpc_view::Live>>,
    fees: RollingFees,
}
/// go-quai's rolling fee window (core/state_processor.go
/// `calcRollingFeeInfo`, `GetRollingFeeInfo`) over the blocks this process
/// executed: a minimum that rises 1% a block unless a block goes below it,
/// a maximum that falls 1% a block unless a block goes above it, and the
/// running mean of the blocks' average fees (their Quai fees over the count
/// of their Quai and Qi transactions). Blocks without such a transaction
/// change nothing.
#[derive(Default)]
struct RollingFees {
    min: Option<Uint>,
    max: Option<Uint>,
    avg: Option<Uint>,
    num: Uint,
}
impl RollingFees {
    fn add(&mut self, block: &quai_consensus::zone_execution::TxFees, quai_fees: Uint) {
        if block.count == 0 {
            return;
        }
        // Every divisor is above zero: 100, the block's count, and the
        // number of blocks after this one.
        let hundred = Uint::from_u64(100);
        let percent = |v: Uint, p: u64| v.wrapping_mul(&Uint::from_u64(p)).wrapping_div(&hundred);
        self.min = Some(match self.min {
            Some(m) if block.min >= m => percent(m, 101),
            _ => block.min,
        });
        self.max = Some(match self.max {
            Some(m) if block.max <= m => percent(m, 99),
            _ => block.max,
        });
        let avg = self.avg.unwrap_or(Uint::ONE);
        let block_avg = quai_fees.wrapping_div(&Uint::from_u64(block.count));
        let sum = self.num.wrapping_mul(&avg).wrapping_add(&block_avg);
        self.num = self.num.wrapping_add(&Uint::ONE);
        self.avg = Some(sum.wrapping_div(&self.num));
    }
    fn get(&self) -> Option<[[u8; 32]; 3]> {
        Some([
            self.min?.to_be_bytes(),
            self.max?.to_be_bytes(),
            self.avg?.to_be_bytes(),
        ])
    }
}
struct Fail {
    context: Context,
    check: String,
    expected: String,
    actual: String,
}
fn fail(
    context: Context,
    check: impl Into<String>,
    expected: impl Into<String>,
    actual: impl Into<String>,
) -> Fail {
    Fail {
        context,
        check: check.into(),
        expected: expected.into(),
        actual: actual.into(),
    }
}
const CONTEXTS: [Context; 3] = [Context::Prime, Context::Region, Context::Zone];
fn h32(v: &[u8]) -> Hash {
    v.try_into().unwrap_or([0; 32])
}
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Checker {
    fn new(
        chain: Arc<Chain>,
        caches: Arc<SharedCaches>,
        receipts: bool,
        state: Option<StateReplay>,
    ) -> Self {
        let window = Window::new();
        let profiles = std::array::from_fn(|c| chain.placement_profile(c));
        Self {
            histories: std::array::from_fn(|c| History::with_reader(window.history_reader(c))),
            indexes: std::array::from_fn(|c| CandidateIndex::with_reader(window.index_reader(c))),
            profiles,
            verifier: Verifier::with_shared_caches(caches.clone()),
            caches,
            collect: false,
            live: None,
            fees: RollingFees::default(),
            window,
            heads: [None; 3],
            orders: HashMap::new(),
            order_log: VecDeque::new(),
            rollups: HashMap::new(),
            rollup_log: VecDeque::new(),
            controller: None,
            now: unix_now(),
            receipts,
            state,
            chain,
            trusted: None,
        }
    }
    fn remember_order(&mut self, id: Hash, order: u8) {
        self.orders.insert(id, order);
        self.order_log.push_back(id);
        if self.order_log.len() > 4096 {
            if let Some(old) = self.order_log.pop_front() {
                self.orders.remove(&old);
            }
        }
    }
    fn remember_rollup(&mut self, id: Hash, rollup: Arc<Vec<Transaction>>) {
        if self.rollups.insert(id, rollup).is_none() {
            self.rollup_log.push_back(id);
        }
        if self.rollup_log.len() > state::KEEP {
            if let Some(old) = self.rollup_log.pop_front() {
                self.rollups.remove(&old);
            }
        }
    }
    fn genesis(&mut self, slot: Slot) -> Result<(), Fail> {
        let expected = self.chain.genesis_hash();
        for c in CONTEXTS {
            let (Some(raw), Some(decoded)) = (
                slot.height.records[c.index()].as_ref(),
                slot.records[c.index()].as_ref(),
            ) else {
                return Err(fail(
                    c,
                    "genesis record",
                    "a record in every context",
                    "none",
                ));
            };
            if raw.hash != expected {
                return Err(fail(c, "genesis hash", hex(&expected), hex(&raw.hash)));
            }
            let record = match decoded {
                Ok(r) => r,
                Err(e) => {
                    return Err(fail(
                        c,
                        "decode",
                        "a work object the Rust codec accepts",
                        decode_reason(&raw.block, e),
                    ))
                }
            };
            if record.id() != raw.hash {
                return Err(fail(c, "block hash", hex(&raw.hash), hex(&record.id())));
            }
            if raw.number != 0 {
                return Err(fail(c, "number", "0", raw.number.to_string()));
            }
        }
        self.install_genesis();
        self.window
            .pin_prime_view(Arc::new(self.chain.genesis().clone()));
        for c in 0..3 {
            self.heads[c] = Some((expected, 0));
        }
        self.remember_order(expected, 0);
        self.remember_rollup(expected, Arc::new(vec![]));
        self.controller = Some(
            prime_controller::State::genesis(self.chain.genesis()).map_err(|e| {
                fail(
                    Context::Prime,
                    "prime controllers",
                    "the genesis controller state",
                    describe(&e),
                )
            })?,
        );
        Ok(())
    }
    /// The trusted genesis in every context, as the chain spec builds it.
    fn install_genesis(&mut self) {
        let zone = Arc::new(self.chain.genesis().clone());
        let snapshots: [Arc<Snapshot>; 3] = std::array::from_fn(|c| {
            Arc::new(
                placement::bootstrap_candidate(&zone, &self.profiles[c])
                    .expect("trusted genesis snapshot"),
            )
        });
        self.window.set_genesis(zone, snapshots);
    }
    /// Everything but the execution state, for a checkpoint at the zone head.
    fn checkpoint(
        &self,
        counters: checkpoint::Counters,
        state: Option<checkpoint::StateSection>,
    ) -> Result<Checkpoint, String> {
        let controller = self
            .controller
            .as_ref()
            .ok_or("checkpoint without a prime controller state")?;
        self.checkpoint_at(self.heads, controller, counters, state)
    }
    /// A checkpoint at `heads` with the prime controller state after them
    /// (the zone head, or the base of the tip, below it).
    fn checkpoint_at(
        &self,
        heads: [Option<(Hash, u64)>; 3],
        controller: &prime_controller::State,
        counters: checkpoint::Counters,
        state: Option<checkpoint::StateSection>,
    ) -> Result<Checkpoint, String> {
        let (block, height) = heads[2].ok_or("checkpoint before genesis")?;
        let (windows, views) = self.window.export();
        // Only the ancestors of each head: in tip mode the window also
        // holds blocks of other branches.
        let mut c = 0;
        let windows = windows.map(|w| {
            let ctx = c;
            c += 1;
            let mut keep = std::collections::HashSet::new();
            let mut at = heads[ctx].map(|h| h.0);
            let by_id: HashMap<Hash, &Arc<Record>> = w.iter().map(|(r, _)| (r.id(), r)).collect();
            while let Some(id) = at {
                let Some(r) = by_id.get(&id) else { break };
                keep.insert(id);
                at = r.parent(ctx).ok();
            }
            w.into_iter()
                .filter(|(r, _)| keep.contains(&r.id()))
                .map(|(r, s)| {
                    (
                        r.bytes().to_vec(),
                        checkpoint::Snapshot {
                            manifest: s.manifest.clone(),
                            interlinks: s.interlinks,
                            termini: s.termini.as_ref().map(|t| t.encode()),
                        },
                    )
                })
                .collect()
        });
        Ok(Checkpoint {
            environment: self.chain.spec().environment.clone(),
            genesis: self.chain.genesis_hash(),
            height,
            block,
            counters,
            heads,
            windows,
            prime_views: views.iter().map(|r| r.bytes().to_vec()).collect(),
            orders: self
                .order_log
                .iter()
                .filter_map(|id| Some((*id, *self.orders.get(id)?)))
                .collect(),
            state,
            prime: Some(self.prime_section(controller)),
            trusted: self.trusted,
        })
    }
    /// The prime controller state and the recent region rollups.
    fn prime_section(&self, controller: &prime_controller::State) -> checkpoint::PrimeSection {
        let c = controller.parts();
        checkpoint::PrimeSection {
            head: c.head,
            number: c.number,
            first: c.first,
            rates: c.rates.iter().map(|r| r.to_be_bytes()).collect(),
            inbound: c.inbound.as_ref().map(|v| store::encode_tx_list(v)),
            parent_window: c.parent_window.as_ref().map(|w| w.encode()),
            head_window: c.head_window.encode(),
            rollups: self
                .rollup_log
                .iter()
                .filter_map(|id| Some((*id, store::encode_tx_list(self.rollups.get(id)?))))
                .collect(),
        }
    }
    /// Rebuild the ordered checks from a checkpoint. `rollups` are the
    /// region rollups of a state section without a prime section.
    fn restore(
        &mut self,
        cp: &Checkpoint,
        rollups: Vec<(Hash, Arc<Vec<Transaction>>)>,
    ) -> Result<(), String> {
        self.install_genesis();
        for (id, order) in &cp.orders {
            self.remember_order(*id, *order);
        }
        for (c, window) in cp.windows.iter().enumerate() {
            for (bytes, s) in window {
                let record = Record::decode(bytes)
                    .map_err(|e| format!("checkpoint record: {}", describe(&e)))?;
                let termini = match &s.termini {
                    Some(t) => Some(
                        placement::Termini::decode(t)
                            .map_err(|e| format!("checkpoint termini: {}", describe(&e)))?,
                    ),
                    None => None,
                };
                let snapshot = Snapshot {
                    manifest: s.manifest.clone(),
                    interlinks: s.interlinks,
                    termini,
                };
                self.window.insert(c, Arc::new(record), Arc::new(snapshot));
            }
        }
        for raw in &cp.prime_views {
            let record = Record::decode(raw)
                .map_err(|e| format!("checkpoint prime view: {}", describe(&e)))?;
            self.window.pin_prime_view(Arc::new(record));
        }
        let genesis = self.chain.genesis_hash();
        for (c, head) in cp.heads.iter().enumerate() {
            let (hash, number) = head.ok_or("checkpoint without a head")?;
            let tail = match cp.windows[c].last() {
                Some((bytes, _)) => {
                    let r = Record::decode(bytes).map_err(|e| describe(&e))?;
                    (r.id(), r.number(c).map_err(|e| describe(&e))?)
                }
                None => (genesis, 0),
            };
            if tail != (hash, number) {
                return Err(format!(
                    "checkpoint {} head {} at {number} is not its last record {} at {}",
                    CONTEXTS[c].name(),
                    hex(&hash),
                    hex(&tail.0),
                    tail.1
                ));
            }
        }
        self.heads = cp.heads;
        if self.heads[2] != Some((cp.block, cp.height)) {
            return Err("checkpoint zone head is not its block".into());
        }
        self.trusted = cp.trusted;
        self.restore_prime(cp, rollups)
    }
    /// The prime controller state and region rollups of a checkpoint. A
    /// checkpoint without a prime section gets the state the controllers
    /// imply before ControllerKickInBlock (every prime block keeps
    /// params.ExchangeRate and nothing else is read before the kick-in).
    fn restore_prime(
        &mut self,
        cp: &Checkpoint,
        rollups: Vec<(Hash, Arc<Vec<Transaction>>)>,
    ) -> Result<(), String> {
        let (head, number) = cp.heads[0].ok_or("checkpoint without a prime head")?;
        let head_record = self.histories[0]
            .get(&head)
            .map_err(|e| format!("checkpoint prime head record: {}", describe(&e)))?;
        let parent = (number > 0)
            .then(|| head_record.parent(0))
            .transpose()
            .map_err(|e| describe(&e))?;
        let controller = match &cp.prime {
            Some(p) => {
                for (id, raw) in &p.rollups {
                    let list = store::decode_tx_list(raw)
                        .map_err(|e| format!("checkpoint region rollup: {e}"))?;
                    self.remember_rollup(*id, Arc::new(list));
                }
                let window = |raw: &[u8]| {
                    choice_window::Window::decode(raw)
                        .map_err(|e| format!("checkpoint token choice set: {e}"))
                };
                prime_controller::State::from_parts(prime_controller::Parts {
                    head: p.head,
                    number: p.number,
                    first: p.first,
                    rates: p.rates.iter().map(|r| Uint::from_be_bytes(*r)).collect(),
                    inbound: p
                        .inbound
                        .as_ref()
                        .map(|raw| store::decode_tx_list(raw))
                        .transpose()
                        .map_err(|e| format!("checkpoint prime inbound ETXs: {e}"))?,
                    parent_window: p.parent_window.as_deref().map(window).transpose()?,
                    head_window: window(&p.head_window)?,
                })
                .map_err(|e| format!("checkpoint prime controllers: {}", describe(&e)))?
            }
            None => {
                for (id, list) in rollups {
                    self.remember_rollup(id, list);
                }
                prime_controller::State::before_kick_in(head, number, parent).map_err(|e| {
                    format!(
                        "checkpoint {} has no prime controller state: {}",
                        cp.height,
                        describe(&e)
                    )
                })?
            }
        };
        // The state must follow the prime head, and its rates must be the
        // rates of the prime records the checkpoint holds.
        if controller.head() != (head, number)
            || controller.parent_window().map(|w| w.block()) != parent
        {
            return Err("checkpoint prime controllers do not follow its prime head".into());
        }
        for (raw, _) in &cp.windows[0] {
            let r = Record::decode(raw).map_err(|e| describe(&e))?;
            let n = r.number(0).map_err(|e| describe(&e))?;
            let rate = share_rules::uint(
                r.object()
                    .header()
                    .wire()
                    .exchange_rate
                    .as_deref()
                    .unwrap_or_default(),
            )
            .map_err(|e| describe(&e))?;
            if let Ok(kept) = controller.rate_at(n) {
                if kept != rate {
                    return Err(format!(
                        "checkpoint prime controllers: the rate of prime {n} is {}, its header has {}",
                        block::uint(kept),
                        block::uint(rate)
                    ));
                }
            }
        }
        self.controller = Some(controller);
        Ok(())
    }
    /// The header roots of the zone head, for the state checkpoint check.
    fn head_roots(&self) -> Result<(Hash, Hash, Hash, u64), String> {
        let (id, _) = self.heads[2].ok_or("no zone head")?;
        let record = self.histories[2]
            .get(&id)
            .map_err(|e| format!("zone head record: {}", describe(&e)))?;
        let b = record.object().header().wire();
        let size = share_rules::number(b.quai_state_size.as_deref().unwrap_or_default())
            .map_err(|e| describe(&e))?;
        let root = |v: &Option<quai_primitives::schema::common::ProtoHash>| {
            v.as_ref()
                .map(|h| h32(&h.value))
                .ok_or("zone head without its state roots")
        };
        Ok((
            root(&b.evm_root)?,
            root(&b.etx_set_root)?,
            root(&b.utxo_root)?,
            size,
        ))
    }
    fn check(&mut self, slot: Slot, done: Option<Done>) -> Result<(u8, Option<Accepted>), Fail> {
        let h = &slot.height;
        // 1. decode, hash and record number
        let mut records: [Option<Arc<Record>>; 3] = [None, None, None];
        for c in CONTEXTS {
            let Some(raw) = &h.records[c.index()] else {
                continue;
            };
            // Each record of the height was decoded with the slot.
            let Some(decoded) = slot.records[c.index()].as_ref() else {
                return Err(fail(c, "decode", "a decoded record", "none"));
            };
            let record = match decoded {
                Ok(r) => r,
                Err(e) => {
                    return Err(fail(
                        c,
                        "decode",
                        "a work object the Rust codec accepts",
                        decode_reason(&raw.block, e),
                    ))
                }
            };
            if record.id() != raw.hash {
                return Err(fail(c, "block hash", hex(&raw.hash), hex(&record.id())));
            }
            let n = record.number(c.index()).unwrap_or(u64::MAX);
            if n != raw.number {
                return Err(fail(
                    c,
                    "record number",
                    raw.number.to_string(),
                    n.to_string(),
                ));
            }
            records[c.index()] = Some(Arc::new(record.clone()));
        }
        let zone = records[2].clone().ok_or_else(|| {
            fail(
                Context::Zone,
                "records",
                "a zone record at every height",
                "none",
            )
        })?;
        // The stored receipts against the receipt root, before execution.
        if self.receipts {
            stored_receipts(&zone, &h.zone().receipts)?;
        }
        // Proof-of-work results of the parallel pass enter through the memo;
        // the pipeline checks the block proof again (a memo hit).
        let done = done.ok_or_else(|| {
            fail(
                Context::Zone,
                "proof-of-work",
                "a computed proof",
                "no result",
            )
        })?;
        self.verifier.reuse_header_proof_memo(done.proofs);
        self.verifier.reuse_inclusion_work_memo(done.work);
        // 2-8. Verifier::validate_block against the window: parent links,
        // bodies, proof-of-work, order, header/append rules, uncled entropy,
        // prime controllers, rollups and, with --state, execution.
        let heads = self.heads.map(|h| h.expect("genesis checked first"));
        let controller = self.controller.as_ref().ok_or_else(|| {
            fail(
                Context::Prime,
                "prime controllers",
                "a controller state",
                "none",
            )
        })?;
        let validated = match self.state.as_mut() {
            Some(st) => {
                let (maps, mut parent_state) = st.split();
                let known = state::KnownView {
                    orders: &self.orders,
                    region_rollups: &self.rollups,
                    maps: Some(maps),
                };
                let parent = block::Parent {
                    chain: &self.chain,
                    histories: &self.histories,
                    indexes: &self.indexes,
                    heads,
                    known: &known,
                    controller,
                    now: self.now,
                };
                self.verifier
                    .validate_block(&parent, records, Some(&mut parent_state))
            }
            None => {
                let known = state::KnownView {
                    orders: &self.orders,
                    region_rollups: &self.rollups,
                    maps: None,
                };
                let parent = block::Parent {
                    chain: &self.chain,
                    histories: &self.histories,
                    indexes: &self.indexes,
                    heads,
                    known: &known,
                    controller,
                    now: self.now,
                };
                self.verifier
                    .validate_block::<block::NoPast>(&parent, records, None)
            }
        }
        .map_err(|f| Fail {
            context: CONTEXTS[f.context.min(2)],
            check: f.check,
            expected: f.expected,
            actual: f.actual,
        })?;
        let block::Validated { checked, executed } = validated;
        let order = checked.order;
        let accepted = if self.collect {
            Some(self.accepted(&zone, order, h, executed.as_ref()))
        } else {
            None
        };
        if let (Some(st), Some(x)) = (self.state.as_mut(), executed) {
            st.record(&zone, x.confirmed, x.execution.created_keys)
                .map_err(|e| {
                    fail(
                        Context::Zone,
                        "state: commit",
                        "recorded history",
                        describe(&e),
                    )
                })?;
        }
        // Commit to the window.
        let mut next = checked.next;
        for c in &CONTEXTS[order as usize..] {
            let missing = || fail(*c, "window", "a checked record and its snapshot", "none");
            let (Some(r), Some(snapshot)) =
                (checked.records[c.index()].clone(), next[c.index()].take())
            else {
                return Err(missing());
            };
            let number = r.number(c.index()).map_err(|_| missing())?;
            self.heads[c.index()] = Some((r.id(), number));
            self.window.insert(c.index(), r, Arc::new(snapshot));
        }
        if order == 0 {
            self.window.pin_prime_view(zone.clone());
        }
        self.remember_order(zone.id(), order);
        if order <= 1 {
            self.remember_rollup(zone.id(), Arc::new(checked.cross));
        }
        if let Some(step) = checked.prime {
            self.controller = Some(step.into_state());
        }
        Ok((order, accepted))
    }
    /// What the block store keeps of a block that passed every check.
    fn accepted(
        &mut self,
        zone: &Record,
        order: u8,
        h: &Height,
        executed: Option<&block::Executed>,
    ) -> Accepted {
        // The RPC's unlock events and rolling fee window follow the executed
        // blocks (go-quai: each block its state processor applies).
        if let (Some(live), Some(x)) = (&self.live, executed) {
            live.unlocked(
                zone.id(),
                x.execution.unlocks.iter().map(|u| u.address).collect(),
            );
            self.fees.add(&x.execution.tx_fees, x.execution.quai_fees);
            live.node().set_rolling_fees(self.fees.get());
        }
        let location = self.chain.location();
        let txs = zone.object().transactions();
        let tx_hashes: Vec<Hash> = txs
            .iter()
            .map(|t| t.id(Some(location)).unwrap_or_default())
            .collect();
        let receipts = if self.receipts {
            Some(h.zone().receipts.clone())
        } else {
            executed.map(|x| storage_receipts(zone, &tx_hashes, &x.execution, location))
        };
        let uncle_kinds = zone
            .uncles()
            .unwrap_or_default()
            .iter()
            .map(|u| {
                let pow = self
                    .verifier
                    .precompute_inclusion_work(u, &location)
                    .or_else(|_| uncles::pow_hash(&self.caches, u));
                uncles::code(uncles::kind(u, pow))
            })
            .collect();
        let utxos = match (executed, self.state.as_ref()) {
            (Some(x), Some(st)) => {
                let mut created = vec![];
                for (point, _) in &x.execution.created_keys {
                    if let Ok(Some(u)) = st.state.utxos.get(point) {
                        created.push(Created {
                            point: *point,
                            denomination: u.denomination,
                            address: u.address.clone(),
                            lock: u.lock.clone(),
                        });
                    }
                }
                let mut removed: Vec<OutPoint> =
                    x.execution.trimmed.iter().map(|t| t.point).collect();
                for tx in txs {
                    if let Kind::Qi(q) = tx.kind() {
                        removed.extend(q.inputs.iter().map(|i| OutPoint {
                            hash: i.hash,
                            index: i.index,
                        }));
                    }
                }
                Some(UtxoChanges { created, removed })
            }
            _ => None,
        };
        Accepted {
            number: zone.number(2).unwrap_or_default(),
            hash: zone.id(),
            parent: zone.parent(2).unwrap_or_default(),
            order,
            block: h.zone().block.clone(),
            dominant: dominant_views(h),
            receipts,
            tx_hashes,
            uncle_kinds,
            parent_entropy: zone
                .object()
                .header()
                .wire()
                .parent_entropy
                .get(2)
                .map(|v| u256(v)),
            utxos,
        }
    }
    /// The checkpoint block, as the first block of a new store.
    fn base_block(&mut self) -> Result<Accepted, String> {
        let (hash, number) = self.heads[2].ok_or("no zone head")?;
        let zone = self.histories[2]
            .get(&hash)
            .map_err(|e| format!("checkpoint block record: {}", describe(&e)))?;
        let order = *self.orders.get(&hash).ok_or("checkpoint block order")?;
        let location = self.chain.location();
        let tx_hashes = zone
            .object()
            .transactions()
            .iter()
            .map(|t| t.id(Some(location)).unwrap_or_default())
            .collect();
        let uncle_kinds = zone
            .uncles()
            .unwrap_or_default()
            .iter()
            .map(|u| uncles::code(uncles::kind(u, uncles::pow_hash(&self.caches, u))))
            .collect();
        // Its prime and region views, from the window's histories.
        let dominant = (order as usize..2)
            .filter_map(|c| {
                let r = self.histories[c].get(&hash).ok()?;
                Some(DominantView {
                    ctx: c as u8,
                    number: r.number(c).ok()?,
                    block: r.bytes().to_vec(),
                })
            })
            .collect();
        Ok(Accepted {
            number,
            hash,
            parent: zone.parent(2).unwrap_or_default(),
            order,
            block: zone.bytes().to_vec(),
            dominant,
            receipts: None,
            tx_hashes,
            uncle_kinds,
            parent_entropy: zone
                .object()
                .header()
                .wire()
                .parent_entropy
                .get(2)
                .map(|v| u256(v)),
            utxos: self.state.as_ref().map(|_| UtxoChanges::default()),
        })
    }
    /// Every unspent output at the head, to seed a new store's address index.
    fn all_utxos(&self) -> Option<Vec<Created>> {
        let st = self.state.as_ref()?;
        let all = st.state.utxos.materialize().ok()?;
        Some(
            all.iter()
                .map(|(p, u)| Created {
                    point: *p,
                    denomination: u.denomination,
                    address: u.address.clone(),
                    lock: u.lock.clone(),
                })
                .collect(),
        )
    }
}
fn u256(v: &[u8]) -> Hash {
    let mut out = [0u8; 32];
    let take = v.len().min(32);
    out[32 - take..].copy_from_slice(&v[v.len() - take..]);
    out
}
/// go-quai's storage receipts (`ReceiptForStorage`) of an executed block:
/// the consensus receipt, the transaction hash, the gas the transaction used
/// and, for a successful contract creation, the contract address.
fn storage_receipts(
    zone: &Record,
    tx_hashes: &[Hash],
    x: &quai_consensus::zone_execution::Executed,
    location: [u8; 2],
) -> Vec<u8> {
    use prost::Message;
    use quai_primitives::schema::{
        block::{
            ProtoLogForStorage, ProtoLogsForStorage, ProtoReceiptForStorage,
            ProtoReceiptsForStorage, ProtoTransaction, ProtoTransactions,
        },
        common::{ProtoAddress, ProtoHash},
    };
    let txs = zone.object().transactions();
    let number = zone.number(2).unwrap_or_default();
    let mut previous = 0u64;
    let mut out = ProtoReceiptsForStorage::default();
    for (r, &i) in x.receipts.iter().zip(&x.receipt_txs) {
        let tx = &txs[i as usize];
        let gas_used = r.cumulative_gas_used.saturating_sub(previous);
        previous = r.cumulative_gas_used;
        let contract_address = match tx.kind() {
            Kind::Quai(q) if q.to.is_none() && r.status != 0 => tx.sender().ok().and_then(|from| {
                quai_execution::evm::calls::creation_address(
                    from,
                    q.nonce,
                    &q.data,
                    None,
                    number,
                    location,
                    u64::MAX,
                )
                .ok()
                .map(|(a, _)| ProtoAddress { value: a.to_vec() })
            }),
            _ => None,
        };
        out.receipts.push(ProtoReceiptForStorage {
            // go-quai `statusEncoding`: every status but failed is stored
            // as successful (the coinbase lock status 2 included).
            post_state_or_status: if !r.post_state.is_empty() {
                r.post_state.clone()
            } else if r.status == 0 {
                vec![]
            } else {
                vec![1]
            },
            cumulative_gas_used: r.cumulative_gas_used,
            logs: Some(ProtoLogsForStorage {
                logs: r
                    .logs
                    .iter()
                    .map(|l| ProtoLogForStorage {
                        address: Some(ProtoAddress {
                            value: l.address.to_vec(),
                        }),
                        topics: l
                            .topics
                            .iter()
                            .map(|t| ProtoHash { value: t.to_vec() })
                            .collect(),
                        data: l.data.clone(),
                    })
                    .collect(),
            }),
            tx_hash: Some(ProtoHash {
                value: tx_hashes
                    .get(i as usize)
                    .copied()
                    .unwrap_or_default()
                    .to_vec(),
            }),
            contract_address,
            gas_used,
            outbound_etxs: Some(ProtoTransactions {
                transactions: r
                    .outbound_etxs
                    .iter()
                    .filter_map(|e| ProtoTransaction::decode(e.encode().as_slice()).ok())
                    .collect(),
            }),
        });
    }
    out.encode_to_vec()
}

fn fixed<const N: usize>(v: &[u8]) -> Option<[u8; N]> {
    if v.len() > N {
        return None;
    }
    let mut out = [0u8; N];
    out[N - v.len()..].copy_from_slice(v);
    Some(out)
}
/// Rebuild the consensus receipts from go-quai's storage form (QEX1
/// "Receipts": type from the transaction with the same hash, bloom from the
/// logs) and compare their root with the header receipt root.
fn stored_receipts(zone: &Record, bytes: &[u8]) -> Result<(), Fail> {
    use prost::Message;
    use quai_primitives::receipt::{logs_bloom, receipts_root, Log, Receipt};
    use quai_primitives::schema::{self, block::ProtoReceiptsForStorage};
    let c = Context::Zone;
    let bad = |what: &str, e: String| {
        fail(
            c,
            format!("receipts: {what}"),
            "go-quai storage receipts",
            e,
        )
    };
    let stored = if bytes.is_empty() {
        ProtoReceiptsForStorage::default()
    } else {
        schema::preflight(".block.ProtoReceiptsForStorage", bytes).map_err(|e| bad("decode", e))?;
        ProtoReceiptsForStorage::decode(bytes).map_err(|e| bad("decode", e.to_string()))?
    };
    let location = zone
        .header()
        .wire()
        .location
        .as_ref()
        .map(|l| l.value.clone())
        .ok_or_else(|| bad("location", "missing".into()))?;
    let origin: Option<[u8; 2]> = location.as_slice().try_into().ok();
    let mut types = HashMap::new();
    for tx in zone.object().transactions() {
        let id = tx.id(origin).map_err(|e| bad("transaction hash", e))?;
        types.insert(id, tx.type_id());
    }
    let mut receipts = Vec::with_capacity(stored.receipts.len());
    for r in stored.receipts {
        let tx_hash: Hash = r
            .tx_hash
            .as_ref()
            .and_then(|h| fixed(&h.value))
            .ok_or_else(|| bad("transaction hash", "missing or oversized".into()))?;
        let Some(ty) = types.get(&tx_hash).copied() else {
            return Err(fail(
                c,
                "receipts: transaction",
                "a block transaction for each receipt",
                format!("no transaction {}", hex(&tx_hash)),
            ));
        };
        let (post_state, status) = match r.post_state_or_status.as_slice() {
            [] => (vec![], 0),
            [1] => (vec![], 1),
            v if v.len() == 32 => (v.to_vec(), 0),
            v => return Err(bad("status", format!("invalid receipt status {}", hex(v)))),
        };
        let mut logs = vec![];
        for l in r.logs.unwrap_or_default().logs {
            let address = l
                .address
                .as_ref()
                .and_then(|a| fixed::<20>(&a.value))
                .ok_or_else(|| bad("log address", "missing or oversized".into()))?;
            let topics = l
                .topics
                .iter()
                .map(|t| fixed::<32>(&t.value))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| bad("log topic", "oversized".into()))?;
            logs.push(Log {
                address,
                topics,
                data: l.data,
            });
        }
        let outbound_etxs = r
            .outbound_etxs
            .unwrap_or_default()
            .transactions
            .iter()
            .map(|t| Transaction::decode(&t.encode_to_vec()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| bad("outbound ETX", e))?;
        receipts.push(Receipt {
            transaction_type: ty,
            post_state,
            status,
            cumulative_gas_used: r.cumulative_gas_used,
            bloom: logs_bloom(&logs),
            logs,
            outbound_etxs,
        });
    }
    let root = receipts_root(&receipts).map_err(|e| bad("root", e))?;
    let committed = &zone
        .object()
        .header()
        .wire()
        .receipt_hash
        .as_ref()
        .ok_or_else(|| bad("receipt root", "missing".into()))?
        .value;
    if root.as_slice() != committed.as_slice() {
        return Err(fail(
            c,
            "receipts: receipt root",
            hex(committed),
            hex(&root),
        ));
    }
    Ok(())
}
/// The codec's own reason for a refused work object; `Record::decode` maps
/// every codec error to one ancestry-domain message.
fn decode_reason(bytes: &[u8], e: &Error) -> String {
    let schedule = quai_consensus::verifier::PINNED_SCHEDULE;
    let detail = match work::WorkObject::decode_auxiliary(bytes, schedule) {
        Err(s) => Some(s),
        Ok(o) => match o.auxiliary_work_header(schedule) {
            Err(s) => Some(format!("work header: {s}")),
            Ok(h) => h.block_hash().err().map(|s| format!("block hash: {s}")),
        },
    };
    match detail {
        Some(d) => format!("{} ({d})", describe(e)),
        None => describe(e),
    }
}
fn decode(height: Height, seq: u64) -> Slot {
    let records = std::array::from_fn(|c| {
        height.records[c]
            .as_ref()
            .map(|raw| Record::decode(&raw.block))
    });
    Slot {
        seq,
        height,
        records,
        submitted: false,
    }
}

pub(crate) fn io_err(what: &str) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{what}: {e}")
}
/// Checkpoint directory for this chain and mode.
pub fn checkpoint_dir(datadir: &Path, chain: &Chain, state: bool) -> PathBuf {
    store::chain_dir(datadir, &chain.spec().environment, &chain.genesis_hash()).join(if state {
        "state"
    } else {
        "headers"
    })
}
/// A loaded and checked checkpoint, ready to continue.
struct Resumed {
    checkpoint: Checkpoint,
    state: Option<StateReplay>,
    /// Region rollups of a state section (checkpoints without a prime section).
    rollups: Vec<(Hash, Arc<Vec<Transaction>>)>,
}
fn load_checkpoint(
    dir: &Path,
    chain: &Chain,
    state: bool,
    shadow: bool,
) -> Result<Option<Resumed>, String> {
    let list = checkpoint::list(dir).map_err(io_err("checkpoint directory"))?;
    let Some((height, path)) = list.first() else {
        return Ok(None);
    };
    let cp = checkpoint::read(path).map_err(|e| format!("checkpoint {height} rejected: {e}"))?;
    if let (Some((hash, at)), false) = (cp.trusted, shadow) {
        return Err(format!(
            "checkpoint {height} rejected: it continues a shadow run (a test tool) whose bootstrap trusted zone block {} at {at}; that block and its ancestry were not validated by this node. Without --shadow-bootstrap the node starts only from the genesis or from a checkpoint it validated itself: use another data directory",
            hex(&hash)
        ));
    }
    if cp.height != *height
        || cp.environment != chain.spec().environment
        || cp.genesis != chain.genesis_hash()
    {
        return Err(format!(
            "checkpoint {height} rejected: it is for {} {} at height {}",
            cp.environment,
            hex(&cp.genesis),
            cp.height
        ));
    }
    if cp.state.is_none() && state {
        return Err(format!("checkpoint {height} rejected: state mode differs"));
    }
    let (state, rollups) = match &cp.state {
        None => (None, vec![]),
        // A stateless run can continue from a state checkpoint (copied into
        // its headers/ directory): it keeps the region rollups of the state
        // section and leaves the state.
        Some(section) if !state => {
            let mut rollups = vec![];
            for (id, _, rollup) in &section.confirmations {
                if let Some(r) = rollup {
                    let list = store::decode_tx_list(r)
                        .map_err(|e| format!("checkpoint {height} rejected: region rollup: {e}"))?;
                    rollups.push((*id, Arc::new(list)));
                }
            }
            (None, rollups)
        }
        Some(section) => {
            let profile = state::profile_for(chain).map_err(|e| describe(&e))?;
            let commitment = quai_execution::state_store::Commitment {
                roots: quai_execution::state_image::Roots {
                    evm: section.evm,
                    etx: section.etx,
                    utxo: section.utxo,
                    accounts: section.accounts,
                    utxo_count: section.utxo_count,
                },
                digest: section.digest,
            };
            let store = StateStore::open(
                dir,
                store::profile(&chain.spec().environment, &chain.genesis_hash()),
                *height,
                section.version,
                commitment,
            )
            .map_err(|e| format!("checkpoint {height} rejected: {e}"))?;
            let mut past = PastDb::open(&dir.join("past.redb")).map_err(io_err("past.redb"))?;
            past.truncate_above(*height).map_err(io_err("past.redb"))?;
            let (replay, rollups) = StateReplay::resume(
                profile,
                chain.genesis_hash(),
                store,
                past,
                section,
                *height,
                cp.counters.state_blocks,
            )
            .map_err(|e| format!("checkpoint {height} rejected: state: {e}"))?;
            (Some(replay), rollups)
        }
    };
    Ok(Some(Resumed {
        checkpoint: cp,
        state,
        rollups,
    }))
}
/// Keep the two latest checkpoints, their state files and the history that
/// a replay from the older one still reads.
fn prune(dir: &Path, state: Option<&mut StateReplay>) -> Result<(), String> {
    let list = checkpoint::list(dir).map_err(io_err("checkpoint directory"))?;
    let keep: Vec<u64> = list.iter().take(2).map(|c| c.0).collect();
    let Some(&floor) = keep.last() else {
        return Ok(());
    };
    for (_, path) in list.iter().skip(2) {
        std::fs::remove_file(path).map_err(io_err("old checkpoint"))?;
    }
    for entry in std::fs::read_dir(dir).map_err(io_err("checkpoint directory"))? {
        let entry = entry.map_err(io_err("checkpoint directory"))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let old_state = name
            .strip_prefix("state-")
            .and_then(|n| n.strip_suffix(".redb"))
            .and_then(|n| n.parse::<u64>().ok())
            .is_some_and(|h| h < floor && !keep.contains(&h));
        if old_state || name.ends_with(".tmp") {
            std::fs::remove_file(entry.path()).map_err(io_err("old state file"))?;
        }
    }
    if let Some(st) = state {
        st.past
            .prune(
                (floor + 1).saturating_sub(store::incoming_depth()),
                (floor + 1).saturating_sub(store::TRIM_DEPTH),
            )
            .map_err(io_err("past.redb"))?;
    }
    Ok(())
}

/// How the processor runs.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Proof-of-work threads.
    pub jobs: usize,
    /// Time between progress lines.
    pub progress: Duration,
    /// Execute zone blocks and compare the state roots.
    pub state: bool,
    /// Where checkpoints and state go. None writes nothing (stateless only).
    pub datadir: Option<PathBuf>,
    /// Continue from the latest checkpoint, if there is one.
    pub resume: bool,
    /// Zone heights between checkpoints.
    pub checkpoint_every: u64,
    /// Zone heights between state commits (`--state`).
    pub commit_every: u64,
    /// Keep the accepted blocks in the block store (`blockstore.rs`).
    pub store: bool,
    /// Accept a checkpoint of a shadow run (`--shadow-bootstrap`, a test
    /// tool), whose ancestry below a trusted block was not validated here.
    /// Off, the processor starts only from the genesis or a checkpoint whose
    /// whole ancestry it validated.
    pub shadow: bool,
}
/// Staged blocks the store writes in one non-durable transaction.
const STORE_BATCH: usize = 256;
/// Heights between two published heads while the node is behind.
const PUBLISH_EVERY: u64 = 1_000;

/// The per-height pipeline, its ancestry window and its durable state.
pub struct Processor {
    checker: Checker,
    /// The proof-of-work workers; `finish` stops them.
    pool: Option<Pool>,
    dir: Option<PathBuf>,
    settings: Settings,
    summary: Summary,
    started: Instant,
    start: u64,
    ahead: VecDeque<Slot>,
    seq: u64,
    verify_first: bool,
    stop: Arc<AtomicBool>,
    store: Option<BlockStore>,
    /// Tip mode: go-quai's head choice and the branches above the base.
    tip: Option<Tip>,
    /// Canonical blocks the tip keeps above its final base.
    tip_keep: u64,
    /// The RPC view of a running node, which follows the published head.
    live: Option<Arc<crate::rpc_view::Live>>,
    metrics: Option<Arc<crate::metrics::Metrics>>,
}
impl Processor {
    /// Open the processor for `chain`: resume from the latest checkpoint in
    /// the data directory (`settings.resume`), or start at the genesis.
    /// `started` is when the run began (elapsed time and rates count from it).
    pub fn open(
        chain: Arc<Chain>,
        settings: Settings,
        caches: Option<Arc<SharedCaches>>,
        started: Instant,
    ) -> Result<Self, String> {
        if settings.checkpoint_every == 0 || settings.commit_every == 0 {
            return Err("checkpoint and commit intervals must be positive".into());
        }
        if settings.state && settings.datadir.is_none() {
            return Err("--state keeps its state under the data directory".into());
        }
        let dir = settings
            .datadir
            .as_ref()
            .map(|d| checkpoint_dir(d, &chain, settings.state));
        if let Some(dir) = &dir {
            std::fs::create_dir_all(dir).map_err(io_err("checkpoint directory"))?;
        }
        let resumed = match (&dir, settings.resume) {
            (Some(dir), true) => load_checkpoint(dir, &chain, settings.state, settings.shadow)?,
            (Some(dir), false) => {
                let existing = checkpoint::list(dir).map_err(io_err("checkpoint directory"))?;
                if let Some((h, _)) = existing.first() {
                    if settings.state {
                        // State checkpoints are expensive to rebuild: keep them.
                        return Err(format!(
                            "{} holds checkpoint {h}; use --resume, or remove the directory to start again",
                            dir.display()
                        ));
                    }
                    // Header-only checkpoints are cheap: a run without --resume starts over.
                    for (_, path) in existing {
                        std::fs::remove_file(path).map_err(io_err("old checkpoint"))?;
                    }
                }
                None
            }
            (None, _) => None,
        };
        let caches = match caches {
            Some(c) => c,
            None => Arc::new(SharedCaches::new(4).map_err(|e| describe(&e))?),
        };
        let pool = Pool::new(settings.jobs, caches.clone(), chain.location().to_vec());
        let mut summary = Summary {
            environment: chain.spec().environment.clone(),
            last_zone: 0,
            highest_passed: None,
            heights: 0,
            records: [0; 3],
            shares: 0,
            elapsed: Duration::ZERO,
            end: End::Complete,
            failure: None,
            timing: Timing::default(),
            state_blocks: None,
            resumed_from: None,
            run_heights: 0,
            checkpoint: None,
        };
        let genesis = chain.genesis_hash();
        let (checker, start) = match resumed {
            Some(r) => {
                let start = r.checkpoint.height;
                let mut checker = Checker::new(chain, caches, false, r.state);
                checker
                    .restore(&r.checkpoint, r.rollups)
                    .map_err(|e| format!("checkpoint {start} rejected: {e}"))?;
                if let Some(st) = &checker.state {
                    // The roots the checkpoint holds must be the header roots there.
                    let (evm, etx, utxo, size) = checker.head_roots()?;
                    let roots = &st.commitment().roots;
                    if (roots.evm, roots.etx, roots.utxo, roots.accounts) != (evm, etx, utxo, size)
                    {
                        return Err(format!(
                            "checkpoint {start} rejected: its state roots are not the header roots at that height"
                        ));
                    }
                }
                let c = &r.checkpoint.counters;
                summary.heights = c.heights;
                summary.records = c.records;
                summary.shares = c.shares;
                summary.highest_passed = Some(start);
                summary.resumed_from = Some(start);
                summary.checkpoint = Some(start);
                (checker, start)
            }
            None => {
                let state = if settings.state {
                    let dir = dir.as_ref().ok_or("state replay needs a data directory")?;
                    let profile = state::profile_for(&chain)
                        .map_err(|e| format!("state replay profile: {}", describe(&e)))?;
                    let past_path = dir.join("past.redb");
                    if past_path.exists() {
                        std::fs::remove_file(&past_path).map_err(io_err("past.redb"))?;
                    }
                    let past = PastDb::open(&past_path).map_err(io_err("past.redb"))?;
                    let store = StateStore::create_genesis(
                        dir,
                        store::profile(&chain.spec().environment, &genesis),
                        genesis,
                    )
                    .map_err(io_err("state database"))?;
                    Some(
                        StateReplay::create(profile, genesis, store, past)
                            .map_err(io_err("state replay"))?,
                    )
                } else {
                    None
                };
                (Checker::new(chain, caches, false, state), 0)
            }
        };
        let mut checker = checker;
        let store = match (&dir, settings.store) {
            (Some(dir), true) => Some(open_store(
                dir,
                &mut checker,
                start,
                summary.resumed_from.is_some(),
                settings.state,
            )?),
            (None, true) => {
                return Err("--store keeps the block store under the data directory".into())
            }
            _ => None,
        };
        Ok(Self {
            verify_first: summary.resumed_from.is_some(),
            checker,
            pool: Some(pool),
            dir,
            settings,
            summary,
            started,
            start,
            ahead: VecDeque::new(),
            seq: 0,
            stop: Arc::new(AtomicBool::new(false)),
            store,
            tip: None,
            tip_keep: 4 * crate::forkchoice::ZONE_HORIZON,
            live: None,
            metrics: None,
        })
    }
    /// The first zone height a source must deliver: 0, or the checkpoint
    /// height (the checkpoint's block, which is checked, not validated again).
    pub fn start(&self) -> u64 {
        self.start
    }
    pub fn resumed_from(&self) -> Option<u64> {
        self.summary.resumed_from
    }
    /// The zone block a shadow bootstrap trusted, when this run continues one.
    pub fn trusted(&self) -> Option<(Hash, u64)> {
        self.checker.trusted
    }
    pub fn chain(&self) -> &Arc<Chain> {
        &self.checker.chain
    }
    /// The zone head (hash, number), once the genesis or a checkpoint is in.
    pub fn zone_head(&self) -> Option<(Hash, u64)> {
        self.checker.heads[2]
    }
    /// Setting this flag stops [`Processor::run`] after the current height.
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }
    /// Use a stop flag that a signal handler already sets.
    pub fn set_stop_flag(&mut self, stop: Arc<AtomicBool>) {
        self.stop = stop;
    }
    pub fn summary(&self) -> &Summary {
        &self.summary
    }
    /// The head (hash, number) of each context.
    pub fn heads(&self) -> [Option<(Hash, u64)>; 3] {
        self.checker.heads
    }
    pub fn set_metrics(&mut self, m: Arc<crate::metrics::Metrics>) {
        self.metrics = Some(m);
    }
    /// The RPC view of the store and the head state; the processor
    /// publishes its head to it from now on.
    pub fn live_view(
        &mut self,
    ) -> Result<(crate::rpc_view::ExportView, Arc<crate::rpc_view::Live>), String> {
        let store = self
            .store
            .as_mut()
            .ok_or("live view without a block store")?;
        store.flush(false).map_err(io_err("block store"))?;
        let reader = store.reader().map_err(io_err("block store"))?;
        let (hash, number) = self.checker.heads[2].ok_or("live view before genesis")?;
        let head = (number, hash);
        let state = self.head_state()?;
        let (view, live) =
            crate::rpc_view::ExportView::live(self.checker.chain.clone(), reader, head, state)?;
        self.live = Some(live.clone());
        self.checker.live = Some(live.clone());
        Ok((view, live))
    }
    /// The state after the zone head, for the RPC.
    fn head_state(&self) -> Result<Option<Arc<crate::rpc_view::HeadState>>, String> {
        let Some(st) = self.checker.state.as_ref() else {
            return Ok(None);
        };
        let (evm, etx, _, _) = self.checker.head_roots()?;
        Ok(Some(Arc::new(crate::rpc_view::HeadState::new(
            st.state.clone(),
            evm,
            etx,
        ))))
    }
    /// Make the head visible to the RPC: flush the store and publish.
    fn publish(&mut self) -> Result<(), String> {
        let Some(store) = self.store.as_mut() else {
            return Ok(());
        };
        store.flush(false).map_err(io_err("block store"))?;
        let Some(live) = self.live.clone() else {
            return Ok(());
        };
        let Some((hash, number)) = self.checker.heads[2] else {
            return Ok(());
        };
        let head = (number, hash);
        if live.head() == head {
            return Ok(());
        }
        let reader = store.reader().map_err(io_err("block store"))?;
        let state = self.head_state()?;
        live.publish(reader, head, state);
        Ok(())
    }
    /// Check heights from `source` until it ends, a check fails or the stop
    /// flag is set.
    pub fn run(&mut self, source: &mut dyn BlockSource, log: &Log) -> Result<(), String> {
        self.checker.receipts = source.receipts();
        let lookahead = (self.settings.jobs * 16).max(64);
        let mut source_done = false;
        let mut source_error: Option<SourceError> = None;
        let mut last_progress = Instant::now();
        let mut progress_heights = 0u64;
        loop {
            if self.stop.load(Ordering::Relaxed) {
                self.summary.end = End::Stopped;
                break;
            }
            let t = Instant::now();
            while !source_done && self.ahead.len() < lookahead {
                match source.next() {
                    Ok(Next::Height(height)) => self.submit(*height),
                    Ok(Next::Wait) => break,
                    Ok(Next::End) => source_done = true,
                    Err(e) => {
                        source_error = Some(e);
                        source_done = true;
                    }
                }
            }
            self.summary.timing.read_decode += t.elapsed();
            let Some(slot) = self.ahead.pop_front() else {
                if source_done {
                    break;
                }
                // Nothing to check: make the head visible, then wait.
                self.publish()?;
                source.wait(&self.stop);
                continue;
            };
            if self.verify_first {
                // The source must still hold the checkpoint's block at its height.
                self.verify_first = false;
                let zone = slot.height.zone();
                let expected = self.checker.heads[2].map(|h| h.0).unwrap_or_default();
                if slot.height.zone_height != self.start || zone.hash != expected {
                    return Err(format!(
                        "checkpoint {} rejected: the export has {} at zone {}, the checkpoint has {}",
                        self.start,
                        hex(&zone.hash),
                        slot.height.zone_height,
                        hex(&expected)
                    ));
                }
                continue;
            }
            let t = Instant::now();
            let done = if slot.submitted {
                let pool = self.pool.as_mut().ok_or("proof-of-work workers stopped")?;
                Some(pool.wait(slot.seq).map_err(|e| describe(&e))?)
            } else {
                None
            };
            self.summary.timing.wait_pow += t.elapsed();
            let t = Instant::now();
            let zone_height = slot.height.zone_height;
            let raws: [Option<(u64, Hash)>; 3] = std::array::from_fn(|c| {
                slot.height.records[c].as_ref().map(|r| (r.number, r.hash))
            });
            let shares = done.as_ref().map_or(0, |d| d.shares as u64);
            // The appended block for the RPC's chain events.
            let appended = self.live.is_some().then(|| {
                let z = slot.height.zone();
                (z.hash, Arc::new(z.block.clone()))
            });
            let result = if zone_height == 0 {
                let genesis = self.checker.collect.then(|| {
                    genesis_block(&slot, self.checker.receipts, self.checker.state.is_some())
                });
                self.checker.genesis(slot).map(|_| (0, genesis))
            } else {
                self.checker.check(slot, done)
            };
            self.summary.timing.ordered_checks += t.elapsed();
            match result {
                Ok((order, accepted)) => {
                    if let (Some(store), Some(a)) = (self.store.as_mut(), accepted) {
                        store.stage(a).map_err(io_err("block store"))?;
                        if store.pending() >= STORE_BATCH {
                            store.flush(false).map_err(io_err("block store"))?;
                        }
                    }
                    let s = &mut self.summary;
                    s.highest_passed = Some(zone_height);
                    s.heights += 1;
                    s.run_heights += 1;
                    for c in order as usize..3 {
                        s.records[c] += 1;
                    }
                    s.shares += shares;
                    progress_heights += 1;
                    if let Some(m) = &self.metrics {
                        m.head.store(zone_height, Ordering::Relaxed);
                        m.blocks.fetch_add(1, Ordering::Relaxed);
                    }
                    if let (Some(live), Some((hash, bytes))) = (&self.live, appended) {
                        live.appended(hash, zone_height, order, bytes);
                    }
                    if self.live.is_some() && zone_height % PUBLISH_EVERY == 0 {
                        self.publish()?;
                    }
                }
                Err(f) => {
                    let raw = raws[f.context.index()];
                    self.summary.failure = Some(Failure {
                        zone_height: Some(zone_height),
                        context: f.context.name().into(),
                        number: raw.map(|r| r.0),
                        hash: raw.map(|r| r.1),
                        check: f.check,
                        expected: f.expected,
                        actual: f.actual,
                    });
                    self.summary.end = End::Failed;
                    if let Some(m) = &self.metrics {
                        m.failures.fetch_add(1, Ordering::Relaxed);
                    }
                    break;
                }
            }
            if self.dir.is_some() {
                let t = Instant::now();
                if let (Some(st), Some((block, h))) =
                    (self.checker.state.as_mut(), self.checker.heads[2])
                {
                    if h > 0 && h % self.settings.commit_every == 0 {
                        // The store is durable before the state it serves.
                        if let Some(store) = self.store.as_mut() {
                            store.flush(true).map_err(io_err("block store"))?;
                        }
                        st.commit_state(h, block).map_err(io_err("state commit"))?;
                    }
                }
                if zone_height % self.settings.checkpoint_every == 0
                    && self.summary.checkpoint != Some(zone_height)
                {
                    self.summary.checkpoint = Some(self.write_checkpoint(log)?);
                }
                self.summary.timing.ordered_checks += t.elapsed();
            }
            if last_progress.elapsed() >= self.settings.progress {
                let secs = last_progress.elapsed().as_secs_f64();
                let target = source.target().map_or("?".to_string(), |t| t.to_string());
                let s = &self.summary;
                log.info(&format!(
                    "zone height {zone_height}/{target}: {} prime, {} region, {} zone records, {} workshares; {:.1} blocks/s now, {:.1} in this run",
                    s.records[0],
                    s.records[1],
                    s.records[2],
                    s.shares,
                    progress_heights as f64 / secs,
                    s.run_heights as f64 / self.started.elapsed().as_secs_f64(),
                ));
                last_progress = Instant::now();
                progress_heights = 0;
            }
        }
        if self.summary.failure.is_none() {
            if let Some(e) = source_error {
                self.summary.failure = Some(Failure {
                    zone_height: self.summary.highest_passed.map(|h| h + 1),
                    context: e.context,
                    number: None,
                    hash: None,
                    check: e.check,
                    expected: e.expected,
                    actual: e.actual,
                });
                self.summary.end = End::Failed;
            }
        }
        Ok(())
    }
    /// Decode a height and queue its proof-of-work.
    fn submit(&mut self, height: Height) {
        let mut slot = decode(height, self.seq);
        self.seq += 1;
        let resumed_head = self.summary.resumed_from == Some(slot.height.zone_height);
        if slot.height.zone_height > 0 && !resumed_head {
            if let (Some(Ok(zone)), Some(pool)) = (&slot.records[2], &self.pool) {
                pool.submit(Job {
                    seq: slot.seq,
                    header: zone.header().clone(),
                    uncles: zone.uncles().unwrap_or_default(),
                });
                slot.submitted = true;
            }
        }
        self.ahead.push_back(slot);
    }
    /// Stop the workers, write a final checkpoint at the highest height that
    /// passed and return the summary.
    pub fn finish(mut self, log: &Log) -> Result<Summary, String> {
        // Queued work is abandoned; the workers finish their current job.
        self.ahead.clear();
        drop(self.pool.take());
        if let (Some(_), Some(h)) = (&self.dir, self.summary.highest_passed) {
            // In tip mode at the final base, so that a restart never resumes
            // on a branch that a reorg removed.
            let at = self.tip.as_ref().map_or(h, |t| t.base.1);
            if self.summary.checkpoint != Some(at) {
                self.summary.checkpoint = Some(if self.tip.is_some() {
                    self.write_tip_checkpoint(log)?
                } else {
                    self.write_checkpoint(log)?
                });
            }
        }
        if let Some(store) = self.store.as_mut() {
            store.flush(true).map_err(io_err("block store"))?;
        }
        self.publish()?;
        self.summary.state_blocks = self.checker.state.as_ref().map(|s| s.executed);
        self.summary.elapsed = self.started.elapsed();
        Ok(self.summary)
    }
    fn write_checkpoint(&mut self, log: &Log) -> Result<u64, String> {
        let dir = self.dir.clone().ok_or("no checkpoint directory")?;
        // The store is durable before the checkpoint that it must not trail.
        if let Some(store) = self.store.as_mut() {
            store.flush(true).map_err(io_err("block store"))?;
        }
        let checker = &mut self.checker;
        let (block, height) = checker.heads[2].ok_or("checkpoint before genesis")?;
        let started = Instant::now();
        let mut copied = None;
        let state = match checker.state.as_mut() {
            Some(st) => {
                st.commit_state(height, block)
                    .map_err(io_err("state commit"))?;
                copied = Some(
                    st.store
                        .compact_to(height)
                        .map_err(io_err("state compaction"))?,
                );
                st.rebase().map_err(io_err("state reload"))?;
                Some(st.section())
            }
            None => None,
        };
        let counters = checkpoint::Counters {
            heights: self.summary.heights,
            records: self.summary.records,
            shares: self.summary.shares,
            state_blocks: checker.state.as_ref().map_or(0, |s| s.executed),
        };
        let cp = checker.checkpoint(counters, state)?;
        let path = checkpoint::write(&dir, &cp).map_err(io_err("checkpoint write"))?;
        store::fault(&dir, "checkpoint-written", height).map_err(io_err("checkpoint write"))?;
        prune(&dir, checker.state.as_mut())?;
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        log.info(&format!(
            "checkpoint {height} written ({} KB{}) in {:.1}s",
            size / 1024,
            copied.map_or(String::new(), |c| format!(
                "; state compacted: {} nodes, {} KB",
                c.nodes,
                c.bytes / 1024
            )),
            started.elapsed().as_secs_f64()
        ));
        Ok(height)
    }
}

/// The genesis as the store keeps it.
/// The prime and region views a height carries (a coincident block).
fn dominant_views(h: &Height) -> Vec<DominantView> {
    h.records[..2]
        .iter()
        .flatten()
        .map(|r| DominantView {
            ctx: r.context.index() as u8,
            number: r.number,
            block: r.block.clone(),
        })
        .collect()
}
fn genesis_block(slot: &Slot, receipts: bool, state: bool) -> Accepted {
    let zone = slot.height.zone();
    Accepted {
        number: 0,
        hash: zone.hash,
        parent: [0; 32],
        order: 0,
        block: zone.block.clone(),
        dominant: dominant_views(&slot.height),
        // The genesis has no transactions: its receipts are known to be
        // none whenever the node keeps receipts (Go export or execution).
        receipts: (receipts || state).then(|| zone.receipts.clone()),
        tx_hashes: vec![],
        uncle_kinds: vec![],
        parent_entropy: None,
        utxos: state.then(UtxoChanges::default),
    }
}
/// Open the block store of a checkpoint directory and bring it to the
/// processor's start: a fresh start replaces it; a resume rolls it back to
/// the checkpoint, or starts a new store at the checkpoint block.
fn open_store(
    dir: &Path,
    checker: &mut Checker,
    start: u64,
    resumed: bool,
    state: bool,
) -> Result<BlockStore, String> {
    let path = blockstore::path(dir);
    if !resumed && path.exists() {
        std::fs::remove_file(&path).map_err(io_err("old block store"))?;
    }
    let mut store = BlockStore::open(&path, state).map_err(io_err("block store"))?;
    if resumed {
        match store.head() {
            None => {
                store
                    .stage(checker.base_block()?)
                    .map_err(io_err("block store"))?;
                if let Some(all) = checker.all_utxos() {
                    store.seed_utxos(&all).map_err(io_err("block store"))?;
                }
                store.flush(true).map_err(io_err("block store"))?;
            }
            Some((h, _)) if h > start => {
                store.rollback_to(start).map_err(io_err("block store"))?;
            }
            Some((h, _)) if h < start => {
                return Err(format!(
                    "the block store {} ends at zone {h}, below checkpoint {start}; remove it to start a new store at the checkpoint",
                    path.display()
                ))
            }
            _ => {}
        }
        let expected = checker.heads[2].map(|h| h.0);
        let reader = store.reader().map_err(io_err("block store"))?;
        if reader.hash_at(start).map_err(io_err("block store"))? != expected {
            return Err(format!(
                "the block store {} does not hold the checkpoint block at {start}",
                path.display()
            ));
        }
    }
    checker.collect = true;
    Ok(store)
}

// ---------------------------------------------------------------------------
// Tip mode: go-quai's head choice over checked blocks, with reorgs.

/// What the next block is checked against besides the shared window, orders
/// and rollups (both keyed by block hash, so every branch shares them).
#[derive(Clone)]
struct TipContext {
    heads: [Option<(Hash, u64)>; 3],
    controller: prime_controller::State,
}
/// A block of the tip: checked against its parent's context.
struct TipBlock {
    number: u64,
    parent: Hash,
    height: Height,
    zone: Arc<Record>,
    checked: block::Checked,
    after: TipContext,
    /// With state: the state version committed after the block, while it is
    /// canonical. A reorg restores the version of the common ancestor.
    version: Option<state::Version>,
}
/// The head, the blocks above the tip base and go-quai's choice among them.
///
/// The base is final: it is `keep` (4 x 60) canonical blocks below the head,
/// and go-quai never moves the head more than 60 zone blocks back above
/// MaxCodeSizeForkHeight. Checkpoints in tip mode are written at the base,
/// so a restart never resumes on a branch that a reorg removed. With state,
/// every canonical block commits its own state version, and the checkpoint
/// compaction at the base executes the canonical blocks above it again into
/// the compacted file, so every block a reorg can return to keeps its
/// version.
pub struct Tip {
    choice: crate::forkchoice::Choice,
    blocks: HashMap<Hash, TipBlock>,
    /// The canonical blocks above the base, oldest first.
    canonical: Vec<Hash>,
    base: (Hash, u64),
    base_ctx: TipContext,
    /// With state: the state version of the base.
    base_version: Option<state::Version>,
    /// Canonical blocks kept above the base.
    keep: u64,
}
/// What became of an offered block.
#[derive(Debug, PartialEq, Eq)]
pub enum Offer {
    /// The block was already offered (or is the base or below it).
    Known,
    /// Its zone parent is not known yet: offer that first.
    NeedParent(Hash),
    /// It is more than 60 zone blocks above the head (`ErrSubNotSyncedToDom`).
    TooFar,
    /// Its parent is at or below the tip base and is not the base: it forks
    /// below the final part of the chain, which no head choice can leave.
    BelowBase,
    /// A check failed; the block takes no part in the head choice.
    Rejected(Failure),
    /// Checked. `head` is the head after go-quai's choice; `removed` the
    /// canonical blocks a reorg took out, newest first.
    Accepted {
        head: (Hash, u64),
        removed: Vec<(Hash, u64)>,
        refused: bool,
    },
}
impl Tip {
    /// The final block below the canonical tip blocks.
    pub fn base(&self) -> (Hash, u64) {
        self.base
    }
    /// The canonical hash at `number`, from the base to the head.
    pub fn canonical_at(&self, number: u64) -> Option<Hash> {
        if number == self.base.1 {
            return Some(self.base.0);
        }
        self.canonical
            .iter()
            .find(|h| self.blocks.get(*h).is_some_and(|b| b.number == number))
            .copied()
    }
    /// Whether the tip holds the block (the base or a checked block above it).
    pub fn contains(&self, hash: &Hash) -> bool {
        *hash == self.base.0 || self.blocks.contains_key(hash)
    }
    pub fn head(&self) -> (Hash, u64) {
        self.canonical
            .last()
            .and_then(|h| self.blocks.get(h).map(|b| (*h, b.number)))
            .unwrap_or(self.base)
    }
    fn context_of(&self, hash: &Hash) -> Option<&TipContext> {
        if *hash == self.base.0 {
            return Some(&self.base_ctx);
        }
        self.blocks.get(hash).map(|b| &b.after)
    }
    /// The prime, region and zone heads after a block of the tip (the base
    /// or a checked block): where a sync by number starts from that block.
    pub fn heads_after(&self, hash: &Hash) -> Option<[Option<(Hash, u64)>; 3]> {
        self.context_of(hash).map(|c| c.heads)
    }
    fn is_canonical(&self, hash: &Hash) -> bool {
        *hash == self.base.0 || self.canonical.contains(hash)
    }
    pub fn len(&self) -> usize {
        self.blocks.len()
    }
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}
/// go-quai's `TotalLogEntropy` of a block in the context of its order (the
/// backend that sends its `ChainEvent`): the zone backend adds the
/// workshare entropy, the region and prime backends do not.
fn fork_entropy(
    verifier: &mut Verifier,
    history: &History,
    chain: &Chain,
    record: &Record,
) -> Result<(u8, Uint), Error> {
    if record.number(2)? == 0 {
        return Ok((0, Uint::ZERO));
    }
    let zone = quai_consensus::validation::native_parent_metrics(
        verifier,
        history,
        record,
        &chain.zone_profile(),
    )?;
    if zone.order == 2 {
        return Ok((2, zone.total));
    }
    let proof = verifier.verify_header_proof(record.header())?;
    let b = record.object().header().wire();
    let arr = |v: &[Vec<u8>]| -> Result<[Uint; 3], Error> {
        v.iter()
            .map(|b| share_rules::uint(b))
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| Error::Invalid("entropy array"))
    };
    let e = quai_consensus::rules::entropy(&quai_consensus::rules::EntropyInput {
        hash: proof.hash(),
        difficulty: record.difficulty()?,
        expansion: b.expansion_number.unwrap_or(0) as u8,
        parent_entropy: arr(&b.parent_entropy)?,
        parent_delta: arr(&b.parent_delta_entropy)?,
        parent_uncled_delta: arr(&b.parent_uncled_delta_entropy)?,
        uncled_entropy: share_rules::uint(b.uncled_entropy.as_deref().unwrap_or_default())?,
        postfork_share_count: None,
        node_context: zone.order,
    })?;
    Ok((zone.order, e.total))
}
fn failure_of(f: Fail, slot: &Slot) -> Failure {
    let raw = slot.height.records[f.context.index()].as_ref();
    Failure {
        zone_height: Some(slot.height.zone_height),
        context: f.context.name().into(),
        number: raw.map(|r| r.number),
        hash: raw.map(|r| r.hash),
        check: f.check,
        expected: f.expected,
        actual: f.actual,
    }
}
fn records_of(slot: &Slot) -> Result<[Option<Arc<Record>>; 3], Fail> {
    let mut records: [Option<Arc<Record>>; 3] = [None, None, None];
    for c in CONTEXTS {
        let Some(raw) = &slot.height.records[c.index()] else {
            continue;
        };
        let record = match slot.records[c.index()].as_ref() {
            Some(Ok(r)) => r,
            Some(Err(e)) => {
                return Err(fail(
                    c,
                    "decode",
                    "a work object the Rust codec accepts",
                    decode_reason(&raw.block, e),
                ))
            }
            None => continue,
        };
        if record.id() != raw.hash {
            return Err(fail(c, "block hash", hex(&raw.hash), hex(&record.id())));
        }
        let n = record.number(c.index()).unwrap_or(u64::MAX);
        if n != raw.number {
            return Err(fail(
                c,
                "record number",
                raw.number.to_string(),
                n.to_string(),
            ));
        }
        records[c.index()] = Some(Arc::new(record.clone()));
    }
    Ok(records)
}
impl Processor {
    /// Enter tip mode at the current head: from now on blocks come through
    /// [`Processor::offer`] and go-quai's head choice picks the head.
    pub fn enable_tip(&mut self) -> Result<(), String> {
        let c = &mut self.checker;
        let heads = c.heads;
        let controller = c.controller.clone().ok_or("tip mode before genesis")?;
        let base = heads[2].ok_or("tip mode before genesis")?;
        // The seed node set: each context's head with the entropy go-quai's
        // coordinator holds for it after a run from genesis.
        let mut nodes = [crate::forkchoice::Node {
            hash: base.0,
            entropy: Uint::ZERO,
        }; 3];
        for (ctx, node) in nodes.iter_mut().enumerate() {
            let (hash, _) = heads[ctx].ok_or("tip mode without a head")?;
            let record = c.histories[2]
                .get(&hash)
                .or_else(|_| c.histories[ctx].get(&hash))
                .map_err(|e| format!("tip seed record: {}", describe(&e)))?;
            let (_, entropy) = fork_entropy(&mut c.verifier, &c.histories[2], &c.chain, &record)
                .map_err(|e| format!("tip seed entropy: {}", describe(&e)))?;
            *node = crate::forkchoice::Node { hash, entropy };
        }
        let seed = crate::forkchoice::NodeSet { nodes };
        // With state: the base state as a committed version, so that a
        // reorg to the base finds it.
        let base_version = match c.state.as_mut() {
            Some(st) => {
                st.commit_state(base.1, base.0)
                    .map_err(io_err("state commit"))?;
                Some(st.version_point())
            }
            None => None,
        };
        self.tip = Some(Tip {
            choice: crate::forkchoice::Choice::new(seed),
            blocks: HashMap::new(),
            canonical: vec![],
            base,
            base_ctx: TipContext { heads, controller },
            base_version,
            keep: self.tip_keep,
        });
        Ok(())
    }
    /// Canonical blocks the tip keeps above its final base (default 4 x 60).
    /// A smaller value is for tests: it must stay above the reorg depth.
    pub fn set_tip_keep(&mut self, keep: u64) {
        self.tip_keep = keep.max(1);
    }
    pub fn tip(&self) -> Option<&Tip> {
        self.tip.as_ref()
    }
    /// A gossip workshare against the zone window: go-quai's intake rules
    /// (`worker.AddWorkShare`) and the uncle rules that do not depend on an
    /// including block (`validation::verify_workshare`).
    pub fn check_workshare(
        &mut self,
        header: &quai_primitives::Header,
    ) -> Result<quai_consensus::validation::WorkshareReport, Box<Failure>> {
        let c = &mut self.checker;
        let w = header.wire();
        let number = share_rules::number(w.number.as_deref().unwrap_or_default()).ok();
        quai_consensus::validation::verify_workshare(
            &mut c.verifier,
            &c.histories[2],
            header,
            &c.chain.zone_profile(),
        )
        .map_err(|e| {
            Box::new(Failure {
                zone_height: number,
                context: "zone".into(),
                number,
                hash: header.block_hash().ok(),
                check: format!("workshare: {}", block::message(&e)),
                expected: "a valid workshare (go-quai AddWorkShare and the uncle rules)".into(),
                actual: describe(&e),
            })
        })
    }
    /// Check a block of the tip against its parent's context, let go-quai's
    /// head choice decide and move the head, with a reorg when the choice
    /// leaves the canonical chain.
    pub fn offer(&mut self, height: Height, log: &Log) -> Result<Offer, String> {
        let tip = self.tip.as_ref().ok_or("offer outside tip mode")?;
        let zone_raw = height.zone().clone();
        let hash = zone_raw.hash;
        if tip.blocks.contains_key(&hash) || tip.is_canonical(&hash) {
            return Ok(Offer::Known);
        }
        if zone_raw.number <= tip.base.1 {
            return Ok(Offer::Known);
        }
        let slot = decode(height, 0);
        let records = match records_of(&slot) {
            Ok(r) => r,
            Err(f) => return Ok(Offer::Rejected(failure_of(f, &slot))),
        };
        let zone = records[2]
            .clone()
            .ok_or("tip block without a zone record")?;
        let parent = zone.parent(2).unwrap_or_default();
        let number = zone_raw.number;
        let Some(ctx) = tip.context_of(&parent).cloned() else {
            // Its parent would be at or below the base without being it.
            if number <= tip.base.1 + 1 {
                return Ok(Offer::BelowBase);
            }
            return Ok(Offer::NeedParent(parent));
        };
        if !crate::forkchoice::append_allowed(tip.head().1, number) {
            return Ok(Offer::TooFar);
        }
        // The checks that need no state, against the parent's context.
        let c = &mut self.checker;
        let heads = ctx.heads.map(|h| h.unwrap_or_default());
        let checked = {
            let known = state::KnownView {
                orders: &c.orders,
                region_rollups: &c.rollups,
                maps: None,
            };
            let parent_ctx = block::Parent {
                chain: &c.chain,
                histories: &c.histories,
                indexes: &c.indexes,
                heads,
                known: &known,
                controller: &ctx.controller,
                now: unix_now(),
            };
            block::check(&mut c.verifier, &parent_ctx, records)
        };
        let checked = match checked {
            Ok(x) => x,
            Err(f) => {
                let f = Fail {
                    context: CONTEXTS[f.context.min(2)],
                    check: f.check,
                    expected: f.expected,
                    actual: f.actual,
                };
                let failure = failure_of(f, &slot);
                log.warn(&format!(
                    "tip: block {} at {number} rejected: {} {}: expected {}, actual {}",
                    hex(&hash),
                    failure.context,
                    failure.check,
                    failure.expected,
                    failure.actual
                ));
                return Ok(Offer::Rejected(failure));
            }
        };
        let order = checked.order;
        for ctxi in order as usize..3 {
            let r = checked.records[ctxi].clone().ok_or("checked record")?;
            let snapshot = checked.next[ctxi].clone().ok_or("checked snapshot")?;
            c.window.insert(ctxi, r, Arc::new(snapshot));
        }
        if order == 0 {
            c.window.pin_prime_view(zone.clone());
        }
        c.remember_order(hash, order);
        if order <= 1 {
            c.remember_rollup(hash, Arc::new(checked.cross.clone()));
        }
        let mut after = ctx.clone();
        for (ctxi, head) in after.heads.iter_mut().enumerate().skip(order as usize) {
            let r = checked.records[ctxi].as_ref().ok_or("checked record")?;
            *head = Some((r.id(), r.number(ctxi).map_err(|e| describe(&e))?));
        }
        if let Some(step) = &checked.prime {
            after.controller = step.state().clone();
        }
        let (fork_order, entropy) = fork_entropy(&mut c.verifier, &c.histories[2], &c.chain, &zone)
            .map_err(|e| format!("tip entropy of {}: {}", hex(&hash), describe(&e)))?;
        if fork_order != order {
            return Err(format!(
                "tip block {}: entropy order {fork_order} is not its checked order {order}",
                hex(&hash)
            ));
        }
        let parents = [
            zone.parent(0).unwrap_or_default(),
            zone.parent(1).unwrap_or_default(),
            parent,
        ];
        let tip = self.tip.as_mut().ok_or("tip mode")?;
        tip.blocks.insert(
            hash,
            TipBlock {
                number,
                parent,
                height: slot.height,
                zone,
                checked,
                after,
                version: None,
            },
        );
        // Appended (go-quai's ChainEvent), whatever the head choice does.
        if let Some(live) = &self.live {
            live.appended(hash, number, order, Arc::new(zone_raw.block.clone()));
        }
        let tip = self.tip.as_mut().ok_or("tip mode")?;
        let best = tip.choice.build(parents, hash, order, entropy);
        let target = best.map(|b| b.zone()).unwrap_or(tip.head().0);
        self.move_head(target, log)
    }
    /// `SetCurrentHeader`: make `target` the head, rolling the canonical
    /// chain back to the common ancestor first when needed.
    fn move_head(&mut self, target: Hash, log: &Log) -> Result<Offer, String> {
        let tip = self.tip.as_ref().ok_or("tip mode")?;
        let current = tip.head();
        if target == current.0 {
            return Ok(Offer::Accepted {
                head: current,
                removed: vec![],
                refused: false,
            });
        }
        // The new branch back to the canonical chain.
        let mut path = vec![];
        let mut at = target;
        while !tip.is_canonical(&at) {
            let b = tip
                .blocks
                .get(&at)
                .ok_or("tip branch without its ancestors")?;
            path.push(at);
            at = b.parent;
        }
        path.reverse();
        let common = at;
        let common_number = if common == tip.base.0 {
            tip.base.1
        } else {
            tip.blocks.get(&common).map_or(tip.base.1, |b| b.number)
        };
        if !crate::forkchoice::reorg_allowed(current.1, common_number) {
            log.warn(&format!(
                "tip: head {} at {} not taken: common header {common_number} too old (more than {} below {})",
                hex(&target),
                tip.blocks.get(&target).map_or(0, |b| b.number),
                crate::forkchoice::ZONE_HORIZON,
                current.1
            ));
            return Ok(Offer::Accepted {
                head: current,
                removed: vec![],
                refused: true,
            });
        }
        // Roll back to the common ancestor.
        let mut removed = vec![];
        if common != current.0 {
            let tip = self.tip.as_mut().ok_or("tip mode")?;
            let keep = tip
                .canonical
                .iter()
                .position(|h| *h == common)
                .map_or(0, |i| i + 1);
            for h in tip.canonical.drain(keep..).rev() {
                let n = tip.blocks.get(&h).map_or(0, |b| b.number);
                removed.push((h, n));
            }
            let (ctx, version) = if common == tip.base.0 {
                (tip.base_ctx.clone(), tip.base_version.clone())
            } else {
                let b = tip.blocks.get(&common).ok_or("common block")?;
                (b.after.clone(), b.version.clone())
            };
            if let Some(st) = self.checker.state.as_mut() {
                // Every canonical tip block and the base have a committed
                // version: the reorg returns to the common ancestor's.
                let version = version.ok_or("no state version of the common block")?;
                st.restore_version(common_number, &version)
                    .map_err(io_err("state restore"))?;
                st.past
                    .truncate_above(common_number)
                    .map_err(io_err("past.redb"))?;
                st.executed = st.executed.saturating_sub(removed.len() as u64);
            }
            if let Some(store) = self.store.as_mut() {
                // The logs of the removed blocks, for the RPC's filters.
                store.flush(false).map_err(io_err("block store"))?;
                if let Some(live) = &self.live {
                    let reader = store.reader().map_err(io_err("block store"))?;
                    for (h, n) in removed.iter().rev() {
                        let Some(b) = reader.block(h).map_err(io_err("block store"))? else {
                            continue;
                        };
                        let receipts = b
                            .receipts
                            .as_deref()
                            .map(crate::rpc_view::decode_receipts)
                            .transpose()?
                            .unwrap_or_default();
                        let logs: Vec<_> = receipts
                            .into_iter()
                            .enumerate()
                            .flat_map(|(i, r)| {
                                let tx = r.tx_hash;
                                r.logs.into_iter().map(move |l| (tx, i as u64, l))
                            })
                            .collect();
                        if !logs.is_empty() {
                            live.removed(quai_rpc::view::RemovedLogs {
                                block_hash: *h,
                                block_number: *n,
                                logs,
                            });
                        }
                    }
                }
                store
                    .rollback_to(common_number)
                    .map_err(io_err("block store"))?;
            }
            self.checker.heads = ctx.heads;
            self.checker.controller = Some(ctx.controller);
            if let Some(m) = &self.metrics {
                m.reorgs.fetch_add(1, Ordering::Relaxed);
                m.reorged_blocks
                    .fetch_add(removed.len() as u64, Ordering::Relaxed);
            }
            log.info(&format!(
                "tip: reorg to {} at {}: {} blocks removed, common ancestor {common_number}",
                hex(&target),
                self.tip
                    .as_ref()
                    .and_then(|t| t.blocks.get(&target))
                    .map_or(0, |b| b.number),
                removed.len()
            ));
            if let Some(dir) = &self.dir {
                store::fault(dir, "reorg-rolled-back", common_number).map_err(io_err("reorg"))?;
            }
        }
        // Apply the new branch.
        let reorg = !removed.is_empty();
        for h in path {
            let number = self.apply_tip_block(h, log)?;
            if let (Some(dir), true) = (&self.dir, reorg) {
                store::fault(dir, "reorg-applied", number).map_err(io_err("reorg"))?;
            }
        }
        self.publish()?;
        Ok(Offer::Accepted {
            head: self.tip.as_ref().ok_or("tip mode")?.head(),
            removed,
            refused: false,
        })
    }
    /// Make a checked tip block canonical: execute it (with state) and commit
    /// its state version, store it and move the heads. Returns its number.
    fn apply_tip_block(&mut self, hash: Hash, log: &Log) -> Result<u64, String> {
        let tip = self.tip.as_mut().ok_or("tip mode")?;
        let b = tip.blocks.get(&hash).ok_or("tip block")?;
        let parent_ctx = tip
            .context_of(&b.parent)
            .cloned()
            .ok_or("tip parent context")?;
        let zone = b.zone.clone();
        let number = b.number;
        let c = &mut self.checker;
        let executed = match c.state.as_mut() {
            Some(st) => {
                let (maps, mut parent_state) = st.split();
                let known = state::KnownView {
                    orders: &c.orders,
                    region_rollups: &c.rollups,
                    maps: Some(maps),
                };
                let parent = block::Parent {
                    chain: &c.chain,
                    histories: &c.histories,
                    indexes: &c.indexes,
                    heads: parent_ctx.heads.map(|h| h.unwrap_or_default()),
                    known: &known,
                    controller: &parent_ctx.controller,
                    now: unix_now(),
                };
                let b = tip.blocks.get(&hash).ok_or("tip block")?;
                Some(
                    block::execute(&mut c.verifier, &parent, &mut parent_state, &b.checked)
                        .map_err(|f| format!("tip block {} at {number}: {f}", hex(&hash)))?,
                )
            }
            None => None,
        };
        let b = tip.blocks.get(&hash).ok_or("tip block")?;
        let accepted = if c.collect {
            Some(c.accepted(&zone, b.checked.order, &b.height, executed.as_ref()))
        } else {
            None
        };
        if let (Some(st), Some(x)) = (c.state.as_mut(), executed) {
            st.record(&zone, x.confirmed, x.execution.created_keys)
                .map_err(|e| format!("tip state record: {}", describe(&e)))?;
        }
        let after = b.after.clone();
        c.heads = after.heads;
        c.controller = Some(after.controller);
        // Every canonical block commits its state as a version of its own.
        let version = match c.state.as_mut() {
            Some(st) => {
                st.commit_state(number, hash)
                    .map_err(io_err("state commit"))?;
                Some(st.version_point())
            }
            None => None,
        };
        let b = tip.blocks.get_mut(&hash).ok_or("tip block")?;
        b.version = version;
        let order = b.checked.order;
        tip.canonical.push(hash);
        // Keep the tip bounded: the oldest canonical block becomes the final
        // base, and blocks at or below it leave the tree.
        let mut advanced = None;
        if tip.canonical.len() as u64 > tip.keep {
            let old = tip.canonical.remove(0);
            if let Some(b) = tip.blocks.remove(&old) {
                tip.base = (old, b.number);
                tip.base_ctx = b.after;
                tip.base_version = b.version;
                advanced = Some(b.number);
            }
            let floor = tip.base.1;
            tip.blocks.retain(|_, b| b.number > floor);
        }
        if let (Some(store), Some(a)) = (self.store.as_mut(), accepted) {
            store.stage(a).map_err(io_err("block store"))?;
        }
        let s = &mut self.summary;
        s.highest_passed = Some(number);
        s.heights += 1;
        s.run_heights += 1;
        for c in order as usize..3 {
            s.records[c] += 1;
        }
        if let Some(m) = &self.metrics {
            m.head.store(number, Ordering::Relaxed);
            m.blocks.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(base) = advanced {
            if self.dir.is_some()
                && base.is_multiple_of(self.settings.checkpoint_every)
                && self.summary.checkpoint != Some(base)
            {
                self.summary.checkpoint = Some(self.write_tip_checkpoint(log)?);
            }
        }
        Ok(number)
    }
    /// A checkpoint at the final base of the tip. With state, the base
    /// version is compacted into `state-<base>.redb`, and the canonical
    /// blocks above the base are executed again into it, so that each keeps
    /// a version a reorg can return to.
    fn write_tip_checkpoint(&mut self, log: &Log) -> Result<u64, String> {
        let dir = self.dir.clone().ok_or("no checkpoint directory")?;
        let tip = self.tip.as_ref().ok_or("tip mode")?;
        let (_, base) = tip.base;
        let ctx = tip.base_ctx.clone();
        let base_version = tip.base_version.clone();
        if let Some(store) = self.store.as_mut() {
            store.flush(true).map_err(io_err("block store"))?;
        }
        let started = Instant::now();
        let mut copied = None;
        let state = match self.checker.state.as_mut() {
            Some(st) => {
                let version = base_version.ok_or("no state version of the tip base")?;
                st.restore_version(base, &version)
                    .map_err(io_err("state restore"))?;
                copied = Some(
                    st.store
                        .compact_to(base)
                        .map_err(io_err("state compaction"))?,
                );
                st.rebase().map_err(io_err("state reload"))?;
                Some(st.section())
            }
            None => None,
        };
        let counters = checkpoint::Counters {
            heights: self.summary.heights,
            records: self.summary.records,
            shares: self.summary.shares,
            state_blocks: self.checker.state.as_ref().map_or(0, |s| s.executed),
        };
        let cp = self
            .checker
            .checkpoint_at(ctx.heads, &ctx.controller, counters, state)?;
        let path = checkpoint::write(&dir, &cp).map_err(io_err("checkpoint write"))?;
        store::fault(&dir, "checkpoint-written", base).map_err(io_err("checkpoint write"))?;
        prune(&dir, self.checker.state.as_mut())?;
        let again = if self.checker.state.is_some() {
            self.reexecute_tip()?
        } else {
            0
        };
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        log.info(&format!(
            "checkpoint {base} written at the tip base ({} KB{}{}) in {:.1}s",
            size / 1024,
            copied.map_or(String::new(), |c| format!(
                "; state compacted: {} nodes, {} KB",
                c.nodes,
                c.bytes / 1024
            )),
            if again > 0 {
                format!("; {again} canonical blocks above it executed again")
            } else {
                String::new()
            },
            started.elapsed().as_secs_f64()
        ));
        Ok(base)
    }
    /// Execute the canonical blocks above the base again on the base state
    /// and commit a version for each (after a compaction dropped them).
    fn reexecute_tip(&mut self) -> Result<u64, String> {
        let Processor { tip, checker, .. } = self;
        let tip = tip.as_mut().ok_or("tip mode")?;
        let c = checker;
        let mut count = 0;
        for hash in tip.canonical.clone() {
            let b = tip.blocks.get(&hash).ok_or("tip block")?;
            let parent_ctx = tip
                .context_of(&b.parent)
                .cloned()
                .ok_or("tip parent context")?;
            let st = c.state.as_mut().ok_or("no state")?;
            {
                let (maps, mut parent_state) = st.split();
                let known = state::KnownView {
                    orders: &c.orders,
                    region_rollups: &c.rollups,
                    maps: Some(maps),
                };
                let parent = block::Parent {
                    chain: &c.chain,
                    histories: &c.histories,
                    indexes: &c.indexes,
                    heads: parent_ctx.heads.map(|h| h.unwrap_or_default()),
                    known: &known,
                    controller: &parent_ctx.controller,
                    now: unix_now(),
                };
                block::execute(&mut c.verifier, &parent, &mut parent_state, &b.checked)
                    .map_err(|f| format!("tip block {} at {} again: {f}", hex(&hash), b.number))?;
            }
            st.commit_state(b.number, hash)
                .map_err(io_err("state commit"))?;
            let version = st.version_point();
            if let Some(b) = tip.blocks.get_mut(&hash) {
                b.version = Some(version);
            }
            count += 1;
        }
        Ok(count)
    }
}

// ---------------------------------------------------------------------------
// Shadow bootstrap (`quai-node run --stateless --shadow-bootstrap <hash>`,
// a test tool): the ancestry windows, orders, region rollups and prime
// controller state at a trusted zone block, derived from blocks fetched by
// hash links back from it instead of a replay from the genesis. The trusted
// block and its ancestry are not validated; the checkpoint carries the
// trusted block, and a run without the test flag refuses it.

/// What a bootstrap starts from besides the zone heights of its source.
pub struct BootstrapSeed {
    /// The trusted zone block (hash, zone height); the source ends there.
    pub trusted: (Hash, u64),
    /// The prime head at the trusted block (hash, prime number).
    pub prime_head: (Hash, u64),
    /// The prime and region views of the source's first zone block, which
    /// must be a prime block (its zone view is the source's first height).
    pub prime_view: Vec<u8>,
    pub region_view: Vec<u8>,
    /// Header exchange rates of the canonical prime blocks by prime number,
    /// from `prime_head - 2 * WINDOW` to the first prime block: each token
    /// choice row reads the rate WINDOW prime blocks before its block.
    pub rates: std::collections::BTreeMap<u64, Uint>,
}
/// What a bootstrap derived.
#[derive(Clone, Debug, Default)]
pub struct BootstrapReport {
    /// The first zone height (a prime block) and the trusted height.
    pub start: u64,
    pub trusted: u64,
    /// Zone heights adopted and records per context.
    pub heights: u64,
    pub records: [u64; 3],
    /// Region rollups derived and token choice rows computed.
    pub rollups: u64,
    pub choices: u64,
    /// Records per context in the checkpoint.
    pub windows: [usize; 3],
    pub checkpoint: PathBuf,
}
/// The prime controller inputs gathered while the bootstrap adopts heights.
struct Warmup {
    prime_head: (Hash, u64),
    rates: std::collections::BTreeMap<u64, Uint>,
    /// The last prime record: its interlinks come with its child's body
    /// (a prime body carries its parent's interlinks).
    pending: Option<(Arc<Record>, Snapshot)>,
    window: Option<choice_window::Window>,
    parent_window: Option<choice_window::Window>,
    inbound: Option<Vec<Transaction>>,
    choices: u64,
    rollups: u64,
}
fn err(e: Error) -> String {
    describe(&e)
}
impl Checker {
    /// Seed the windows at a prime block from its three views. A
    /// prime-coincident block starts a manifest in the region and zone
    /// contexts and sets the termini it is the origin of; every other termini
    /// entry of slice [0 0] keeps the genesis.
    fn seed(&mut self, views: [Arc<Record>; 3], warm: &mut Warmup) -> Result<(), String> {
        self.install_genesis();
        let id = views[2].id();
        if views.iter().any(|r| r.id() != id) {
            return Err("the seed views are of different blocks".into());
        }
        let genesis = placement::Termini::genesis(self.chain.genesis_hash());
        let mut claim = [0u8; 32];
        let mut snapshots = vec![];
        for (c, r) in views.iter().enumerate() {
            let profile = self.chain.placement_profile(c);
            let location = location_of(r);
            let (prev, termini) = placement::pcrc(
                &genesis,
                id,
                &location,
                &profile.location,
                c > 0,
                claim,
                &profile.genesis,
            )
            .map_err(err)?;
            claim = prev;
            snapshots.push(Snapshot {
                manifest: Some(placement::next_manifest(c, 0, id, Some(&[])).map_err(err)?),
                interlinks: None,
                termini: Some(termini),
            });
        }
        let mut snapshots = snapshots.into_iter();
        let prime = snapshots.next().ok_or("seed snapshot")?;
        // The region and zone views with their snapshots.
        for ((c, view), s) in views.iter().enumerate().skip(1).zip(snapshots) {
            self.window.insert(c, view.clone(), Arc::new(s));
        }
        for (c, view) in views.iter().enumerate() {
            self.heads[c] = Some((id, view.number(c).map_err(err)?));
        }
        warm.pending = Some((views[0].clone(), prime));
        self.remember_order(id, 0);
        self.window.pin_prime_view(views[2].clone());
        let rate = rate_of(&views[0])?;
        warm.rates.insert(views[0].number(0).map_err(err)?, rate);
        Ok(())
    }
    /// One height of the trusted ancestry: the append snapshots, the order,
    /// the region rollup and the prime controller inputs that the checks
    /// derive, with the commitments the headers carry for them (parent
    /// manifest and interlinks, PCRC, ETX rollup root, region manifest), but
    /// without the block rules: these heights are trusted by their hash
    /// links to the trusted block.
    fn adopt(&mut self, slot: &Slot, warm: &mut Warmup) -> Result<u8, String> {
        let records = records_of(slot).map_err(|f| {
            format!(
                "{} {}: expected {}, actual {}",
                f.context.name(),
                f.check,
                f.expected,
                f.actual
            )
        })?;
        let zone = records[2].clone().ok_or("a height without a zone record")?;
        let order = (0..3)
            .find(|c| records[*c].is_some())
            .ok_or("a height without records")? as u8;
        let id = zone.id();
        for (c, head) in self.heads.iter().enumerate().skip(order as usize) {
            let r = records[c].as_ref().ok_or("coincident record")?;
            let (ph, pn) = head.ok_or("no head")?;
            if r.parent(c).map_err(err)? != ph || r.number(c).map_err(err)? != pn + 1 {
                return Err(format!(
                    "{} record {} does not extend the {} head {} at {pn}",
                    CONTEXTS[c].name(),
                    hex(&r.id()),
                    CONTEXTS[c].name(),
                    hex(&ph)
                ));
            }
        }
        if order == 0 {
            // The prime body carries the parent's interlinks.
            let r = records[0].as_ref().ok_or("prime record")?;
            let (pending, mut snapshot) = warm.pending.take().ok_or("no prime parent record")?;
            if pending.id() != r.parent(0).map_err(err)? {
                return Err("prime parent is not the last prime record".into());
            }
            snapshot.interlinks = Some(
                r.object()
                    .interlink_hashes()
                    .try_into()
                    .map_err(|_| "prime body interlinks are not four hashes")?,
            );
            self.window.insert(0, pending, Arc::new(snapshot));
        }
        let mut claim = [0u8; 32];
        let mut next: [Option<Snapshot>; 3] = [None, None, None];
        for c in order as usize..3 {
            let r = records[c].clone().ok_or("coincident record")?;
            let profile = self.chain.placement_profile(c);
            placement::check_append_commitments(&self.indexes[c], &r, c)
                .map_err(|e| format!("{} {}: {}", CONTEXTS[c].name(), hex(&r.id()), err(e)))?;
            let parent = self.indexes[c]
                .get(&r.parent(c).map_err(err)?)
                .map_err(err)?;
            let (prev, termini) = placement::pcrc(
                parent.termini.as_ref().ok_or("parent termini")?,
                r.id(),
                &location_of(&r),
                &profile.location,
                (order as usize) < c,
                claim,
                &profile.genesis,
            )
            .map_err(|e| format!("{} {}: {}", CONTEXTS[c].name(), hex(&r.id()), err(e)))?;
            claim = prev;
            next[c] = Some(Snapshot {
                manifest: Some(
                    placement::next_manifest(c, order, r.id(), parent.manifest.as_deref())
                        .map_err(err)?,
                ),
                interlinks: None,
                termini: Some(termini),
            });
        }
        if order <= 1 {
            // The cross-prime rollup of the zone parent's manifest, as the
            // region Append builds it, checked against the header and the
            // region body.
            let parent = self.indexes[2]
                .get(&zone.parent(2).map_err(err)?)
                .map_err(err)?;
            let manifest = parent.manifest.as_ref().ok_or("zone parent manifest")?;
            let region = records[1].as_ref().ok_or("region record")?;
            if region.object().manifest_hashes() != *manifest {
                return Err(format!(
                    "region record {}: its manifest is not the zone parent's",
                    hex(&id)
                ));
            }
            let mut accumulated = vec![];
            for z in manifest {
                accumulated.extend_from_slice(
                    self.histories[2]
                        .get(z)
                        .map_err(err)?
                        .object()
                        .outbound_etxs(),
                );
            }
            let cross =
                confirmation::cross_prime(&accumulated, self.chain.location()[0]).map_err(err)?;
            let root = work::transactions_root(&cross)?;
            let committed = zone
                .object()
                .header()
                .wire()
                .etx_rollup_hash
                .as_ref()
                .map(|h| h.value.clone())
                .unwrap_or_default();
            if root.as_slice() != committed.as_slice() {
                return Err(format!(
                    "zone {}: ETX rollup root {} is not the header's {}",
                    hex(&id),
                    hex(&root),
                    hex(&committed)
                ));
            }
            self.remember_rollup(id, Arc::new(cross));
            warm.rollups += 1;
        }
        for c in order as usize..3 {
            let r = records[c].clone().ok_or("coincident record")?;
            let snapshot = next[c].take().ok_or("snapshot")?;
            self.heads[c] = Some((r.id(), r.number(c).map_err(err)?));
            if c == 0 {
                warm.pending = Some((r, snapshot));
            } else {
                self.window.insert(c, r, Arc::new(snapshot));
            }
        }
        self.remember_order(id, order);
        if order == 0 {
            self.window.pin_prime_view(zone);
            let prime = records[0].clone().ok_or("prime record")?;
            self.warm_prime(&prime, warm)?;
        }
        Ok(order)
    }
    /// The token choice row and the inbound ETXs of one prime block (the
    /// economic branch of Append), for the last WINDOW + 1 prime blocks. A
    /// row depends only on the block, its parent, its inbound ETXs and the
    /// rate WINDOW prime blocks before it, so WINDOW rows rebuild the whole
    /// set from an empty one.
    fn warm_prime(&mut self, record: &Arc<Record>, warm: &mut Warmup) -> Result<(), String> {
        let n = record.number(0).map_err(err)?;
        warm.rates.insert(n, rate_of(record)?);
        // Up to ControllerKickInBlock no token choice set or inbound list is
        // read (prime_controller::State::before_kick_in).
        if n < warm
            .prime_head
            .1
            .saturating_sub(quai_consensus::controllers::WINDOW)
            || n <= quai_primitives::spec::FORKS.controller_kick_in
        {
            return Ok(());
        }
        let known = state::KnownView {
            orders: &self.orders,
            region_rollups: &self.rollups,
            maps: None,
        };
        let inbound = confirmation::prime_inbound(&self.histories, &known, record)
            .map_err(|e| format!("prime {n} inbound ETXs: {}", err(e)))?;
        let parent = self.histories[0]
            .get(&record.parent(0).map_err(err)?)
            .map_err(err)?;
        let prior = match warm.window.take() {
            Some(w) => w,
            None => choice_window::Window::empty_at(parent.id(), n - 1),
        };
        let window = quai_consensus::controllers::WINDOW;
        let old = if n > window {
            Some(
                *warm
                    .rates
                    .get(&(n - window))
                    .ok_or_else(|| format!("no exchange rate of prime {}", n - window))?,
            )
        } else {
            None
        };
        let prepared = quai_consensus::prime_forward::prepare_component(
            record,
            &parent,
            Some(&inbound),
            &prior,
            old,
            Some(prime_controller::stored_rate(
                record.parent(2).map_err(err)?,
            )),
        )
        .map_err(|e| format!("prime {n} token choices: {}", err(e)))?;
        warm.parent_window = Some(prior);
        warm.window = Some(prepared.window().clone());
        warm.inbound = Some(inbound);
        warm.choices += 1;
        Ok(())
    }
    /// The prime head's interlinks from its proof-of-work rank and the prime
    /// controller state after it.
    fn finish_bootstrap(&mut self, warm: Warmup) -> Result<(), String> {
        let (head, mut snapshot) = warm.pending.ok_or("no prime head record")?;
        if self.heads[0] != Some(warm.prime_head) || head.id() != warm.prime_head.0 {
            return Err(format!(
                "the prime head is {}, the trusted block's is {} at {}",
                self.heads[0].map_or("none".into(), |h| format!("{} at {}", hex(&h.0), h.1)),
                hex(&warm.prime_head.0),
                warm.prime_head.1
            ));
        }
        let profile = self.chain.placement_profile(0);
        let parent_links: [Hash; 4] = head
            .object()
            .interlink_hashes()
            .try_into()
            .map_err(|_| "prime body interlinks are not four hashes")?;
        let rank = placement::verify_placement(&mut self.verifier, &head, &profile)
            .map_err(|e| format!("prime head proof-of-work: {}", err(e)))?
            .rank
            .ok_or("prime head interlink rank")?;
        snapshot.interlinks = Some(
            placement::next_interlinks(
                head.id(),
                Some(&parent_links),
                rank,
                profile.genesis.contains(&head.id()),
            )
            .map_err(err)?,
        );
        // The parent's interlinks by the same rule must be the ones the head's
        // body carries.
        let parent = self.histories[0]
            .get(&head.parent(0).map_err(err)?)
            .map_err(err)?;
        let parent_rank = placement::verify_placement(&mut self.verifier, &parent, &profile)
            .map_err(|e| format!("prime parent proof-of-work: {}", err(e)))?
            .rank
            .ok_or("prime parent interlink rank")?;
        let grand_links: [Hash; 4] = parent
            .object()
            .interlink_hashes()
            .try_into()
            .map_err(|_| "prime body interlinks are not four hashes")?;
        let derived =
            placement::next_interlinks(parent.id(), Some(&grand_links), parent_rank, false)
                .map_err(err)?;
        if derived != parent_links {
            return Err(
                "the prime parent's interlinks by rank are not the ones the head carries".into(),
            );
        }
        self.window.insert(0, head.clone(), Arc::new(snapshot));
        let (hash, number) = warm.prime_head;
        if number < quai_primitives::spec::FORKS.controller_kick_in {
            self.controller = Some(
                prime_controller::State::before_kick_in(hash, number, Some(parent.id()))
                    .map_err(|e| format!("prime controller state: {}", err(e)))?,
            );
            return Ok(());
        }
        let kept = prime_controller::RATES as u64;
        let first = (number + 1).saturating_sub(kept);
        let rates = (first..=number)
            .map(|n| {
                warm.rates
                    .get(&n)
                    .copied()
                    .ok_or_else(|| format!("no exchange rate of prime {n}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let controller = prime_controller::State::from_parts(prime_controller::Parts {
            head: hash,
            number,
            first,
            rates,
            inbound: warm.inbound,
            parent_window: warm.parent_window,
            head_window: warm.window.ok_or("no token choice set")?,
        })
        .map_err(|e| format!("prime controller state: {}", err(e)))?;
        self.controller = Some(controller);
        Ok(())
    }
}
fn location_of(r: &Record) -> Vec<u8> {
    r.header()
        .wire()
        .location
        .as_ref()
        .map(|l| l.value.clone())
        .unwrap_or_default()
}
fn rate_of(r: &Record) -> Result<Uint, String> {
    share_rules::uint(
        r.object()
            .header()
            .wire()
            .exchange_rate
            .as_deref()
            .unwrap_or_default(),
    )
    .map_err(err)
}
/// Build a stateless checkpoint at the trusted block from `source`, whose
/// first height is the prime block of `seed` and whose last is the trusted
/// block, and write it to the headers/ directory of `datadir`. Each height
/// must extend the previous one in every context it is coincident in, so the
/// whole ancestry links to the trusted hash.
pub fn bootstrap_checkpoint(
    chain: Arc<Chain>,
    datadir: &Path,
    seed: BootstrapSeed,
    source: &mut dyn BlockSource,
    stop: &AtomicBool,
    log: &Log,
) -> Result<BootstrapReport, String> {
    let dir = checkpoint_dir(datadir, &chain, false);
    std::fs::create_dir_all(&dir).map_err(io_err("checkpoint directory"))?;
    if let Some((h, _)) = checkpoint::list(&dir)
        .map_err(io_err("checkpoint directory"))?
        .first()
    {
        return Err(format!(
            "{} already holds checkpoint {h}; a bootstrap starts an empty directory",
            dir.display()
        ));
    }
    let caches = Arc::new(SharedCaches::new(2).map_err(err)?);
    let mut c = Checker::new(chain.clone(), caches, false, None);
    c.trusted = Some(seed.trusted);
    let mut warm = Warmup {
        prime_head: seed.prime_head,
        rates: seed.rates,
        pending: None,
        window: None,
        parent_window: None,
        inbound: None,
        choices: 0,
        rollups: 0,
    };
    let prime = Arc::new(Record::decode(&seed.prime_view).map_err(err)?);
    let region = Arc::new(Record::decode(&seed.region_view).map_err(err)?);
    let mut report = BootstrapReport {
        trusted: seed.trusted.1,
        ..Default::default()
    };
    let started = Instant::now();
    let mut last_progress = Instant::now();
    let mut first = true;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        let height = match source.next().map_err(|e| {
            format!(
                "{} {}: expected {}, actual {}",
                e.context, e.check, e.expected, e.actual
            )
        })? {
            Next::Height(h) => *h,
            Next::Wait => {
                source.wait(stop);
                continue;
            }
            Next::End => {
                return Err(format!(
                    "the source ended before the trusted block at {}",
                    seed.trusted.1
                ))
            }
        };
        let z = height.zone_height;
        let slot = decode(height, 0);
        if first {
            first = false;
            let zone = match slot.records[2].as_ref() {
                Some(Ok(r)) => Arc::new(r.clone()),
                _ => return Err(format!("zone height {z}: no zone record")),
            };
            if zone.id() != prime.id() || zone.number(2).map_err(err)? != z {
                return Err(format!(
                    "zone height {z} is {}, not the seed prime block {}",
                    hex(&zone.id()),
                    hex(&prime.id())
                ));
            }
            c.seed([prime.clone(), region.clone(), zone], &mut warm)?;
            report.start = z;
        } else {
            let order = c
                .adopt(&slot, &mut warm)
                .map_err(|e| format!("zone height {z}: {e}"))?;
            for n in report.records.iter_mut().skip(order as usize) {
                *n += 1;
            }
        }
        report.heights += 1;
        if last_progress.elapsed() >= Duration::from_secs(30) {
            last_progress = Instant::now();
            log.info(&format!(
                "bootstrap: zone height {z}/{} adopted ({} prime, {} region records, {} token choice rows, {:.0}s)",
                seed.trusted.1,
                report.records[0],
                report.records[1],
                warm.choices,
                started.elapsed().as_secs_f64()
            ));
        }
        if z >= seed.trusted.1 {
            break;
        }
    }
    if c.heads[2] != Some(seed.trusted) {
        return Err(format!(
            "the fetched ancestry ends at {}, not at the trusted block {} at {}",
            c.heads[2].map_or("none".into(), |h| format!("{} at {}", hex(&h.0), h.1)),
            hex(&seed.trusted.0),
            seed.trusted.1
        ));
    }
    report.rollups = warm.rollups;
    report.choices = warm.choices;
    c.finish_bootstrap(warm)?;
    let cp = c.checkpoint(checkpoint::Counters::default(), None)?;
    report.windows = std::array::from_fn(|i| cp.windows[i].len());
    report.checkpoint = checkpoint::write(&dir, &cp).map_err(io_err("checkpoint write"))?;
    Ok(report)
}

#[cfg(test)]
mod rolling_fee_tests {
    use super::*;
    use quai_consensus::zone_execution::TxFees;

    /// go-quai's `calcTxStats` and `calcRollingFeeInfo`, copied verbatim
    /// from core/state_processor.go at 33db82f2 and run over the same blocks
    /// (Quai fees, and Qi fees in Quai that count but add no Quai fee),
    /// printed these minimum, maximum and average figures after each block.
    #[test]
    fn the_rolling_fee_window_equals_go() {
        let blocks: [&[(bool, u64)]; 7] = [
            &[(true, 21_000_000), (true, 30_000_000), (false, 5_000_000)],
            &[],
            &[(true, 40_000_000)],
            &[(false, 1_000)],
            &[(true, 25_000_000), (true, 25_000_000)],
            &[(true, 100_000_000), (false, 2_000_000), (true, 7)],
            &[(true, 30_000_000)],
        ];
        let go: [[u64; 3]; 7] = [
            [5_000_000, 30_000_000, 17_000_000],
            [5_000_000, 30_000_000, 17_000_000],
            [5_050_000, 40_000_000, 28_500_000],
            [1_000, 39_600_000, 19_000_000],
            [1_010, 39_204_000, 20_500_000],
            [7, 100_000_000, 23_066_667],
            [7, 99_000_000, 24_222_222],
        ];
        let mut r = RollingFees::default();
        assert_eq!(r.get(), None);
        for (block, want) in blocks.iter().zip(go) {
            let mut fees = TxFees::default();
            let mut quai = Uint::ZERO;
            for &(is_quai, fee) in block.iter() {
                fees.note(Uint::from_u64(fee));
                if is_quai {
                    quai = quai.wrapping_add(&Uint::from_u64(fee));
                }
            }
            r.add(&fees, quai);
            assert_eq!(r.get(), Some(want.map(|v| Uint::from_u64(v).to_be_bytes())));
        }
    }
}
