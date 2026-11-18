//! `quai-node replay`: stream a QEX1 export and validate every block with
//! `Verifier::validate_block` against the replay's ancestry window, in import
//! order. Proof-of-work runs in parallel (see `pow.rs`) and enters through the
//! verifier memo; everything else runs on one thread, height by height.
//!
//! Per zone height:
//! 1. decode each record and compare its hash and number with the values
//!    go-quai recorded, and the stored receipts with the receipt root;
//! 2. `Verifier::validate_block` (`quai_consensus::block`): coincident
//!    binding, parent hash and number in each context, body commitments, the
//!    block proof-of-work, the computed order against the exported records,
//!    header/inclusion/append rules and PCRC in each coincident context, the
//!    uncled entropy, the prime starting values, the ETX rollup root and the
//!    region/zone rollup agreement; with `--state`, the confirmed ETXs and
//!    the execution of the zone block with every state root.
use crate::checkpoint::{self, Checkpoint};
use crate::log::Log;
use crate::pow::{Done, Job, Pool};
use crate::qex::{self, Context, ExportError, ExportReader, Height, Manifest};
use crate::spec::Chain;
use crate::state::{self, StateReplay};
use crate::store::{self, PastDb, StateStore};
use crate::window::Window;
use quai_consensus::{
    block,
    history::{History, Record},
    placement::{self, CandidateIndex, Snapshot},
    pow::SharedCaches,
    share_rules,
    verifier::Verifier,
    Error,
};
use quai_primitives::{encode_hex, work, Hash};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct Options {
    pub export: PathBuf,
    pub to: Option<u64>,
    pub jobs: usize,
    pub progress: Duration,
    /// Also execute zone blocks from genesis and compare the state roots.
    pub state: bool,
    /// Where checkpoints go (`<datadir>/replay/...`). None writes none.
    pub datadir: Option<PathBuf>,
    /// Continue from the latest checkpoint, if there is one.
    pub resume: bool,
    /// Zone heights between checkpoints.
    pub checkpoint_every: u64,
    /// Zone heights between state commits (`--state`).
    pub commit_every: u64,
}
impl Options {
    pub fn new(export: PathBuf) -> Self {
        Self {
            export,
            to: None,
            jobs: 1,
            progress: Duration::from_secs(10),
            state: false,
            datadir: None,
            resume: false,
            checkpoint_every: 10_000,
            commit_every: 1_000,
        }
    }
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
    now: u64,
    /// The export carries the stored zone receipts.
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
            now: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
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
    fn genesis(&mut self, slot: Slot) -> Result<(), Fail> {
        let expected = self.chain.genesis_hash();
        let mut zone = None;
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
            if c == Context::Zone {
                zone = Some(record.clone());
            }
        }
        drop(zone);
        self.install_genesis();
        self.window
            .pin_prime_view(Arc::new(self.chain.genesis().clone()));
        for c in 0..3 {
            self.heads[c] = Some((expected, 0));
        }
        self.remember_order(expected, 0);
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
        })
    }
    /// Rebuild the ordered checks from a checkpoint.
    fn restore(&mut self, cp: &Checkpoint) -> Result<(), String> {
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
        // prime starting values, rollups and, with --state, execution.
        let heads = self.heads.map(|h| h.expect("genesis checked first"));
        let validated = match self.state.as_mut() {
            Some(st) => {
                let (maps, mut parent_state) = st.split();
                let known = state::KnownView {
                    orders: &self.orders,
                    maps: Some(maps),
                };
                let parent = block::Parent {
                    chain: &self.chain,
                    histories: &self.histories,
                    indexes: &self.indexes,
                    heads,
                    known: &known,
                    now: self.now,
                };
                self.verifier
                    .validate_block(&parent, records, Some(&mut parent_state))
            }
            None => {
                let known = state::KnownView {
                    orders: &self.orders,
                    maps: None,
                };
                let parent = block::Parent {
                    chain: &self.chain,
                    histories: &self.histories,
                    indexes: &self.indexes,
                    heads,
                    known: &known,
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
            st.record(
                &zone,
                order,
                checked.cross.clone(),
                x.confirmed,
                x.execution.created_keys,
            )
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
    use quai_primitives::transaction::Transaction;
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

pub fn run(opts: &Options, log: &Log) -> Result<Summary, String> {
    run_with(opts, log, None)
}
fn io_err(what: &str) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{what}: {e}")
}
/// Checkpoint directory for this chain and mode.
pub fn checkpoint_dir(datadir: &std::path::Path, chain: &Chain, state: bool) -> PathBuf {
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
}
fn load_checkpoint(
    dir: &std::path::Path,
    chain: &Chain,
    state: bool,
) -> Result<Option<Resumed>, String> {
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
    let state = match &cp.state {
        None => None,
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
            Some(
                StateReplay::resume(
                    profile,
                    chain.genesis_hash(),
                    store,
                    past,
                    section,
                    *height,
                    cp.counters.state_blocks,
                )
                .map_err(|e| format!("checkpoint {height} rejected: state: {e}"))?,
            )
        }
    };
    Ok(Some(Resumed {
        checkpoint: cp,
        state,
    }))
}
/// Keep the two latest checkpoints, their state files and the history that
/// a replay from the older one still reads.
fn prune(dir: &std::path::Path, state: Option<&mut StateReplay>) -> Result<(), String> {
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
fn write_checkpoint(
    checker: &mut Checker,
    dir: &std::path::Path,
    summary: &Summary,
    log: &Log,
) -> Result<u64, String> {
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
        heights: summary.heights,
        records: summary.records,
        shares: summary.shares,
        state_blocks: checker.state.as_ref().map_or(0, |s| s.executed),
    };
    let cp = checker.checkpoint(counters, state)?;
    let path = checkpoint::write(dir, &cp).map_err(io_err("checkpoint write"))?;
    store::fault(dir, "checkpoint-written", height).map_err(io_err("checkpoint write"))?;
    prune(dir, checker.state.as_mut())?;
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
/// As `run`, with an epoch cache store that outlives the run (tests reuse it).
pub fn run_with(
    opts: &Options,
    log: &Log,
    caches: Option<Arc<SharedCaches>>,
) -> Result<Summary, String> {
    let started = Instant::now();
    let manifest = Manifest::load(&opts.export).map_err(|e| e.to_string())?;
    let spec = Chain::for_environment(&manifest.environment).map_err(|e| {
        format!(
            "chain spec for environment {:?}: {}",
            manifest.environment,
            describe(&e)
        )
    })?;
    if manifest.genesis_hash != spec.genesis_hash() {
        return Err(format!(
            "manifest genesis_hash {} is not the {} genesis {}",
            hex(&manifest.genesis_hash),
            spec.spec().environment,
            hex(&spec.genesis_hash())
        ));
    }
    if opts.checkpoint_every == 0 || opts.commit_every == 0 {
        return Err("checkpoint and commit intervals must be positive".into());
    }
    if opts.state && opts.datadir.is_none() {
        return Err("--state keeps its state under the data directory".into());
    }
    let dir = opts
        .datadir
        .as_ref()
        .map(|d| checkpoint_dir(d, &spec, opts.state));
    if let Some(dir) = &dir {
        std::fs::create_dir_all(dir).map_err(io_err("checkpoint directory"))?;
    }
    let resumed = match (&dir, opts.resume) {
        (Some(dir), true) => load_checkpoint(dir, &spec, opts.state)?,
        (Some(dir), false) => {
            let existing = checkpoint::list(dir).map_err(io_err("checkpoint directory"))?;
            if let Some((h, _)) = existing.first() {
                if opts.state {
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
    let start = resumed.as_ref().map_or(0, |r| r.checkpoint.height);
    if start > manifest.last_zone {
        return Err(format!(
            "the export ends at zone {}, below checkpoint {start}",
            manifest.last_zone
        ));
    }
    log.info(&format!(
        "replay {}: environment {} (chain {}), genesis {}, {} segments, zone heights {start}..={}{}, {} proof-of-work threads{}",
        opts.export.display(),
        spec.spec().environment,
        spec.spec().chain_id,
        hex(&spec.genesis_hash()),
        manifest.segments.len(),
        manifest.last_zone,
        opts.to.map_or(String::new(), |t| format!(" (stop after {t})")),
        opts.jobs,
        match &resumed {
            Some(r) => format!(", resuming from checkpoint {}", r.checkpoint.height),
            None => String::new(),
        }
    ));
    let last_zone = manifest.last_zone;
    let receipts = manifest.receipts;
    let environment = manifest.environment.clone();
    let mut reader = ExportReader::starting_at(&opts.export, manifest, start, opts.to);
    let seg_log = *log;
    reader.on_segment(Box::new(move |s: &qex::SegmentInfo| {
        seg_log.info(&format!(
            "segment {} (zone {}..={}, {} records): checking sha256",
            s.file, s.first_zone, s.last_zone, s.records
        ))
    }));
    let caches = match caches {
        Some(c) => c,
        None => Arc::new(SharedCaches::new(4).map_err(|e| describe(&e))?),
    };
    let mut pool = Pool::new(opts.jobs, caches.clone(), spec.location().to_vec());
    let genesis = spec.genesis_hash();
    let mut summary = Summary {
        environment,
        last_zone,
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
    let mut checker = match resumed {
        Some(r) => {
            let mut checker = Checker::new(spec, caches, receipts, r.state);
            checker.restore(&r.checkpoint)?;
            if let Some(st) = &checker.state {
                // The roots the checkpoint holds must be the header roots there.
                let (evm, etx, utxo, size) = checker.head_roots()?;
                let roots = &st.commitment().roots;
                if (roots.evm, roots.etx, roots.utxo, roots.accounts) != (evm, etx, utxo, size) {
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
            checker
        }
        None => {
            let state = if opts.state {
                let dir = dir.as_ref().unwrap();
                let profile = state::profile_for(&spec)
                    .map_err(|e| format!("state replay profile: {}", describe(&e)))?;
                let past_path = dir.join("past.redb");
                if past_path.exists() {
                    std::fs::remove_file(&past_path).map_err(io_err("past.redb"))?;
                }
                let past = PastDb::open(&past_path).map_err(io_err("past.redb"))?;
                let store = StateStore::create_genesis(
                    dir,
                    store::profile(&spec.spec().environment, &genesis),
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
            Checker::new(spec, caches, receipts, state)
        }
    };
    let lookahead = (opts.jobs * 16).max(64);
    let mut ahead: VecDeque<Slot> = VecDeque::new();
    let mut reader_done = false;
    let mut reader_error: Option<ExportError> = None;
    let mut seq = 0u64;
    let mut verify_first = summary.resumed_from.is_some();
    let mut last_progress = Instant::now();
    let mut progress_heights = 0u64;
    loop {
        let t = Instant::now();
        while !reader_done && ahead.len() < lookahead {
            match reader.next_height() {
                Ok(Some(height)) => {
                    let mut slot = decode(height, seq);
                    seq += 1;
                    let resumed_head = summary.resumed_from == Some(slot.height.zone_height);
                    if slot.height.zone_height > 0 && !resumed_head {
                        if let Some(Ok(zone)) = &slot.records[2] {
                            pool.submit(Job {
                                seq: slot.seq,
                                header: zone.header().clone(),
                                uncles: zone.uncles().unwrap_or_default(),
                            });
                            slot.submitted = true;
                        }
                    }
                    ahead.push_back(slot);
                }
                Ok(None) => reader_done = true,
                Err(e) => {
                    reader_error = Some(e);
                    reader_done = true;
                }
            }
        }
        summary.timing.read_decode += t.elapsed();
        let Some(slot) = ahead.pop_front() else { break };
        if verify_first {
            // The export must still hold the checkpoint's block at its height.
            verify_first = false;
            let zone = slot.height.zone();
            let expected = checker.heads[2].map(|h| h.0).unwrap_or_default();
            if slot.height.zone_height != start || zone.hash != expected {
                return Err(format!(
                    "checkpoint {start} rejected: the export has {} at zone {}, the checkpoint has {}",
                    hex(&zone.hash),
                    slot.height.zone_height,
                    hex(&expected)
                ));
            }
            continue;
        }
        let t = Instant::now();
        let done = if slot.submitted {
            Some(pool.wait(slot.seq).map_err(|e| describe(&e))?)
        } else {
            None
        };
        summary.timing.wait_pow += t.elapsed();
        let t = Instant::now();
        let zone_height = slot.height.zone_height;
        let raws: [Option<(u64, Hash)>; 3] =
            std::array::from_fn(|c| slot.height.records[c].as_ref().map(|r| (r.number, r.hash)));
        let shares = done.as_ref().map_or(0, |d| d.shares as u64);
        let result = if zone_height == 0 {
            checker.genesis(slot).map(|_| 0)
        } else {
            checker.check(slot, done)
        };
        summary.timing.ordered_checks += t.elapsed();
        match result {
            Ok(order) => {
                summary.highest_passed = Some(zone_height);
                summary.heights += 1;
                summary.run_heights += 1;
                for c in order as usize..3 {
                    summary.records[c] += 1;
                }
                summary.shares += shares;
                progress_heights += 1;
            }
            Err(f) => {
                let raw = raws[f.context.index()];
                summary.failure = Some(Failure {
                    zone_height: Some(zone_height),
                    context: f.context.name().into(),
                    number: raw.map(|r| r.0),
                    hash: raw.map(|r| r.1),
                    check: f.check,
                    expected: f.expected,
                    actual: f.actual,
                });
                summary.end = End::Failed;
                break;
            }
        }
        if let Some(dir) = &dir {
            let t = Instant::now();
            if let (Some(st), Some((block, h))) = (checker.state.as_mut(), checker.heads[2]) {
                if h > 0 && h % opts.commit_every == 0 {
                    st.commit_state(h, block).map_err(io_err("state commit"))?;
                }
            }
            if zone_height % opts.checkpoint_every == 0 && summary.checkpoint != Some(zone_height) {
                summary.checkpoint = Some(write_checkpoint(&mut checker, dir, &summary, log)?);
            }
            summary.timing.ordered_checks += t.elapsed();
        }
        if last_progress.elapsed() >= opts.progress {
            let secs = last_progress.elapsed().as_secs_f64();
            log.info(&format!(
                "zone height {zone_height}/{last_zone}: {} prime, {} region, {} zone records, {} workshares; {:.1} blocks/s now, {:.1} in this run",
                summary.records[0],
                summary.records[1],
                summary.records[2],
                summary.shares,
                progress_heights as f64 / secs,
                summary.run_heights as f64 / started.elapsed().as_secs_f64(),
            ));
            last_progress = Instant::now();
            progress_heights = 0;
        }
    }
    if summary.failure.is_none() {
        if let Some(e) = reader_error {
            summary.failure = Some(Failure {
                zone_height: summary.highest_passed.map(|h| h + 1),
                context: "export".into(),
                number: None,
                hash: None,
                check: "QEX1 container".into(),
                expected: "a well-formed export".into(),
                actual: e.to_string(),
            });
            summary.end = End::Failed;
        } else if opts.to.is_some_and(|to| to < last_zone) {
            summary.end = End::ReachedTo;
        }
    }
    drop(pool);
    // A final checkpoint at the highest height that passed.
    if let (Some(dir), Some(h)) = (&dir, summary.highest_passed) {
        if summary.checkpoint != Some(h) {
            summary.checkpoint = Some(write_checkpoint(&mut checker, dir, &summary, log)?);
        }
    }
    summary.state_blocks = checker.state.as_ref().map(|s| s.executed);
    summary.elapsed = started.elapsed();
    Ok(summary)
}
