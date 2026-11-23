//! Durable replay state under `<datadir>/replay/<environment>-<genesis>/`:
//!
//! - `state-<height>.redb`: execution state (`quai_store::state_db`), the
//!   lazy World, UTXO set, ETX set and lockups. Each file is the compacted
//!   state of the checkpoint at `<height>`; it then receives the intermediate
//!   versions until the next checkpoint.
//! - `past.redb`: the redemption and trim history that execution reads at
//!   fixed depths (`historical::History`): External transactions and created
//!   UTXO keys per zone height, pruned below the deepest lock period and the
//!   deepest trim depth.
//! - `checkpoint-<height>.qrc`: see `checkpoint.rs`.
use quai_execution::{
    economic_state::{DAY, LOCK_PERIODS},
    historical,
    qi::OutPoint,
    settlement,
    state_image::Roots,
    state_store::{self, Commitment},
};
use quai_primitives::{transaction::Transaction, Hash};
use quai_store::state_db::{CopyStats, StateDatabase};
use redb::{
    Database, Durability, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// Deepest redemption lookback: the longest coinbase lock period
/// (`historical::redeem` reads `height - depth` for each lock period).
pub fn incoming_depth() -> u64 {
    LOCK_PERIODS.iter().copied().max().unwrap_or(0)
}
/// Deepest trim lookback: denomination 5 in `historical::trim`,
/// `2 * (denomination + 1) * 7 * DAY`.
pub const TRIM_DEPTH: u64 = 2 * 6 * 7 * DAY;

/// Replay directory for one chain.
pub fn chain_dir(datadir: &Path, environment: &str, genesis: &Hash) -> PathBuf {
    datadir
        .join("replay")
        .join(format!("{environment}-{}", hex8(genesis)))
}
fn hex8(h: &Hash) -> String {
    h[..4].iter().map(|b| format!("{b:02x}")).collect()
}
pub fn state_path(dir: &Path, height: u64) -> PathBuf {
    dir.join(format!("state-{height:010}.redb"))
}
/// fsync a directory so that a rename in it is durable.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

const INCOMING: TableDefinition<u64, &[u8]> = TableDefinition::new("replay.incoming.v1");
const CREATED: TableDefinition<u64, &[u8]> = TableDefinition::new("replay.created.v1");

fn encode_txs(txs: &[Transaction]) -> Vec<u8> {
    let mut out = (txs.len() as u32).to_be_bytes().to_vec();
    for tx in txs {
        let b = tx.encode();
        out.extend((b.len() as u32).to_be_bytes());
        out.extend(b);
    }
    out
}
fn decode_txs(mut raw: &[u8]) -> io::Result<Vec<Transaction>> {
    let mut take = |n: usize| -> io::Result<&[u8]> {
        if raw.len() < n {
            return Err(bad("truncated transaction list"));
        }
        let (a, b) = raw.split_at(n);
        raw = b;
        Ok(a)
    };
    let word = |b: &[u8]| -> io::Result<usize> {
        let b: [u8; 4] = b.try_into().map_err(|_| bad("transaction list word"))?;
        Ok(u32::from_be_bytes(b) as usize)
    };
    let count = word(take(4)?)?;
    let mut out = Vec::with_capacity(count.min(1 << 16));
    for _ in 0..count {
        let len = word(take(4)?)?;
        out.push(Transaction::decode(take(len)?).map_err(bad)?);
    }
    if !raw.is_empty() {
        return Err(bad("transaction list trailing bytes"));
    }
    Ok(out)
}
pub fn encode_tx_list(txs: &[Transaction]) -> Vec<u8> {
    encode_txs(txs)
}
pub fn decode_tx_list(raw: &[u8]) -> io::Result<Vec<Transaction>> {
    decode_txs(raw)
}
fn encode_created(keys: &[(OutPoint, u8)]) -> Vec<u8> {
    let mut out = (keys.len() as u32).to_be_bytes().to_vec();
    for (p, d) in keys {
        out.extend(p.hash);
        out.extend(p.index.to_be_bytes());
        out.push(*d);
    }
    out
}
fn decode_created(raw: &[u8]) -> io::Result<Vec<(OutPoint, u8)>> {
    if raw.len() < 4 {
        return Err(bad("truncated created-key list"));
    }
    let count = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
    let body = &raw[4..];
    if body.len()
        != count
            .checked_mul(35)
            .ok_or_else(|| bad("created-key count"))?
    {
        return Err(bad("created-key list width"));
    }
    Ok(body
        .chunks_exact(35)
        .map(|c| {
            (
                OutPoint {
                    hash: std::array::from_fn(|i| c[i]),
                    index: u16::from_be_bytes([c[32], c[33]]),
                },
                c[34],
            )
        })
        .collect())
}

/// External transactions and created UTXO keys of one zone height.
type Entry = (Vec<Transaction>, Vec<(OutPoint, u8)>);
/// Redemption and trim history (`historical::History`) on disk, with the
/// heights since the last flush in memory.
pub struct PastDb {
    db: Database,
    pending: BTreeMap<u64, Entry>,
}
impl PastDb {
    pub fn open(path: &Path) -> io::Result<Self> {
        let db = Database::builder()
            .set_cache_size(32 << 20)
            .create(path)
            .map_err(bad)?;
        let mut write = db.begin_write().map_err(bad)?;
        write.set_durability(Durability::Immediate).map_err(bad)?;
        write.open_table(INCOMING).map_err(bad)?;
        write.open_table(CREATED).map_err(bad)?;
        write.commit().map_err(bad)?;
        Ok(Self {
            db,
            pending: BTreeMap::new(),
        })
    }
    pub fn record(
        &mut self,
        height: u64,
        incoming: Vec<Transaction>,
        created: Vec<(OutPoint, u8)>,
    ) {
        self.pending.insert(height, (incoming, created));
    }
    /// Write the pending heights in one durable transaction.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut write = self.db.begin_write().map_err(bad)?;
        write.set_durability(Durability::Immediate).map_err(bad)?;
        {
            let mut incoming = write.open_table(INCOMING).map_err(bad)?;
            let mut created = write.open_table(CREATED).map_err(bad)?;
            for (h, (i, c)) in &self.pending {
                incoming.insert(*h, encode_txs(i).as_slice()).map_err(bad)?;
                created
                    .insert(*h, encode_created(c).as_slice())
                    .map_err(bad)?;
            }
        }
        write.commit().map_err(bad)?;
        self.pending.clear();
        Ok(())
    }
    /// Forget everything above `height` (resume from a checkpoint there).
    pub fn truncate_above(&mut self, height: u64) -> io::Result<()> {
        self.pending.retain(|h, _| *h <= height);
        let mut write = self.db.begin_write().map_err(bad)?;
        write.set_durability(Durability::Immediate).map_err(bad)?;
        {
            for table in [INCOMING, CREATED] {
                let mut t = write.open_table(table).map_err(bad)?;
                t.retain_in(height.saturating_add(1).., |_, _| false)
                    .map_err(bad)?;
            }
        }
        write.commit().map_err(bad)
    }
    /// Drop heights that no replay from a kept checkpoint can read again.
    pub fn prune(&mut self, incoming_below: u64, created_below: u64) -> io::Result<u64> {
        let mut write = self.db.begin_write().map_err(bad)?;
        write.set_durability(Durability::Immediate).map_err(bad)?;
        let mut removed = 0u64;
        {
            for (table, below) in [(INCOMING, incoming_below), (CREATED, created_below)] {
                let mut t = write.open_table(table).map_err(bad)?;
                let before = t.len().map_err(bad)?;
                t.retain_in(..below, |_, _| false).map_err(bad)?;
                removed += before - t.len().map_err(bad)?;
            }
        }
        write.commit().map_err(bad)?;
        Ok(removed)
    }
    /// Lowest and highest stored height (diagnostics and tests).
    pub fn range(&self) -> io::Result<Option<(u64, u64)>> {
        let read = self.db.begin_read().map_err(bad)?;
        let t = read.open_table(INCOMING).map_err(bad)?;
        let first = t.first().map_err(bad)?.map(|(k, _)| k.value());
        let last = t.last().map_err(bad)?.map(|(k, _)| k.value());
        Ok(first.zip(last))
    }
    fn read(&self, table: TableDefinition<u64, &[u8]>, height: u64) -> io::Result<Option<Vec<u8>>> {
        let read = self.db.begin_read().map_err(bad)?;
        let t = read.open_table(table).map_err(bad)?;
        Ok(t.get(height).map_err(bad)?.map(|v| v.value().to_vec()))
    }
}
impl historical::History for PastDb {
    fn incoming_at(&self, height: u64) -> Result<Vec<Transaction>, settlement::Error> {
        if let Some((i, _)) = self.pending.get(&height) {
            return Ok(i.clone());
        }
        match self.read(INCOMING, height) {
            Ok(Some(raw)) => decode_txs(&raw)
                .map_err(|_| settlement::Error::Invalid("replay redemption history encoding")),
            Ok(None) => Err(settlement::Error::Invalid(
                "missing replay redemption ancestry",
            )),
            Err(_) => Err(settlement::Error::Invalid("replay redemption history read")),
        }
    }
    fn created_at(&self, height: u64) -> Result<Vec<(OutPoint, u8)>, settlement::Error> {
        if let Some((_, c)) = self.pending.get(&height) {
            return Ok(c.clone());
        }
        match self.read(CREATED, height) {
            Ok(Some(raw)) => decode_created(&raw)
                .map_err(|_| settlement::Error::Invalid("replay trim history encoding")),
            Ok(None) => Err(settlement::Error::Invalid("missing replay trim ancestry")),
            Err(_) => Err(settlement::Error::Invalid("replay trim history read")),
        }
    }
}

