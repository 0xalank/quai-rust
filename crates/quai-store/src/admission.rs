//! Durable local admission receipts with indexed branch ancestry. Callers supply
//! already validated payloads and fork choice; this store grants no consensus authority.
use quai_primitives::Hash;
use redb::{
    Database, Durability, ReadTransaction, ReadableDatabase, ReadableTable, ReadableTableMetadata,
    TableDefinition,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
const RECORDS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("native.admission.records.v1");
const BLOCKS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("native.admission.blocks.v1");
const PAYLOADS: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("native.admission.payloads.v1");
const META: TableDefinition<&[u8], &[u8]> = TableDefinition::new("native.admission.meta.v1");
const MAGIC: &[u8] = b"QUAI-ADMISSION-RECEIPT-1\n";
const SCHEMA: &[u8] = b"quai-native-admission-1";
pub const MAX_PAYLOAD: usize = 12 << 20;
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
fn digest(v: &[u8]) -> Hash {
    Sha256::digest(v).into()
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    pub count: u64,
    pub last: Hash,
    pub selected: Hash,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub id: Hash,
    pub block: Hash,
    pub checkpoint: Hash,
    pub parent: Option<Hash>,
    pub height: u64,
    pub sequence: u64,
    pub payload: Vec<u8>,
    pub jumps: Vec<Hash>,
}
pub struct Header {
    pub block: Hash,
    pub checkpoint: Hash,
    pub parent: Option<Hash>,
    pub height: u64,
    pub sequence: u64,
    pub payload_digest: Hash,
    pub jumps: Vec<Hash>,
}
fn encode(profile: Hash, h: &Header) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend(profile);
    out.extend(h.block);
    out.extend(h.checkpoint);
    out.push(u8::from(h.parent.is_some()));
    out.extend(h.parent.unwrap_or([0; 32]));
    out.extend(h.height.to_be_bytes());
    out.extend(h.sequence.to_be_bytes());
    out.extend(h.payload_digest);
    out.push(h.jumps.len() as u8);
    for id in &h.jumps {
        out.extend(id);
    }
    out
}
struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(n)
            .ok_or_else(|| bad("admission cursor overflow"))?;
        let v = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| bad("admission truncation"))?;
        self.offset = end;
        Ok(v)
    }
    fn hash(&mut self) -> io::Result<Hash> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    fn number(&mut self) -> io::Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
}
fn decode(profile: Hash, id: Hash, bytes: &[u8]) -> io::Result<Header> {
    need(
        bytes.len() <= 4096 && digest(bytes) == id,
        "admission header hash/size",
    )?;
    let mut r = Cursor { bytes, offset: 0 };
    need(
        r.take(MAGIC.len())? == MAGIC && r.hash()? == profile,
        "admission format/profile",
    )?;
    let block = r.hash()?;
    let checkpoint = r.hash()?;
    let flag = r.take(1)?[0];
    let raw = r.hash()?;
    need(
        flag <= 1 && (flag != 0 || raw == [0; 32]),
        "admission parent flag",
    )?;
    let parent = (flag == 1).then_some(raw);
    let height = r.number()?;
    let sequence = r.number()?;
    let payload_digest = r.hash()?;
    let count = r.take(1)?[0] as usize;
    need(count <= 64, "admission jump count")?;
    let mut jumps = Vec::with_capacity(count);
    for _ in 0..count {
        jumps.push(r.hash()?);
    }
    let expected = if height == 0 {
        0
    } else {
        64 - height.leading_zeros() as usize
    };
    need(
        r.offset == bytes.len()
            && (height == 0) == parent.is_none()
            && count == expected
            && jumps.first().copied() == parent,
        "admission ancestry shape",
    )?;
    Ok(Header {
        block,
        checkpoint,
        parent,
        height,
        sequence,
        payload_digest,
        jumps,
    })
}
fn header(read: &ReadTransaction, profile: Hash, id: Hash) -> io::Result<Header> {
    let table = read.open_table(RECORDS).map_err(bad)?;
    let v = table
        .get(id.as_slice())
        .map_err(bad)?
        .ok_or_else(|| bad("missing admission receipt"))?;
    decode(profile, id, v.value())
}
fn head(read: &ReadTransaction) -> io::Result<Option<Head>> {
    let table = read.open_table(META).map_err(bad)?;
    let Some(v) = table.get(b"head".as_slice()).map_err(bad)? else {
        return Ok(None);
    };
    let b = v.value();
    need(
        b.len() == 104 && digest(&b[..72]) == b[72..],
        "admission head checksum/width",
    )?;
    let count = u64::from_be_bytes(b[..8].try_into().unwrap());
    need(count > 0, "admission head count")?;
    Ok(Some(Head {
        count,
        last: b[8..40].try_into().unwrap(),
        selected: b[40..72].try_into().unwrap(),
    }))
}
fn encode_head(h: Head) -> Vec<u8> {
    let mut out = h.count.to_be_bytes().to_vec();
    out.extend(h.last);
    out.extend(h.selected);
    out.extend(digest(&out));
    out
}
fn identity(path: &Path) -> io::Result<(u64, u64)> {
    let m = fs::symlink_metadata(path)?;
    need(
        m.is_file() && !m.file_type().is_symlink(),
        "admission database file replaced",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok((m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = m;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "admission file identity requires Unix",
        ))
    }
}
static OPEN_DATABASES: OnceLock<Mutex<BTreeMap<(u64, u64), Weak<Database>>>> = OnceLock::new();
pub struct Catalog {
    db: Arc<Database>,
    profile: Hash,
    path: PathBuf,
    identity: (u64, u64),
}
pub struct Snapshot {
    read: ReadTransaction,
    _database: Arc<Database>,
    profile: Hash,
    head: Option<Head>,
}
impl Catalog {
    pub fn open(path: &Path, profile: Hash) -> io::Result<Self> {
        match fs::symlink_metadata(path) {
            Ok(m) => need(
                m.is_file() && !m.file_type().is_symlink(),
                "admission database type",
            )?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // A pinned generation must remain readable across writer reopen. Keep
        // the same database alive for its read transactions and reuse that
        // handle when this process reopens the same inode.
        let mut registry = OPEN_DATABASES
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .map_err(bad)?;
        registry.retain(|_, value| value.strong_count() != 0);
        let existing = identity(path)
            .ok()
            .and_then(|key| registry.get(&key).and_then(Weak::upgrade));
        let db = if let Some(db) = existing {
            db
        } else {
            let mut builder = Database::builder();
            builder.set_cache_size(16 << 20);
            Arc::new(builder.create(path).map_err(bad)?)
        };
        {
            let read = db.begin_read().map_err(bad)?;
            match read.open_table(META) {
                Ok(m) => {
                    need(
                        m.get(b"schema".as_slice())
                            .map_err(bad)?
                            .is_some_and(|v| v.value() == SCHEMA)
                            && m.get(b"profile".as_slice())
                                .map_err(bad)?
                                .is_some_and(|v| v.value() == profile),
                        "admission schema/profile",
                    )?;
                    read.open_table(RECORDS).map_err(bad)?;
                    read.open_table(BLOCKS).map_err(bad)?;
                    read.open_table(PAYLOADS).map_err(bad)?;
                }
                Err(redb::TableError::TableDoesNotExist(_)) => need(
                    read.list_tables().map_err(bad)?.next().is_none(),
                    "refusing unknown admission migration",
                )?,
                Err(e) => return Err(bad(e)),
            }
        }
        {
            let mut write = db.begin_write().map_err(bad)?;
            write.set_durability(Durability::Immediate).map_err(bad)?;
            {
                let mut meta = write.open_table(META).map_err(bad)?;
                if meta.get(b"schema".as_slice()).map_err(bad)?.is_none() {
                    meta.insert(b"schema".as_slice(), SCHEMA).map_err(bad)?;
                    meta.insert(b"profile".as_slice(), profile.as_slice())
                        .map_err(bad)?;
                }
            }
            write.open_table(RECORDS).map_err(bad)?;
            write.open_table(BLOCKS).map_err(bad)?;
            write.open_table(PAYLOADS).map_err(bad)?;
            write.commit().map_err(bad)?;
        }
        let c = Self {
            db,
            profile,
            path: path.to_path_buf(),
            identity: identity(path)?,
        };
        c.snapshot()?;
        registry.insert(c.identity, Arc::downgrade(&c.db));
        Ok(c)
    }
    pub fn check_file(&self) -> io::Result<()> {
        need(
            identity(&self.path)? == self.identity,
            "admission database file replaced",
        )
    }
    pub fn snapshot(&self) -> io::Result<Snapshot> {
        self.check_file()?;
        let read = self.db.begin_read().map_err(bad)?;
        let head = head(&read)?;
        if let Some(h) = head {
            let last = header(&read, self.profile, h.last)?;
            let selected = header(&read, self.profile, h.selected)?;
            need(
                last.sequence.checked_add(1) == Some(h.count) && selected.sequence < h.count,
                "admission head order",
            )?;
        } else {
            need(
                read.open_table(RECORDS)
                    .map_err(bad)?
                    .is_empty()
                    .map_err(bad)?
                    && read
                        .open_table(BLOCKS)
                        .map_err(bad)?
                        .is_empty()
                        .map_err(bad)?
                    && read
                        .open_table(PAYLOADS)
                        .map_err(bad)?
                        .is_empty()
                        .map_err(bad)?,
                "missing admission head",
            )?;
        }
        Ok(Snapshot {
            read,
            _database: self.db.clone(),
            profile: self.profile,
            head,
        })
    }
    /// Appends one local validated receipt, block index and selected-head CAS in
    /// one durable transaction. This does not write or validate execution state.
    pub fn append(
        &self,
        expected: Option<Head>,
        parent: Option<Hash>,
        block: Hash,
        checkpoint: Hash,
        payload: &[u8],
        select: bool,
    ) -> io::Result<Receipt> {
        self.check_file()?;
        need(payload.len() <= MAX_PAYLOAD, "admission payload bound")?;
        let mut write = self.db.begin_write().map_err(bad)?;
        write.set_durability(Durability::Immediate).map_err(bad)?;
        let mut meta = write.open_table(META).map_err(bad)?;
        let current = meta
            .get(b"head".as_slice())
            .map_err(bad)?
            .map(|v| v.value().to_vec());
        need(
            current == expected.map(encode_head),
            "stale admission generation",
        )?;
        let mut records = write.open_table(RECORDS).map_err(bad)?;
        let mut blocks = write.open_table(BLOCKS).map_err(bad)?;
        let mut payloads = write.open_table(PAYLOADS).map_err(bad)?;
        need(
            blocks.get(block.as_slice()).map_err(bad)?.is_none(),
            "duplicate admitted block",
        )?;
        let sequence = expected.map_or(0, |h| h.count);
        let mut jumps = vec![];
        let height = if let Some(parent) = parent {
            need(expected.is_some(), "admission parent before genesis")?;
            let v = records
                .get(parent.as_slice())
                .map_err(bad)?
                .ok_or_else(|| bad("unadmitted parent"))?;
            let h = decode(self.profile, parent, v.value())?;
            need(h.sequence < sequence, "admission parent order")?;
            let height = h
                .height
                .checked_add(1)
                .ok_or_else(|| bad("admission height overflow"))?;
            jumps.push(parent);
            let mut bit = 1;
            while let Some(previous) = jumps.get(bit - 1) {
                let v = records
                    .get(previous.as_slice())
                    .map_err(bad)?
                    .ok_or_else(|| bad("missing admission jump parent"))?;
                let h = decode(self.profile, *previous, v.value())?;
                let Some(next) = h.jumps.get(bit - 1) else {
                    break;
                };
                jumps.push(*next);
                bit += 1;
            }
            height
        } else {
            need(
                expected.is_none() && select,
                "admission genesis order/selection",
            )?;
            0
        };
        let header = Header {
            block,
            checkpoint,
            parent,
            height,
            sequence,
            payload_digest: digest(payload),
            jumps,
        };
        let raw = encode(self.profile, &header);
        let id = digest(&raw);
        decode(self.profile, id, &raw)?;
        need(
            records.get(id.as_slice()).map_err(bad)?.is_none(),
            "duplicate admission receipt",
        )?;
        records.insert(id.as_slice(), raw.as_slice()).map_err(bad)?;
        blocks
            .insert(block.as_slice(), id.as_slice())
            .map_err(bad)?;
        payloads.insert(id.as_slice(), payload).map_err(bad)?;
        let next = Head {
            count: sequence
                .checked_add(1)
                .ok_or_else(|| bad("admission count overflow"))?,
            last: id,
            selected: if select {
                id
            } else {
                expected.unwrap().selected
            },
        };
        meta.insert(b"head".as_slice(), encode_head(next).as_slice())
            .map_err(bad)?;
        drop(payloads);
        drop(blocks);
        drop(records);
        drop(meta);
        #[cfg(test)]
        fault("before-commit");
        write.commit().map_err(bad)?;
        #[cfg(test)]
        fault("after-commit");
        Ok(Receipt {
            id,
            block,
            checkpoint,
            parent,
            height,
            sequence,
            payload: payload.to_vec(),
            jumps: header.jumps,
        })
    }
}
impl Snapshot {
    pub fn head(&self) -> Option<Head> {
        self.head
    }
    pub fn get(&self, id: Hash) -> io::Result<Receipt> {
        let h = header(&self.read, self.profile, id)?;
        let table = self.read.open_table(PAYLOADS).map_err(bad)?;
        let v = table
            .get(id.as_slice())
            .map_err(bad)?
            .ok_or_else(|| bad("missing admission payload"))?;
        need(
            v.value().len() <= MAX_PAYLOAD && digest(v.value()) == h.payload_digest,
            "admission payload digest/size",
        )?;
        Ok(Receipt {
            id,
            block: h.block,
            checkpoint: h.checkpoint,
            parent: h.parent,
            height: h.height,
            sequence: h.sequence,
            payload: v.value().to_vec(),
            jumps: h.jumps,
        })
    }
    pub fn header(&self, id: Hash) -> io::Result<Header> {
        header(&self.read, self.profile, id)
    }
    pub fn id_for_block(&self, block: Hash) -> io::Result<Option<Hash>> {
        let table = self.read.open_table(BLOCKS).map_err(bad)?;
        let Some(v) = table.get(block.as_slice()).map_err(bad)? else {
            return Ok(None);
        };
        let id: Hash = v.value().try_into().map_err(bad)?;
        need(
            self.header(id)?.block == block,
            "admission block index binding",
        )?;
        Ok(Some(id))
    }
    pub fn by_block(&self, block: Hash) -> io::Result<Option<Receipt>> {
        let table = self.read.open_table(BLOCKS).map_err(bad)?;
        let Some(v) = table.get(block.as_slice()).map_err(bad)? else {
            return Ok(None);
        };
        let id: Hash = v.value().try_into().map_err(bad)?;
        let receipt = self.get(id)?;
        need(receipt.block == block, "admission block index binding")?;
        Ok(Some(receipt))
    }
    pub fn ancestor(&self, id: Hash, height: u64) -> io::Result<Option<Receipt>> {
        let mut current = id;
        let mut h = header(&self.read, self.profile, current)?;
        let Some(mut left) = h.height.checked_sub(height) else {
            return Ok(None);
        };
        let mut bit = 0;
        while left != 0 {
            if left & 1 != 0 {
                let next = *h
                    .jumps
                    .get(bit)
                    .ok_or_else(|| bad("missing admission binary link"))?;
                let next_h = header(&self.read, self.profile, next)?;
                need(
                    next_h.height.checked_add(1u64 << bit) == Some(h.height),
                    "admission binary-link height",
                )?;
                current = next;
                h = next_h;
            }
            left >>= 1;
            bit += 1;
        }
        need(h.height == height, "admission ancestor height")?;
        self.get(current).map(Some)
    }
}

#[cfg(test)]
fn fault(phase: &str) {
    if std::env::var("QUAI_ADMISSION_FAULT").is_ok_and(|v| v == phase) {
        std::process::exit(75);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    fn dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "quai-admission-{name}-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        p
    }
    fn id(n: u64) -> Hash {
        let mut h = [0; 32];
        h[24..].copy_from_slice(&n.to_be_bytes());
        h
    }
    #[test]
    fn indexed_receipts_exceed_old_history_cap_with_pinned_forks_and_recovery() {
        let dir = dir("long");
        let db = Catalog::open(&dir.join("receipts.redb"), [7; 32]).unwrap();
        let mut row = db
            .append(None, None, id(0), id(100000), b"genesis", true)
            .unwrap();
        let pinned = db.snapshot().unwrap();
        let mut branch = None;
        for n in 1..=6000 {
            let current = db.snapshot().unwrap().head();
            row = db
                .append(
                    current,
                    Some(row.id),
                    id(n),
                    id(100000 + n),
                    &n.to_be_bytes(),
                    true,
                )
                .unwrap();
            if n == 4000 {
                branch = Some(row.clone());
            }
        }
        let all = db.snapshot().unwrap();
        let head = all.head().unwrap();
        assert_eq!(head.count, 6001);
        assert_eq!(all.get(head.selected).unwrap(), row);
        assert!(pinned.by_block(id(1)).unwrap().is_none());
        for height in [0, 1, 31, 1024, 4096, 5999, 6000] {
            let ancestor = all.ancestor(row.id, height).unwrap().unwrap();
            assert_eq!(ancestor.block, id(height));
            assert_eq!(ancestor.height, height);
        }
        assert!(all.ancestor(row.id, 6001).unwrap().is_none());
        let branch = branch.unwrap();
        let fork = db
            .append(
                Some(head),
                Some(branch.id),
                id(9000),
                id(109000),
                b"higher-score-fork",
                true,
            )
            .unwrap();
        let selected = db.snapshot().unwrap();
        assert_eq!(selected.head().unwrap().selected, fork.id);
        assert_eq!(selected.ancestor(fork.id, 4000).unwrap().unwrap(), branch);
        assert!(selected.ancestor(fork.id, 4002).unwrap().is_none());
        assert_eq!(all.head(), Some(head));
        assert!(all.by_block(fork.block).unwrap().is_none());
        drop(pinned);
        drop(all);
        drop(selected);
        drop(db);
        let db = Catalog::open(&dir.join("receipts.redb"), [7; 32]).unwrap();
        let reopened = db.snapshot().unwrap();
        assert_eq!(
            reopened.get(reopened.head().unwrap().selected).unwrap(),
            fork
        );
        assert_eq!(reopened.by_block(row.block).unwrap().unwrap(), row);
        drop(reopened);
        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn pinned_generation_survives_writer_reopen_and_later_appends() {
        let dir = dir("pinned-reopen");
        let path = dir.join("receipts.redb");
        let db = Catalog::open(&path, [10; 32]).unwrap();
        let payload = vec![7; 32768];
        let genesis = db
            .append(None, None, id(0), id(100), &payload, true)
            .unwrap();
        let pinned = db.snapshot().unwrap();
        drop(db);
        let db = Catalog::open(&path, [10; 32]).unwrap();
        assert!(Arc::ptr_eq(&db.db, &pinned._database));
        let next = db
            .append(
                pinned.head(),
                Some(genesis.id),
                id(1),
                id(101),
                b"child",
                true,
            )
            .unwrap();
        assert_eq!(pinned.get(genesis.id).unwrap().payload, payload);
        assert!(pinned.by_block(next.block).unwrap().is_none());
        assert!(db
            .snapshot()
            .unwrap()
            .by_block(next.block)
            .unwrap()
            .is_some());
        assert!(db
            .append(
                pinned.head(),
                Some(genesis.id),
                id(2),
                id(102),
                b"stale writer",
                true
            )
            .is_err());
        drop(db);
        assert_eq!(pinned.get(genesis.id).unwrap(), genesis);
        drop(pinned);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn stale_duplicate_foreign_parent_and_corrupt_payload_cannot_change_selected_head() {
        let dir = dir("reject");
        let path = dir.join("receipts.redb");
        let db = Catalog::open(&path, [8; 32]).unwrap();
        let genesis = db
            .append(None, None, id(0), id(100), b"genesis", true)
            .unwrap();
        let head = db.snapshot().unwrap().head();
        assert!(db
            .append(None, Some(genesis.id), id(1), id(101), b"stale", true)
            .is_err());
        assert!(db
            .append(head, Some(id(999)), id(1), id(101), b"missing-parent", true)
            .is_err());
        assert!(db
            .append(head, Some(genesis.id), id(0), id(101), b"duplicate", true)
            .is_err());
        assert!(db
            .append(head, None, id(1), id(101), b"second-genesis", true)
            .is_err());
        assert_eq!(db.snapshot().unwrap().head(), head);
        let write = db.db.begin_write().unwrap();
        {
            let mut p = write.open_table(PAYLOADS).unwrap();
            p.insert(genesis.id.as_slice(), b"damaged".as_slice())
                .unwrap();
        }
        write.commit().unwrap();
        assert!(db.snapshot().unwrap().get(genesis.id).is_err());
        // An already opened database cannot keep publishing through a replaced path.
        fs::rename(&path, dir.join("old.redb")).unwrap();
        fs::write(&path, b"replacement").unwrap();
        assert!(db.snapshot().is_err());
        assert!(db
            .append(head, Some(genesis.id), id(1), id(101), b"replaced", true)
            .is_err());
        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn admission_commit_process_child() {
        let Ok(dir) = std::env::var("QUAI_ADMISSION_CHILD_DIR") else {
            return;
        };
        let db = Catalog::open(&PathBuf::from(dir).join("receipts.redb"), [9; 32]).unwrap();
        let s = db.snapshot().unwrap();
        let head = s.head().unwrap();
        let parent = s.get(head.selected).unwrap();
        drop(s);
        db.append(Some(head), Some(parent.id), id(1), id(101), b"child", true)
            .unwrap();
        panic!("fault did not exit");
    }
    #[test]
    fn process_exit_recovers_complete_receipt_index_and_selected_head_transaction() {
        for phase in ["before-commit", "after-commit"] {
            let dir = dir(phase);
            let path = dir.join("receipts.redb");
            let db = Catalog::open(&path, [9; 32]).unwrap();
            db.append(None, None, id(0), id(100), b"genesis", true)
                .unwrap();
            drop(db);
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "admission::tests::admission_commit_process_child",
                ])
                .env("QUAI_ADMISSION_CHILD_DIR", &dir)
                .env("QUAI_ADMISSION_FAULT", phase)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(75));
            let db = Catalog::open(&path, [9; 32]).unwrap();
            let s = db.snapshot().unwrap();
            let head = s.head().unwrap();
            let committed = phase == "after-commit";
            assert_eq!(head.count, if committed { 2 } else { 1 });
            assert_eq!(
                s.get(head.selected).unwrap().block,
                id(u64::from(committed))
            );
            assert_eq!(s.by_block(id(1)).unwrap().is_some(), committed);
            drop(s);
            drop(db);
            fs::remove_dir_all(dir).unwrap();
        }
    }
}
