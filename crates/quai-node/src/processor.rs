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
use crate::checkpoint::{self, Checkpoint};
use crate::log::Log;
use crate::pow::{Done, Job, Pool};
use crate::qex::{Context, Height};
use crate::spec::Chain;
use crate::state::{self, StateReplay};
use crate::store::{self, PastDb, StateStore};
use crate::window::Window;
use crypto_bigint::Encoding as _;
use quai_consensus::{
    block, choice_window,
    history::{History, Record},
    placement::{self, CandidateIndex, Snapshot},
    pow::SharedCaches,
    prime_controller,
    rules::Uint,
    share_rules,
    verifier::Verifier,
    Error,
};
use quai_primitives::transaction::Transaction;
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
            verifier: Verifier::with_shared_caches(caches),
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
        }
    }
    fn remember_order(&mut self, id: Hash, order: u8) {
        self.orders.insert(id, order);
        self.order_log.push_back(id);
        if self.order_log.len() > 4096 {
            let old = self.order_log.pop_front().unwrap();
            self.orders.remove(&old);
        }
    }
    fn remember_rollup(&mut self, id: Hash, rollup: Arc<Vec<Transaction>>) {
        if self.rollups.insert(id, rollup).is_none() {
            self.rollup_log.push_back(id);
        }
        if self.rollup_log.len() > state::KEEP {
            let old = self.rollup_log.pop_front().unwrap();
            self.rollups.remove(&old);
        }
    }
    fn genesis(&mut self, slot: Slot) -> Result<(), Fail> {
        let expected = self.chain.genesis_hash();
        for c in CONTEXTS {
            let raw = slot.height.records[c.index()].as_ref().unwrap();
            if raw.hash != expected {
                return Err(fail(c, "genesis hash", hex(&expected), hex(&raw.hash)));
            }
            let record = match slot.records[c.index()].as_ref().unwrap() {
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
        let (block, height) = self.heads[2].ok_or("checkpoint before genesis")?;
        let (windows, views) = self.window.export();
        let windows = windows.map(|w| {
            w.into_iter()
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
            heads: self.heads,
            windows,
            prime_views: views.iter().map(|r| r.bytes().to_vec()).collect(),
            orders: self
                .order_log
                .iter()
                .filter_map(|id| Some((*id, *self.orders.get(id)?)))
                .collect(),
            state,
            prime: Some(self.prime_section()?),
        })
    }
    /// The prime controller state and the recent region rollups.
    fn prime_section(&self) -> Result<checkpoint::PrimeSection, String> {
        let c = self
            .controller
            .as_ref()
            .ok_or("checkpoint without a prime controller state")?
            .parts();
        Ok(checkpoint::PrimeSection {
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
        })
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
        Ok((
            h32(&b.evm_root.as_ref().unwrap().value),
            h32(&b.etx_set_root.as_ref().unwrap().value),
            h32(&b.utxo_root.as_ref().unwrap().value),
            size,
        ))
    }
    fn check(&mut self, slot: Slot, done: Option<Done>) -> Result<u8, Fail> {
        let h = &slot.height;
        // 1. decode, hash and record number
        let mut records: [Option<Arc<Record>>; 3] = [None, None, None];
        for c in CONTEXTS {
            let Some(raw) = &h.records[c.index()] else {
                continue;
            };
            let record = match slot.records[c.index()].as_ref().unwrap() {
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
            let r = checked.records[c.index()].clone().unwrap();
            self.heads[c.index()] = Some((r.id(), r.number(c.index()).unwrap()));
            self.window
                .insert(c.index(), r, Arc::new(next[c.index()].take().unwrap()));
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
        Ok(order)
    }
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
        .unwrap()
        .value
        .clone();
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
        .unwrap()
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
fn load_checkpoint(dir: &Path, chain: &Chain, state: bool) -> Result<Option<Resumed>, String> {
    let list = checkpoint::list(dir).map_err(io_err("checkpoint directory"))?;
    let Some((height, path)) = list.first() else {
        return Ok(None);
    };
    let cp = checkpoint::read(path).map_err(|e| format!("checkpoint {height} rejected: {e}"))?;
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
    if cp.state.is_some() != state {
        return Err(format!("checkpoint {height} rejected: state mode differs"));
    }
    let (state, rollups) = match &cp.state {
        None => (None, vec![]),
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
}

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
            (Some(dir), true) => load_checkpoint(dir, &chain, settings.state)?,
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
                    let dir = dir.as_ref().unwrap();
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
                slot.height.records[c]
                    .as_ref()
                    .map(|r| (r.number, r.hash))
            });
            let shares = done.as_ref().map_or(0, |d| d.shares as u64);
            let result = if zone_height == 0 {
                self.checker.genesis(slot).map(|_| 0)
            } else {
                self.checker.check(slot, done)
            };
            self.summary.timing.ordered_checks += t.elapsed();
            match result {
                Ok(order) => {
                    let s = &mut self.summary;
                    s.highest_passed = Some(zone_height);
                    s.heights += 1;
                    s.run_heights += 1;
                    for c in order as usize..3 {
                        s.records[c] += 1;
                    }
                    s.shares += shares;
                    progress_heights += 1;
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
                    break;
                }
            }
            if self.dir.is_some() {
                let t = Instant::now();
                if let (Some(st), Some((block, h))) =
                    (self.checker.state.as_mut(), self.checker.heads[2])
                {
                    if h > 0 && h % self.settings.commit_every == 0 {
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
                let target = source
                    .target()
                    .map_or("?".to_string(), |t| t.to_string());
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
            if self.summary.checkpoint != Some(h) {
                self.summary.checkpoint = Some(self.write_checkpoint(log)?);
            }
        }
        self.summary.state_blocks = self.checker.state.as_ref().map(|s| s.executed);
        self.summary.elapsed = self.started.elapsed();
        Ok(self.summary)
    }
    fn write_checkpoint(&mut self, log: &Log) -> Result<u64, String> {
        let dir = self.dir.clone().ok_or("no checkpoint directory")?;
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
