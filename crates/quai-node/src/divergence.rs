//! The divergence detector of the shadow run (ROADMAP Phase 2 step 4): the
//! cases where this node and the go-quai peers disagree.
//!
//! - block: a zone block this node rejected, which the peers' canonical
//!   chain holds (asked of several peers once the block is a few blocks
//!   deep). A rejected block the peers do not hold is logged, not counted.
//! - workshare: a gossip workshare this node rejected, which a block that
//!   passed every check later includes as an uncle.
//! - head: this node's canonical block `DEPTH` blocks below its head differs
//!   from the block most answering peers have there, for `PERSIST` or longer.
//! - stall: the peers' chain is `STALL_BLOCKS` or more above this node's
//!   head for `STALL_TIME` or longer.
//!
//! Each one is logged at warn level with the block, the check, the expected
//! and the actual value, appended as one JSON line to
//! `<datadir>/node/divergences.jsonl`, and counted in `/metrics`
//! (`quai_node_divergences_total{kind}`). Every rejection is logged too.
use crate::log::Log;
use crate::metrics::Metrics;
use crate::processor::Failure;
use libp2p::PeerId;
use quai_primitives::{encode_hex, Hash};
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Blocks below the head where the canonical chains are compared: deeper
/// than the reorgs seen at the head (at most 3 in 31 minutes of gossip).
pub const DEPTH: u64 = 5;
/// A head difference counts once it lasts this long.
pub const PERSIST: Duration = Duration::from_secs(60);
/// The peers' chain this far above the head ...
pub const STALL_BLOCKS: u64 = 10;
/// ... for this long counts as a stall.
pub const STALL_TIME: Duration = Duration::from_secs(180);
/// Rejected workshares remembered for the inclusion check.
const SHARES: usize = 8192;

