//! Durable execution-only private branches. Local selection is explicit and
//! grants no header/hierarchy/fork-choice authority or validated RPC access.
use crate::{
    fields,
    history::{History, Record, MAX_RECORDS},
    private_profile::PrivateChain,
    share_rules,
    verifier::Verifier,
    zone_execution::{self, Profile, State},
};
use quai_execution::{historical, qi::OutPoint, settlement, state_image};
use quai_primitives::{
    transaction::{Kind, Transaction},
    trie, Hash,
};
use quai_store::journal::{Checkpoint, Id};
mod reader;
mod store;
use std::sync::Arc;
use std::{collections::BTreeMap, io, path::Path};
use store::Store;

const MAGIC: &[u8] = b"QUAI-PRIVATE-BRANCH-1\n";
const WITNESS_MAGIC: &[u8] = b"QUAI-PRIVATE-BRANCH-2\n";
const KEYED_MAGIC: &[u8] = b"QUAI-PRIVATE-BRANCH-5\n";
const MAX_BYTES: usize = 63 << 20;
const MAX_KEYS: usize = 1_000_000;
fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}
fn require(ok: bool, why: &str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(bad(why))
    }
}

#[derive(Default)]
pub struct BranchHistory {
    incoming: BTreeMap<u64, Vec<Transaction>>,
    created: BTreeMap<u64, Vec<(OutPoint, u8)>>,
    source: Option<Arc<reader::Reader>>,
}
impl historical::History for BranchHistory {
    fn incoming_at(&self, height: u64) -> Result<Vec<Transaction>, settlement::Error> {
        if let Some(source) = &self.source {
            return source.incoming_at(height);
        }
        self.incoming
            .get(&height)
            .cloned()
            .ok_or(settlement::Error::Invalid(
                "missing private redemption ancestry",
            ))
    }
    fn created_at(&self, height: u64) -> Result<Vec<(OutPoint, u8)>, settlement::Error> {
        if let Some(source) = &self.source {
            return source.created_at(height);
        }
        self.created
            .get(&height)
            .cloned()
            .ok_or(settlement::Error::Invalid("missing private trim ancestry"))
    }
}
pub struct Restored {
    pub checkpoint: Id,
    pub anchor: state_image::Anchor,
    pub state: State,
    pub history: History,
    pub past: BranchHistory,
    pub receipt_rlp: Vec<Vec<u8>>,
    pub deleted_hashes: Vec<Hash>,
    confirmation_views: BTreeMap<Hash, Vec<(u8, Vec<u8>)>>,
    source: Option<Arc<reader::Reader>>,
}
#[derive(Clone, Debug)]
pub struct Candidate {
    pub checkpoint: Id,
    pub block: Hash,
    pub height: u64,
    pub work_hash: Hash,
    pub header_components_checked: bool,
    pub inbound_derived: bool,
    pub confirmed_etxs: usize,
    pub execution_inbound_etxs: usize,
    pub hierarchy_contexts: usize,
    pub fully_validated: bool,
    pub(crate) zone_entropy: Option<crate::rules::Entropy>,
}
/// The trusted root is the local private journal and its content-addressed head,
/// not a peer-supplied state digest. No snapshot import or generic stage API.
pub struct PrivateBranch {
    journal: Store,
    profile: PrivateChain,
    // A single locally checked prefix, keyed by its immutable checkpoint.
    // Fresh journal checksums and candidate policy still run on every append.
    hierarchy_cache: Option<(Id, crate::confirmation::Hierarchy)>,
    hierarchy_window: usize,
    decoded_history: Arc<reader::DecodeMemo>,
}
struct Data<'a> {
    reference: state_image::Reference,
    record: Record,
    image: &'a [u8],
    receipts: Vec<Vec<u8>>,
    created: Vec<(OutPoint, u8)>,
    deleted: Vec<Hash>,
    views: Vec<(u8, Vec<u8>)>,
    hierarchy_delta: &'a [u8],
}
fn count(out: &mut Vec<u8>, n: usize) -> io::Result<()> {
    out.extend(u32::try_from(n).map_err(bad)?.to_be_bytes());
    Ok(())
}
fn blob(out: &mut Vec<u8>, bytes: &[u8]) -> io::Result<()> {
    require(
        out.len()
            .checked_add(bytes.len() + 4)
            .is_some_and(|n| n <= MAX_BYTES),
        "private commit byte limit",
    )?;
    count(out, bytes.len())?;
    out.extend(bytes);
    Ok(())
}
fn encode(data: &Data<'_>) -> io::Result<Vec<u8>> {
    encode_mode(data, false)
}
fn encode_mode(data: &Data<'_>, keyed: bool) -> io::Result<Vec<u8>> {
    require(
        data.created.len() <= MAX_KEYS
            && data.deleted.len() <= MAX_KEYS
            && data.receipts.len() <= trie::MAX_LIST_ITEMS,
        "private commit count limit",
    )?;
    let mut out = if keyed {
        KEYED_MAGIC.to_vec()
    } else if data.views.is_empty() {
        MAGIC.to_vec()
    } else {
        WITNESS_MAGIC.to_vec()
    };
    let r = &data.reference;
    let a = &r.anchor;
    out.extend(r.digest);
    out.extend(a.profile);
    out.extend(a.block);
    out.extend(a.parent);
    out.extend(a.height.to_be_bytes());
    out.extend(a.timestamp.to_be_bytes());
    out.extend(r.roots.evm);
    out.extend(r.roots.etx);
    out.extend(r.roots.utxo);
    out.extend(r.roots.accounts.to_be_bytes());
    out.extend(r.roots.utxo_count.to_be_bytes());
    blob(&mut out, data.record.bytes())?;
    blob(&mut out, data.image)?;
    count(&mut out, data.receipts.len())?;
    for receipt in &data.receipts {
        blob(&mut out, receipt)?;
    }
    count(&mut out, data.created.len())?;
    for (p, d) in &data.created {
        out.extend(p.hash);
        out.extend(p.index.to_be_bytes());
        out.push(*d);
    }
    count(&mut out, data.deleted.len())?;
    for h in &data.deleted {
        out.extend(h);
    }
    if !data.views.is_empty() || keyed {
        count(&mut out, data.views.len())?;
        for (c, v) in &data.views {
            out.push(*c);
            blob(&mut out, v)?;
        }
    }
    require(out.len() <= MAX_BYTES, "private commit byte limit")?;
    if keyed {
        blob(&mut out, data.hierarchy_delta)?;
    }
    Ok(out)
}
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(n)
            .ok_or_else(|| bad("private commit offset"))?;
        let v = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| bad("truncated private commit"))?;
        self.offset = end;
        Ok(v)
    }
    fn fixed<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        Ok(crate::slice::array(self.take(N)?)?)
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_be_bytes(self.fixed()?))
    }
    fn count(&mut self, max: usize) -> io::Result<usize> {
        let n = u32::from_be_bytes(self.fixed()?) as usize;
        require(n <= max, "private commit count limit")?;
        Ok(n)
    }
    fn blob(&mut self, max: usize) -> io::Result<&'a [u8]> {
        let n = self.count(max)?;
        self.take(n)
    }
}
fn decode<'a>(checkpoint: &'a Checkpoint, profile: &PrivateChain) -> io::Result<Data<'a>> {
    require(
        checkpoint.payload.len() <= MAX_BYTES,
        "private commit byte limit",
    )?;
    let mut r = Reader {
        bytes: &checkpoint.payload,
        offset: 0,
    };
    let magic = r.take(MAGIC.len())?;
    require(
        magic == MAGIC || magic == WITNESS_MAGIC || magic == KEYED_MAGIC,
        "private branch format; no automatic legacy migration",
    )?;
    let digest = r.fixed()?;
    let anchor = state_image::Anchor {
        profile: r.fixed()?,
        block: r.fixed()?,
        parent: r.fixed()?,
        height: r.u64()?,
        timestamp: r.u64()?,
    };
    let roots = state_image::Roots {
        evm: r.fixed()?,
        etx: r.fixed()?,
        utxo: r.fixed()?,
        accounts: r.u64()?,
        utxo_count: r.u64()?,
    };
    let record =
        Record::decode(r.blob(quai_primitives::schema::MAX_MESSAGE_BYTES)?).map_err(bad)?;
    let image = r.blob(MAX_BYTES)?;
    let n = r.count(trie::MAX_LIST_ITEMS)?;
    let mut receipts = Vec::with_capacity(n);
    for _ in 0..n {
        receipts.push(r.blob(trie::MAX_VALUE_BYTES)?.to_vec());
    }
    let n = r.count(MAX_KEYS)?;
    let mut created = Vec::with_capacity(n);
    for _ in 0..n {
        let p = OutPoint {
            hash: r.fixed()?,
            index: u16::from_be_bytes(r.fixed()?),
        };
        let d = r.take(1)?[0];
        require(
            (d as usize) < quai_execution::qi::DENOMINATIONS.len()
                && created.last().is_none_or(|(_, previous)| *previous <= d),
            "private created denomination order",
        )?;
        created.push((p, d));
    }
    let n = r.count(MAX_KEYS)?;
    let mut deleted = Vec::with_capacity(n);
    for _ in 0..n {
        deleted.push(r.fixed()?);
    }
    let mut views = vec![];
    if magic == WITNESS_MAGIC || magic == KEYED_MAGIC {
        let n = r.count(2)?;
        require(
            (n > 0 || magic == KEYED_MAGIC) && profile.hierarchy_admission,
            "unexpected hierarchy witness format",
        )?;
        for _ in 0..n {
            let c = r.take(1)?[0];
            require(
                c < 2 && views.last().is_none_or(|(previous, _)| *previous < c),
                "hierarchy witness order",
            )?;
            views.push((
                c,
                r.blob(quai_primitives::schema::MAX_MESSAGE_BYTES)?.to_vec(),
            ));
        }
    }
    let hierarchy_delta = if magic == KEYED_MAGIC {
        r.blob(MAX_BYTES)?
    } else {
        &[]
    };
    require(r.offset == r.bytes.len(), "trailing private commit bytes")?;
    require(
        anchor.profile == profile.fingerprint
            && anchor.block == checkpoint.block
            && anchor.block == record.id()
            && anchor.height == checkpoint.height
            && anchor.height == record.number(2).map_err(bad)?
            && anchor.parent == record.parents[2]
            && anchor.timestamp
                == fields::some(record.header().wire().time, "missing time").map_err(bad)?,
        "private commit anchor binding",
    )?;
    if checkpoint.parent.is_none() {
        require(
            anchor.height == 0
                && record.bytes() == profile.genesis[2].bytes()
                && receipts.is_empty()
                && created.is_empty()
                && deleted.is_empty(),
            "private genesis binding",
        )?;
    } else {
        require(anchor.height > 0, "private child height")?;
        record.verify_binding().map_err(bad)?;
        let body = record.object().header().wire();
        require(
            roots.evm.as_slice()
                == fields::hash(&body.evm_root, "missing evm root").map_err(bad)?
                && roots.etx.as_slice()
                    == fields::hash(&body.etx_set_root, "missing etx set root").map_err(bad)?
                && roots.utxo.as_slice()
                    == fields::hash(&body.utxo_root, "missing utxo root").map_err(bad)?
                && crate::rules::Uint::from_u64(roots.accounts)
                    == share_rules::uint(
                        fields::bytes(&body.quai_state_size, "missing quai state size")
                            .map_err(bad)?,
                    )
                    .map_err(bad)?,
            "private commit state roots",
        )?;
        require(
            trie::ordered_root(&receipts).map_err(bad)?.as_slice()
                == fields::hash(&body.receipt_hash, "missing receipt hash").map_err(bad)?,
            "private commit receipt root",
        )?;
    }
    Ok(Data {
        reference: state_image::Reference {
            digest,
            anchor,
            roots,
        },
        record,
        image,
        receipts,
        created,
        deleted,
        views,
        hierarchy_delta,
    })
}
impl PrivateBranch {
    pub fn open(directory: &Path) -> io::Result<Self> {
        let profile = PrivateChain::reviewed().map_err(bad)?;
        Self::with_profile(directory, profile)
    }
    /// Separate pinned profile and directory marker; never upgrades an existing
    /// execution-only store or accepts caller-supplied consensus settings.
    pub fn open_admission(directory: &Path) -> io::Result<Self> {
        Self::with_profile(directory, PrivateChain::reviewed_admission().map_err(bad)?)
    }
    pub fn open_hierarchy(directory: &Path) -> io::Result<Self> {
        Self::with_profile(directory, PrivateChain::reviewed_hierarchy().map_err(bad)?)
    }
    fn with_profile(directory: &Path, profile: PrivateChain) -> io::Result<Self> {
        let journal = Store::images(directory, &profile)?;
        Self::with_store(journal, profile)
    }
    pub fn open_hierarchy_database(directory: &Path) -> io::Result<Self> {
        Self::open_hierarchy_database_profile(
            directory,
            PrivateChain::reviewed_hierarchy().map_err(bad)?,
        )
    }
    pub(crate) fn open_hierarchy_database_profile(
        directory: &Path,
        profile: PrivateChain,
    ) -> io::Result<Self> {
        require(
            profile.hierarchy_admission,
            "hierarchy runtime profile required",
        )?;
        let journal = Store::keyed(directory, &profile)?;
        Self::with_store(journal, profile)
    }
    fn with_store(journal: Store, profile: PrivateChain) -> io::Result<Self> {
        let mut s = Self {
            journal,
            profile,
            hierarchy_cache: None,
            hierarchy_window: 64,
            decoded_history: Arc::new(reader::DecodeMemo::new()),
        };
        if s.journal.head().is_none() {
            let record = s.profile.genesis[2].clone();
            let state = State::default();
            let anchor = state_image::Anchor {
                profile: s.profile.fingerprint,
                block: record.id(),
                parent: record.parents[2],
                height: 0,
                timestamp: fields::some(record.header().wire().time, "missing time")
                    .map_err(bad)?,
            };
            let staged = match &s.journal {
                Store::Keyed(k) => k.database.version_for_block(record.id())?,
                Store::Images(_) => None,
            };
            let id = if let Some(existing) = staged {
                let state = s.restore(existing)?;
                require(
                    state.anchor.height == 0 && state.state.world.is_empty().map_err(bad)?,
                    "staged database genesis state",
                )?;
                require(
                    state.state.utxos.raw_len() == 0
                        && state.state.utxos.consensus_size() == 0
                        && state.state.utxos.accumulator()
                            == quai_execution::utxo::UtxoState::default().accumulator(),
                    "staged database genesis UTXOs",
                )?;
                existing
            } else {
                s.stage_state(
                    None,
                    record,
                    anchor,
                    vec![],
                    vec![],
                    vec![],
                    vec![],
                    &state,
                    &state,
                    vec![],
                )?
            };
            s.journal.publish(None, id)?;
        }
        // Reject legacy payloads and inconsistent branch data on reopen, rather
        // than silently starting from an empty image or importing peer claims.
        s.restore(s.head())?;
        Ok(s)
    }
    /// Successful decodes, verified reuses, retained entries and encoded bytes.
    /// The byte count is logical payload accounting, not heap or RSS.
    pub fn history_decode_stats(&self) -> (usize, usize, usize, usize) {
        self.decoded_history.stats()
    }
    pub fn head(&self) -> Id {
        self.journal.head().expect("private genesis initialized")
    }
    pub fn restore(&self, id: Id) -> io::Result<Restored> {
        if let Store::Keyed(keyed) = &self.journal {
            let source = reader::Reader::new(
                keyed.snapshot(id)?,
                &self.profile,
                self.decoded_history.clone(),
            )?;
            let tip = source.tip().map_err(bad)?;
            let state = source.restore_state(&tip)?;
            return Ok(Restored {
                checkpoint: id,
                anchor: tip.reference.anchor.clone(),
                state: State {
                    world: state.world,
                    utxos: state.utxos,
                },
                history: History::with_reader(source.clone()),
                past: BranchHistory {
                    source: Some(source.clone()),
                    ..Default::default()
                },
                receipt_rlp: tip.receipts.clone(),
                deleted_hashes: tip.deleted.clone(),
                confirmation_views: BTreeMap::new(),
                source: Some(source),
            });
        }
        let mut cursor = Some(id);
        let mut child: Option<(Hash, u64)> = None;
        let mut history = History::default();
        let mut past = BranchHistory::default();
        let mut tip = None;
        let mut confirmation_views = BTreeMap::new();
        while let Some(current) = cursor {
            require(history.len() < MAX_RECORDS, "private branch history bound")?;
            let cp = self.journal.load(current)?;
            let d = decode(&cp, &self.profile)?;
            if let Some((parent, height)) = child {
                require(
                    parent == cp.block && cp.height.checked_add(1) == Some(height),
                    "private branch block ancestry",
                )?;
            }
            if tip.is_none() {
                let restored = match &self.journal {
                    Store::Images(_) => state_image::decode(d.image, &d.reference).map_err(bad)?,
                    Store::Keyed(k) => {
                        require(
                            d.image.is_empty(),
                            "keyed execution contains a legacy image",
                        )?;
                        let snapshot = k.snapshot(current)?;
                        let (world, utxos) = quai_execution::state_store::restore(
                            &snapshot,
                            &quai_execution::state_store::Commitment {
                                roots: d.reference.roots.clone(),
                                digest: d.reference.digest,
                            },
                        )?;
                        state_image::Restored { world, utxos }
                    }
                };
                tip = Some((
                    d.reference.anchor.clone(),
                    State {
                        world: restored.world,
                        utxos: restored.utxos,
                    },
                    d.receipts,
                    d.deleted,
                ));
            }
            child = Some((d.record.parents[2], cp.height));
            confirmation_views.insert(d.record.id(), d.views);
            past.incoming.insert(
                cp.height,
                d.record
                    .object()
                    .transactions()
                    .iter()
                    .filter(|tx| matches!(tx.kind(), Kind::External(_)))
                    .cloned()
                    .collect(),
            );
            past.created.insert(cp.height, d.created);
            history.insert(d.record).map_err(bad)?;
            cursor = cp.parent;
        }
        let (anchor, state, receipt_rlp, deleted_hashes) =
            tip.ok_or_else(|| bad("missing private branch tip"))?;
        Ok(Restored {
            checkpoint: id,
            anchor,
            state,
            history,
            past,
            receipt_rlp,
            deleted_hashes,
            confirmation_views,
            source: None,
        })
    }
    /// Stage a natively executed candidate without changing the selected head.
    /// Inbound eligibility and complete header/hierarchy policy remain external
    /// unqualified inputs; this result is never an admitted-block token.
    pub fn execute_candidate(
        &mut self,
        verifier: &mut Verifier,
        parent: Id,
        bytes: &[u8],
        inbound: &[Transaction],
    ) -> io::Result<Candidate> {
        require(
            !self.profile.header_admission,
            "use the local admission entrypoint",
        )?;
        self.execute_inner(
            verifier,
            parent,
            bytes,
            inbound,
            None,
            vec![],
            None,
            None,
            vec![],
            None,
        )
    }
    /// Native header/manifest/inclusion/PCRC components precede execution. This
    /// entrypoint derives an empty inbound set only for the supported local-zone
    /// segment; a dominant block requires unavailable higher-order admission.
    pub fn execute_admission_candidate(
        &mut self,
        verifier: &mut Verifier,
        parent: Id,
        bytes: &[u8],
        now: u64,
    ) -> io::Result<Candidate> {
        require(
            self.profile.header_admission && !self.profile.hierarchy_admission,
            "local admission profile required",
        )?;
        self.execute_inner(
            verifier,
            parent,
            bytes,
            &[],
            Some(now),
            vec![],
            None,
            None,
            vec![],
            None,
        )
    }
    /// Derive execution inbounds from authenticated prior rollups. Context views
    /// are witnesses for the same candidate header, never an inbound list.
    pub fn execute_hierarchy_candidate(
        &mut self,
        v: &mut Verifier,
        parent: Id,
        bytes: &[u8],
        views: Vec<(u8, Vec<u8>)>,
        now: u64,
    ) -> io::Result<Candidate> {
        require(
            self.profile.hierarchy_admission,
            "hierarchy profile required",
        )?;
        let previous = self.restore(parent)?;
        let mut graph = match self.hierarchy_cache.take() {
            Some((id, graph)) if id == parent => graph,
            _ => previous
                .rebuild_confirmation_components(v, now)
                .map_err(bad)?,
        };
        let block = Record::decode(bytes).map_err(bad)?;
        if graph.indexed_history() && graph.overlay_records() >= self.hierarchy_window {
            graph = previous
                .rebuild_confirmation_components(v, now)
                .map_err(bad)?;
        }
        let prepared = match graph.prepare_for_runtime(v, &block, &views, now) {
            // Resource bounds apply to the imported overlay as well as to each
            // candidate. Retire a full overlay once, then validate the identical
            // candidate against the same persisted parent. Candidate limits are
            // unchanged and a second failure remains an error.
            Err(crate::Error::ResourceLimit(_))
                if graph.indexed_history() && graph.overlay_records() > 0 =>
            {
                graph = previous
                    .rebuild_confirmation_components(v, now)
                    .map_err(bad)?;
                graph
                    .prepare_for_runtime(v, &block, &views, now)
                    .map_err(bad)?
            }
            result => result.map_err(bad)?,
        };
        let counts = (
            prepared.newly_confirmed().len(),
            prepared.inbound_for_execution().len(),
            prepared.context_components(),
        );
        let zone_entropy =
            crate::scoring::native_entropy_components(v, &previous.history, &block, 2)
                .map_err(bad)?;
        let hierarchy_delta = prepared.durable_delta().map_err(bad)?;
        let mut candidate = self.execute_inner(
            v,
            parent,
            bytes,
            &[],
            None,
            views,
            Some(counts),
            Some(previous),
            hierarchy_delta,
            Some(&prepared),
        )?;
        graph.commit(prepared).map_err(bad)?;
        // Only a successful durable stage can become the next reusable prefix.
        // Any error drops the prefix; switching branches or reopening rebuilds it.
        candidate.zone_entropy = Some(zone_entropy);
        self.hierarchy_cache = Some((candidate.checkpoint, graph));
        Ok(candidate)
    }
    fn execute_inner(
        &mut self,
        verifier: &mut Verifier,
        parent: Id,
        bytes: &[u8],
        inbound: &[Transaction],
        now: Option<u64>,
        views: Vec<(u8, Vec<u8>)>,
        confirmation_counts: Option<(usize, usize, usize)>,
        restored: Option<Restored>,
        hierarchy_delta: Vec<u8>,
        prepared: Option<&crate::confirmation::PreparedConfirmation>,
    ) -> io::Result<Candidate> {
        let block = Record::decode(bytes).map_err(bad)?;
        block.verify_binding().map_err(bad)?;
        let work_hash = verifier
            .verify_header_proof(block.header())
            .map_err(bad)?
            .hash();
        // Only the private hierarchy caller can supply this value, freshly
        // restored and checksummed during this same append. No cross-call
        // execution state or integrity result is cached.
        let mut previous = match restored {
            Some(state) => {
                require(state.checkpoint == parent, "restored parent binding")?;
                state
            }
            None => self.restore(parent)?,
        };
        require(
            block.parents[2] == previous.anchor.block,
            "private candidate parent",
        )?;
        if let Some(now) = now {
            crate::zone_admission::inspect_local(verifier, &previous.history, &block, now)
                .map_err(bad)?;
        }
        let before = previous.state.clone();
        require(
            prepared.is_some() == confirmation_counts.is_some(),
            "hierarchy preparation required",
        )?;
        let execution_profile = Profile::from_private(&self.profile).map_err(bad)?;
        let result = match prepared {
            Some(prepared) => zone_execution::execute_prepared_components(
                verifier,
                &mut previous.state,
                &previous.history,
                &previous.past,
                &block,
                &execution_profile,
                prepared,
            ),
            None => zone_execution::native_private_components(
                verifier,
                &mut previous.state,
                &previous.history,
                &previous.past,
                &block,
                &execution_profile,
                inbound,
            ),
        }
        .map_err(bad)?;
        require(
            !result.fully_validated,
            "unexpected private admission authority",
        )?;
        let anchor = state_image::Anchor {
            profile: self.profile.fingerprint,
            block: block.id(),
            parent: block.parents[2],
            height: block.number(2).map_err(bad)?,
            timestamp: fields::some(block.header().wire().time, "missing time").map_err(bad)?,
        };
        let height = block.number(2).map_err(bad)?;
        let receipts = result
            .receipts
            .iter()
            .map(|r| r.typed_rlp().map_err(bad))
            .collect::<io::Result<Vec<_>>>()?;
        let checkpoint = self.stage_state(
            Some(parent),
            block.clone(),
            anchor,
            receipts,
            result.created_keys,
            result.deleted_hashes,
            views,
            &before,
            &previous.state,
            hierarchy_delta,
        )?;
        Ok(Candidate {
            checkpoint,
            block: block.id(),
            height,
            work_hash,
            header_components_checked: now.is_some() || confirmation_counts.is_some(),
            inbound_derived: now.is_some() || confirmation_counts.is_some(),
            confirmed_etxs: confirmation_counts.map_or(0, |n| n.0),
            execution_inbound_etxs: confirmation_counts.map_or(0, |n| n.1),
            hierarchy_contexts: confirmation_counts.map_or(0, |n| n.2),
            fully_validated: false,
            zone_entropy: None,
        })
    }
    fn stage_state(
        &mut self,
        parent: Option<Id>,
        record: Record,
        anchor: state_image::Anchor,
        receipts: Vec<Vec<u8>>,
        created: Vec<(OutPoint, u8)>,
        deleted: Vec<Hash>,
        views: Vec<(u8, Vec<u8>)>,
        before: &State,
        after: &State,
        hierarchy_delta: Vec<u8>,
    ) -> io::Result<Id> {
        match &mut self.journal {
            Store::Images(j) => {
                let (image, reference) =
                    state_image::encode(anchor, &after.world, &after.utxos).map_err(bad)?;
                let payload = encode(&Data {
                    reference,
                    record: record.clone(),
                    image: &image,
                    receipts,
                    created,
                    deleted,
                    views,
                    hierarchy_delta: &hierarchy_delta,
                })?;
                let cp = Checkpoint {
                    block: record.id(),
                    height: record.number(2).map_err(bad)?,
                    parent,
                    payload,
                };
                decode(&cp, &self.profile)?;
                j.stage(&cp)
            }
            Store::Keyed(k) => {
                k.check_file()?;
                let head = k.database.head()?;
                let profile = &self.profile;
                let height = record.number(2).map_err(bad)?;
                let block = record.id();
                let commit = k
                    .database
                    .commit_prepared(head, parent, block, false, |w| {
                        let commitment = quai_execution::state_store::apply_delta(
                            w,
                            &before.world,
                            &before.utxos,
                            &after.world,
                            &after.utxos,
                        )?;
                        let reference = state_image::Reference {
                            digest: commitment.digest,
                            anchor,
                            roots: commitment.roots,
                        };
                        let payload = encode_mode(
                            &Data {
                                reference,
                                record,
                                image: &[],
                                receipts,
                                created,
                                deleted,
                                views,
                                hierarchy_delta: &hierarchy_delta,
                            },
                            true,
                        )?;
                        decode(
                            &Checkpoint {
                                block,
                                parent,
                                height,
                                payload: payload.clone(),
                            },
                            profile,
                        )?;
                        Ok(payload)
                    })?;
                Ok(commit.version.id)
            }
        }
    }
    pub fn indexed_ancestry(&self) -> bool {
        matches!(self.journal, Store::Keyed(_))
    }
    pub fn lazy_accounts(&self) -> bool {
        self.lazy_qi()
    }
    pub fn lazy_storage(&self) -> bool {
        self.lazy_qi()
    }
    pub fn lazy_etx(&self) -> bool {
        self.lazy_qi()
    }
    pub fn lazy_lockups(&self) -> bool {
        self.lazy_qi()
    }
    pub fn lazy_qi(&self) -> bool {
        matches!(self.journal, Store::Keyed(_))
    }
    pub fn storage_backend(&self) -> &'static str {
        self.journal.backend()
    }
    pub fn durable_head(&self) -> io::Result<Id> {
        self.journal
            .durable_head()?
            .ok_or_else(|| bad("missing durable branch head"))
    }
    /// Explicit private execution-head choice. This does not run PoEM fork
    /// choice or publish a validated canonical chain/RPC generation.
    pub fn select_execution_head(&mut self, expected: Id, next: Id) -> io::Result<()> {
        require(self.head() == expected, "stale private execution head")?;
        self.restore(next)?;
        self.journal.publish(Some(expected), next)
    }
    pub(crate) fn inspect_checkpoint(
        &self,
        id: Id,
    ) -> io::Result<(
        Option<Id>,
        state_image::Reference,
        Vec<u8>,
        Vec<(u8, Vec<u8>)>,
    )> {
        let cp = self.journal.load(id)?;
        let d = decode(&cp, &self.profile)?;
        Ok((cp.parent, d.reference, d.record.bytes().to_vec(), d.views))
    }
    pub fn reorg_path(&self, next: Id) -> io::Result<(Vec<Id>, Vec<Id>)> {
        self.restore(next)?;
        self.journal.reorg_path(next)
    }
}
impl Restored {
    pub fn indexed_ancestry(&self) -> bool {
        self.source.is_some()
    }
    pub fn loaded_ancestry_records(&self) -> usize {
        self.source
            .as_ref()
            .map_or(self.history.len(), |s| s.loaded_records())
    }
    /// Recheck component witnesses from this trusted local branch. This does
    /// not establish network ancestry or rerun historical state execution.
    pub fn rebuild_confirmation_components(
        &self,
        v: &mut Verifier,
        now: u64,
    ) -> Result<crate::confirmation::Hierarchy, crate::Error> {
        let p = PrivateChain::hierarchy_for_fingerprint(self.anchor.profile)?;
        if let Some(source) = &self.source {
            return crate::confirmation::Hierarchy::from_reader_for_chain(source.clone(), p);
        }
        if self.anchor.profile != p.fingerprint {
            return Err(crate::Error::Invalid("confirmation recovery profile"));
        }
        let mut cursor = self.history.get(&self.anchor.block)?;
        let mut rows = vec![];
        while cursor.id() != p.genesis[2].id() {
            if rows.len() >= MAX_RECORDS {
                return Err(crate::Error::ResourceLimit("confirmation recovery depth"));
            }
            let parent = cursor.parents[2];
            rows.push(cursor);
            cursor = self.history.get(&parent)?;
        }
        let mut graph = crate::confirmation::Hierarchy::new_for_chain(p)?;
        for row in rows.into_iter().rev() {
            let views = self
                .confirmation_views
                .get(&row.id())
                .ok_or(crate::Error::Unavailable("persisted hierarchy witnesses"))?;
            let prepared = graph.prepare_for_runtime(v, &row, views, now)?;
            graph.commit(prepared)?;
        }
        Ok(graph)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn memo_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "quai-history-memo-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    fn memo_append(
        branch: &mut PrivateBranch,
        verifier: &mut Verifier,
        parent: Id,
        row: &serde_json::Value,
        now: u64,
    ) -> Id {
        let raw = quai_primitives::decode_hex(row["work_object"].as_str().unwrap()).unwrap();
        let views = row["contexts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| {
                let c = r["context"].as_u64().unwrap() as u8;
                (c < 2).then(|| {
                    (
                        c,
                        quai_primitives::decode_hex(r["work_object"].as_str().unwrap()).unwrap(),
                    )
                })
            })
            .collect();
        branch
            .execute_hierarchy_candidate(verifier, parent, &raw, views, now)
            .unwrap()
            .checkpoint
    }
    #[test]
    fn shared_decode_memo_reuses_verified_records_but_never_crosses_fork_membership() {
        let path = memo_path("fork");
        let mut branch = PrivateBranch::open_hierarchy_database(&path).unwrap();
        let genesis = branch.head();
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json"))
                .unwrap();
        let now = corpus["now"].as_u64().unwrap();
        let mut verifier = Verifier::default();
        let mut ends = vec![];
        for rows in corpus["branches"].as_array().unwrap() {
            let mut parent = genesis;
            for row in &rows["vectors"].as_array().unwrap()[..10] {
                parent = memo_append(&mut branch, &mut verifier, parent, row, now);
            }
            ends.push(parent);
        }
        let b = branch.restore(ends[1]).unwrap();
        let id = b.anchor.block;
        let first = b.history.get(&id).unwrap();
        drop(b);
        let before = branch.history_decode_stats();
        let again = branch.restore(ends[1]).unwrap();
        let second = again.history.get(&id).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let after = branch.history_decode_stats();
        assert_eq!(after.0, before.0);
        assert!(after.1 > before.1);
        drop(again);
        let a = branch.restore(ends[0]).unwrap();
        assert!(a.history.get(&id).is_err());
        drop(a);
        let (_, _, entries, bytes) = branch.history_decode_stats();
        assert!(entries <= 32 && bytes <= 16 << 20);
        drop(branch);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn warm_decode_memo_cannot_hide_later_ancestor_payload_corruption() {
        let path = memo_path("corrupt");
        let mut branch = PrivateBranch::open_hierarchy_database(&path).unwrap();
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/confirmation.json"))
                .unwrap();
        let now = corpus["now"].as_u64().unwrap();
        let mut verifier = Verifier::default();
        let mut parent = branch.head();
        let mut versions = vec![];
        for row in &corpus["branches"][0]["vectors"].as_array().unwrap()[..2] {
            parent = memo_append(&mut branch, &mut verifier, parent, row, now);
            versions.push(parent);
        }
        let old = branch.head();
        branch.select_execution_head(old, parent).unwrap();
        let head = branch.head();
        let ancestor = branch.restore(versions[0]).unwrap().anchor.block;
        {
            let state = branch.restore(head).unwrap();
            assert_eq!(state.history.get(&ancestor).unwrap().id(), ancestor);
        }
        let digest = match &branch.journal {
            Store::Keyed(k) => {
                k.database
                    .snapshot(Some(versions[0]))
                    .unwrap()
                    .version()
                    .metadata_digest
            }
            _ => unreachable!(),
        };
        let memo = branch.decoded_history.clone();
        assert!(memo.stats().2 > 0);
        drop(branch);
        let db = redb::Database::open(path.join("state.redb")).unwrap();
        let tx = db.begin_write().unwrap();
        {
            let mut table = tx
                .open_table(redb::TableDefinition::<&[u8], &[u8]>::new(
                    "native.state.payloads.v1",
                ))
                .unwrap();
            table
                .insert(digest.as_slice(), b"damaged".as_slice())
                .unwrap();
        }
        tx.commit().unwrap();
        drop(db);
        let mut reopened = PrivateBranch::open_hierarchy_database(&path).unwrap();
        reopened.decoded_history = memo;
        let before = reopened.head();
        assert_eq!(before, head);
        {
            let state = reopened.restore(head).unwrap();
            assert!(state.history.get(&ancestor).is_err());
        }
        assert_eq!(reopened.head(), before);
        drop(reopened);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn shared_decode_memo_binds_versions_and_keeps_encoded_payload_budget() {
        let path = memo_path("version");
        let branch = PrivateBranch::open_hierarchy_database(&path).unwrap();
        let head = branch.head();
        let template = branch.restore(head).unwrap().source.unwrap().tip().unwrap();
        let bad = match &branch.journal {
            Store::Keyed(k) => {
                let snapshot = k.database.snapshot(Some(head)).unwrap();
                let payload = snapshot.metadata().unwrap();
                k.database
                    .commit(
                        Some(head),
                        Some(head),
                        [99; 32],
                        &payload,
                        false,
                        |_| Ok(()),
                    )
                    .unwrap()
                    .version
                    .id
            }
            _ => unreachable!(),
        };
        assert!(branch.restore(bad).is_err());
        let memo = reader::DecodeMemo::new();
        for i in 0u64..48 {
            let mut id = [0; 32];
            id[..8].copy_from_slice(&i.to_be_bytes());
            memo.get_for_test(branch.profile.fingerprint, id, &template, 1 << 20)
                .unwrap();
        }
        let (_, _, entries, bytes) = memo.stats();
        assert_eq!(entries, 16);
        assert_eq!(bytes, 16 << 20);
        drop(template);
        drop(branch);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn bounded_hierarchy_overlay_rebases_without_changing_native_execution() {
        let path = std::env::temp_dir().join(format!(
            "quai-hierarchy-window-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/confirmation.json"))
                .unwrap();
        let now = corpus["now"].as_u64().unwrap();
        let mut branch = PrivateBranch::open_hierarchy_database(&path).unwrap();
        branch.hierarchy_window = 4;
        let mut verifier = Verifier::default();
        let mut rebases = 0;
        for row in corpus["branches"][0]["vectors"].as_array().unwrap() {
            let raw = quai_primitives::decode_hex(row["work_object"].as_str().unwrap()).unwrap();
            let views = row["contexts"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| {
                    let c = v["context"].as_u64().unwrap() as u8;
                    (c < 2).then(|| {
                        (
                            c,
                            quai_primitives::decode_hex(v["work_object"].as_str().unwrap())
                                .unwrap(),
                        )
                    })
                })
                .collect();
            let previous = branch.head();
            let candidate = branch
                .execute_hierarchy_candidate(&mut verifier, previous, &raw, views, now)
                .unwrap();
            branch
                .select_execution_head(previous, candidate.checkpoint)
                .unwrap();
            let overlay = branch.hierarchy_cache.as_ref().unwrap().1.overlay_records();
            assert!(overlay <= 4);
            if candidate.height > 1 && overlay == 1 {
                rebases += 1;
            }
            let state = branch.restore(candidate.checkpoint).unwrap();
            assert_eq!(
                quai_primitives::encode_hex(&state.state.world.root().unwrap()),
                row["evm_root"]
            );
            assert_eq!(
                quai_primitives::encode_hex(&state.state.world.etxs().root().unwrap()),
                row["etx_root"]
            );
        }
        assert_eq!(rebases, 3);
        drop(branch);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn legacy_image_directories_are_preserved_and_refused_by_keyed_open() {
        let path = std::env::temp_dir().join(format!(
            "quai-no-migration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let old = PrivateBranch::open_hierarchy(&path).unwrap();
        let head = old.head();
        drop(old);
        assert!(PrivateBranch::open_hierarchy_database(&path).is_err());
        assert!(!path.join("state.redb").exists());
        let old = PrivateBranch::open_hierarchy(&path).unwrap();
        assert_eq!(old.head(), head);
        drop(old);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn keyed_store_recovers_committed_unselected_genesis_without_reinitializing() {
        let dir = std::env::temp_dir().join(format!(
            "quai-keyed-genesis-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let profile = PrivateChain::reviewed_hierarchy().unwrap();
        let store = Store::keyed(&dir, &profile).unwrap();
        let mut branch = PrivateBranch {
            journal: store,
            profile,
            hierarchy_cache: None,
            hierarchy_window: 64,
            decoded_history: Arc::new(reader::DecodeMemo::new()),
        };
        let record = branch.profile.genesis[2].clone();
        let anchor = state_image::Anchor {
            profile: branch.profile.fingerprint,
            block: record.id(),
            parent: record.parents[2],
            height: 0,
            timestamp: record.header().wire().time.unwrap(),
        };
        let state = State::default();
        let staged = branch
            .stage_state(
                None,
                record,
                anchor,
                vec![],
                vec![],
                vec![],
                vec![],
                &state,
                &state,
                vec![],
            )
            .unwrap();
        assert!(branch.journal.head().is_none());
        drop(branch);
        let recovered = PrivateBranch::open_hierarchy_database(&dir).unwrap();
        assert_eq!(recovered.head(), staged);
        assert_eq!(recovered.durable_head().unwrap(), staged);
        assert!(recovered
            .restore(staged)
            .unwrap()
            .state
            .world
            .is_empty()
            .unwrap());
        drop(recovered);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn private_commit_format_anchor_counts_and_legacy_payloads_fail_closed() {
        let d = std::env::temp_dir().join(format!(
            "quai-branch-codec-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let s = PrivateBranch::open(&d).unwrap();
        let cp = s.journal.load(s.head()).unwrap();
        assert!(decode(&cp, &s.profile).is_ok());
        let mut wrong = cp.clone();
        wrong.block[0] ^= 1;
        assert!(decode(&wrong, &s.profile).is_err());
        let mut wrong = cp.clone();
        wrong.height = 1;
        assert!(decode(&wrong, &s.profile).is_err());
        let mut wrong = cp.clone();
        wrong.payload[MAGIC.len() + 32] ^= 1;
        assert!(decode(&wrong, &s.profile).is_err());
        let mut wrong = cp.clone();
        wrong.payload.extend([0]);
        assert!(decode(&wrong, &s.profile).is_err());
        let mut wrong = cp.clone();
        let n = wrong.payload.len();
        wrong.payload[n - 4..].fill(255);
        assert!(decode(&wrong, &s.profile).is_err());
        let mut wrong = cp;
        wrong.payload = b"QUAI-STATE-IMAGE-1\n".to_vec();
        assert!(decode(&wrong, &s.profile).is_err());
        drop(s);
        std::fs::remove_dir_all(d).unwrap();
    }
}
