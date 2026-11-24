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
    historical::{self, ScheduledUnlock},
    qi::OutPoint,
    settlement,
    state_image::Roots,
    state_store::{self, Commitment},
};
use quai_primitives::{
    spec::TRIM_DEPTHS,
    transaction::{External, Transaction},
    Hash,
};
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
/// Compact v2 metadata: schema version, recorded height bounds, and prune floors.
const META_V2: TableDefinition<&str, u64> = TableDefinition::new("replay.past.meta.v2");
/// Scheduled unlocks keyed by `[tier (1B: 0..=3) || unlock_height_be (8B)]`.
/// Only non-empty redeemable ETX entries are stored, and each lock tier
/// (2w / 3m / 6m / 1y) is pruned as soon as its exact unlock height passes.
const UNLOCKS_V2: TableDefinition<&[u8], &[u8]> = TableDefinition::new("replay.unlocks.v2");
/// Scheduled UTXO trims keyed by `[denom (1B: 0..=5) || trim_height_be (8B)]`.
/// Only trimmable denominations (0..=5) are stored and pruned immediately after
/// each denomination's exact trim depth (2..=12 weeks).
const TRIMS_V2: TableDefinition<&[u8], &[u8]> = TableDefinition::new("replay.trims.v2");

const PAST_SCHEMA_V2: u64 = 2;

fn scheduled_key(bucket: u8, target_height: u64) -> [u8; 9] {
    let mut k = [0u8; 9];
    k[0] = bucket;
    k[1..].copy_from_slice(&target_height.to_be_bytes());
    k
}

fn encode_unlocks(list: &[ScheduledUnlock]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + list.len() * 32);
    out.extend((list.len() as u32).to_be_bytes());
    for u in list {
        let tag = match u.lock {
            Some(l) if (l as usize) < LOCK_PERIODS.len() => l,
            _ => 4,
        };
        out.push(tag);
        out.extend(u.to);
        let vlen = u.value.len().min(32);
        out.push(vlen as u8);
        out.extend(&u.value[u.value.len() - vlen..]);
    }
    out
}

fn decode_unlocks(mut raw: &[u8]) -> io::Result<Vec<ScheduledUnlock>> {
    if raw.len() < 4 {
        return Err(bad("truncated unlock list"));
    }
    let count = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
    raw = &raw[4..];
    let mut out = Vec::with_capacity(count.min(1 << 16));
    for _ in 0..count {
        if raw.len() < 22 {
            return Err(bad("truncated unlock entry"));
        }
        let tag = raw[0];
        let to: [u8; 20] = raw[1..21].try_into().map_err(|_| bad("unlock address"))?;
        let vlen = raw[21] as usize;
        if vlen > 32 || raw.len() < 22 + vlen {
            return Err(bad("truncated unlock value"));
        }
        let value = raw[22..22 + vlen].to_vec();
        raw = &raw[22 + vlen..];
        let lock = match tag {
            0..=3 => Some(tag),
            4 => None,
            _ => return Err(bad("unknown unlock tag")),
        };
        out.push(ScheduledUnlock { to, value, lock });
    }
    if !raw.is_empty() {
        return Err(bad("unlock list trailing bytes"));
    }
    Ok(out)
}

fn encode_outpoints(list: &[OutPoint]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + list.len() * 34);
    out.extend((list.len() as u32).to_be_bytes());
    for p in list {
        out.extend(p.hash);
        out.extend(p.index.to_be_bytes());
    }
    out
}

fn decode_outpoints(raw: &[u8]) -> io::Result<Vec<OutPoint>> {
    if raw.len() < 4 {
        return Err(bad("truncated outpoint list"));
    }
    let count = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
    let body = &raw[4..];
    if body.len() != count.checked_mul(34).ok_or_else(|| bad("outpoint count"))? {
        return Err(bad("outpoint list width"));
    }
    Ok(body
        .chunks_exact(34)
        .map(|c| OutPoint {
            hash: std::array::from_fn(|i| c[i]),
            index: u16::from_be_bytes([c[32], c[33]]),
        })
        .collect())
}

fn unlock_to_synthetic_tx(u: &ScheduledUnlock) -> io::Result<Transaction> {
    let (etx_type, data) = match u.lock {
        Some(lock) => {
            let mut d = vec![0u8; 33];
            d[0] = lock;
            (1u64, d)
        }
        None => (2u64, vec![]),
    };
    Transaction::external(External {
        originating_hash: [0u8; 32],
        index: 0,
        gas: 0,
        to: u.to,
        value: u.value.clone(),
        data,
        access_list: vec![],
        sender: [0u8; 20],
        etx_type,
    })
    .map_err(bad)
}

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