/// The execution state database of one replay directory.
pub struct StateStore {
    dir: PathBuf,
    profile: Hash,
    db: StateDatabase,
    /// Checkpoint height whose state file is the working database.
    pub file_height: u64,
    /// Last committed version and its commitment.
    pub version: Hash,
    pub commitment: Commitment,
}
pub fn profile(environment: &str, genesis: &Hash) -> Hash {
    let mut h = Sha256::new();
    h.update(b"quai-node-replay-state-1\n");
    h.update(environment.as_bytes());
    h.update(genesis);
    h.finalize().into()
}
fn empty_commitment_check(c: &Commitment) -> bool {
    c.roots.accounts == 0 && c.roots.utxo_count == 0
}
impl StateStore {
    /// A new state file at height 0 holding the empty genesis state.
    pub fn create_genesis(dir: &Path, profile: Hash, genesis: Hash) -> io::Result<Self> {
        let path = state_path(dir, 0);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        let db = StateDatabase::open(&path, profile)?;
        let empty = quai_consensus::zone_execution::State::default();
        let mut commitment = None;
        let commit = db.commit_prepared(db.head()?, None, genesis, true, |w| {
            let c = state_store::apply_delta(
                w,
                &empty.world,
                &empty.utxos,
                &empty.world,
                &empty.utxos,
            )?;
            let meta = metadata(0, &genesis, &c);
            commitment = Some(c);
            Ok(meta)
        })?;
        let commitment = commitment.ok_or_else(|| bad("genesis state commitment"))?;
        if !empty_commitment_check(&commitment) {
            return Err(bad("genesis state is not empty"));
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            profile,
            db,
            file_height: 0,
            version: commit.version.id,
            commitment,
        })
    }
    /// Open the state file of the checkpoint at `height` as the working database.
    pub fn open(
        dir: &Path,
        profile: Hash,
        height: u64,
        version: Hash,
        commitment: Commitment,
    ) -> io::Result<Self> {
        let path = state_path(dir, height);
        if !path.is_file() {
            return Err(bad(format!("missing state file {}", path.display())));
        }
        let db = StateDatabase::open(&path, profile)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            profile,
            db,
            file_height: height,
            version,
            commitment,
        })
    }
    /// Restore the lazy state of the last committed version.
    pub fn restore(&self) -> io::Result<quai_consensus::zone_execution::State> {
        self.restore_version(self.version, &self.commitment)
    }
    /// Restore the lazy state of an earlier committed version without changing
    /// the store's active head version (used for isolated tip-fork validation).
    pub fn restore_version(
        &self,
        version: Hash,
        commitment: &Commitment,
    ) -> io::Result<quai_consensus::zone_execution::State> {
        let snapshot = self.db.snapshot(Some(version))?;
        let (world, utxos) =
            state_store::restore_sparse(Arc::new(Mutex::new(snapshot)), commitment)?;
        Ok(quai_consensus::zone_execution::State { world, utxos })
    }
    /// Persist the changes from `base` to `current` as the version of `block`.
    pub fn commit(
        &mut self,
        height: u64,
        block: Hash,
        base: &quai_consensus::zone_execution::State,
        current: &quai_consensus::zone_execution::State,
    ) -> io::Result<()> {
        let mut commitment = None;
        // The index key binds the parent version too, so runs that commit at
        // different heights never claim the same key for different versions.
        let key: Hash = Sha256::new()
            .chain_update(block)
            .chain_update(self.version)
            .finalize()
            .into();
        let commit =
            self.db
                .commit_prepared(self.db.head()?, Some(self.version), key, true, |w| {
                    let c = state_store::apply_delta(
                        w,
                        &base.world,
                        &base.utxos,
                        &current.world,
                        &current.utxos,
                    )?;
                    let meta = metadata(height, &block, &c);
                    commitment = Some(c);
                    Ok(meta)
                })?;
        self.version = commit.version.id;
        self.commitment = commitment.ok_or_else(|| bad("state commitment"))?;
        Ok(())
    }
    /// Copy the last committed version into `state-<height>.redb` without
    /// older nodes, durably, and make it the working database.
    pub fn compact_to(&mut self, height: u64) -> io::Result<CopyStats> {
        let target = state_path(&self.dir, height);
        if height == self.file_height {
            return Ok(CopyStats::default());
        }
        let tmp = target.with_extension("redb.tmp");
        if tmp.exists() {
            fs::remove_file(&tmp)?;
        }
        let stats = state_store::compact(&self.db, self.version, &tmp)?;
        fault(&self.dir, "state-copied", height)?;
        fs::rename(&tmp, &target)?;
        sync_dir(&self.dir)?;
        let db = StateDatabase::open(&target, self.profile)?;
        if db.head()? != Some(self.version) {
            return Err(bad("compacted state head"));
        }
        self.db = db;
        self.file_height = height;
        Ok(stats)
    }
    pub fn roots(&self) -> &Roots {
        &self.commitment.roots
    }
}
fn metadata(height: u64, block: &Hash, c: &Commitment) -> Vec<u8> {
    let mut out = b"quai-node-replay-version-1\n".to_vec();
    out.extend(height.to_be_bytes());
    out.extend(block);
    out.extend(c.digest);
    out
}

