//! The block validation pipeline against an explicit parent context.
//!
//! `check` runs every rule that needs no execution state, in this order:
//! parent links, body commitments, the block proof-of-work, the computed
//! order against the coincident records, the header/inclusion/append rules
//! and PCRC of each coincident context, the uncled entropy, the prime
//! starting values, the ETX rollup root and the region/zone rollup agreement.
//! `execute` derives the ETXs the block confirms (the shared
//! `confirmation::confirm`), executes the zone block on the parent state with
//! the ETXs its parent confirmed, and compares every state root of the header.
//! `Verifier::validate_block` runs both; `quai-node replay` runs the same two
//! calls on mainnet exports. A failure names the context and the check, with
//! the expected and the actual value where the rule has one.
use crate::{
    chain::Chain,
    confirmation::{self, Known},
    economics,
    history::{History, Record},
    placement::{self, CandidateIndex, Snapshot},
    rules::{self, Uint},
    share_rules, validation,
    verifier::Verifier,
    zone_execution::{self, State},
    zone_header, Error,
};
use crypto_bigint::Encoding as _;
use quai_execution::historical;
use quai_primitives::{encode_hex, transaction::Transaction, work, Hash};
use std::sync::Arc;

/// Context names in index order.
pub const CONTEXT_NAMES: [&str; 3] = ["prime", "region", "zone"];

/// The first rule a block breaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// 0 prime, 1 region, 2 zone.
    pub context: usize,
    pub check: String,
    pub expected: String,
    pub actual: String,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}: expected {}, actual {}",
            CONTEXT_NAMES[self.context.min(2)],
            self.check,
            self.expected,
            self.actual
        )
    }
}
fn fail(
    context: usize,
    check: impl Into<String>,
    expected: impl Into<String>,
    actual: impl Into<String>,
) -> Failure {
    Failure {
        context,
        check: check.into(),
        expected: expected.into(),
        actual: actual.into(),
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
pub fn message(e: &Error) -> &'static str {
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
/// Decimal for values that fit u128, hex otherwise.
pub fn uint(v: Uint) -> String {
    let b = v.to_be_bytes();
    if b[..16].iter().all(|x| *x == 0) {
        u128::from_be_bytes(b[16..].try_into().unwrap()).to_string()
    } else {
        hex(&b)
    }
}
fn h32(v: &[u8]) -> Hash {
    v.try_into().unwrap_or([0; 32])
}
fn body_context(c: usize) -> work::Context {
    match c {
        0 => work::Context::Prime,
        1 => work::Context::Region,
        _ => work::Context::Zone,
    }
}

/// The ancestry a block is checked against: the chain spec, the recent
/// records and append snapshots of each context, the heads the block must
/// extend, the orders (and, for execution, the rollups and confirmations) of
/// earlier blocks, and the clock for the future-block rule.
pub struct Parent<'a> {
    pub chain: &'a Chain,
    pub histories: &'a [History; 3],
    pub indexes: &'a [CandidateIndex; 3],
    /// The head (hash, number) of each context.
    pub heads: [(Hash, u64); 3],
    pub known: &'a dyn Known,
    pub now: u64,
}
/// The execution state of the zone parent and the history execution reads at
/// fixed depths (redemptions and trims).
pub struct ParentState<'a, P: historical::History> {
    pub state: &'a mut State,
    pub past: &'a P,
    pub profile: &'a zone_execution::Profile,
}
/// A history with no records, for validation without a parent state.
pub struct NoPast;
impl historical::History for NoPast {
    fn incoming_at(&self, _: u64) -> Result<Vec<Transaction>, quai_execution::settlement::Error> {
        Err(quai_execution::settlement::Error::Unavailable(
            "no parent history".into(),
        ))
    }
    fn created_at(
        &self,
        _: u64,
    ) -> Result<Vec<(quai_execution::qi::OutPoint, u8)>, quai_execution::settlement::Error> {
        Err(quai_execution::settlement::Error::Unavailable(
            "no parent history".into(),
        ))
    }
}
/// A block whose state-free rules hold.
pub struct Checked {
    pub order: u8,
    /// The coincident records, index = context.
    pub records: [Option<Arc<Record>>; 3],
    /// The append snapshots for the contexts the block extends.
    pub next: [Option<Snapshot>; 3],
    /// Zone outbound ETXs of the zone parent's manifest.
    pub accumulated: Vec<Transaction>,
    /// The cross-prime rollup of those ETXs (the region rollup).
    pub cross: Vec<Transaction>,
}
impl Checked {
    pub fn zone(&self) -> &Arc<Record> {
        self.records[2].as_ref().expect("checked zone record")
    }
}
/// A zone block executed with every state root equal to its header.
pub struct Executed {
    /// The ETXs this block confirms for its zone children.
    pub confirmed: Vec<Transaction>,
    pub execution: zone_execution::Executed,
}
pub struct Validated {
    pub checked: Checked,
    /// Present when a parent state was supplied.
    pub executed: Option<Executed>,
}