/// Compact filtered consensus lookback entries recorded at one zone height:
/// - `unlocks[tier]` holds redeemable ETXs for `LOCK_PERIODS[tier]`
/// - `trims[denom]` holds created UTXO outpoints for `TRIM_DEPTHS[denom]` (0..=5)
#[derive(Clone, Debug, Default)]
struct CompactEntry {
    unlocks: [Vec<ScheduledUnlock>; 4],
    trims: [Vec<OutPoint>; 6],
}

impl CompactEntry {
    fn from_raw(incoming: &[Transaction], created: &[(OutPoint, u8)]) -> Self {
        let mut entry = Self::default();
        for tx in incoming {
            if let Some((tier, u)) = historical::extract_redeemable(tx) {
                if let Some(slot) = entry.unlocks.get_mut(tier as usize) {
                    slot.push(u);
                }
            }
        }
        for &(point, denom) in created {
            if let Some(slot) = entry.trims.get_mut(denom as usize) {
                slot.push(point);
            }
        }
        entry
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PastBounds {
    min_height: u64,
    max_height: u64,
    pruned_unlock_below: u64,
    pruned_trim_below: u64,
}

fn read_bounds(db: &Database) -> io::Result<PastBounds> {
    let read = db.begin_read().map_err(bad)?;
    let meta = read.open_table(META_V2).map_err(bad)?;
    let get =
        |k: &str| -> io::Result<u64> { Ok(meta.get(k).map_err(bad)?.map_or(0, |v| v.value())) };
    Ok(PastBounds {
        min_height: get("min_height")?,
        max_height: get("max_height")?,
        pruned_unlock_below: get("pruned_unlock_below")?,
        pruned_trim_below: get("pruned_trim_below")?,
    })
}

/// Lock-free concurrent read handle on `past.redb` (`historical::History`) so
/// the stateful mining block builder can read matured lockup redemptions and
/// UTXO trim history even when `blocks.redb` is aggressively pruned to recent
/// blocks (`--compact` / `--compact-miner` / `--prune-blocks`).
#[derive(Clone)]
pub struct PastReader {
    db: std::sync::Arc<Database>,
}

impl PastReader {
    pub fn unlocks_opt(&self, exec_height: u64) -> Option<Vec<ScheduledUnlock>> {
        let bounds = read_bounds(&self.db).ok()?;
        if bounds.max_height == 0 || exec_height < bounds.pruned_unlock_below {
            return None;
        }
        let read = self.db.begin_read().ok()?;
        let t = read.open_table(UNLOCKS_V2).ok()?;
        let mut out = Vec::new();
        for (tier, &depth) in LOCK_PERIODS.iter().enumerate() {
            if exec_height <= depth {
                continue;
            }
            let src = exec_height - depth;
            if src < bounds.min_height || src > bounds.max_height {
                return None;
            }
            let k = scheduled_key(tier as u8, exec_height);
            if let Some(v) = t.get(k.as_slice()).ok()? {
                out.extend(decode_unlocks(v.value()).ok()?);
            }
        }
        Some(out)
    }

    pub fn trims_opt(&self, exec_height: u64, denomination: u8) -> Option<Vec<OutPoint>> {
        let Some(&depth) = TRIM_DEPTHS.get(denomination as usize) else {
            return Some(vec![]);
        };
        if exec_height <= depth {
            return Some(vec![]);
        }
        let bounds = read_bounds(&self.db).ok()?;
        let src = exec_height - depth;
        if bounds.max_height == 0
            || exec_height < bounds.pruned_trim_below
            || src < bounds.min_height
            || src > bounds.max_height
        {
            return None;
        }
        let read = self.db.begin_read().ok()?;
        let t = read.open_table(TRIMS_V2).ok()?;
        let k = scheduled_key(denomination, exec_height);
        match t.get(k.as_slice()).ok()? {
            Some(v) => decode_outpoints(v.value()).ok(),
            None => Some(vec![]),
        }
    }

    pub fn incoming_opt(&self, src_height: u64) -> Option<Vec<Transaction>> {
        let bounds = read_bounds(&self.db).ok()?;
        if bounds.max_height == 0
            || src_height < bounds.min_height
            || src_height > bounds.max_height
        {
            return None;
        }
        let read = self.db.begin_read().ok()?;
        let t = read.open_table(UNLOCKS_V2).ok()?;
        let mut out = Vec::new();
        let mut any_live = false;
        for (tier, &depth) in LOCK_PERIODS.iter().enumerate() {
            let unlock_h = src_height.saturating_add(depth);
            if unlock_h < bounds.pruned_unlock_below {
                continue;
            }
            any_live = true;
            let k = scheduled_key(tier as u8, unlock_h);
            if let Some(v) = t.get(k.as_slice()).ok()? {
                for u in decode_unlocks(v.value()).ok()? {
                    out.push(unlock_to_synthetic_tx(&u).ok()?);
                }
            }
        }
        any_live.then_some(out)
    }

    pub fn created_opt(&self, src_height: u64) -> Option<Vec<(OutPoint, u8)>> {
        let bounds = read_bounds(&self.db).ok()?;
        if bounds.max_height == 0
            || src_height < bounds.min_height
            || src_height > bounds.max_height
        {
            return None;
        }
        let read = self.db.begin_read().ok()?;
        let t = read.open_table(TRIMS_V2).ok()?;
        let mut out = Vec::new();
        let mut any_live = false;
        for (denom, &depth) in TRIM_DEPTHS.iter().enumerate() {
            let trim_h = src_height.saturating_add(depth);
            if trim_h < bounds.pruned_trim_below {
                continue;
            }
            any_live = true;
            let k = scheduled_key(denom as u8, trim_h);
            if let Some(v) = t.get(k.as_slice()).ok()? {
                for pt in decode_outpoints(v.value()).ok()? {
                    out.push((pt, denom as u8));
                }
            }
        }
        any_live.then_some(out)
    }
}

/// Compact scheduled redemption and trim history (`historical::History`) on disk,
/// with heights since the last flush buffered in memory.
///
/// Format v2 stores ONLY consensus-redeemable ETXs (`etx_type == 1` with 33B
/// lockup payload and `etx_type == 2` conversions, non-Qi destinations) keyed by
/// `(lock_tier, unlock_height)`, and ONLY trimmable Qi UTXO outpoints
/// (denominations `0..=5`) keyed by `(denom, trim_height)`. Each tier/denomination
/// is pruned immediately once its exact maturity height passes, cutting `past.redb`
/// from ~13–14 GB down to ~1–3 GB on mainnet while preserving bit-exact consensus.
/// Legacy v1 `past.redb` files are transparently migrated in-place on `open`.
pub struct PastDb {
    db: std::sync::Arc<Database>,
    pending: BTreeMap<u64, CompactEntry>,
    bounds: PastBounds,
}
impl PastDb {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut db = Database::builder()
            .set_cache_size(32 << 20)
            .create(path)
            .map_err(bad)?;
        let mut migrated = false;
        {
            let mut write = db.begin_write().map_err(bad)?;
            write.set_durability(Durability::Immediate).map_err(bad)?;
            {
                let mut meta = write.open_table(META_V2).map_err(bad)?;
                let mut unlocks = write.open_table(UNLOCKS_V2).map_err(bad)?;
                let mut trims = write.open_table(TRIMS_V2).map_err(bad)?;
                let mut legacy_in = write.open_table(INCOMING).map_err(bad)?;
                let mut legacy_cr = write.open_table(CREATED).map_err(bad)?;

                let schema = meta.get("schema").map_err(bad)?.map_or(0, |v| v.value());
                if schema < PAST_SCHEMA_V2 {
                    let mut min_h = u64::MAX;
                    let mut max_h = 0u64;
                    let has_legacy =
                        legacy_in.len().map_err(bad)? > 0 || legacy_cr.len().map_err(bad)? > 0;
                    if has_legacy {
                        for row in legacy_in.iter().map_err(bad)? {
                            let (k, v) = row.map_err(bad)?;
                            let h = k.value();
                            min_h = min_h.min(h);
                            max_h = max_h.max(h);
                            let txs = decode_txs(v.value())?;
                            let entry = CompactEntry::from_raw(&txs, &[]);
                            for (tier, list) in entry.unlocks.iter().enumerate() {
                                if !list.is_empty() {
                                    let sk = scheduled_key(
                                        tier as u8,
                                        h.saturating_add(LOCK_PERIODS[tier]),
                                    );
                                    unlocks
                                        .insert(sk.as_slice(), encode_unlocks(list).as_slice())
                                        .map_err(bad)?;
                                }
                            }
                        }
                        for row in legacy_cr.iter().map_err(bad)? {
                            let (k, v) = row.map_err(bad)?;
                            let h = k.value();
                            min_h = min_h.min(h);
                            max_h = max_h.max(h);
                            let created = decode_created(v.value())?;
                            let entry = CompactEntry::from_raw(&[], &created);
                            for (denom, list) in entry.trims.iter().enumerate() {
                                if !list.is_empty() {
                                    let sk = scheduled_key(
                                        denom as u8,
                                        h.saturating_add(TRIM_DEPTHS[denom]),
                                    );
                                    trims
                                        .insert(sk.as_slice(), encode_outpoints(list).as_slice())
                                        .map_err(bad)?;
                                }
                            }
                        }
                        legacy_in
                            .retain_in(0u64..=u64::MAX, |_, _| false)
                            .map_err(bad)?;
                        legacy_cr
                            .retain_in(0u64..=u64::MAX, |_, _| false)
                            .map_err(bad)?;
                        if max_h > 0 {
                            meta.insert("min_height", min_h).map_err(bad)?;
                            meta.insert("max_height", max_h).map_err(bad)?;
                        }
                        migrated = true;
                    }
                    meta.insert("schema", PAST_SCHEMA_V2).map_err(bad)?;
                }
            }
            write.commit().map_err(bad)?;
        }
        if migrated {
            let _ = db.compact();
        }
        let bounds = read_bounds(&db)?;
        Ok(Self {
            db: std::sync::Arc::new(db),
            pending: BTreeMap::new(),
            bounds,
        })
    }
    pub fn reader(&self) -> PastReader {
        PastReader {
            db: self.db.clone(),
        }
    }
    pub fn record(
        &mut self,
        height: u64,
        incoming: Vec<Transaction>,
        created: Vec<(OutPoint, u8)>,
    ) {
        let compact = CompactEntry::from_raw(&incoming, &created);
        self.pending.insert(height, compact);
    }
    /// Write the pending heights in one durable transaction.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut write = self.db.begin_write().map_err(bad)?;
        write.set_durability(Durability::Immediate).map_err(bad)?;
        let mut min_h = self.bounds.min_height;
        let mut max_h = self.bounds.max_height;
        {
            let mut meta = write.open_table(META_V2).map_err(bad)?;
            let mut unlocks = write.open_table(UNLOCKS_V2).map_err(bad)?;
            let mut trims = write.open_table(TRIMS_V2).map_err(bad)?;
            for (&h, entry) in &self.pending {
                if min_h == 0 || h < min_h {
                    min_h = h;
                }
                if h > max_h {
                    max_h = h;
                }
                for (tier, list) in entry.unlocks.iter().enumerate() {
                    let sk = scheduled_key(tier as u8, h.saturating_add(LOCK_PERIODS[tier]));
                    if list.is_empty() {
                        let _ = unlocks.remove(sk.as_slice()).map_err(bad)?;
                    } else {
                        unlocks
                            .insert(sk.as_slice(), encode_unlocks(list).as_slice())
                            .map_err(bad)?;
                    }
                }
                for (denom, list) in entry.trims.iter().enumerate() {
                    let sk = scheduled_key(denom as u8, h.saturating_add(TRIM_DEPTHS[denom]));
                    if list.is_empty() {
                        let _ = trims.remove(sk.as_slice()).map_err(bad)?;
                    } else {
                        trims
                            .insert(sk.as_slice(), encode_outpoints(list).as_slice())
                            .map_err(bad)?;
                    }
                }
            }
            meta.insert("min_height", min_h).map_err(bad)?;
            meta.insert("max_height", max_h).map_err(bad)?;
        }
        write.commit().map_err(bad)?;
        self.bounds.min_height = min_h;
        self.bounds.max_height = max_h;
        self.pending.clear();
        Ok(())
    }
    /// Forget everything above `height` (resume from a checkpoint or reorg there).
    pub fn truncate_above(&mut self, height: u64) -> io::Result<()> {
        self.pending.retain(|h, _| *h <= height);
        let mut write = self.db.begin_write().map_err(bad)?;
        write.set_durability(Durability::Immediate).map_err(bad)?;
        let (new_min, new_max) = if self.bounds.max_height == 0 || height < self.bounds.min_height {
            (0, 0)
        } else {
            (self.bounds.min_height, self.bounds.max_height.min(height))
        };
        {
            let mut unlocks = write.open_table(UNLOCKS_V2).map_err(bad)?;
            for (tier, &depth) in LOCK_PERIODS.iter().enumerate() {
                let start =
                    scheduled_key(tier as u8, height.saturating_add(depth).saturating_add(1));
                let end = scheduled_key(tier as u8, u64::MAX);
                unlocks
                    .retain_in(start.as_slice()..=end.as_slice(), |_, _| false)
                    .map_err(bad)?;
            }
            let mut trims = write.open_table(TRIMS_V2).map_err(bad)?;
            for (denom, &depth) in TRIM_DEPTHS.iter().enumerate() {
                let start =
                    scheduled_key(denom as u8, height.saturating_add(depth).saturating_add(1));
                let end = scheduled_key(denom as u8, u64::MAX);
                trims
                    .retain_in(start.as_slice()..=end.as_slice(), |_, _| false)
                    .map_err(bad)?;
            }
            let mut meta = write.open_table(META_V2).map_err(bad)?;
            meta.insert("min_height", new_min).map_err(bad)?;
            meta.insert("max_height", new_max).map_err(bad)?;
        }
        write.commit().map_err(bad)?;
        self.bounds.min_height = new_min;
        self.bounds.max_height = new_max;
        Ok(())
    }
    /// Prune every scheduled unlock and UTXO trim whose exact maturity height
    /// is at or below `checkpoint_floor` (a replay resuming from `checkpoint_floor`
    /// executes `height >= checkpoint_floor + 1` and will never read them again).
    ///
    /// Because entries are indexed by maturity height per lock tier / denomination,
    /// 2-week coinbases and conversions are dropped after 2 weeks instead of 1 year,
    /// and denomination-0 UTXOs are dropped after 2 weeks instead of 12 weeks.
    pub fn prune_to_checkpoint(&mut self, checkpoint_floor: u64) -> io::Result<u64> {
        let next_floor = checkpoint_floor.saturating_add(1);
        self.prune_scheduled(next_floor, next_floor)
    }
    /// Drop heights that no replay from a kept checkpoint can read again.
    pub fn prune(&mut self, incoming_below: u64, created_below: u64) -> io::Result<u64> {
        let unlock_below = incoming_below.saturating_add(incoming_depth());
        let trim_below = created_below.saturating_add(TRIM_DEPTH);
        self.prune_scheduled(unlock_below, trim_below)
    }
    fn prune_scheduled(&mut self, unlock_below: u64, trim_below: u64) -> io::Result<u64> {
        let mut write = self.db.begin_write().map_err(bad)?;
        write.set_durability(Durability::Immediate).map_err(bad)?;
        let mut removed = 0u64;
        let new_unlock_floor = self.bounds.pruned_unlock_below.max(unlock_below);
        let new_trim_floor = self.bounds.pruned_trim_below.max(trim_below);
        {
            let mut unlocks = write.open_table(UNLOCKS_V2).map_err(bad)?;
            let before = unlocks.len().map_err(bad)?;
            if new_unlock_floor > 0 {
                for tier in 0..LOCK_PERIODS.len() {
                    let start = scheduled_key(tier as u8, 0);
                    let end = scheduled_key(tier as u8, new_unlock_floor - 1);
                    unlocks
                        .retain_in(start.as_slice()..=end.as_slice(), |_, _| false)
                        .map_err(bad)?;
                }
            }
            removed += before.saturating_sub(unlocks.len().map_err(bad)?);

            let mut trims = write.open_table(TRIMS_V2).map_err(bad)?;
            let before = trims.len().map_err(bad)?;
            if new_trim_floor > 0 {
                for denom in 0..TRIM_DEPTHS.len() {
                    let start = scheduled_key(denom as u8, 0);
                    let end = scheduled_key(denom as u8, new_trim_floor - 1);
                    trims
                        .retain_in(start.as_slice()..=end.as_slice(), |_, _| false)
                        .map_err(bad)?;
                }
            }
            removed += before.saturating_sub(trims.len().map_err(bad)?);

            let mut meta = write.open_table(META_V2).map_err(bad)?;
            meta.insert("pruned_unlock_below", new_unlock_floor)
                .map_err(bad)?;
            meta.insert("pruned_trim_below", new_trim_floor)
                .map_err(bad)?;
        }
        write.commit().map_err(bad)?;
        self.bounds.pruned_unlock_below = new_unlock_floor;
        self.bounds.pruned_trim_below = new_trim_floor;
        if removed > 0 {
            if let Some(db) = Arc::get_mut(&mut self.db) {
                let _ = db.compact();
            }
        }
        Ok(removed)
    }
    /// Lowest and highest stored source height (diagnostics and tests).
    pub fn range(&self) -> io::Result<Option<(u64, u64)>> {
        let mut lo = if self.bounds.max_height > 0 {
            Some(self.bounds.min_height)
        } else {
            None
        };
        let mut hi = if self.bounds.max_height > 0 {
            Some(self.bounds.max_height)
        } else {
            None
        };
        if let Some(&first) = self.pending.keys().next() {
            lo = Some(lo.map_or(first, |v| v.min(first)));
        }
        if let Some(&last) = self.pending.keys().next_back() {
            hi = Some(hi.map_or(last, |v| v.max(last)));
        }
        Ok(lo.zip(hi))
    }
    /// Read scheduled unlocks maturing at `exec_height` for one lock `tier` (`0..=3`).
    pub fn unlocks_for_tier(
        &self,
        exec_height: u64,
        tier: u8,
    ) -> Result<Vec<ScheduledUnlock>, settlement::Error> {
        let Some(&depth) = LOCK_PERIODS.get(tier as usize) else {
            return Ok(vec![]);
        };
        if exec_height <= depth {
            return Ok(vec![]);
        }
        let src = exec_height - depth;
        if let Some(entry) = self.pending.get(&src) {
            return Ok(entry.unlocks[tier as usize].clone());
        }
        if exec_height < self.bounds.pruned_unlock_below
            || self.bounds.max_height == 0
            || src < self.bounds.min_height
            || src > self.bounds.max_height
        {
            return Err(settlement::Error::Invalid(
                "missing replay redemption ancestry",
            ));
        }
        let read = self
            .db
            .begin_read()
            .map_err(|_| settlement::Error::Invalid("replay redemption history read"))?;
        let t = read
            .open_table(UNLOCKS_V2)
            .map_err(|_| settlement::Error::Invalid("replay redemption history read"))?;
        let k = scheduled_key(tier, exec_height);
        match t
            .get(k.as_slice())
            .map_err(|_| settlement::Error::Invalid("replay redemption history read"))?
        {
            Some(raw) => decode_unlocks(raw.value())
                .map_err(|_| settlement::Error::Invalid("replay redemption history encoding")),
            None => Ok(vec![]),
        }
    }
}
impl historical::History for PastDb {
    fn incoming_at(&self, height: u64) -> Result<Vec<Transaction>, settlement::Error> {
        if let Some(entry) = self.pending.get(&height) {
            let mut out = Vec::new();
            for tier_list in &entry.unlocks {
                for u in tier_list {
                    out.push(unlock_to_synthetic_tx(u).map_err(|_| {
                        settlement::Error::Invalid("replay redemption history encoding")
                    })?);
                }
            }
            return Ok(out);
        }
        if self.bounds.max_height == 0
            || height < self.bounds.min_height
            || height > self.bounds.max_height
        {
            return Err(settlement::Error::Invalid(
                "missing replay redemption ancestry",
            ));
        }
        let read = self
            .db
            .begin_read()
            .map_err(|_| settlement::Error::Invalid("replay redemption history read"))?;
        let t = read
            .open_table(UNLOCKS_V2)
            .map_err(|_| settlement::Error::Invalid("replay redemption history read"))?;
        let mut out = Vec::new();
        let mut any_live = false;
        for (tier, &depth) in LOCK_PERIODS.iter().enumerate() {
            let unlock_h = height.saturating_add(depth);
            if unlock_h < self.bounds.pruned_unlock_below {
                continue;
            }
            any_live = true;
            let k = scheduled_key(tier as u8, unlock_h);
            if let Some(raw) = t
                .get(k.as_slice())
                .map_err(|_| settlement::Error::Invalid("replay redemption history read"))?
            {
                for u in decode_unlocks(raw.value())
                    .map_err(|_| settlement::Error::Invalid("replay redemption history encoding"))?
                {
                    out.push(unlock_to_synthetic_tx(&u).map_err(|_| {
                        settlement::Error::Invalid("replay redemption history encoding")
                    })?);
                }
            }
        }
        if !any_live {
            return Err(settlement::Error::Invalid(
                "missing replay redemption ancestry",
            ));
        }
        Ok(out)
    }
    fn created_at(&self, height: u64) -> Result<Vec<(OutPoint, u8)>, settlement::Error> {
        if let Some(entry) = self.pending.get(&height) {
            let mut out = Vec::new();
            for (denom, list) in entry.trims.iter().enumerate() {
                for &pt in list {
                    out.push((pt, denom as u8));
                }
            }
            return Ok(out);
        }
        if self.bounds.max_height == 0
            || height < self.bounds.min_height
            || height > self.bounds.max_height
        {
            return Err(settlement::Error::Invalid("missing replay trim ancestry"));
        }
        let read = self
            .db
            .begin_read()
            .map_err(|_| settlement::Error::Invalid("replay trim history read"))?;
        let t = read
            .open_table(TRIMS_V2)
            .map_err(|_| settlement::Error::Invalid("replay trim history read"))?;
        let mut out = Vec::new();
        let mut any_live = false;
        for (denom, &depth) in TRIM_DEPTHS.iter().enumerate() {
            let trim_h = height.saturating_add(depth);
            if trim_h < self.bounds.pruned_trim_below {
                continue;
            }
            any_live = true;
            let k = scheduled_key(denom as u8, trim_h);
            if let Some(raw) = t
                .get(k.as_slice())
                .map_err(|_| settlement::Error::Invalid("replay trim history read"))?
            {
                for pt in decode_outpoints(raw.value())
                    .map_err(|_| settlement::Error::Invalid("replay trim history encoding"))?
                {
                    out.push((pt, denom as u8));
                }
            }
        }
        if !any_live {
            return Err(settlement::Error::Invalid("missing replay trim ancestry"));
        }
        Ok(out)
    }
    fn unlocks_at(&self, height: u64) -> Result<Option<Vec<ScheduledUnlock>>, settlement::Error> {
        let mut out = Vec::new();
        for tier in 0..LOCK_PERIODS.len() {
            out.extend(self.unlocks_for_tier(height, tier as u8)?);
        }
        Ok(Some(out))
    }
    fn trims_at(
        &self,
        height: u64,
        denomination: u8,
    ) -> Result<Option<Vec<OutPoint>>, settlement::Error> {
        let Some(&depth) = TRIM_DEPTHS.get(denomination as usize) else {
            return Ok(Some(vec![]));
        };
        if height <= depth {
            return Ok(Some(vec![]));
        }
        let src = height - depth;
        if let Some(entry) = self.pending.get(&src) {
            return Ok(Some(entry.trims[denomination as usize].clone()));
        }
        if height < self.bounds.pruned_trim_below
            || self.bounds.max_height == 0
            || src < self.bounds.min_height
            || src > self.bounds.max_height
        {
            return Err(settlement::Error::Invalid("missing replay trim ancestry"));
        }
        let read = self
            .db
            .begin_read()
            .map_err(|_| settlement::Error::Invalid("replay trim history read"))?;
        let t = read
            .open_table(TRIMS_V2)
            .map_err(|_| settlement::Error::Invalid("replay trim history read"))?;
        let k = scheduled_key(denomination, height);
        match t
            .get(k.as_slice())
            .map_err(|_| settlement::Error::Invalid("replay trim history read"))?
        {
            Some(raw) => decode_outpoints(raw.value())
                .map(Some)
                .map_err(|_| settlement::Error::Invalid("replay trim history encoding")),
            None => Ok(Some(vec![])),
        }
    }
}

