//! Profile-scoped native admission, fork choice and atomic canonical snapshots.
//! This early private chain is separate from global full-block admission/RPC.
use crate::{
    frontier::{Frontier, Tip},
    history::Record,
    private_branch::PrivateBranch,
    private_profile::PrivateChain,
    rules,
    verifier::{HeaderProofMemo, Verifier, WorkMemoStats},
};
use crypto_bigint::{Encoding, U512};
use quai_execution::state_image::Roots;
use quai_primitives::Hash;
use quai_store::admission::{Catalog, Head as CatalogHead, Snapshot as CatalogSnapshot};
mod catalog;
use catalog::Source;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
const MAGIC: &[u8] = b"QUAI-PRIVATE-CANONICAL-5\n";
pub const MAX_BLOCK_BYTES: usize = 3 << 20;
const MAX_RECORD_BYTES: usize = 3 * MAX_BLOCK_BYTES + 1024;
static SERIAL: AtomicU64 = AtomicU64::new(1);
fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}
fn need(ok: bool, e: &str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(bad(e))
    }
}
fn digest(b: &[u8]) -> Hash {
    Sha256::digest(b).into()
}
fn read(path: &Path, max: usize) -> io::Result<Vec<u8>> {
    let m = fs::symlink_metadata(path)?;
    need(
        m.is_file() && !m.file_type().is_symlink() && m.len() <= max as u64,
        "canonical object type/size",
    )?;
    let mut b = vec![];
    File::open(path)?.take(max as u64 + 1).read_to_end(&mut b)?;
    need(b.len() <= max, "canonical object grew")?;
    Ok(b)
}
fn atomic(path: &Path, b: &[u8]) -> io::Result<()> {
    if path.exists() {
        return need(
            read(path, MAX_RECORD_BYTES)? == b,
            "immutable canonical object conflict",
        );
    }
    let parent = path
        .parent()
        .ok_or_else(|| bad("canonical parent directory"))?;
    let tmp = parent.join(format!(
        ".{}-{}-{}.pending",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    f.write_all(b)?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    File::open(parent)?.sync_all()
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockBundle {
    pub zone: Vec<u8>,
    pub contexts: Vec<(u8, Vec<u8>)>,
}
impl BlockBundle {
    fn normalize(self) -> io::Result<Self> {
        let record = self.checked()?;
        let mut contexts = vec![];
        for (c, bytes) in self.contexts {
            let r = Record::decode(&bytes).map_err(bad)?;
            contexts.push((c, r.bytes().to_vec()));
        }
        let out = Self {
            zone: record.bytes().to_vec(),
            contexts,
        };
        out.checked()?;
        Ok(out)
    }
    pub fn checked(&self) -> io::Result<Record> {
        need(
            self.zone.len() <= MAX_BLOCK_BYTES && self.contexts.len() <= 2,
            "private bundle bound",
        )?;
        let mut last = None;
        for (c, b) in &self.contexts {
            need(
                *c < 2 && last.is_none_or(|v| v < *c) && b.len() <= MAX_BLOCK_BYTES,
                "private context witness bound/order",
            )?;
            last = Some(*c);
        }
        let r = Record::decode(&self.zone).map_err(bad)?;
        if r.number(2).map_err(bad)? == 0 {
            need(
                r.id() == PrivateChain::hierarchy_template().map_err(bad)?.genesis[2].id()
                    && self.contexts.is_empty(),
                "untrusted canonical genesis",
            )?;
        } else {
            check_scope(&r)?;
        }
        Ok(r)
    }
}
/// Scope is deliberately closed: no later-fork, expansion or mixed transaction
/// domain can become canonical by falling through a component capability limit.
pub fn check_scope(r: &Record) -> io::Result<()> {
    let h = r.header().wire();
    let b = r.object().header().wire();
    need(
        r.prime() < 262000
            && b.expansion_number == Some(0)
            && h.aux_pow.is_none()
            && h.location.as_ref().is_some_and(|l| l.value == [0, 0]),
        "outside reviewed early canonical profile",
    )
}
struct Stored {
    seq: u64,
    checkpoint: Hash,
    parent: Option<Hash>,
    score: U512,
    roots: Roots,
    bundle: BlockBundle,
    frontier: Frontier,
}
fn blob(out: &mut Vec<u8>, b: &[u8]) {
    out.extend((b.len() as u32).to_be_bytes());
    out.extend(b);
}
fn encode(s: &Stored, policy: Hash) -> Vec<u8> {
    let mut b = MAGIC.to_vec();
    b.extend(policy);
    b.extend(s.seq.to_be_bytes());
    b.extend(s.checkpoint);
    b.push(u8::from(s.parent.is_some()));
    b.extend(s.parent.unwrap_or([0; 32]));
    b.extend(s.score.to_be_bytes());
    for h in [s.roots.evm, s.roots.etx, s.roots.utxo] {
        b.extend(h);
    }
    b.extend(s.roots.accounts.to_be_bytes());
    b.extend(s.roots.utxo_count.to_be_bytes());
    for location in [vec![], vec![0], vec![0, 0]] {
        let tip = &s.frontier.tips()[&location];
        b.extend(tip.id);
        for number in tip.numbers {
            b.extend(number.to_be_bytes());
        }
        b.extend(tip.entropy.to_be_bytes());
    }
    blob(&mut b, &s.bundle.zone);
    b.push(s.bundle.contexts.len() as u8);
    for (c, v) in &s.bundle.contexts {
        b.push(*c);
        blob(&mut b, v);
    }
    b.extend(digest(&b));
    b
}
struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self
            .p
            .checked_add(n)
            .ok_or_else(|| bad("canonical offset"))?;
        let b = self
            .b
            .get(self.p..end)
            .ok_or_else(|| bad("canonical truncation"))?;
        self.p = end;
        Ok(b)
    }
    fn fixed<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    fn blob(&mut self) -> io::Result<Vec<u8>> {
        let n = u32::from_be_bytes(self.fixed()?) as usize;
        need(n <= MAX_BLOCK_BYTES, "canonical body bound")?;
        Ok(self.take(n)?.to_vec())
    }
}
fn decode(b: &[u8], policy: Hash) -> io::Result<Stored> {
    need(
        b.len() >= 32 && b.len() <= MAX_RECORD_BYTES,
        "canonical record size",
    )?;
    let end = b.len() - 32;
    need(digest(&b[..end]) == b[end..], "canonical record checksum")?;
    let mut r = Reader { b: &b[..end], p: 0 };
    need(
        r.take(MAGIC.len())? == MAGIC && r.fixed::<32>()? == policy,
        "canonical policy mismatch",
    )?;
    let seq = u64::from_be_bytes(r.fixed()?);
    let checkpoint = r.fixed()?;
    let flag = r.take(1)?[0];
    need(flag <= 1, "canonical parent flag")?;
    let raw = r.fixed()?;
    need(flag != 0 || raw == [0; 32], "canonical empty parent")?;
    let parent = (flag == 1).then_some(raw);
    let score = U512::from_be_bytes(r.fixed()?);
    let roots = Roots {
        evm: r.fixed()?,
        etx: r.fixed()?,
        utxo: r.fixed()?,
        accounts: u64::from_be_bytes(r.fixed()?),
        utxo_count: u64::from_be_bytes(r.fixed()?),
    };
    let mut tips = vec![];
    for location in [vec![], vec![0], vec![0, 0]] {
        let id = r.fixed()?;
        let numbers = [
            u64::from_be_bytes(r.fixed()?),
            u64::from_be_bytes(r.fixed()?),
            u64::from_be_bytes(r.fixed()?),
        ];
        let entropy = rules::Uint::from_be_bytes(r.fixed()?);
        tips.push((
            location,
            Tip {
                id,
                numbers,
                entropy,
            },
        ));
    }
    let frontier = Frontier::from_imported(0, tips).map_err(bad)?;
    need(frontier.total() == score, "stored frontier score")?;
    let zone = r.blob()?;
    let n = r.take(1)?[0];
    need(n <= 2, "canonical witness count")?;
    let mut contexts = vec![];
    for _ in 0..n {
        let c = r.take(1)?[0];
        contexts.push((c, r.blob()?));
    }
    need(r.p == end, "canonical trailing bytes")?;
    Ok(Stored {
        seq,
        checkpoint,
        parent,
        score,
        roots,
        bundle: BlockBundle { zone, contexts },
        frontier,
    })
}
#[derive(Clone)]
struct Entry {
    receipt: Hash,
    parent: Option<Hash>,
    seq: u64,
    checkpoint: Hash,
    block: Hash,
    height: u64,
    roots: Roots,
    frontier: Frontier,
}
/// A selected generation retains one receipt and an immutable indexed reader,
/// rather than ownership of the complete historical entry graph.
#[derive(Clone)]
pub struct CanonicalView {
    tip: Arc<Entry>,
    source: Arc<Source>,
    profile: Hash,
}
impl CanonicalView {
    pub fn block(&self) -> Hash {
        self.tip.block
    }
    pub fn checkpoint(&self) -> Hash {
        self.tip.checkpoint
    }
    pub fn height(&self) -> u64 {
        self.tip.height
    }
    pub fn sequence(&self) -> u64 {
        self.tip.seq
    }
    pub fn roots(&self) -> &Roots {
        &self.tip.roots
    }
    pub fn score(&self) -> U512 {
        self.tip.frontier.total()
    }
    pub fn profile(&self) -> Hash {
        self.profile
    }
    pub fn hash_at(&self, height: u64) -> io::Result<Option<Hash>> {
        Ok(self
            .source
            .ancestor(self.tip.receipt, height)?
            .map(|e| e.block))
    }
    pub fn roots_at(&self, height: u64) -> io::Result<Option<Roots>> {
        Ok(self
            .source
            .ancestor(self.tip.receipt, height)?
            .map(|e| e.roots.clone()))
    }
    pub fn global_consensus_qualified(&self) -> bool {
        false
    }
}
/// Membership is pinned to a local admitted generation, including its known
/// competing forks. Acquisition can query individual hashes without an all-history set.
#[derive(Clone)]
pub struct AdmittedView {
    source: Arc<Source>,
    count: u64,
}
impl AdmittedView {
    pub fn contains(&self, block: &Hash) -> io::Result<bool> {
        Ok(self.source.by_block(*block)?.is_some())
    }
    pub fn len(&self) -> u64 {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}
/// Privately constructed after native hierarchy and execution validation.
/// It is scoped to this writer and cannot be supplied as a peer claim.
pub struct PrivateAdmittedBlock {
    owner: u64,
    stored: Stored,
    block: Hash,
    height: u64,
    parent: Arc<Entry>,
}
impl PrivateAdmittedBlock {
    pub fn block(&self) -> Hash {
        self.block
    }
    pub fn height(&self) -> u64 {
        self.height
    }
    pub fn score(&self) -> U512 {
        self.stored.score
    }
}
#[derive(Debug)]
pub struct Import {
    pub block: Hash,
    pub selected: bool,
    pub duplicate: bool,
    pub detached: Vec<Hash>,
    pub attached: Vec<Hash>,
    pub global_consensus_qualified: bool,
}
pub struct PrivateCanonical {
    branch: PrivateBranch,
    activated_execution: bool,
    all_etx_execution: bool,
    profile: Hash,
    policy: Hash,
    owner: u64,
    verifier: Verifier,
    catalog: Catalog,
    source: Arc<Source>,
    tip: Arc<Entry>,
    head: CatalogHead,
    poisoned: bool,
    durable_admissions: Arc<AtomicU64>,
}
fn genesis_frontier(record: &Record) -> io::Result<Frontier> {
    let tip = Tip {
        id: record.id(),
        numbers: record.numbers,
        entropy: rules::Uint::ZERO,
    };
    Frontier::from_imported(
        0,
        vec![
            (vec![], tip.clone()),
            (vec![0], tip.clone()),
            (vec![0, 0], tip),
        ],
    )
    .map_err(bad)
}
impl PrivateCanonical {
    pub fn open(dir: &Path, now: u64) -> io::Result<Self> {
        Self::open_profile(dir, now, PrivateChain::hierarchy_template().map_err(bad)?)
    }
    /// Experimental genesis-only path across transaction activation. Every
    /// parent requires a local native admission receipt; no state import exists.
    pub fn open_execution_v2(dir: &Path, now: u64) -> io::Result<Self> {
        Self::open_profile(dir, now, PrivateChain::execution_template().map_err(bad)?)
    }
    /// Genesis-only runtime for all defined incoming ETX kinds. Its immutable
    /// policy is distinct from v2 and cannot open or import its local state.
    pub fn open_execution_v3(dir: &Path, now: u64) -> io::Result<Self> {
        Self::open_profile(dir, now, PrivateChain::all_etx_template().map_err(bad)?)
    }
    pub fn profile_name(&self) -> &'static str {
        if self.all_etx_execution {
            "private-execution-v3"
        } else if self.activated_execution {
            "private-execution-v2"
        } else {
            "private-hierarchy-v1"
        }
    }
    pub fn activated_execution(&self) -> bool {
        self.activated_execution
    }
    pub fn all_etx_execution(&self) -> bool {
        self.all_etx_execution
    }
    fn open_profile(dir: &Path, now: u64, p: Arc<PrivateChain>) -> io::Result<Self> {
        let policy = digest(&[MAGIC, p.fingerprint.as_slice()].concat());
        if !dir.exists() {
            fs::create_dir_all(dir)?;
        }
        need(
            fs::symlink_metadata(dir)?.is_dir()
                && !fs::symlink_metadata(dir)?.file_type().is_symlink(),
            "canonical directory type",
        )?;
        let marker = dir.join("CANONICAL-PROFILE");
        if !marker.exists() {
            need(
                fs::read_dir(dir)?.next().is_none(),
                "refusing automatic canonical migration",
            )?;
            atomic(&marker, &policy)?;
        }
        need(read(&marker, 32)? == policy, "canonical directory profile")?;
        for entry in fs::read_dir(dir)? {
            need(
                matches!(
                    entry?.file_name().to_str(),
                    Some("CANONICAL-PROFILE" | "branch" | "catalog.redb")
                ),
                "unexpected canonical directory entry",
            )?;
        }
        let mut branch =
            PrivateBranch::open_hierarchy_database_profile(&dir.join("branch"), (*p).clone())?;
        let catalog = Catalog::open(&dir.join("catalog.redb"), policy)?;
        if catalog.snapshot()?.head().is_none() {
            let (parent, reference, zone, contexts) = branch.inspect_checkpoint(branch.head())?;
            need(
                parent.is_none() && reference.anchor.height == 0,
                "canonical genesis receipt missing",
            )?;
            let record = Record::decode(&zone).map_err(bad)?;
            need(
                record.id() == p.genesis[2].id() && contexts.is_empty(),
                "canonical genesis binding",
            )?;
            let s = Stored {
                seq: 0,
                checkpoint: branch.head(),
                parent: None,
                score: U512::ZERO,
                roots: reference.roots,
                bundle: BlockBundle { zone, contexts },
                frontier: genesis_frontier(&record)?,
            };
            catalog.append(
                None,
                None,
                record.id(),
                s.checkpoint,
                &encode(&s, policy),
                true,
            )?;
        }
        let source = Source::new(catalog.snapshot()?, policy);
        let head = source.head()?;
        let tip = source.load(head.selected)?.entry.clone();
        let genesis = source
            .ancestor(tip.receipt, 0)?
            .ok_or_else(|| bad("canonical genesis ancestor missing"))?;
        need(
            genesis.block == p.genesis[2].id()
                && genesis.seq == 0
                && genesis.frontier.total() == U512::ZERO,
            "canonical genesis receipt",
        )?;
        // A committed catalog generation can precede the execution-head CAS.
        // Both old and new selected versions must have native admission receipts.
        let (_, old, _, _) = branch.inspect_checkpoint(branch.head())?;
        need(
            source
                .by_block(old.anchor.block)?
                .is_some_and(|e| e.entry.checkpoint == branch.head()),
            "selected execution version lacks admission receipt",
        )?;
        let selected = source.load(tip.receipt)?;
        let (parent, reference, zone, contexts) = branch.inspect_checkpoint(tip.checkpoint)?;
        let expected_parent = tip
            .parent
            .map(|id| source.load(id).map(|p| p.entry.checkpoint))
            .transpose()?;
        need(
            parent == expected_parent
                && reference.roots == tip.roots
                && zone == selected.bundle.zone
                && contexts == selected.bundle.contexts,
            "canonical selected checkpoint binding",
        )?;
        let mut verifier = Verifier::default();
        branch
            .restore(tip.checkpoint)?
            .rebuild_confirmation_components(&mut verifier, now)
            .map_err(bad)?;
        if branch.head() != tip.checkpoint {
            branch.select_execution_head(branch.head(), tip.checkpoint)?;
        }
        Ok(Self {
            branch,
            profile: p.fingerprint,
            activated_execution: p.activated_hierarchy,
            all_etx_execution: p.all_etx_hierarchy,
            policy,
            owner: SERIAL.fetch_add(1, Ordering::Relaxed),
            verifier,
            catalog,
            source,
            tip,
            head,
            poisoned: false,
            durable_admissions: Arc::new(AtomicU64::new(head.count)),
        })
    }
    fn ready(&self) -> io::Result<()> {
        need(
            !self.poisoned,
            "reopen canonical chain after commit failure",
        )?;
        self.catalog.check_file()
    }
    pub fn indexed_ancestry(&self) -> bool {
        self.branch.indexed_ancestry()
    }
    pub fn indexed_catalog(&self) -> bool {
        true
    }
    /// Nonblocking publication progress. The counter advances after a complete
    /// durable catalog transaction, before its execution-head CAS may finish.
    /// It conveys neither a selected height nor global validation authority.
    pub fn admission_progress(&self) -> Arc<AtomicU64> {
        self.durable_admissions.clone()
    }
    pub fn admitted_count(&self) -> u64 {
        self.head.count
    }
    pub fn loaded_catalog_entries(&self) -> usize {
        self.source.loads()
    }
    pub fn lazy_accounts(&self) -> bool {
        self.branch.lazy_accounts()
    }
    pub fn lazy_storage(&self) -> bool {
        self.branch.lazy_storage()
    }
    pub fn lazy_etx(&self) -> bool {
        self.lazy_qi()
    }
    pub fn lazy_lockups(&self) -> bool {
        self.lazy_qi()
    }
    pub fn lazy_qi(&self) -> bool {
        self.branch.lazy_qi()
    }
    pub fn storage_backend(&self) -> &'static str {
        self.branch.storage_backend()
    }
    pub fn durable_checkpoint(&self) -> io::Result<Hash> {
        self.branch.durable_head()
    }
    pub fn view(&self) -> io::Result<CanonicalView> {
        self.ready()?;
        Ok(CanonicalView {
            tip: self.tip.clone(),
            source: self.source.clone(),
            profile: self.profile,
        })
    }
    pub fn contains(&self, hash: &Hash) -> io::Result<bool> {
        self.ready()?;
        Ok(self.source.by_block(*hash)?.is_some())
    }
    pub fn admitted_view(&self) -> io::Result<AdmittedView> {
        self.ready()?;
        Ok(AdmittedView {
            source: self.source.clone(),
            count: self.head.count,
        })
    }
    pub fn bundle(&self, hash: Hash) -> io::Result<BlockBundle> {
        self.ready()?;
        self.source
            .by_block(hash)?
            .map(|e| e.bundle.clone())
            .ok_or_else(|| bad("unknown admitted block"))
    }
    /// Process-local cryptographic hints only. These do not create receipts or
    /// change any durable state; import still runs complete profile admission.
    pub fn reuse_header_proof_memo(&mut self, memo: HeaderProofMemo) -> io::Result<usize> {
        self.ready()?;
        Ok(self.verifier.reuse_header_proof_memo(memo))
    }
    pub fn history_decode_stats(&self) -> (usize, usize, usize, usize) {
        self.branch.history_decode_stats()
    }
    pub fn full_proof_inclusion_reuses(&self) -> u64 {
        self.verifier.full_proof_inclusion_reuses()
    }
    pub fn header_proof_memo_stats(&self) -> WorkMemoStats {
        self.verifier.header_proof_memo_stats()
    }
    pub fn admit(&mut self, bundle: BlockBundle, now: u64) -> io::Result<PrivateAdmittedBlock> {
        self.ready()?;
        let bundle = bundle.normalize()?;
        let record = bundle.checked()?;
        need(
            encode_bundle_bound(&bundle) <= MAX_RECORD_BYTES,
            "canonical admission record bound",
        )?;
        need(!self.contains(&record.id())?, "block already admitted")?;
        let parent = self
            .source
            .by_block(record.parents[2])?
            .ok_or_else(|| bad("unadmitted parent"))?
            .entry
            .clone();
        let candidate = self.branch.execute_hierarchy_candidate(
            &mut self.verifier,
            parent.checkpoint,
            &bundle.zone,
            bundle.contexts.clone(),
            now,
        )?;
        need(
            candidate.header_components_checked
                && candidate.inbound_derived
                && !candidate.fully_validated,
            "private admission stages incomplete",
        )?;
        let metrics = candidate
            .zone_entropy
            .ok_or_else(|| bad("missing native score"))?;
        let frontier = parent
            .frontier
            .updated_projection(&record, metrics.order, metrics.total)
            .map_err(bad)?;
        let (cp_parent, reference, zone, views) =
            self.branch.inspect_checkpoint(candidate.checkpoint)?;
        need(
            zone == bundle.zone && views == bundle.contexts && cp_parent == Some(parent.checkpoint),
            "canonical staged binding",
        )?;
        let stored = Stored {
            seq: self.head.count,
            checkpoint: candidate.checkpoint,
            parent: cp_parent,
            score: frontier.total(),
            roots: reference.roots,
            bundle,
            frontier,
        };
        #[cfg(test)]
        fault("after-stage");
        Ok(PrivateAdmittedBlock {
            owner: self.owner,
            stored,
            block: record.id(),
            height: candidate.height,
            parent,
        })
    }
    pub fn publish(&mut self, mut permit: PrivateAdmittedBlock) -> io::Result<Import> {
        self.ready()?;
        need(permit.owner == self.owner, "foreign admission permit")?;
        need(!self.contains(&permit.block)?, "duplicate publication")?;
        permit.stored.seq = self.head.count;
        self.branch.restore(permit.stored.checkpoint)?;
        let selected = permit.score() > self.tip.frontier.total();
        let mut detached = vec![];
        let mut attached = vec![];
        if selected {
            attached.push(permit.block);
            let mut a = self.tip.clone();
            let mut b = permit.parent.clone();
            while a.height > b.height {
                detached.push(a.block);
                a = self
                    .source
                    .load(a.parent.ok_or_else(|| bad("missing detached parent"))?)?
                    .entry
                    .clone();
            }
            while b.height > a.height {
                attached.push(b.block);
                b = self
                    .source
                    .load(b.parent.ok_or_else(|| bad("missing attached parent"))?)?
                    .entry
                    .clone();
            }
            while a.block != b.block {
                detached.push(a.block);
                attached.push(b.block);
                a = self
                    .source
                    .load(a.parent.ok_or_else(|| bad("missing fork ancestor"))?)?
                    .entry
                    .clone();
                b = self
                    .source
                    .load(b.parent.ok_or_else(|| bad("missing fork ancestor"))?)?
                    .entry
                    .clone();
            }
            attached.reverse();
        }
        let raw = encode(&permit.stored, self.policy);
        need(
            raw.len() <= MAX_RECORD_BYTES,
            "canonical publication record bound",
        )?;
        self.poisoned = true;
        let receipt = self.catalog.append(
            Some(self.head),
            Some(permit.parent.receipt),
            permit.block,
            permit.stored.checkpoint,
            &raw,
            selected,
        )?;
        self.durable_admissions
            .store(receipt.sequence + 1, Ordering::Release);
        #[cfg(test)]
        fault("after-catalog");
        if selected {
            self.branch
                .select_execution_head(self.tip.checkpoint, permit.stored.checkpoint)?;
            #[cfg(test)]
            fault("after-head");
        }
        let source = Source::new(self.catalog.snapshot()?, self.policy);
        self.head = source.head()?;
        self.tip = source.load(self.head.selected)?.entry.clone();
        self.source = source;
        self.poisoned = false;
        need(
            receipt.sequence + 1 == self.head.count,
            "canonical publication generation",
        )?;
        Ok(Import {
            block: permit.block,
            selected,
            duplicate: false,
            detached,
            attached,
            global_consensus_qualified: false,
        })
    }
    pub fn import(&mut self, bundle: BlockBundle, now: u64) -> io::Result<Import> {
        self.ready()?;
        let bundle = bundle.normalize()?;
        let r = bundle.checked()?;
        if self.contains(&r.id())? {
            need(
                self.bundle(r.id())? == bundle,
                "conflicting duplicate bundle",
            )?;
            return Ok(Import {
                block: r.id(),
                selected: false,
                duplicate: true,
                detached: vec![],
                attached: vec![],
                global_consensus_qualified: false,
            });
        }
        let permit = self.admit(bundle, now)?;
        self.publish(permit)
    }
}
fn encode_bundle_bound(b: &BlockBundle) -> usize {
    b.zone.len() + b.contexts.iter().map(|(_, v)| v.len()).sum::<usize>() + 1024
}
#[cfg(test)]
fn fault(phase: &str) {
    if std::env::var("QUAI_CANONICAL_FAULT").is_ok_and(|v| v == phase) {
        std::process::exit(74);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    fn row() -> (BlockBundle, u64) {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json"))
                .unwrap();
        let r = &v["branches"][0]["vectors"][0];
        (
            BlockBundle {
                zone: decode_hex_local(r["work_object"].as_str().unwrap()),
                contexts: vec![],
            },
            v["now"].as_u64().unwrap(),
        )
    }
    fn decode_hex_local(v: &str) -> Vec<u8> {
        quai_primitives::decode_hex(v).unwrap()
    }
    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "quai-canonical-{name}-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&d).unwrap();
        d
    }
    #[test]
    fn indexed_recovery_reads_one_tip_and_excludes_competing_fork_records() {
        let dir = dir("indexed-recovery");
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json"))
                .unwrap();
        let now = corpus["now"].as_u64().unwrap();
        let mut chain = PrivateCanonical::open(&dir, now).unwrap();
        let pack = |row: &serde_json::Value| BlockBundle {
            zone: quai_primitives::decode_hex(row["work_object"].as_str().unwrap()).unwrap(),
            contexts: row["contexts"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|c| {
                    let context = c["context"].as_u64().unwrap() as u8;
                    (context < 2).then(|| {
                        (
                            context,
                            quai_primitives::decode_hex(c["work_object"].as_str().unwrap())
                                .unwrap(),
                        )
                    })
                })
                .collect(),
        };
        let first = corpus["first_branch"].as_u64().unwrap() as usize;
        let winning = corpus["winning_branch"].as_u64().unwrap() as usize;
        for row in corpus["branches"][first]["vectors"].as_array().unwrap() {
            chain.import(pack(row), now).unwrap();
        }
        let old = chain.view().unwrap();
        let pinned = chain.branch.restore(old.checkpoint()).unwrap();
        assert!(pinned.indexed_ancestry());
        assert!(pinned.state.world.etxs().is_lazy());
        assert_eq!(pinned.state.world.etxs().loaded_entries(), 0);
        assert!(pinned.state.world.lazy_lockups());
        assert_eq!(pinned.state.world.loaded_lockups(), 0);
        assert!(pinned.state.world.is_lazy());
        assert_eq!(pinned.state.world.loaded_accounts(), 0);
        assert!(pinned.state.utxos.is_lazy());
        assert_eq!(pinned.state.utxos.loaded_entries(), 0);
        assert_eq!(pinned.loaded_ancestry_records(), 1);
        assert!(pinned
            .rebuild_confirmation_components(&mut Verifier::default(), now)
            .unwrap()
            .indexed_history());
        assert_eq!(pinned.loaded_ancestry_records(), 1);
        for row in corpus["branches"][winning]["vectors"].as_array().unwrap() {
            chain.import(pack(row), now).unwrap();
        }
        let current = chain.view().unwrap();
        assert_ne!(current.block(), old.block());
        let historical = chain.branch.restore(old.checkpoint()).unwrap();
        assert_eq!(historical.loaded_ancestry_records(), 1);
        assert!(historical.history.get(&current.block()).is_err());
        assert_eq!(
            historical.history.get(&old.block()).unwrap().id(),
            old.block()
        );
        assert_eq!(pinned.history.get(&old.block()).unwrap().id(), old.block());
        let current_state = chain.branch.restore(current.checkpoint()).unwrap();
        assert!(current_state.history.get(&old.block()).is_err());
        drop(current_state);
        drop(historical);
        drop(pinned);
        drop(chain);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn canonical_commit_process_child() {
        let Ok(d) = std::env::var("QUAI_CANONICAL_CHILD_DIR") else {
            return;
        };
        let (bundle, now) = row();
        let mut c = PrivateCanonical::open(Path::new(&d), now).unwrap();
        c.import(bundle, now).unwrap();
        panic!("fault point did not exit");
    }
    #[test]
    fn canonical_crash_recovers_old_or_complete_new_generation() {
        for phase in ["after-stage", "after-catalog", "after-head"] {
            let d = dir(phase);
            let (bundle, now) = row();
            let root = PrivateCanonical::open(&d, now)
                .unwrap()
                .view()
                .unwrap()
                .block();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "private_canonical::tests::canonical_commit_process_child",
                ])
                .env("QUAI_CANONICAL_CHILD_DIR", &d)
                .env("QUAI_CANONICAL_FAULT", phase)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(74));
            let c = PrivateCanonical::open(&d, now).unwrap();
            let want = if phase == "after-stage" {
                root
            } else {
                bundle.checked().unwrap().id()
            };
            assert_eq!(c.view().unwrap().block(), want);
            assert_eq!(
                c.view().unwrap().height(),
                if phase == "after-stage" { 0 } else { 1 }
            );
            drop(c);
            fs::remove_dir_all(d).unwrap();
        }
    }
    #[test]
    fn canonical_permits_are_store_bound_and_unpublished_stages_cannot_promote() {
        let a = dir("permit-a");
        let b = dir("permit-b");
        let (bundle, now) = row();
        let mut x = PrivateCanonical::open(&a, now).unwrap();
        let progress = x.admission_progress();
        let permit = x.admit(bundle, now).unwrap();
        assert_eq!(progress.load(Ordering::Acquire), 1);
        let mut y = PrivateCanonical::open(&b, now).unwrap();
        assert!(y.publish(permit).is_err());
        assert_eq!(y.admission_progress().load(Ordering::Acquire), 1);
        assert_eq!(progress.load(Ordering::Acquire), 1);
        assert_eq!(y.view().unwrap().height(), 0);
        drop(x);
        let x = PrivateCanonical::open(&a, now).unwrap();
        assert_eq!(x.view().unwrap().height(), 0);
        drop(x);
        drop(y);
        fs::remove_dir_all(a).unwrap();
        fs::remove_dir_all(b).unwrap();
    }
    #[test]
    fn canonical_publication_rechecks_staged_objects_before_receipt() {
        let d = dir("staged-corruption");
        let (bundle, now) = row();
        let mut c = PrivateCanonical::open(&d, now).unwrap();
        let permit = c.admit(bundle, now).unwrap();
        let cp = permit.stored.checkpoint;
        // Damage the checkpoint after admission. A cached permit must not
        // publish a receipt for an object that no longer passes restoration.
        let _ = cp;
        let path = d.join("branch/state.redb");
        let saved = d.join("state-backup");
        fs::rename(&path, &saved).unwrap();
        fs::write(&path, b"corrupt").unwrap();
        assert!(c.publish(permit).is_err());
        assert_eq!(c.view().unwrap().height(), 0);
        assert_eq!(c.admitted_count(), 1);
        fs::remove_file(&path).unwrap();
        fs::rename(saved, path).unwrap();
        drop(c);
        assert_eq!(
            PrivateCanonical::open(&d, now)
                .unwrap()
                .view()
                .unwrap()
                .height(),
            0
        );
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn canonical_record_bound_rejects_oversize_input_before_staging() {
        let d = dir("record-budget");
        let (mut bundle, now) = row();
        let mut c = PrivateCanonical::open(&d, now).unwrap();
        let before = c.durable_checkpoint().unwrap();
        let admitted = c.admitted_count();
        bundle.zone.resize(MAX_BLOCK_BYTES + 1, 0);
        assert!(c.admit(bundle, now).is_err());
        assert_eq!(c.admitted_count(), admitted);
        assert_eq!(c.durable_checkpoint().unwrap(), before);
        drop(c);
        assert_eq!(
            PrivateCanonical::open(&d, now)
                .unwrap()
                .view()
                .unwrap()
                .height(),
            0
        );
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn previous_canonical_policy_is_refused_without_touching_database_files() {
        fn files(dir: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
            let mut out = std::collections::BTreeMap::new();
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    out.extend(files(&path));
                } else {
                    out.insert(path.clone(), fs::read(path).unwrap());
                }
            }
            out
        }
        let directory = dir("previous-format");
        let chain = PrivateCanonical::open(&directory, 1_800_000_000).unwrap();
        drop(chain);
        let profile = PrivateChain::hierarchy_template().unwrap();
        let old = digest(
            &[
                b"QUAI-PRIVATE-CANONICAL-4\n".as_slice(),
                profile.fingerprint.as_slice(),
            ]
            .concat(),
        );
        fs::write(directory.join("CANONICAL-PROFILE"), old).unwrap();
        let before = files(&directory);
        assert!(PrivateCanonical::open(&directory, 1_800_000_000).is_err());
        assert_eq!(files(&directory), before);
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn indexed_catalog_recovery_and_membership_remain_bound_to_their_generation() {
        let d = dir("catalog-recovery");
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json"))
                .unwrap();
        let now = corpus["now"].as_u64().unwrap();
        let pack = |row: &serde_json::Value| BlockBundle {
            zone: decode_hex_local(row["work_object"].as_str().unwrap()),
            contexts: row["contexts"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| {
                    let c = v["context"].as_u64().unwrap() as u8;
                    (c < 2).then(|| (c, decode_hex_local(v["work_object"].as_str().unwrap())))
                })
                .collect(),
        };
        let mut chain = PrivateCanonical::open(&d, now).unwrap();
        let first = corpus["first_branch"].as_u64().unwrap() as usize;
        let winning = corpus["winning_branch"].as_u64().unwrap() as usize;
        for row in corpus["branches"][first]["vectors"].as_array().unwrap() {
            chain.import(pack(row), now).unwrap();
        }
        let pinned = chain.admitted_view().unwrap();
        let old = chain.view().unwrap();
        for row in corpus["branches"][winning]["vectors"].as_array().unwrap() {
            chain.import(pack(row), now).unwrap();
        }
        let current = chain.view().unwrap();
        assert_ne!(old.block(), current.block());
        assert!(!pinned.contains(&current.block()).unwrap());
        assert!(chain
            .admitted_view()
            .unwrap()
            .contains(&old.block())
            .unwrap());
        assert_eq!(old.hash_at(old.height()).unwrap(), Some(old.block()));
        let expected = (
            current.block(),
            current.roots().clone(),
            chain.admitted_count(),
        );
        drop(current);
        drop(old);
        drop(pinned);
        drop(chain);
        let chain = PrivateCanonical::open(&d, now).unwrap();
        assert_eq!(chain.view().unwrap().block(), expected.0);
        assert_eq!(chain.view().unwrap().roots(), &expected.1);
        assert_eq!(chain.admitted_count(), expected.2);
        assert!(chain.indexed_catalog());
        assert!(chain.loaded_catalog_entries() <= 3);
        drop(chain);
        fs::remove_dir_all(d).unwrap();
    }
}