fn body(record: &Record, c: usize) -> Result<(), Failure> {
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
    let root = |r: Result<Hash, String>, what: &str| -> Result<Hash, Failure> {
        r.map_err(|e| {
            fail(
                c,
                format!("body: {what}"),
                "a root within the codec bounds",
                e,
            )
        })
    };
    if c == 2 {
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
    } else {
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
        let committed = &b.manifest_hash[c + 1].value;
        if manifest == empty || manifest.as_slice() != committed.as_slice() {
            return Err(fail(
                c,
                "body: subordinate manifest root",
                hex(committed),
                hex(&manifest),
            ));
        }
        if c == 0 {
            let links = root(o.interlink_root(), "interlink root")?;
            let committed = &b.interlink_root_hash.as_ref().unwrap().value;
            if links.as_slice() != committed.as_slice() {
                return Err(fail(c, "body: interlink root", hex(committed), hex(&links)));
            }
        }
    }
    // The library check must agree with the explicit one above.
    o.verify_commitments(body_context(c))
        .map_err(|e| fail(c, "body: commitments", "commitments hold", e))
}

/// Expected and actual values for the zone header rules that have a public
/// calculation; other rules report the refusal text.
fn explain(
    v: &mut Verifier,
    parent: &Parent<'_>,
    c: usize,
    record: &Record,
    e: &Error,
) -> (String, String) {
    let fallback = ("the rule holds".to_string(), describe(e));
    if c != 2 {
        return fallback;
    }
    let history = &parent.histories[2];
    let Ok(prev) = history.get(&record.parent(2).unwrap_or([0; 32])) else {
        return fallback;
    };
    let b = record.object().header().wire();
    let pb = prev.object().header().wire();
    let w = record.header().wire();
    let p = &parent.chain.zone_profile();
    let prev_number = if p.genesis.contains(&prev.id()) {
        0
    } else {
        prev.number(2).unwrap_or(0)
    };
    match message(e) {
        "header difficulty" => match validation::expected_difficulty(history, &prev, p) {
            Ok(x) => (
                uint(x),
                uint_hex(w.difficulty.as_deref().unwrap_or_default()),
            ),
            Err(x) => ("difficulty".into(), describe(&x)),
        },
        "gas limit schedule" | "gas use/limit" => (
            format!(
                "gas_limit {} and gas_used <= gas_limit",
                rules::next_limit(
                    prev.number(2).unwrap_or(0),
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
                    prev.number(2).unwrap_or(0),
                    pb.state_limit.unwrap_or(0),
                    quai_primitives::spec::STATE_CEIL
                )
            ),
            format!(
                "state_limit {} state_used {}",
                b.state_limit.unwrap_or(0),
                b.state_used.unwrap_or(0)
            ),
        ),
        "base fee" => match economics::expected_base_fee(history, &prev, p) {
            Ok(x) => (uint(x), uint_hex(b.base_fee.as_deref().unwrap_or_default())),
            Err(x) => ("base fee".into(), describe(&x)),
        },
        "header number" => (
            (prev_number + 1).to_string(),
            record.number(2).map(|n| n.to_string()).unwrap_or_default(),
        ),
        "older block time" => (
            format!(">= {}", prev.header().wire().time.unwrap_or(0)),
            w.time.unwrap_or(0).to_string(),
        ),
        "extra data size" => (
            format!(
                "<= {} bytes",
                quai_primitives::spec::MAXIMUM_EXTRA_DATA_SIZE
            ),
            format!("{} bytes", b.extra.as_deref().unwrap_or_default().len()),
        ),
        "parent total entropy" | "parent delta entropy" | "parent uncled delta entropy" => {
            match validation::native_parent_metrics(v, history, &prev, p) {
                Ok(m) => (
                    format!(
                        "order {} total {} delta {} uncled_delta {}",
                        m.order,
                        uint(m.total),
                        uint(if m.order < 2 { Uint::ZERO } else { m.delta }),
                        uint(if m.order < 2 {
                            Uint::ZERO
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
        "zone expansion" => match parent.known.order(&prev.id()) {
            Ok(order) => match zone_header::expected_expansion(history, &prev, p, order) {
                Ok(x) => (x.to_string(), b.expansion_number.unwrap_or(0).to_string()),
                Err(x) => ("expansion".into(), describe(&x)),
            },
            Err(_) => fallback,
        },
        "prime terminus binding" => match parent.known.order(&prev.id()) {
            Ok(order) => {
                let (ph, pn) = if order == 0 || p.genesis.contains(&prev.id()) {
                    (prev.id(), prev.number(0).unwrap_or(0))
                } else {
                    (
                        h32(&pb.prime_terminus_hash.as_ref().unwrap().value),
                        prev.prime(),
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
            Err(_) => fallback,
        },
        _ => fallback,
    }
}

/// Every rule of a block that needs no execution state.
pub fn check(
    v: &mut Verifier,
    parent: &Parent<'_>,
    records: [Option<Arc<Record>>; 3],
) -> Result<Checked, Failure> {
    let zone = records[2]
        .clone()
        .ok_or_else(|| fail(2, "records", "a zone record", "none"))?;
    // Coincident records are views of the zone block: one work header and
    // one body header, with the context's own manifest and links.
    for (c, r) in records[..2].iter().enumerate() {
        let Some(r) = r else { continue };
        if r.id() != zone.id()
            || r.header().encode() != zone.header().encode()
            || r.object().header().encode() != zone.object().header().encode()
        {
            return Err(fail(c, "coincident binding", hex(&zone.id()), hex(&r.id())));
        }
    }
    // Parent links.
    for (c, r) in records.iter().enumerate() {
        let Some(r) = r else { continue };
        let (ph, pn) = parent.heads[c];
        let link = r.parent(c).unwrap_or([0; 32]);
        if link != ph {
            return Err(fail(c, "parent hash", hex(&ph), hex(&link)));
        }
        let n = r.number(c).unwrap_or(u64::MAX);
        if Some(n) != pn.checked_add(1) {
            return Err(fail(c, "number", (pn + 1).to_string(), n.to_string()));
        }
    }
    // Body commitments.
    for (c, r) in records.iter().enumerate() {
        if let Some(r) = r {
            body(r, c)?;
        }
    }
    // The block proof-of-work (a memo hit when a worker already proved it).
    if let Err(e) = v.verify_header_proof(zone.header()) {
        let w = zone.header().wire();
        let algorithm = if w.aux_pow.is_some() {
            "KAWPOW auxpow"
        } else {
            "ProgPoW"
        };
        return Err(fail(
            2,
            "proof-of-work",
            format!(
                "{algorithm} proof meeting difficulty {} at prime terminus {}",
                uint_hex(w.difficulty.as_deref().unwrap_or_default()),
                zone.prime()
            ),
            describe(&e),
        ));
    }
    // The computed order against the coincident records.
    let chain = parent.chain;
    let order = placement::verify_placement(v, &zone, &chain.placement_profile(2))
        .map_err(|e| fail(2, "order", "a placed block", describe(&e)))?
        .order;
    let present: Vec<&str> = (0..3)
        .filter(|c| records[*c].is_some())
        .map(|c| CONTEXT_NAMES[c])
        .collect();
    let expected: Vec<&str> = CONTEXT_NAMES[order as usize..].to_vec();
    if present != expected {
        return Err(fail(
            2,
            "coincident order",
            format!("order {order}: records {}", expected.join("+")),
            format!("records {}", present.join("+")),
        ));
    }
    // Header rules, inclusion, parent commitments and PCRC per context.
    let mut claim = [0u8; 32];
    let mut next: [Option<Snapshot>; 3] = [None, None, None];
    for c in order as usize..3 {
        let r = records[c].clone().unwrap();
        match placement::prepare_append_components(
            v,
            &parent.histories[c],
            &parent.indexes[c],
            &r,
            &chain.placement_profile(c),
            parent.now,
            (order as usize) < c,
            claim,
        ) {
            Ok(p) => {
                claim = p.previous_dominant;
                next[c] = Some(p.next);
            }
            Err(e) => {
                let (expected, actual) = explain(v, parent, c, &r, &e);
                return Err(fail(
                    c,
                    format!("{} rules: {}", CONTEXT_NAMES[c], message(&e)),
                    expected,
                    actual,
                ));
            }
        }
    }
    // Uncled entropy: pre-KawPow share entropy, UncledLogEntropy after.
    let zb = zone.object().header().wire();
    let expected = confirmation::expected_uncled_entropy(v, &parent.histories[2], &zone)
        .map_err(|e| fail(2, "uncled entropy", "native share entropy", describe(&e)))?;
    let actual = share_rules::uint(zb.uncled_entropy.as_deref().unwrap_or_default())
        .map_err(|e| fail(2, "uncled entropy", "a 256-bit value", describe(&e)))?;
    if actual != expected {
        return Err(fail(2, "uncled entropy", uint(expected), uint(actual)));
    }
    // Prime controller starting values (core/slice.go Append).
    if order == 0 {
        confirmation::check_prime_economics(chain, &zone).map_err(|e| {
            fail(
                0,
                "prime economics",
                "the starting controller values",
                describe(&e),
            )
        })?;
    }
    // ETX rollup root over the zone parent's manifest.
    let snapshot = parent.indexes[2]
        .get(&zone.parent(2).unwrap_or([0; 32]))
        .map_err(|e| fail(2, "ETX rollup", "parent manifest", describe(&e)))?;
    let manifest = snapshot
        .manifest
        .as_ref()
        .ok_or_else(|| fail(2, "ETX rollup", "parent manifest", "none"))?;
    let mut accumulated = vec![];
    for id in manifest {
        let block = parent.histories[2].get(id).map_err(|e| {
            fail(
                2,
                "ETX rollup",
                format!("manifest block {}", hex(id)),
                describe(&e),
            )
        })?;
        accumulated.extend_from_slice(block.object().outbound_etxs());
    }
    let cross = confirmation::cross_prime(&accumulated, chain.location()[0])
        .map_err(|e| fail(2, "ETX rollup", "rollup within bounds", describe(&e)))?;
    let root =
        work::transactions_root(&cross).map_err(|e| fail(2, "ETX rollup", "rollup root", e))?;
    let committed = &zb.etx_rollup_hash.as_ref().unwrap().value;
    // Go checks EtxRollupHash in the region Append: region-coincident blocks.
    if order <= 1 && root.as_slice() != committed.as_slice() {
        return Err(fail(2, "ETX rollup root", hex(&root), hex(committed)));
    }
    // The region manifest must roll up the same zone ETXs.
    if order <= 1 {
        let region = records[1].as_ref().unwrap();
        let mut from_region = vec![];
        for id in region.object().manifest_hashes() {
            let block = parent.histories[2].get(&id).map_err(|e| {
                fail(
                    1,
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
                1,
                "region/zone rollup",
                format!("{} ETXs from the zone parent manifest", from_zone.len()),
                format!("{} ETXs from the region manifest", from_region.len()),
            ));
        }
    }
    Ok(Checked {
        order,
        records,
        next,
        accumulated,
        cross,
    })
}

/// Execute a checked zone block on its parent state and compare every state
/// root. On success the state holds the block's post-state.
pub fn execute<P: historical::History>(
    v: &mut Verifier,
    parent: &Parent<'_>,
    state: &mut ParentState<'_, P>,
    checked: &Checked,
) -> Result<Executed, Failure> {
    let zone = checked.zone().clone();
    let confirmed = confirmation::confirm(
        parent.histories,
        parent.known,
        [
            checked.records[0].as_deref(),
            checked.records[1].as_deref(),
            checked.records[2].as_deref(),
        ],
        checked.order,
        &checked.accumulated,
        parent.chain.location(),
    )
    .map_err(|e| fail(2, "state: confirmation", "confirmed ETXs", describe(&e)))?;
    let inbound = parent
        .known
        .confirmed(&zone.parent(2).unwrap_or([0; 32]))
        .map_err(|e| {
            fail(
                2,
                "state: inbound ETXs",
                "parent confirmation",
                describe(&e),
            )
        })?;
    let e = match zone_execution::native_components(
        v,
        state.state,
        &parent.histories[2],
        state.past,
        &zone,
        state.profile,
        &inbound,
    ) {
        Ok(execution) => {
            return Ok(Executed {
                confirmed,
                execution,
            })
        }
        Err(e) => e,
    };
    // Execute again without the commitment comparison to report values.
    let location = parent.chain.location();
    let computed = zone_execution::diagnose(
        state.state,
        &parent.histories[2],
        state.past,
        &zone,
        state.profile,
        &inbound,
        |h| v.precompute_inclusion_work(h, &location),
    );
    let b = zone.object().header().wire();
    let field =
        |x: &Option<quai_primitives::schema::common::ProtoHash>| hex(&x.as_ref().unwrap().value);
    let (expected, actual) = match &computed {
        Ok(c) => match message(&e) {
            "execution account root" => (field(&b.evm_root), hex(&c.evm_root)),
            "execution ETX root" => (field(&b.etx_set_root), hex(&c.etx_root)),
            "execution UTXO root" => (field(&b.utxo_root), hex(&c.utxo_root)),
            "execution receipt root" => (field(&b.receipt_hash), hex(&c.receipt_root)),
            "execution outbound root" => (field(&b.outbound_etx_hash), hex(&c.outbound_root)),
            "execution gas counter" => {
                (b.gas_used.unwrap_or(0).to_string(), c.gas_used.to_string())
            }
            "execution state counter" => (
                b.state_used.unwrap_or(0).to_string(),
                c.state_used.to_string(),
            ),
            "execution account count" => (
                uint_hex(b.quai_state_size.as_deref().unwrap_or_default()),
                c.account_count.to_string(),
            ),
            _ => ("execution succeeds".to_string(), describe(&e)),
        },
        Err(_) => ("execution succeeds".to_string(), describe(&e)),
    };
    Err(fail(2, format!("state: {}", message(&e)), expected, actual))
}

/// Orders, region rollups and confirmations kept in memory.
#[derive(Default)]
pub struct MemoryKnown {
    pub orders: std::collections::HashMap<Hash, u8>,
    pub region_rollups: std::collections::HashMap<Hash, Arc<Vec<Transaction>>>,
    pub confirmed: std::collections::HashMap<Hash, Arc<Vec<Transaction>>>,
}
impl Known for MemoryKnown {
    fn order(&self, id: &Hash) -> Result<u8, Error> {
        self.orders
            .get(id)
            .copied()
            .ok_or(Error::Unavailable("confirmed ancestry order"))
    }
    fn region_rollup(&self, id: &Hash) -> Result<Arc<Vec<Transaction>>, Error> {
        self.region_rollups
            .get(id)
            .cloned()
            .ok_or(Error::Unavailable("committed region rollup"))
    }
    fn confirmed(&self, id: &Hash) -> Result<Arc<Vec<Transaction>>, Error> {
        self.confirmed
            .get(id)
            .cloned()
            .ok_or(Error::Unavailable("confirmed parent ETXs"))
    }
}
/// An in-memory ancestry from a chain's genesis: histories, append snapshots,
/// heads, orders, rollups and confirmations. Blocks enter it only through
/// `commit` of a validated block. The replay keeps a bounded window instead.
pub struct Ancestry {
    chain: Arc<Chain>,
    histories: [History; 3],
    indexes: [CandidateIndex; 3],
    heads: [(Hash, u64); 3],
    known: MemoryKnown,
}
impl Ancestry {
    pub fn new(chain: Arc<Chain>) -> Result<Self, Error> {
        let id = chain.genesis_hash();
        let mut histories: [History; 3] = std::array::from_fn(|_| History::default());
        let mut indexes: [CandidateIndex; 3] = std::array::from_fn(|_| CandidateIndex::default());
        for c in 0..3 {
            histories[c].insert(chain.genesis().clone())?;
            indexes[c].insert_imported(
                id,
                placement::bootstrap_candidate(chain.genesis(), &chain.placement_profile(c))?,
            )?;
        }
        let mut known = MemoryKnown::default();
        known.orders.insert(id, 0);
        known.region_rollups.insert(id, Arc::new(vec![]));
        known.confirmed.insert(id, Arc::new(vec![]));
        Ok(Self {
            chain,
            histories,
            indexes,
            heads: [(id, 0); 3],
            known,
        })
    }
    pub fn heads(&self) -> [(Hash, u64); 3] {
        self.heads
    }
    /// The parent context of the next block.
    pub fn parent(&self, now: u64) -> Parent<'_> {
        Parent {
            chain: &self.chain,
            histories: &self.histories,
            indexes: &self.indexes,
            heads: self.heads,
            known: &self.known,
            now,
        }
    }
    /// Append a block that `Verifier::validate_block` accepted against this
    /// ancestry's current heads.
    pub fn commit(&mut self, validated: Validated) -> Result<(), Error> {
        let Validated { checked, executed } = validated;
        let zone = checked.zone().clone();
        for c in checked.order as usize..3 {
            let r = checked.records[c]
                .as_ref()
                .ok_or(Error::Invalid("validated context record"))?;
            if r.parent(c)? != self.heads[c].0 {
                return Err(Error::Invalid("stale validated block"));
            }
        }
        let mut next = checked.next;
        for c in checked.order as usize..3 {
            let r = checked.records[c].clone().unwrap();
            let snapshot = next[c]
                .take()
                .ok_or(Error::Invalid("validated append snapshot"))?;
            self.heads[c] = (r.id(), r.number(c)?);
            self.indexes[c].insert_imported(r.id(), snapshot)?;
            self.histories[c].insert((*r).clone())?;
        }
        let id = zone.id();
        self.known.orders.insert(id, checked.order);
        if checked.order <= 1 {
            self.known
                .region_rollups
                .insert(id, Arc::new(checked.cross));
        }
        if let Some(e) = executed {
            self.known.confirmed.insert(id, Arc::new(e.confirmed));
        }
        Ok(())
    }
}

/// Validate one block against the bare genesis of `chain`, without a parent
/// state: the context a block without ancestry gets. A block that does not
/// extend the genesis fails its parent links.
pub fn validate_on_genesis(
    v: &mut Verifier,
    chain: Arc<Chain>,
    records: [Option<Arc<Record>>; 3],
    now: u64,
) -> Result<Validated, Failure> {
    let ancestry = Ancestry::new(chain)
        .map_err(|e| fail(2, "ancestry", "the genesis ancestry", describe(&e)))?;
    v.validate_block::<NoPast>(&ancestry.parent(now), records, None)
}