fn hex(h: &[u8]) -> String {
    encode_hex(h)
}
fn unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
/// The hash most answering peers gave, and how many gave it.
pub fn majority(answers: &[(PeerId, Option<Hash>)]) -> Option<(Hash, usize)> {
    let mut tally: HashMap<Hash, usize> = HashMap::new();
    for h in answers.iter().filter_map(|(_, h)| *h) {
        *tally.entry(h).or_default() += 1;
    }
    tally.into_iter().max_by_key(|(_, n)| *n)
}
fn answers_json(answers: &[(PeerId, Option<Hash>)]) -> serde_json::Value {
    answers
        .iter()
        .map(|(p, h)| (p.to_string(), serde_json::json!(h.map(|h| hex(&h)))))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// A block this node rejected, waiting for the peers' answer.
#[derive(Clone, Debug)]
pub struct Rejected {
    pub hash: Hash,
    pub number: u64,
    pub failure: Failure,
    pub at: Instant,
}
struct Episode {
    number: u64,
    ours: Hash,
    theirs: Hash,
    since: Instant,
    counted: bool,
}
pub struct Detector {
    path: Option<PathBuf>,
    metrics: Option<Arc<Metrics>>,
    log: Log,
    pending: VecDeque<Rejected>,
    shares: HashMap<Hash, serde_json::Value>,
    order: VecDeque<Hash>,
    fork: Option<Episode>,
    stall: Option<(Instant, bool)>,
}
impl Detector {
    /// `path`: the JSON lines file (None: logs and metrics only).
    pub fn new(path: Option<PathBuf>, metrics: Option<Arc<Metrics>>, log: Log) -> Self {
        if let Some(dir) = path.as_ref().and_then(|p| p.parent()) {
            let _ = std::fs::create_dir_all(dir);
        }
        Self {
            path,
            metrics,
            log,
            pending: VecDeque::new(),
            shares: HashMap::new(),
            order: VecDeque::new(),
            fork: None,
            stall: None,
        }
    }
    fn count(&self, f: impl Fn(&Metrics) -> &AtomicU64) {
        if let Some(m) = &self.metrics {
            f(m).fetch_add(1, Ordering::Relaxed);
        }
    }
    /// Count, log and append one divergence.
    fn diverged(&self, kind: &str, detail: serde_json::Value) {
        match kind {
            "block" => self.count(|m| &m.divergences_block),
            "workshare" => self.count(|m| &m.divergences_workshare),
            "head" => self.count(|m| &m.divergences_head),
            _ => self.count(|m| &m.divergences_stall),
        }
        let line = serde_json::json!({"utc": unix(), "divergence": kind, "detail": detail});
        self.log.warn(&format!("DIVERGENCE {kind}: {line}"));
        if let Some(path) = &self.path {
            let written = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| writeln!(f, "{line}"));
            if let Err(e) = written {
                self.log
                    .error(&format!("divergence log {}: {e}", path.display()));
            }
        }
    }
    /// A block this node rejected: logged now, checked against the peers
    /// once it is buried.
    pub fn block_rejected(&mut self, hash: Hash, number: u64, failure: Failure) {
        self.count(|m| &m.blocks_rejected);
        if failure.actual.starts_with("unavailable") {
            self.count(|m| &m.checks_unavailable);
        }
        self.log.warn(&format!(
            "rejected zone block {} at {number}: {} {}: expected {}, actual {}",
            hex(&hash),
            failure.context,
            failure.check,
            failure.expected,
            failure.actual
        ));
        if self.pending.iter().all(|r| r.hash != hash) {
            self.pending.push_back(Rejected {
                hash,
                number,
                failure,
                at: Instant::now(),
            });
        }
    }
    /// Rejected blocks to ask the peers about: a minute old or `DEPTH`
    /// below the head.
    pub fn due(&mut self, head: u64) -> Vec<Rejected> {
        let (due, keep): (Vec<_>, Vec<_>) = self
            .pending
            .drain(..)
            .partition(|r| r.at.elapsed() >= PERSIST || head >= r.number + DEPTH);
        self.pending = keep.into();
        due
    }
    /// The peers' canonical hashes at a rejected block's number.
    pub fn block_verdict(&mut self, r: Rejected, answers: &[(PeerId, Option<Hash>)]) {
        let detail = serde_json::json!({
            "block": hex(&r.hash),
            "number": r.number,
            "context": r.failure.context,
            "check": r.failure.check,
            "expected": r.failure.expected,
            "actual": r.failure.actual,
            "peers": answers_json(answers),
        });
        match majority(answers) {
            Some((h, n)) if h == r.hash && n >= 2 => self.diverged("block", detail),
            Some(_) => self.log.info(&format!(
                "rejected zone block {} at {} is not the peers' canonical block there: {}",
                hex(&r.hash),
                r.number,
                answers_json(answers)
            )),
            None => {
                // Nobody answered: ask again later.
                self.pending.push_back(Rejected {
                    at: Instant::now(),
                    ..r
                })
            }
        }
    }
    /// A gossip workshare this node rejected.
    pub fn share_rejected(&mut self, hash: Hash, number: u64, failure: &Failure) {
        self.count(|m| &m.workshares_rejected);
        if failure.actual.starts_with("unavailable") {
            self.count(|m| &m.checks_unavailable);
        }
        let detail = serde_json::json!({
            "workshare": hex(&hash),
            "number": number,
            "check": failure.check,
            "expected": failure.expected,
            "actual": failure.actual,
        });
        self.log.warn(&format!("rejected workshare: {detail}"));
        if self.shares.insert(hash, detail).is_none() {
            self.order.push_back(hash);
            while self.order.len() > SHARES {
                if let Some(old) = self.order.pop_front() {
                    self.shares.remove(&old);
                }
            }
        }
    }
    /// A workshare that passed (with or without the parent rules).
    pub fn count_share(&self, parent_checked: bool) {
        if parent_checked {
            self.count(|m| &m.workshares_valid);
        } else {
            self.count(|m| &m.workshares_parent_unknown);
        }
    }
    /// A workshare older than go-quai takes (ignored, as go-quai does).
    pub fn count_old(&self) {
        self.count(|m| &m.workshares_old);
    }
    /// A block that passed every check includes these uncles.
    pub fn block_included(&mut self, block: Hash, number: u64, uncles: &[Hash]) {
        for u in uncles {
            if let Some(detail) = self.shares.remove(u) {
                self.diverged(
                    "workshare",
                    serde_json::json!({
                        "included_in": hex(&block),
                        "number": number,
                        "rejected": detail,
                    }),
                );
            }
        }
    }
    /// The peers' canonical hashes at `number`, where this node has `ours`.
    pub fn head_verdict(&mut self, number: u64, ours: Hash, answers: &[(PeerId, Option<Hash>)]) {
        let Some((theirs, n)) = majority(answers) else {
            return;
        };
        if n < 2 {
            return;
        }
        self.count(|m| &m.head_checks);
        if theirs == ours {
            self.count(|m| &m.head_agreements);
            if let Some(e) = self.fork.take() {
                self.log.info(&format!(
                    "head: the canonical chain at {} agrees with the peers again after {}s",
                    e.number,
                    e.since.elapsed().as_secs()
                ));
            }
            return;
        }
        let e = self.fork.get_or_insert(Episode {
            number,
            ours,
            theirs,
            since: Instant::now(),
            counted: false,
        });
        if !e.counted && e.since.elapsed() >= PERSIST {
            e.counted = true;
            let detail = serde_json::json!({
                "number": number,
                "first_number": e.number,
                "ours": hex(&ours),
                "peers_majority": hex(&theirs),
                "first_ours": hex(&e.ours),
                "first_peers_majority": hex(&e.theirs),
                "seconds": e.since.elapsed().as_secs(),
                "depth": DEPTH,
                "peers": answers_json(answers),
            });
            self.diverged("head", detail);
        } else if !e.counted {
            self.log.warn(&format!(
                "head: canonical block at {number} is {}, the peers' is {} ({n} peers); counted if it lasts {}s",
                hex(&ours),
                hex(&theirs),
                PERSIST.as_secs()
            ));
        }
    }
    /// Whether the peers hold a block `STALL_BLOCKS` above the head.
    pub fn lag_verdict(&mut self, head: u64, answers: &[(PeerId, Option<Hash>)]) {
        let ahead = answers.iter().filter(|(_, h)| h.is_some()).count();
        if ahead < 2 {
            if let Some((since, _)) = self.stall.take() {
                self.log.info(&format!(
                    "head: caught up with the peers after {}s",
                    since.elapsed().as_secs()
                ));
            }
            return;
        }
        let (since, counted) = self.stall.get_or_insert((Instant::now(), false));
        if !*counted && since.elapsed() >= STALL_TIME {
            *counted = true;
            let detail = serde_json::json!({
                "head": head,
                "peers_with_block_at": head + STALL_BLOCKS,
                "seconds": since.elapsed().as_secs(),
                "peers": answers_json(answers),
            });
            self.diverged("stall", detail);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn failure() -> Failure {
        Failure {
            zone_height: Some(7),
            context: "zone".into(),
            number: Some(7),
            hash: Some([7; 32]),
            check: "zone rules: header difficulty".into(),
            expected: "1".into(),
            actual: "2".into(),
        }
    }
    #[test]
    fn counts_each_kind_once_and_writes_json_lines() {
        let dir = std::env::temp_dir().join(format!("quai-node-divergence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("divergences.jsonl");
        let m = Arc::new(Metrics::new(0));
        let mut d = Detector::new(Some(path.clone()), Some(m.clone()), Log::new(crate::log::Level::Error));
        let peers: Vec<PeerId> = (0..3).map(|_| PeerId::random()).collect();
        // A rejected block that two of three peers hold: one block divergence.
        d.block_rejected([7; 32], 7, failure());
        let due = d.due(7 + DEPTH);
        assert_eq!(due.len(), 1);
        let answers = vec![(peers[0], Some([7; 32])), (peers[1], Some([7; 32])), (peers[2], None)];
        d.block_verdict(due[0].clone(), &answers);
        assert_eq!(m.divergences_block.load(Ordering::Relaxed), 1);
        // One the peers do not hold: no divergence.
        d.block_rejected([8; 32], 8, failure());
        let due = d.due(8 + DEPTH);
        d.block_verdict(due[0].clone(), &[(peers[0], Some([9; 32])), (peers[1], Some([9; 32]))]);
        assert_eq!(m.divergences_block.load(Ordering::Relaxed), 1);
        assert_eq!(m.blocks_rejected.load(Ordering::Relaxed), 2);
        // A rejected workshare that a valid block includes.
        d.share_rejected([5; 32], 6, &failure());
        d.block_included([6; 32], 9, &[[4; 32], [5; 32]]);
        d.block_included([6; 32], 9, &[[5; 32]]);
        assert_eq!(m.divergences_workshare.load(Ordering::Relaxed), 1);
        // Agreement, then a difference that must last PERSIST to count.
        d.head_verdict(20, [1; 32], &[(peers[0], Some([1; 32])), (peers[1], Some([1; 32]))]);
        d.head_verdict(21, [1; 32], &[(peers[0], Some([2; 32])), (peers[1], Some([2; 32]))]);
        assert_eq!(m.divergences_head.load(Ordering::Relaxed), 0);
        d.fork.as_mut().unwrap().since -= PERSIST;
        d.head_verdict(22, [1; 32], &[(peers[0], Some([2; 32])), (peers[1], Some([2; 32]))]);
        d.head_verdict(23, [1; 32], &[(peers[0], Some([2; 32])), (peers[1], Some([2; 32]))]);
        assert_eq!(m.divergences_head.load(Ordering::Relaxed), 1);
        assert_eq!(m.head_checks.load(Ordering::Relaxed), 4);
        assert_eq!(m.head_agreements.load(Ordering::Relaxed), 1);
        let lines = std::fs::read_to_string(&path).unwrap();
        assert_eq!(lines.lines().count(), 3);
        for l in lines.lines() {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            assert!(v["divergence"].is_string() && v["detail"].is_object());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