/// Real-time disk usage breakdown of a node's data directory for telemetry RPCs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiskUsage {
    pub state_trie_bytes: u64,
    pub past_bytes: u64,
    pub blocks_bytes: u64,
    pub checkpoints_bytes: u64,
    pub light_headers_bytes: u64,
    pub other_bytes: u64,
    pub total_bytes: u64,
}

/// Measure the node's on-disk footprint across state tries, `past.redb`,
/// `blocks.redb`, checkpoints, and light headers.
pub fn datadir_disk_usage(datadir: &Path, environment: &str, genesis: &Hash) -> DiskUsage {
    let mut u = DiskUsage::default();
    let replay_root = chain_dir(datadir, environment, genesis);
    for sub in ["state", "headers"] {
        let dir = replay_root.join(sub);
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let Ok(meta) = entry.metadata() else { continue };
                if !meta.is_file() {
                    continue;
                }
                let len = meta.len();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("state-") && name.ends_with(".redb") {
                    u.state_trie_bytes = u.state_trie_bytes.saturating_add(len);
                } else if name == "past.redb" {
                    u.past_bytes = u.past_bytes.saturating_add(len);
                } else if name == "blocks.redb" {
                    u.blocks_bytes = u.blocks_bytes.saturating_add(len);
                } else if name.starts_with("checkpoint-") && name.ends_with(".qrc") {
                    u.checkpoints_bytes = u.checkpoints_bytes.saturating_add(len);
                } else {
                    u.other_bytes = u.other_bytes.saturating_add(len);
                }
            }
        }
    }
    let light_dir = datadir.join("light").join(environment);
    if let Ok(entries) = fs::read_dir(&light_dir) {
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let len = meta.len();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "headers.redb" {
                u.light_headers_bytes = u.light_headers_bytes.saturating_add(len);
            } else {
                u.other_bytes = u.other_bytes.saturating_add(len);
            }
        }
    }
    u.total_bytes = u
        .state_trie_bytes
        .saturating_add(u.past_bytes)
        .saturating_add(u.blocks_bytes)
        .saturating_add(u.checkpoints_bytes)
        .saturating_add(u.light_headers_bytes)
        .saturating_add(u.other_bytes);
    u
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