static FAULTS: Mutex<Vec<(PathBuf, String, u64)>> = Mutex::new(Vec::new());
/// Test hook: the next `fault(dir, point, height)` returns an error, as if
/// the process stopped there. Scoped to one directory.
pub fn inject_fault(dir: &Path, point: &str, height: u64) {
    FAULTS.lock().unwrap_or_else(|e| e.into_inner()).push((
        dir.to_path_buf(),
        point.into(),
        height,
    ));
}
/// Fault points in the checkpoint write. `QUAI_NODE_TEST_FAULT=<point>@<height>`
/// ends the process there (exit status 75), to test that a killed
/// checkpoint write leaves the previous checkpoint usable.
pub fn fault(dir: &Path, point: &str, height: u64) -> io::Result<()> {
    if std::env::var("QUAI_NODE_TEST_FAULT").is_ok_and(|v| v == format!("{point}@{height}")) {
        eprintln!("quai-node: test fault at {point}@{height}");
        std::process::exit(75);
    }
    let mut faults = FAULTS.lock().map_err(bad)?;
    if let Some(i) = faults
        .iter()
        .position(|(d, p, h)| d == dir && p == point && *h == height)
    {
        faults.remove(i);
        return Err(io::Error::other(format!("test fault at {point}@{height}")));
    }
    Ok(())
}
