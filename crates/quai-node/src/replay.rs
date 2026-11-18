//! `quai-node replay`: stream a QEX1 export and run every consensus check that
//! needs no execution state, in import order. Proof-of-work runs in parallel
//! (see `pow.rs`); everything else runs on one thread, height by height.
//!
//! Per zone height the checks are, in order:
//! 1. decode each record and compare its hash with the hash go-quai recorded;
//! 2. parent hash and number in each context;
//! 3. body commitments (header hash, transactions, outbound ETXs, uncles,
//!    manifest, interlinks);
//! 4. the block proof-of-work;
//! 5. the computed order must match the exported prime/region records;
//! 6. `placement::prepare_append_components` in each coincident context:
//!    header rules, workshare inclusion, parent manifest/interlink commitments
//!    and PCRC;
//! 7. pre-KAWPOW uncled entropy, the ETX rollup root and the region/zone
//!    rollup agreement.
use crate::log::Log;
use crate::pow::{Done, Job, Pool};
use crate::qex::{self, Context, ExportError, ExportReader, Height, Manifest};
use crate::spec::ChainSpec;
use crate::window::Window;
use crypto_bigint::Encoding as _;
use quai_consensus::{
    confirmation::cross_prime,
    economics,
    history::{History, Record},
    placement::{self, CandidateIndex, Snapshot},
    pow::SharedCaches,
    prefork_shares, rules, share_rules, validation,
    verifier::Verifier,
    zone_header, Error,
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
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
    pub read_decode: Duration,
    pub wait_pow: Duration,
    pub ordered_checks: Duration,
}
impl Summary {
    pub fn rate(&self) -> f64 {
        let s = self.elapsed.as_secs_f64();
        if s > 0.0 {
            self.heights as f64 / s
        } else {
            0.0
        }
    }
    /// The single result line.
    pub fn line(&self) -> String {
        let passed = self
            .highest_passed
            .map_or("none".to_string(), |h| h.to_string());
        let stats = format!(
            "{} heights, {} prime, {} region, {} zone records, {} workshares, {:.1} blocks/s, {:.1}s",
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
    match e {
        Error::Invalid(m) => format!("invalid: {m}"),
        Error::Unsupported(m) => format!("unsupported: {m}"),
        Error::ResourceLimit(m) => format!("resource limit: {m}"),
        Error::Unavailable(m) => format!("unavailable: {m}"),
    }
}
fn message(e: &Error) -> &'static str {
    match e {
        Error::Invalid(m)
        | Error::Unsupported(m)
        | Error::ResourceLimit(m)
        | Error::Unavailable(m) => m,
    }
}
fn hex(h: &[u8]) -> String {
    encode_hex(h)
}
fn uint_hex(bytes: &[u8]) -> String {
    match share_rules::uint(bytes) {
        Ok(v) => uint(v),
        Err(_) => hex(bytes),
    }
}
fn uint(v: rules::Uint) -> String {
    // Decimal for values that fit u128, hex otherwise.
    let b = v.to_be_bytes();
    if b[..16].iter().all(|x| *x == 0) {
        u128::from_be_bytes(b[16..].try_into().unwrap()).to_string()
    } else {
        hex(&b)
    }
}

/// A decoded zone height waiting for its proof-of-work result.
struct Slot {
    seq: u64,
    height: Height,
    records: [Option<Result<Record, Error>>; 3],
    submitted: bool,
}

struct Checker {
    spec: ChainSpec,
    profiles: [placement::Profile; 3],
    zone_profile: validation::Profile,
    verifier: Verifier,
    window: Arc<Window>,
    histories: [History; 3],
    indexes: [CandidateIndex; 3],
    heads: [Option<(Hash, u64)>; 3],
    orders: HashMap<Hash, u8>,
    order_log: VecDeque<Hash>,
    now: u64,
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
fn wo_context(c: Context) -> work::Context {
    match c {
        Context::Prime => work::Context::Prime,
        Context::Region => work::Context::Region,
        Context::Zone => work::Context::Zone,
    }
}
fn h32(v: &[u8]) -> Hash {
    v.try_into().unwrap_or([0; 32])
}

impl Checker {
    fn new(spec: ChainSpec, caches: Arc<SharedCaches>) -> Self {
        let window = Window::new();
        let profiles = std::array::from_fn(|c| spec.profile(c));
        let zone_profile = validation::Profile {
            location: spec.location.to_vec(),
            minimum_difficulty: spec.minimum_difficulty,
            duration: spec.duration,
            gas_ceiling: spec.gas_ceiling,
            p2p_threshold: spec.p2p_threshold,
            genesis: [spec.genesis_hash()].into(),
            default_genesis: spec.default_genesis,
        };
        Self {
            histories: std::array::from_fn(|c| History::with_reader(window.history_reader(c))),
            indexes: std::array::from_fn(|c| CandidateIndex::with_reader(window.index_reader(c))),
            profiles,
            zone_profile,
            verifier: Verifier::with_shared_caches(caches),
            window,
            heads: [None; 3],
            orders: HashMap::new(),
            order_log: VecDeque::new(),
            now: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            spec,
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
        let expected = self.spec.genesis_hash();
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
        let zone = Arc::new(zone.unwrap());
        let snapshots: [Arc<Snapshot>; 3] = std::array::from_fn(|c| {
            Arc::new(
                placement::bootstrap_candidate(&zone, &self.profiles[c])
                    .expect("trusted genesis snapshot"),
            )
        });
        self.window.set_genesis(zone.clone(), snapshots);
        self.window.pin_prime_view(zone.clone());
        for c in 0..3 {
            self.heads[c] = Some((expected, 0));
        }
        self.remember_order(expected, 0);
        Ok(())
    }
    fn body(record: &Record, c: Context) -> Result<(), Fail> {
        let o = record.object();
        let b = o.header().wire();
        let committed = h32(&record.header().wire().header_hash.as_ref().unwrap().value);
        if committed != o.header().hash() {
            return Err(fail(
                c,
                "body: header hash",
                hex(&committed),
                hex(&o.header().hash()),
            ));
        }
        let empty = quai_primitives::trie::ordered_root(&[]).unwrap();
        let root = |r: Result<Hash, String>, what: &str| -> Result<Hash, Fail> {
            r.map_err(|e| {
                fail(
                    c,
                    format!("body: {what}"),
                    "a root within the codec bounds",
                    e,
                )
            })
        };
        match c {
            Context::Zone => {
                let n = o.manifest_hashes().len();
                if n != 0 {
                    return Err(fail(
                        c,
                        "body: zone manifest",
                        "0 hashes",
                        format!("{n} hashes"),
                    ));
                }
                let links = root(o.interlink_root(), "zone interlinks")?;
                if links != empty {
                    return Err(fail(c, "body: zone interlinks", hex(&empty), hex(&links)));
                }
                let tx = root(o.transaction_root(), "transaction root")?;
                if tx != h32(&b.tx_hash.as_ref().unwrap().value) {
                    return Err(fail(
                        c,
                        "body: transaction root",
                        hex(&b.tx_hash.as_ref().unwrap().value),
                        hex(&tx),
                    ));
                }
                let etx = root(o.outbound_root(), "outbound ETX root")?;
                if etx != h32(&b.outbound_etx_hash.as_ref().unwrap().value) {
                    return Err(fail(
                        c,
                        "body: outbound ETX root",
                        hex(&b.outbound_etx_hash.as_ref().unwrap().value),
                        hex(&etx),
                    ));
                }
                let uncles = o.uncle_root();
                if uncles != h32(&b.uncle_hash.as_ref().unwrap().value) {
                    return Err(fail(
                        c,
                        "body: uncle root",
                        hex(&b.uncle_hash.as_ref().unwrap().value),
                        hex(&uncles),
                    ));
                }
            }
            Context::Prime | Context::Region => {
                let txs = o.transactions().len();
                let etxs = o.outbound_etxs().len();
                let uncles = record.uncles().map(|u| u.len()).unwrap_or(usize::MAX);
                if txs != 0 || etxs != 0 || uncles != 0 {
                    return Err(fail(
                        c,
                        "body: dominant body contents",
                        "no transactions, outbound ETXs or uncles",
                        format!("{txs} transactions, {etxs} outbound ETXs, {uncles} uncles"),
                    ));
                }
                let manifest = root(o.manifest_root(), "manifest root")?;
                let committed = &b.manifest_hash[c.index() + 1].value;
                if manifest == empty || manifest.as_slice() != committed.as_slice() {
                    return Err(fail(
                        c,
                        "body: subordinate manifest root",
                        hex(committed),
                        hex(&manifest),
                    ));
                }
                if c == Context::Prime {
                    let links = root(o.interlink_root(), "interlink root")?;
                    let committed = &b.interlink_root_hash.as_ref().unwrap().value;
                    if links.as_slice() != committed.as_slice() {
                        return Err(fail(c, "body: interlink root", hex(committed), hex(&links)));
                    }
                }
            }
        }
        // The library check must agree with the explicit one above.
        o.verify_commitments(wo_context(c))
            .map_err(|e| fail(c, "body: commitments", "commitments hold", e))
    }
    /// Expected and actual values for the zone header rules that have a
    /// public calculation; other rules report the refusal text.
    fn explain(&mut self, c: Context, record: &Record, e: &Error) -> (String, String) {
        let fallback = ("the rule holds".to_string(), describe(e));
        if c != Context::Zone {
            return fallback;
        }
        let Ok(parent) = self.histories[2].get(&record.parent(2).unwrap_or([0; 32])) else {
            return fallback;
        };
        let b = record.object().header().wire();
        let pb = parent.object().header().wire();
        let w = record.header().wire();
        let p = &self.zone_profile;
        let parent_number = if p.genesis.contains(&parent.id()) {
            0
        } else {
            parent.number(2).unwrap_or(0)
        };
        match message(e) {
            "header difficulty" => {
                match validation::expected_difficulty(&self.histories[2], &parent, p) {
                    Ok(v) => (
                        uint(v),
                        uint_hex(w.difficulty.as_deref().unwrap_or_default()),
                    ),
                    Err(x) => ("difficulty".into(), describe(&x)),
                }
            }
            "gas limit schedule" | "gas use/limit" => (
                format!(
                    "gas_limit {} and gas_used <= gas_limit",
                    rules::next_limit(
                        parent.number(2).unwrap_or(0),
                        pb.gas_limit.unwrap_or(0),
                        p.gas_ceiling
                    )
                ),
                format!(
                    "gas_limit {} gas_used {}",
                    b.gas_limit.unwrap_or(0),
                    b.gas_used.unwrap_or(0)
                ),
            ),
            "state limit schedule" => (
                format!(
                    "state_limit {} and state_used <= state_limit",
                    rules::next_limit(
                        parent.number(2).unwrap_or(0),
                        pb.state_limit.unwrap_or(0),
                        50_000_000
                    )
                ),
                format!(
                    "state_limit {} state_used {}",
                    b.state_limit.unwrap_or(0),
                    b.state_used.unwrap_or(0)
                ),
            ),
            "base fee" => match economics::expected_base_fee(&self.histories[2], &parent, p) {
                Ok(v) => (uint(v), uint_hex(b.base_fee.as_deref().unwrap_or_default())),
                Err(x) => ("base fee".into(), describe(&x)),
            },
            "header number" => (
                (parent_number + 1).to_string(),
                record.number(2).map(|n| n.to_string()).unwrap_or_default(),
            ),
            "older block time" => (
                format!(">= {}", parent.header().wire().time.unwrap_or(0)),
                w.time.unwrap_or(0).to_string(),
            ),
            "extra data size" => (
                "<= 32 bytes".into(),
                format!("{} bytes", b.extra.as_deref().unwrap_or_default().len()),
            ),
            "parent total entropy" | "parent delta entropy" | "parent uncled delta entropy" => {
                match validation::native_parent_metrics(
                    &mut self.verifier,
                    &self.histories[2],
                    &parent,
                    p,
                ) {
                    Ok(m) => (
                        format!(
                            "order {} total {} delta {} uncled_delta {}",
                            m.order,
                            uint(m.total),
                            uint(if m.order < 2 {
                                rules::Uint::ZERO
                            } else {
                                m.delta
                            }),
                            uint(if m.order < 2 {
                                rules::Uint::ZERO
                            } else {
                                m.uncled_delta
                            }),
                        ),
                        format!(
                            "total {} delta {} uncled_delta {}",
                            uint_hex(&b.parent_entropy[2]),
                            uint_hex(&b.parent_delta_entropy[2]),
                            uint_hex(&b.parent_uncled_delta_entropy[2]),
                        ),
                    ),
                    Err(x) => ("parent metrics".into(), describe(&x)),
                }
            }
            "zone expansion" => match self.orders.get(&parent.id()).copied() {
                Some(order) => {
                    match zone_header::expected_expansion(&self.histories[2], &parent, p, order) {
                        Ok(v) => (v.to_string(), b.expansion_number.unwrap_or(0).to_string()),
                        Err(x) => ("expansion".into(), describe(&x)),
                    }
                }
                None => fallback,
            },
            "prime terminus binding" => match self.orders.get(&parent.id()).copied() {
                Some(order) => {
                    let (ph, pn) = if order == 0 || p.genesis.contains(&parent.id()) {
                        (parent.id(), parent.number(0).unwrap_or(0))
                    } else {
                        (
                            h32(&pb.prime_terminus_hash.as_ref().unwrap().value),
                            parent.prime(),
                        )
                    };
                    (
                        format!("{} at prime {pn}", hex(&ph)),
                        format!(
                            "{} at prime {}",
                            hex(&b.prime_terminus_hash.as_ref().unwrap().value),
                            record.prime()
                        ),
                    )
                }
                None => fallback,
            },
            _ => fallback,
        }
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
        // 2. parent links
        for c in CONTEXTS {
            let Some(r) = &records[c.index()] else {
                continue;
            };
            let (ph, pn) = self.heads[c.index()].expect("genesis checked first");
            let parent = r.parent(c.index()).unwrap_or([0; 32]);
            if parent != ph {
                return Err(fail(c, "parent hash", hex(&ph), hex(&parent)));
            }
            let n = r.number(c.index()).unwrap_or(u64::MAX);
            if n != pn + 1 {
                return Err(fail(c, "number", (pn + 1).to_string(), n.to_string()));
            }
        }
        // 3. body commitments
        for c in CONTEXTS {
            if let Some(r) = &records[c.index()] {
                Self::body(r, c)?;
            }
        }
        let zone = records[2].clone().unwrap();
        // 4. proof-of-work, computed by the parallel pass
        let done = done.ok_or_else(|| {
            fail(
                Context::Zone,
                "proof-of-work",
                "a computed proof",
                "no result",
            )
        })?;
        if let Err(e) = &done.block {
            let w = zone.header().wire();
            let algorithm = if w.aux_pow.is_some() {
                "KAWPOW auxpow"
            } else {
                "ProgPoW"
            };
            return Err(fail(
                Context::Zone,
                "proof-of-work",
                format!(
                    "{algorithm} proof meeting difficulty {} at prime terminus {}",
                    uint_hex(w.difficulty.as_deref().unwrap_or_default()),
                    zone.prime()
                ),
                describe(e),
            ));
        }
        self.verifier.reuse_header_proof_memo(done.proofs);
        self.verifier.reuse_inclusion_work_memo(done.work);
        // 5. computed order against the exported records
        let order = placement::verify_placement(&mut self.verifier, &zone, &self.profiles[2])
            .map_err(|e| fail(Context::Zone, "order", "a placed block", describe(&e)))?
            .order;
        let present: Vec<&str> = CONTEXTS
            .iter()
            .filter(|c| records[c.index()].is_some())
            .map(|c| c.name())
            .collect();
        let expected: Vec<&str> = CONTEXTS[order as usize..]
            .iter()
            .map(|c| c.name())
            .collect();
        if present != expected {
            return Err(fail(
                Context::Zone,
                "coincident order",
                format!("order {order}: records {}", expected.join("+")),
                format!("records {}", present.join("+")),
            ));
        }
        // 6. header rules, inclusion, parent commitments and PCRC per context
        let mut claim = [0u8; 32];
        let mut next: [Option<Snapshot>; 3] = [None, None, None];
        for c in &CONTEXTS[order as usize..] {
            let r = records[c.index()].clone().unwrap();
            let prepared = placement::prepare_append_components(
                &mut self.verifier,
                &self.histories[c.index()],
                &self.indexes[c.index()],
                &r,
                &self.profiles[c.index()],
                self.now,
                (order as usize) < c.index(),
                claim,
            );
            match prepared {
                Ok(p) => {
                    claim = p.previous_dominant;
                    next[c.index()] = Some(p.next);
                }
                Err(e) => {
                    let (expected, actual) = self.explain(*c, &r, &e);
                    return Err(fail(
                        *c,
                        format!("{} rules: {}", c.name(), message(&e)),
                        expected,
                        actual,
                    ));
                }
            }
        }
        // 7a. pre-KAWPOW uncled entropy
        let zb = zone.object().header().wire();
        if zone.prime() < rules::KAWPOW_FORK {
            let shares =
                prefork_shares::native_components(&mut self.verifier, &self.histories[2], &zone)
                    .map_err(|e| {
                        fail(
                            Context::Zone,
                            "uncled entropy",
                            "native share entropy",
                            describe(&e),
                        )
                    })?;
            let actual = share_rules::uint(zb.uncled_entropy.as_deref().unwrap_or_default())
                .map_err(|e| {
                    fail(
                        Context::Zone,
                        "uncled entropy",
                        "a 256-bit value",
                        describe(&e),
                    )
                })?;
            if actual != shares.uncled {
                return Err(fail(
                    Context::Zone,
                    "uncled entropy",
                    uint(shares.uncled),
                    uint(actual),
                ));
            }
        }
        // 7b. ETX rollup root over the zone parent's manifest
        let parent = self.indexes[2]
            .get(&zone.parent(2).unwrap_or([0; 32]))
            .map_err(|e| fail(Context::Zone, "ETX rollup", "parent manifest", describe(&e)))?;
        let manifest = parent
            .manifest
            .as_ref()
            .ok_or_else(|| fail(Context::Zone, "ETX rollup", "parent manifest", "none"))?;
        let mut accumulated = vec![];
        for id in manifest {
            let block = self.histories[2].get(id).map_err(|e| {
                fail(
                    Context::Zone,
                    "ETX rollup",
                    format!("manifest block {}", hex(id)),
                    describe(&e),
                )
            })?;
            accumulated.extend_from_slice(block.object().outbound_etxs());
        }
        let cross = cross_prime(&accumulated, self.spec.location[0]).map_err(|e| {
            fail(
                Context::Zone,
                "ETX rollup",
                "rollup within bounds",
                describe(&e),
            )
        })?;
        let root = work::transactions_root(&cross)
            .map_err(|e| fail(Context::Zone, "ETX rollup", "rollup root", e))?;
        let committed = &zb.etx_rollup_hash.as_ref().unwrap().value;
        if root.as_slice() != committed.as_slice() {
            return Err(fail(
                Context::Zone,
                "ETX rollup root",
                hex(&root),
                hex(committed),
            ));
        }
        // 7c. the region manifest must roll up the same zone ETXs
        if order <= 1 {
            let region = records[1].as_ref().unwrap();
            let mut from_region = vec![];
            for id in region.object().manifest_hashes() {
                let block = self.histories[2].get(&id).map_err(|e| {
                    fail(
                        Context::Region,
                        "region/zone rollup",
                        format!("manifest block {}", hex(&id)),
                        describe(&e),
                    )
                })?;
                from_region.extend(block.object().outbound_etxs().iter().map(|t| t.encode()));
            }
            let from_zone: Vec<_> = accumulated.iter().map(|t| t.encode()).collect();
            if from_region != from_zone {
                return Err(fail(
                    Context::Region,
                    "region/zone rollup",
                    format!("{} ETXs from the zone parent manifest", from_zone.len()),
                    format!("{} ETXs from the region manifest", from_region.len()),
                ));
            }
        }
        // Commit to the window.
        for c in &CONTEXTS[order as usize..] {
            let r = records[c.index()].clone().unwrap();
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
/// As `run`, with an epoch cache store that outlives the run (tests reuse it).
pub fn run_with(
    opts: &Options,
    log: &Log,
    caches: Option<Arc<SharedCaches>>,
) -> Result<Summary, String> {
    let started = Instant::now();
    let manifest = Manifest::load(&opts.export).map_err(|e| e.to_string())?;
    let spec = ChainSpec::for_environment(&manifest.environment).map_err(|e| {
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
            spec.environment,
            hex(&spec.genesis_hash())
        ));
    }
    log.info(&format!(
        "replay {}: environment {} (chain {}), genesis {}, {} segments, zone heights 0..={}{}, {} proof-of-work threads",
        opts.export.display(),
        spec.environment,
        spec.chain_id,
        hex(&spec.genesis_hash()),
        manifest.segments.len(),
        manifest.last_zone,
        opts.to.map_or(String::new(), |t| format!(" (stop after {t})")),
        opts.jobs
    ));
    let last_zone = manifest.last_zone;
    let environment = manifest.environment.clone();
    let mut reader = ExportReader::new(&opts.export, manifest, opts.to);
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
    let mut pool = Pool::new(opts.jobs, caches.clone(), spec.location.to_vec());
    let mut checker = Checker::new(spec, caches);
    let lookahead = (opts.jobs * 16).max(64);
    let mut ahead: VecDeque<Slot> = VecDeque::new();
    let mut reader_done = false;
    let mut reader_error: Option<ExportError> = None;
    let mut seq = 0u64;
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
    };
    let mut last_progress = Instant::now();
    let mut progress_heights = 0u64;
    loop {
        let t = Instant::now();
        while !reader_done && ahead.len() < lookahead {
            match reader.next_height() {
                Ok(Some(height)) => {
                    let mut slot = decode(height, seq);
                    seq += 1;
                    if slot.height.zone_height > 0 {
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
        if last_progress.elapsed() >= opts.progress {
            let secs = last_progress.elapsed().as_secs_f64();
            log.info(&format!(
                "zone height {zone_height}/{last_zone}: {} prime, {} region, {} zone records, {} workshares; {:.1} blocks/s now, {:.1} overall",
                summary.records[0],
                summary.records[1],
                summary.records[2],
                summary.shares,
                progress_heights as f64 / secs,
                summary.heights as f64 / started.elapsed().as_secs_f64(),
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
    summary.elapsed = started.elapsed();
    Ok(summary)
}