#[cfg(test)]
mod past_v2_tests {
    use super::*;
    use quai_execution::historical::History as _;

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "quai-past-v2-{}-{}-{tag}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn mk_etx(etx_type: u64, to: [u8; 20], val: u128, data: Vec<u8>) -> Transaction {
        Transaction::external(External {
            originating_hash: [0u8; 32],
            index: 0,
            gas: 21_000,
            to,
            value: val.to_be_bytes().to_vec(),
            data,
            access_list: vec![],
            sender: [0u8; 20],
            etx_type,
        })
        .unwrap()
    }

    #[test]
    fn scheduled_pruning_drops_short_tiers_early_and_preserves_long_tiers() {
        let dir = temp_dir("prune");
        let path = dir.join("past.redb");
        let mut past = PastDb::open(&path).unwrap();

        let to_a = [
            0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xaa,
        ];
        let mut lock0 = vec![0u8; 33];
        lock0[0] = 0;
        let mut lock3 = vec![0u8; 33];
        lock3[0] = 3;

        let pt0 = OutPoint {
            hash: [1; 32],
            index: 0,
        };
        let pt5 = OutPoint {
            hash: [5; 32],
            index: 1,
        };
        let pt8 = OutPoint {
            hash: [8; 32],
            index: 2,
        };

        // Record height 100 (with all tiers) and mark ancestry recorded through h_1y - 1.
        let h_2w = 100 + LOCK_PERIODS[0];
        let h_1y = 100 + LOCK_PERIODS[3];
        past.record(
            100,
            vec![
                mk_etx(0, to_a, 999, vec![0xde, 0xad]),
                mk_etx(1, to_a, 1_000, lock0),
                mk_etx(2, to_a, 2_000, vec![]),
                mk_etx(1, to_a, 3_000, lock3),
            ],
            vec![(pt0, 0), (pt5, 5), (pt8, 8)],
        );
        past.record(h_1y - 1, vec![], vec![]);
        past.flush().unwrap();

        // Query before pruning: 2-week unlocks, 1-year unlocks, denom-0 and denom-5 trims all present.
        let u_2w = past.unlocks_at(h_2w).unwrap().unwrap();
        assert_eq!(u_2w.len(), 2); // lock0 + conversion
        let u_1y = past.unlocks_at(h_1y).unwrap().unwrap();
        assert_eq!(u_1y.len(), 1); // lock3
        assert_eq!(
            past.trims_at(100 + TRIM_DEPTHS[0], 0).unwrap().unwrap(),
            vec![pt0]
        );
        assert_eq!(
            past.trims_at(100 + TRIM_DEPTHS[5], 5).unwrap().unwrap(),
            vec![pt5]
        );
        // Non-trimmable denom 8 was filtered out.
        assert_eq!(past.created_at(100).unwrap().len(), 2);

        // Lock-free reader observes the same scheduled entries.
        let reader = past.reader();
        assert_eq!(reader.unlocks_opt(h_2w).unwrap().len(), 2);
        assert_eq!(reader.unlocks_opt(h_1y).unwrap().len(), 1);
        assert_eq!(
            reader.trims_opt(100 + TRIM_DEPTHS[0], 0).unwrap(),
            vec![pt0]
        );
        drop(reader);

        // Prune to checkpoint just past 2 weeks (e.g. h_2w + 10):
        // - tier-0 unlock & denom-0 trim are immediately deleted!
        // - tier-3 (1-year) unlock & denom-5 (12-week) trim are STILL preserved!
        let removed = past.prune_to_checkpoint(h_2w + 10).unwrap();
        assert!(
            removed >= 2,
            "expected 2w unlock + denom-0 trim pruned, got {removed}"
        );
        assert!(past.unlocks_at(h_2w).is_err());
        assert!(past.trims_at(100 + TRIM_DEPTHS[0], 0).is_err());
        assert_eq!(past.unlocks_at(h_1y).unwrap().unwrap().len(), 1);
        assert_eq!(
            past.trims_at(100 + TRIM_DEPTHS[5], 5).unwrap().unwrap(),
            vec![pt5]
        );

        // Truncate above height 50 clears height 100's future scheduled entries cleanly.
        past.truncate_above(50).unwrap();
        assert!(past.unlocks_at(h_1y).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrates_legacy_v1_past_redb_transparently() {
        let dir = temp_dir("migrate");
        let path = dir.join("past.redb");

        let to_a = [
            0x00, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xbb,
        ];
        let mut lock1 = vec![0u8; 33];
        lock1[0] = 1;
        let pt2 = OutPoint {
            hash: [0x22; 32],
            index: 3,
        };

        // Write a legacy v1 past.redb directly.
        {
            let db = Database::create(&path).unwrap();
            let w = db.begin_write().unwrap();
            {
                let mut inc = w.open_table(INCOMING).unwrap();
                let mut cre = w.open_table(CREATED).unwrap();
                let txs = vec![
                    mk_etx(0, to_a, 123, vec![1, 2, 3, 4]),
                    mk_etx(1, to_a, 42_000, lock1),
                ];
                inc.insert(77u64, encode_txs(&txs).as_slice()).unwrap();
                let mut raw_cre = 1u32.to_be_bytes().to_vec();
                raw_cre.extend(pt2.hash);
                raw_cre.extend(pt2.index.to_be_bytes());
                raw_cre.push(2u8);
                cre.insert(77u64, raw_cre.as_slice()).unwrap();
            }
            w.commit().unwrap();
        }

        // Reopen with PastDb v2 -> auto-migrates and answers scheduled queries.
        let past = PastDb::open(&path).unwrap();
        assert_eq!(past.range().unwrap(), Some((77, 77)));
        let u = past.unlocks_for_tier(77 + LOCK_PERIODS[1], 1).unwrap();
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].to, to_a);
        assert_eq!(u[0].lock, Some(1));
        assert_eq!(
            past.trims_at(77 + TRIM_DEPTHS[2], 2).unwrap().unwrap(),
            vec![pt2]
        );

        // Legacy v1 tables are empty after migration.
        let read = past.db.begin_read().unwrap();
        assert_eq!(read.open_table(INCOMING).unwrap().len().unwrap(), 0);
        assert_eq!(read.open_table(CREATED).unwrap().len().unwrap(), 0);

        let _ = fs::remove_dir_all(&dir);
    }
}
