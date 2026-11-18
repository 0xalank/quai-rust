//! Transactional, versioned local trie nodes. This database supplies storage
//! primitives, not consensus authority; callers must validate blocks first.
use quai_primitives::{
    mpt::{self, ReadNodes, Trie},
    Hash,
};
use redb::{
    Database, Durability, ReadTransaction, ReadableDatabase, ReadableTable, ReadableTableMetadata,
    Table, TableDefinition,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    fs, io,
    path::Path,
    sync::{Arc, Mutex},
};
#[cfg(unix)]
use std::{
    path::PathBuf,
    sync::{OnceLock, Weak},
};

const NODES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("native.state.nodes.v1");
const VERSIONS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("native.state.versions.v1");
const BLOCKS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("native.state.blocks.v1");
const META: TableDefinition<&[u8], &[u8]> = TableDefinition::new("native.state.meta.v1");
const PAYLOADS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("native.state.payloads.v1");
const MAGIC: &[u8] = b"QUAI-STATE-VERSION-1\n";
const SCHEMA: &[u8] = b"quai-native-state-1";
const MAX_METADATA: usize = 63 << 20;
const MAX_TREES: usize = 256;
fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}
fn need(ok: bool, message: &str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(bad(message))
    }
}
fn digest(bytes: &[u8]) -> Hash {
    Sha256::digest(bytes).into()
}
fn valid_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub id: Hash,
    pub parent: Option<Hash>,
    pub block: Hash,
    pub height: u64,
    pub roots: BTreeMap<Vec<u8>, Hash>,
    pub metadata_digest: Hash,
    pub jumps: Vec<Hash>,
}
fn encode(
    profile: Hash,
    parent: Option<&Version>,
    block: Hash,
    roots: &BTreeMap<Vec<u8>, Hash>,
    metadata: &[u8],
    jumps: &[Hash],
) -> io::Result<Vec<u8>> {
    need(
        roots.len() <= MAX_TREES && metadata.len() <= MAX_METADATA,
        "version metadata/tree limit",
    )?;
    let mut out = MAGIC.to_vec();
    out.extend(profile);
    out.push(u8::from(parent.is_some()));
    if let Some(parent) = parent {
        out.extend(parent.id);
    }
    out.extend(block);
    out.extend(
        parent
            .map_or(Ok(0), |p| {
                p.height
                    .checked_add(1)
                    .ok_or_else(|| bad("version height overflow"))
            })?
            .to_be_bytes(),
    );
    out.extend((roots.len() as u32).to_be_bytes());
    for (name, root) in roots {
        need(valid_name(name), "state tree name")?;
        out.push(name.len() as u8);
        out.extend(name);
        out.extend(root);
    }
    out.extend(digest(metadata));
    need(jumps.len() <= 64, "version jump count")?;
    out.push(jumps.len() as u8);
    for id in jumps {
        out.extend(id);
    }
    Ok(out)
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
            .ok_or_else(|| bad("version cursor overflow"))?;
        let v = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| bad("truncated state version"))?;
        self.offset = end;
        Ok(v)
    }
    fn hash(&mut self) -> io::Result<Hash> {
        Ok(crate::slice::array(&self.take(32)?)?)
    }
    fn u32(&mut self) -> io::Result<usize> {
        Ok(u32::from_be_bytes(crate::slice::array(&self.take(4)?)?) as usize)
    }
}
fn decode(profile: Hash, id: Hash, raw: &[u8]) -> io::Result<Version> {
    need(
        raw.len() <= 32768 && digest(raw) == id,
        "state version digest/bytes",
    )?;
    let mut r = Cursor {
        bytes: raw,
        offset: 0,
    };
    need(
        r.take(MAGIC.len())? == MAGIC && r.hash()? == profile,
        "state version profile/format",
    )?;
    let parent = match r.take(1)?[0] {
        0 => None,
        1 => Some(r.hash()?),
        _ => return Err(bad("state version parent flag")),
    };
    let block = r.hash()?;
    let height = u64::from_be_bytes(crate::slice::array(&r.take(8)?)?);
    need(
        (height == 0) == parent.is_none(),
        "state version parent/height",
    )?;
    let count = r.u32()?;
    need(count <= MAX_TREES, "state version trees")?;
    let mut roots = BTreeMap::new();
    let mut previous = vec![];
    for _ in 0..count {
        let len = r.take(1)?[0] as usize;
        let name = r.take(len)?.to_vec();
        need(
            valid_name(&name) && previous < name,
            "state version tree ordering",
        )?;
        previous = name.clone();
        roots.insert(name, r.hash()?);
    }
    let metadata_digest = r.hash()?;
    let count = r.take(1)?[0] as usize;
    need(
        count == (64 - height.leading_zeros()) as usize,
        "state version jump count",
    )?;
    let mut jumps = Vec::with_capacity(count);
    for _ in 0..count {
        jumps.push(r.hash()?);
    }
    need(
        jumps.first().copied() == parent,
        "state version first ancestor",
    )?;
    need(r.offset == raw.len(), "state version trailing data")?;
    Ok(Version {
        id,
        parent,
        block,
        height,
        roots,
        metadata_digest,
        jumps,
    })
}
fn table_version<T: ReadableTable<&'static [u8], &'static [u8]>>(
    table: &T,
    profile: Hash,
    id: Hash,
) -> io::Result<Version> {
    let value = table
        .get(id.as_slice())
        .map_err(bad)?
        .ok_or_else(|| bad("unavailable state version"))?;
    decode(profile, id, value.value())
}
fn check_payload<T: ReadableTable<&'static [u8], &'static [u8]>>(
    table: &T,
    digest_id: Hash,
) -> io::Result<()> {
    let raw = table
        .get(digest_id.as_slice())
        .map_err(bad)?
        .ok_or_else(|| bad("missing metadata payload"))?;
    need(
        raw.value().len() <= MAX_METADATA && digest(raw.value()) == digest_id,
        "metadata payload digest/bytes",
    )
}
fn table_head<T: ReadableTable<&'static [u8], &'static [u8]>>(
    table: &T,
) -> io::Result<Option<Hash>> {
    table
        .get(b"head".as_slice())
        .map_err(bad)?
        .map(|v| v.value().try_into().map_err(bad))
        .transpose()
}
struct Nodes<'a, T>(&'a T);
impl<T: ReadableTable<&'static [u8], &'static [u8]>> ReadNodes for Nodes<'_, T> {
    fn get_node(&self, h: &Hash) -> Result<Option<Vec<u8>>, mpt::Error> {
        let v = self
            .0
            .get(h.as_slice())
            .map_err(|e| mpt::Error::Backend(e.to_string()))?;
        v.map(|v| {
            let raw = v.value();
            if raw.len() > mpt::MAX_NODE_BYTES {
                return Err(mpt::Error::Invalid("stored node byte bound"));
            }
            Ok(raw.to_vec())
        })
        .transpose()
    }
}
fn write_node(
    table: &mut Table<'_, &'static [u8], &'static [u8]>,
    hash: Hash,
    raw: &[u8],
) -> Result<bool, mpt::Error> {
    if raw.len() > mpt::MAX_NODE_BYTES || quai_primitives::transaction::keccak(raw) != hash {
        return Err(mpt::Error::Invalid("node write hash/bytes"));
    }
    {
        let old = table
            .get(hash.as_slice())
            .map_err(|e| mpt::Error::Backend(e.to_string()))?;
        if let Some(old) = old {
            return if old.value() == raw {
                Ok(false)
            } else {
                Err(mpt::Error::Invalid("immutable state node conflict"))
            };
        }
    }
    table
        .insert(hash.as_slice(), raw)
        .map_err(|e| mpt::Error::Backend(e.to_string()))?;
    Ok(true)
}
/// One transaction's mutable root set. Child tries (e.g. account storage) may
/// be edited/persisted explicitly; no full-state image is serialized here.
pub struct Workspace<'a, 'txn> {
    nodes: &'a mut Table<'txn, &'static [u8], &'static [u8]>,
    roots: BTreeMap<Vec<u8>, Hash>,
    changed: BTreeMap<Vec<u8>, Trie>,
    stats: mpt::Batch,
}
impl Workspace<'_, '_> {
    pub fn root(&self, name: &[u8]) -> Hash {
        self.changed
            .get(name)
            .map(Trie::root)
            .or_else(|| self.roots.get(name).copied())
            .unwrap_or_else(mpt::empty_root)
    }
    pub fn get(&self, name: &[u8], key: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let reader = Nodes(&*self.nodes);
        match self.changed.get(name) {
            Some(t) => t.get(&reader, key),
            None => Trie::from_root(self.root(name)).get(&reader, key),
        }
        .map_err(bad)
    }
    pub fn put(&mut self, name: &[u8], key: &[u8], value: &[u8]) -> io::Result<()> {
        need(valid_name(name), "state tree name")?;
        need(
            self.roots.contains_key(name)
                || self.changed.contains_key(name)
                || self.roots.len()
                    + self
                        .changed
                        .keys()
                        .filter(|k| !self.roots.contains_key(*k))
                        .count()
                    < MAX_TREES,
            "state tree limit",
        )?;
        let root = self
            .roots
            .get(name)
            .copied()
            .unwrap_or_else(mpt::empty_root);
        let trie = self
            .changed
            .entry(name.to_vec())
            .or_insert_with(|| Trie::from_root(root));
        trie.update(&Nodes(&*self.nodes), key, value).map_err(bad)
    }
    pub fn read_at(&self, root: Hash, key: &[u8]) -> io::Result<Option<Vec<u8>>> {
        Trie::from_root(root)
            .get(&Nodes(&*self.nodes), key)
            .map_err(bad)
    }
    pub fn update_trie(&self, trie: &mut Trie, key: &[u8], value: &[u8]) -> io::Result<()> {
        trie.update(&Nodes(&*self.nodes), key, value).map_err(bad)
    }
    pub fn persist_trie(&mut self, trie: &Trie) -> io::Result<Hash> {
        trie.visit_hash_links(&mut |hash| {
            let raw = Nodes(&*self.nodes)
                .get_node(&hash)?
                .ok_or(mpt::Error::Missing(hash))?;
            if quai_primitives::transaction::keccak(&raw) != hash {
                return Err(mpt::Error::Invalid("referenced node hash"));
            }
            Ok(())
        })
        .map_err(bad)?;
        let mut inserted = mpt::Batch::default();
        trie.visit_new_nodes(&mut |hash, raw| {
            if write_node(self.nodes, hash, raw)? {
                inserted.nodes += 1;
                inserted.bytes = inserted
                    .bytes
                    .checked_add(raw.len())
                    .ok_or(mpt::Error::Invalid("node byte accounting"))?;
            }
            Ok(())
        })
        .map_err(bad)?;
        self.stats.nodes += inserted.nodes;
        self.stats.bytes += inserted.bytes;
        Ok(trie.root())
    }
    pub fn attach_root(&mut self, name: &[u8], root: Hash) -> io::Result<()> {
        need(valid_name(name), "state tree name")?;
        need(
            self.roots.contains_key(name)
                || self.changed.contains_key(name)
                || self.roots.len()
                    + self
                        .changed
                        .keys()
                        .filter(|k| !self.roots.contains_key(*k))
                        .count()
                    < MAX_TREES,
            "state tree limit",
        )?;
        if root != mpt::empty_root() {
            let reader = Nodes(&*self.nodes);
            need(
                reader.get_node(&root).map_err(bad)?.is_some(),
                "unpersisted attached root",
            )?;
            Trie::from_root(root).check_root(&reader).map_err(bad)?;
        }
        self.changed.remove(name);
        self.roots.insert(name.to_vec(), root);
        Ok(())
    }
    fn finish(mut self) -> io::Result<(BTreeMap<Vec<u8>, Hash>, mpt::Batch)> {
        for (name, trie) in std::mem::take(&mut self.changed) {
            let root = self.persist_trie(&trie)?;
            self.roots.insert(name, root);
        }
        Ok((self.roots, self.stats))
    }
}
#[cfg(unix)]
fn file_identity(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path)?;
    need(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "state database file replaced",
    )?;
    Ok((metadata.dev(), metadata.ino()))
}
#[cfg(unix)]
struct RegisteredDatabase {
    database: Weak<Database>,
    writer: Weak<()>,
}
#[cfg(unix)]
static OPEN_DATABASES: OnceLock<Mutex<BTreeMap<(u64, u64), RegisteredDatabase>>> = OnceLock::new();

pub struct StateDatabase {
    db: Arc<Database>,
    // Snapshots retain the database, but never this exclusive writer lease.
    // Drop db first so a new writer cannot race database teardown.
    _writer: Arc<()>,
    profile: Hash,
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(unix)]
    identity: (u64, u64),
}
struct HeaderCache {
    entries: BTreeMap<Hash, Arc<Version>>,
    order: VecDeque<Hash>,
}
pub struct Snapshot {
    read: ReadTransaction,
    // Drop the read transaction before its database handle.
    _database: Arc<Database>,
    profile: Hash,
    version: Arc<Version>,
    headers: Mutex<HeaderCache>,
}
#[derive(Clone, Debug)]
pub struct Commit {
    pub version: Version,
    pub new_nodes: mpt::Batch,
    pub selected: bool,
}
impl StateDatabase {
    pub fn open(path: &Path, profile: Hash) -> io::Result<Self> {
        match fs::symlink_metadata(path) {
            Ok(m) => need(
                m.is_file() && !m.file_type().is_symlink(),
                "state database file type",
            )?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Reuse the database held by pinned read transactions when this
        // process reopens the same inode. Hard-link aliases share the writer
        // lease. Other processes remain subject to redb's file lock.
        #[cfg(unix)]
        let mut registry = OPEN_DATABASES
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .map_err(bad)?;
        #[cfg(unix)]
        registry.retain(|_, entry| {
            entry.database.strong_count() != 0 || entry.writer.strong_count() != 0
        });
        #[cfg(unix)]
        let original_identity = match file_identity(path) {
            Ok(identity) => Some(identity),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        #[cfg(unix)]
        let existing =
            if let Some(entry) = original_identity.and_then(|identity| registry.get(&identity)) {
                if entry.writer.upgrade().is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "state database writer already open",
                    ));
                }
                entry.database.upgrade()
            } else {
                None
            };
        #[cfg(not(unix))]
        let existing: Option<Arc<Database>> = None;
        let db = if let Some(database) = existing {
            database
        } else {
            let mut builder = Database::builder();
            builder.set_cache_size(64 << 20);
            Arc::new(builder.create(path).map_err(bad)?)
        };
        #[cfg(unix)]
        let identity = file_identity(path)?;
        #[cfg(unix)]
        need(
            original_identity.is_none_or(|original| original == identity),
            "state database file replaced during open",
        )?;
        {
            let read = db.begin_read().map_err(bad)?;
            match read.open_table(META) {
                Ok(meta) => {
                    need(
                        meta.get(b"schema".as_slice())
                            .map_err(bad)?
                            .is_some_and(|v| v.value() == SCHEMA)
                            && meta
                                .get(b"profile".as_slice())
                                .map_err(bad)?
                                .is_some_and(|v| v.value() == profile),
                        "state database schema/profile",
                    )?;
                    read.open_table(NODES).map_err(bad)?;
                    read.open_table(VERSIONS).map_err(bad)?;
                    read.open_table(BLOCKS).map_err(bad)?;
                    read.open_table(PAYLOADS).map_err(bad)?;
                }
                Err(redb::TableError::TableDoesNotExist(_)) => {
                    need(
                        read.list_tables().map_err(bad)?.next().is_none(),
                        "refusing unknown state database migration",
                    )?;
                }
                Err(e) => return Err(bad(e)),
            };
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
            write.open_table(NODES).map_err(bad)?;
            write.open_table(VERSIONS).map_err(bad)?;
            write.open_table(BLOCKS).map_err(bad)?;
            write.open_table(PAYLOADS).map_err(bad)?;
            write.commit().map_err(bad)?;
        }
        let result = Self {
            db,
            _writer: Arc::new(()),
            profile,
            #[cfg(unix)]
            path: path.to_path_buf(),
            #[cfg(unix)]
            identity,
        };
        if let Some(id) = result.head()? {
            result.snapshot(Some(id))?.metadata()?;
        }
        #[cfg(unix)]
        registry.insert(
            identity,
            RegisteredDatabase {
                database: Arc::downgrade(&result.db),
                writer: Arc::downgrade(&result._writer),
            },
        );
        Ok(result)
    }
    fn check_file(&self) -> io::Result<()> {
        #[cfg(unix)]
        need(
            file_identity(&self.path)? == self.identity,
            "state database file replaced",
        )?;
        Ok(())
    }
    pub fn head(&self) -> io::Result<Option<Hash>> {
        self.check_file()?;
        let read = self.db.begin_read().map_err(bad)?;
        table_head(&read.open_table(META).map_err(bad)?)
    }
    pub fn version_for_block(&self, block: Hash) -> io::Result<Option<Hash>> {
        self.check_file()?;
        let read = self.db.begin_read().map_err(bad)?;
        read.open_table(BLOCKS)
            .map_err(bad)?
            .get(block.as_slice())
            .map_err(bad)?
            .map(|v| v.value().try_into().map_err(bad))
            .transpose()
    }
    pub fn snapshot(&self, id: Option<Hash>) -> io::Result<Snapshot> {
        self.check_file()?;
        let read = self.db.begin_read().map_err(bad)?;
        let id = id
            .or(table_head(&read.open_table(META).map_err(bad)?)?)
            .ok_or_else(|| bad("state database has no selected version"))?;
        let version = table_version(&read.open_table(VERSIONS).map_err(bad)?, self.profile, id)?;
        Ok(Snapshot {
            read,
            _database: self.db.clone(),
            version: Arc::new(version),
            profile: self.profile,
            headers: Mutex::new(HeaderCache {
                entries: BTreeMap::new(),
                order: VecDeque::new(),
            }),
        })
    }
    /// Root/node/version/block index and selected-head CAS commit in one durable
    /// transaction. A callback error or failed expectation publishes nothing.
    pub fn commit(
        &self,
        expected_head: Option<Hash>,
        parent: Option<Hash>,
        block: Hash,
        metadata: &[u8],
        select: bool,
        apply: impl FnOnce(&mut Workspace<'_, '_>) -> io::Result<()>,
    ) -> io::Result<Commit> {
        need(
            metadata.len() <= MAX_METADATA,
            "state version metadata bound",
        )?;
        self.commit_prepared(expected_head, parent, block, select, |workspace| {
            apply(workspace)?;
            Ok(metadata.to_vec())
        })
    }
    /// Prepare state and its metadata inside one transaction. Metadata may bind
    /// the resulting root set without a separate non-atomic follow-up write.
    pub fn commit_prepared(
        &self,
        expected_head: Option<Hash>,
        parent: Option<Hash>,
        block: Hash,
        select: bool,
        apply: impl FnOnce(&mut Workspace<'_, '_>) -> io::Result<Vec<u8>>,
    ) -> io::Result<Commit> {
        self.check_file()?;
        let mut write = self.db.begin_write().map_err(bad)?;
        write.set_durability(Durability::Immediate).map_err(bad)?;
        let mut meta = write.open_table(META).map_err(bad)?;
        need(
            table_head(&meta)? == expected_head,
            "stale state database head",
        )?;
        let mut versions = write.open_table(VERSIONS).map_err(bad)?;
        let previous = parent
            .map(|id| table_version(&versions, self.profile, id))
            .transpose()?;
        need(
            previous.is_some() || versions.is_empty().map_err(bad)?,
            "second parentless state genesis",
        )?;
        let (roots, new_nodes, metadata) = {
            let mut nodes = write.open_table(NODES).map_err(bad)?;
            let mut workspace = Workspace {
                nodes: &mut nodes,
                roots: previous
                    .as_ref()
                    .map(|v| v.roots.clone())
                    .unwrap_or_default(),
                changed: BTreeMap::new(),
                stats: mpt::Batch::default(),
            };
            let metadata = apply(&mut workspace)?;
            need(
                metadata.len() <= MAX_METADATA,
                "state version metadata bound",
            )?;
            let (roots, new_nodes) = workspace.finish()?;
            (roots, new_nodes, metadata)
        };
        let mut jumps = vec![];
        if let Some(parent) = &previous {
            jumps.push(parent.id);
            for i in 0..63 {
                let ancestor = table_version(&versions, self.profile, jumps[i])?;
                if let Some(id) = ancestor.jumps.get(i) {
                    jumps.push(*id);
                } else {
                    break;
                }
            }
        }
        let raw = encode(
            self.profile,
            previous.as_ref(),
            block,
            &roots,
            &metadata,
            &jumps,
        )?;
        let id = digest(&raw);
        let version = decode(self.profile, id, &raw)?;
        let mut payloads = write.open_table(PAYLOADS).map_err(bad)?;
        let new_payload = {
            let old = payloads
                .get(version.metadata_digest.as_slice())
                .map_err(bad)?;
            match old {
                Some(old) => {
                    need(
                        old.value() == metadata.as_slice(),
                        "immutable metadata payload conflict",
                    )?;
                    false
                }
                None => true,
            }
        };
        if new_payload {
            payloads
                .insert(version.metadata_digest.as_slice(), metadata.as_slice())
                .map_err(bad)?;
        }
        let mut blocks = write.open_table(BLOCKS).map_err(bad)?;
        {
            let old = blocks.get(block.as_slice()).map_err(bad)?;
            if let Some(old) = old {
                need(old.value() == id, "conflicting block state version")?;
            }
        }
        {
            let old = versions.get(id.as_slice()).map_err(bad)?;
            if let Some(old) = old {
                need(old.value() == raw, "immutable state version conflict")?;
            }
        }
        versions
            .insert(id.as_slice(), raw.as_slice())
            .map_err(bad)?;
        blocks
            .insert(block.as_slice(), id.as_slice())
            .map_err(bad)?;
        if select {
            meta.insert(b"head".as_slice(), id.as_slice())
                .map_err(bad)?;
        }
        drop(meta);
        drop(versions);
        drop(blocks);
        drop(payloads);
        #[cfg(test)]
        fault("before-commit");
        self.check_file()?;
        write.commit().map_err(bad)?;
        #[cfg(test)]
        fault("after-commit");
        Ok(Commit {
            version,
            new_nodes,
            selected: select,
        })
    }
    pub fn select(&self, expected: Hash, next: Hash) -> io::Result<()> {
        self.select_optional(Some(expected), next)
    }
    pub fn select_optional(&self, expected: Option<Hash>, next: Hash) -> io::Result<()> {
        self.check_file()?;
        let mut write = self.db.begin_write().map_err(bad)?;
        write.set_durability(Durability::Immediate).map_err(bad)?;
        let mut meta = write.open_table(META).map_err(bad)?;
        need(table_head(&meta)? == expected, "stale state selection")?;
        let version = table_version(
            &write.open_table(VERSIONS).map_err(bad)?,
            self.profile,
            next,
        )?;
        check_payload(
            &write.open_table(PAYLOADS).map_err(bad)?,
            version.metadata_digest,
        )?;
        let nodes = write.open_table(NODES).map_err(bad)?;
        for root in version.roots.values() {
            Trie::from_root(*root)
                .check_root(&Nodes(&nodes))
                .map_err(bad)?;
        }
        drop(nodes);
        meta.insert(b"head".as_slice(), next.as_slice())
            .map_err(bad)?;
        drop(meta);
        self.check_file()?;
        write.commit().map_err(bad)
    }
}
impl Snapshot {
    /// Verified version headers are shared only inside this immutable read
    /// transaction. A fresh snapshot starts empty and rechecks storage.
    fn header(&self, id: Hash) -> io::Result<Arc<Version>> {
        if id == self.version.id {
            return Ok(self.version.clone());
        }
        {
            let cache = self.headers.lock().map_err(bad)?;
            if let Some(value) = cache.entries.get(&id) {
                return Ok(value.clone());
            }
        }
        let version = Arc::new(table_version(
            &self.read.open_table(VERSIONS).map_err(bad)?,
            self.profile,
            id,
        )?);
        let mut cache = self.headers.lock().map_err(bad)?;
        if let Some(existing) = cache.entries.get(&id) {
            return Ok(existing.clone());
        }
        // Format bounds cap every header; retaining 128 entries never retains
        // the complete branch. No cache is shared across read transactions.
        while cache.entries.len() >= 128 {
            let first = cache
                .order
                .pop_front()
                .ok_or_else(|| bad("version cache accounting"))?;
            cache.entries.remove(&first);
        }
        cache.order.push_back(id);
        cache.entries.insert(id, version.clone());
        Ok(version)
    }
    pub fn cached_version_headers(&self) -> io::Result<usize> {
        Ok(self.headers.lock().map_err(bad)?.entries.len())
    }
    pub fn version(&self) -> &Version {
        &self.version
    }
    pub fn metadata(&self) -> io::Result<Vec<u8>> {
        let payloads = self.read.open_table(PAYLOADS).map_err(bad)?;
        let raw = payloads
            .get(self.version.metadata_digest.as_slice())
            .map_err(bad)?
            .ok_or_else(|| bad("missing metadata payload"))?;
        let bytes = raw.value();
        need(
            bytes.len() <= MAX_METADATA && digest(bytes) == self.version.metadata_digest,
            "metadata payload digest/bytes",
        )?;
        Ok(bytes.to_vec())
    }
    /// Binary ancestors are immutable, branch-specific and read through the
    /// same pinned transaction; large block/receipt payloads remain separate.
    pub fn ancestor(&self, height: u64) -> io::Result<Arc<Version>> {
        need(height <= self.version.height, "unavailable state height")?;
        let mut current = self.version.clone();
        while current.height > height {
            let distance = current.height - height;
            let k = (63 - distance.leading_zeros()) as usize;
            let id = *current
                .jumps
                .get(k)
                .ok_or_else(|| bad("missing version ancestor index"))?;
            let next = self.header(id)?;
            need(
                next.height == current.height - (1u64 << k),
                "state ancestor index height",
            )?;
            current = next;
        }
        Ok(current)
    }
    /// Query other immutable versions through this same pinned read transaction.
    pub fn version_for_block(&self, block: Hash) -> io::Result<Option<Arc<Version>>> {
        let blocks = self.read.open_table(BLOCKS).map_err(bad)?;
        let Some(id) = blocks.get(block.as_slice()).map_err(bad)? else {
            return Ok(None);
        };
        let id: Hash = id.value().try_into().map_err(bad)?;
        let version = self.header(id)?;
        need(version.block == block, "version block index binding")?;
        Ok(Some(version))
    }
    pub fn metadata_for(&self, id: Hash) -> io::Result<Vec<u8>> {
        let version = self.header(id)?;
        let table = self.read.open_table(PAYLOADS).map_err(bad)?;
        let value = table
            .get(version.metadata_digest.as_slice())
            .map_err(bad)?
            .ok_or_else(|| bad("missing version metadata"))?;
        need(
            value.value().len() <= MAX_METADATA && digest(value.value()) == version.metadata_digest,
            "version metadata digest/bytes",
        )?;
        Ok(value.value().to_vec())
    }
    pub fn root(&self, name: &[u8]) -> Hash {
        self.version
            .roots
            .get(name)
            .copied()
            .unwrap_or_else(mpt::empty_root)
    }
    /// Verify presence and canonical encoding of a named root without scanning
    /// its leaves. Individual paths are still verified when queried.
    pub fn check_root(&self, name: &[u8]) -> io::Result<()> {
        let nodes = self.read.open_table(NODES).map_err(bad)?;
        Trie::from_root(self.root(name))
            .check_root(&Nodes(&nodes))
            .map_err(bad)
    }
    pub fn get(&self, name: &[u8], key: &[u8]) -> io::Result<Option<Vec<u8>>> {
        self.get_at(self.root(name), key)
    }
    pub fn scan(
        &self,
        name: &[u8],
        max_entries: usize,
        max_bytes: usize,
        visit: &mut impl FnMut(Hash, &[u8]) -> io::Result<()>,
    ) -> io::Result<mpt::Scan> {
        let nodes = self.read.open_table(NODES).map_err(bad)?;
        Trie::from_root(self.root(name))
            .visit_leaves(&Nodes(&nodes), max_entries, max_bytes, &mut |key, value| {
                visit(key, value).map_err(|e| mpt::Error::Backend(e.to_string()))
            })
            .map_err(bad)
    }
    pub fn get_at(&self, root: Hash, key: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let nodes = self.read.open_table(NODES).map_err(bad)?;
        Trie::from_root(root).get(&Nodes(&nodes), key).map_err(bad)
    }
    pub fn get_hashed(&self, name: &[u8], key: Hash) -> io::Result<Option<Vec<u8>>> {
        let nodes = self.read.open_table(NODES).map_err(bad)?;
        Trie::from_root(self.root(name))
            .get_hashed(&Nodes(&nodes), key)
            .map_err(bad)
    }
    pub fn node(&self, hash: &Hash) -> io::Result<Option<Vec<u8>>> {
        let table = self.read.open_table(NODES).map_err(bad)?;
        let raw = Nodes(&table).get_node(hash).map_err(bad)?;
        if let Some(raw) = &raw {
            need(
                raw.len() <= mpt::MAX_NODE_BYTES
                    && quai_primitives::transaction::keccak(raw) == *hash,
                "state node hash/width",
            )?;
        }
        Ok(raw)
    }
    pub fn check_at(&self, root: Hash) -> io::Result<()> {
        let nodes = self.read.open_table(NODES).map_err(bad)?;
        Trie::from_root(root)
            .check_root(&Nodes(&nodes))
            .map_err(bad)
    }
    pub fn scan_at(
        &self,
        root: Hash,
        max_entries: usize,
        max_bytes: usize,
        visit: &mut impl FnMut(Hash, &[u8]) -> io::Result<()>,
    ) -> io::Result<mpt::Scan> {
        let nodes = self.read.open_table(NODES).map_err(bad)?;
        Trie::from_root(root)
            .visit_leaves(&Nodes(&nodes), max_entries, max_bytes, &mut |key, value| {
                visit(key, value).map_err(|e| mpt::Error::Backend(e.to_string()))
            })
            .map_err(bad)
    }
}

#[cfg(test)]
fn fault(point: &str) {
    if std::env::var("QUAI_STATE_DB_FAULT").is_ok_and(|v| v == point) {
        std::process::exit(74);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "quai-state-db-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&d).unwrap();
        d
    }
    fn id(n: u64) -> Hash {
        let mut h = [0; 32];
        h[..8].copy_from_slice(&n.to_be_bytes());
        h
    }
    fn genesis(db: &StateDatabase) -> Hash {
        db.commit(None, None, id(0), b"genesis", true, |_| Ok(()))
            .unwrap()
            .version
            .id
    }
    #[test]
    fn version_header_cache_is_bounded_and_pinned_but_fresh_reads_detect_corruption() {
        let dir = dir();
        let db = StateDatabase::open(&dir.join("state.redb"), [12; 32]).unwrap();
        let mut tip = db
            .commit(None, None, id(0), b"genesis", true, |_| Ok(()))
            .unwrap()
            .version;
        for number in 1..=200 {
            tip = db
                .commit(
                    Some(tip.id),
                    Some(tip.id),
                    id(number),
                    b"version",
                    true,
                    |_| Ok(()),
                )
                .unwrap()
                .version;
        }
        let pinned = db.snapshot(Some(tip.id)).unwrap();
        let first = pinned.version_for_block(id(100)).unwrap().unwrap();
        let second = pinned.version_for_block(id(100)).unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(pinned.ancestor(100).unwrap().id, first.id);
        for number in 0..=200 {
            assert_eq!(
                pinned
                    .version_for_block(id(number))
                    .unwrap()
                    .unwrap()
                    .height,
                number
            );
        }
        assert!(pinned.cached_version_headers().unwrap() <= 128);
        let first = pinned.version_for_block(id(100)).unwrap().unwrap();
        let write = db.db.begin_write().unwrap();
        {
            let mut versions = write.open_table(VERSIONS).unwrap();
            versions
                .insert(first.id.as_slice(), b"damaged header".as_slice())
                .unwrap();
        }
        write.commit().unwrap();
        assert_eq!(
            pinned.version_for_block(id(100)).unwrap().unwrap().id,
            first.id
        );
        let fresh = db.snapshot(Some(tip.id)).unwrap();
        assert!(fresh.version_for_block(id(100)).is_err());
        drop(fresh);
        drop(pinned);
        drop(db);
        fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn pinned_state_survives_writer_close_and_reopen_with_one_writer() {
        let directory = dir();
        let path = directory.join("state.redb");
        let db = StateDatabase::open(&path, id(99)).unwrap();
        let g = genesis(&db);
        let parent = db
            .commit(Some(g), Some(g), id(1), b"parent", true, |w| {
                w.put(b"accounts", b"alice", b"parent")
            })
            .unwrap()
            .version;
        let pinned = db.snapshot(Some(parent.id)).unwrap();
        assert!(StateDatabase::open(&path, id(99)).is_err());
        drop(db);
        assert_eq!(pinned.metadata().unwrap(), b"parent");
        assert_eq!(
            pinned.get(b"accounts", b"alice").unwrap(),
            Some(b"parent".to_vec())
        );
        let db = StateDatabase::open(&path, id(99)).unwrap();
        assert!(StateDatabase::open(&path, id(99)).is_err());
        let child = db
            .commit(
                Some(parent.id),
                Some(parent.id),
                id(2),
                b"child",
                true,
                |w| w.put(b"accounts", b"alice", b"child"),
            )
            .unwrap()
            .version;
        assert_eq!(
            pinned.get(b"accounts", b"alice").unwrap(),
            Some(b"parent".to_vec())
        );
        assert!(pinned.version_for_block(child.block).unwrap().is_none());
        assert_eq!(
            db.snapshot(None)
                .unwrap()
                .get(b"accounts", b"alice")
                .unwrap(),
            Some(b"child".to_vec())
        );
        assert!(db
            .commit(
                Some(parent.id),
                Some(parent.id),
                id(3),
                b"stale",
                true,
                |_| panic!("stale writer ran callback")
            )
            .is_err());
        drop(db);
        assert_eq!(pinned.metadata().unwrap(), b"parent");
        drop(pinned);
        let db = StateDatabase::open(&path, id(99)).unwrap();
        assert_eq!(db.head().unwrap(), Some(child.id));
        drop(db);
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn pinned_snapshot_owns_database_until_its_final_read() {
        let directory = dir();
        let path = directory.join("state.redb");
        let db = StateDatabase::open(&path, id(99)).unwrap();
        let g = genesis(&db);
        let pinned = db.snapshot(Some(g)).unwrap();
        drop(db);
        assert_eq!(pinned.metadata().unwrap(), b"genesis");
        assert!(pinned.get(b"accounts", b"absent").unwrap().is_none());
        assert_eq!(pinned.version_for_block(id(0)).unwrap().unwrap().id, g);
        drop(pinned);
        let db = StateDatabase::open(&path, id(99)).unwrap();
        assert_eq!(db.head().unwrap(), Some(g));
        drop(db);
        fs::remove_dir_all(directory).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn inode_writer_lease_rejects_aliases_and_all_but_one_concurrent_reopen() {
        use std::{sync::Barrier, thread};
        let directory = dir();
        let path = directory.join("state.redb");
        let alias = directory.join("alias.redb");
        let db = StateDatabase::open(&path, id(99)).unwrap();
        let g = genesis(&db);
        fs::hard_link(&path, &alias).unwrap();
        let pinned = db.snapshot(Some(g)).unwrap();
        let refused = StateDatabase::open(&alias, id(99)).err().unwrap();
        assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
        drop(db);
        assert!(StateDatabase::open(&alias, id(100)).is_err());
        let barrier = Arc::new(Barrier::new(7));
        let threads = (0..6)
            .map(|number| {
                let path = if number % 2 == 0 {
                    path.clone()
                } else {
                    alias.clone()
                };
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    let result = StateDatabase::open(&path, id(99));
                    let outcome = match &result {
                        Ok(db) => {
                            assert_eq!(db.head().unwrap(), Some(g));
                            Ok(())
                        }
                        Err(error) => Err(error.kind()),
                    };
                    // Keep the successful writer alive until every attempt finishes.
                    barrier.wait();
                    drop(result);
                    outcome
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        barrier.wait();
        let outcomes = threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(outcomes.iter().filter(|o| o.is_ok()).count(), 1);
        assert!(outcomes
            .iter()
            .filter_map(|o| o.as_ref().err())
            .all(|kind| *kind == io::ErrorKind::WouldBlock));
        assert_eq!(pinned.metadata().unwrap(), b"genesis");
        let db = StateDatabase::open(&alias, id(99)).unwrap();
        assert!(Arc::ptr_eq(&db.db, &pinned._database));
        drop(db);
        drop(pinned);
        fs::remove_dir_all(directory).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn replacing_state_path_during_commit_cannot_publish_or_reuse_writer() {
        let directory = dir();
        let path = directory.join("state.redb");
        let moved = directory.join("moved.redb");
        let db = StateDatabase::open(&path, id(99)).unwrap();
        let g = genesis(&db);
        let pinned = db.snapshot(Some(g)).unwrap();
        assert!(db
            .commit(Some(g), Some(g), id(1), b"replaced", true, |w| {
                w.put(b"accounts", b"alice", b"unpublished")?;
                fs::rename(&path, &moved)?;
                fs::write(&path, b"replacement")?;
                Ok(())
            })
            .is_err());
        assert_eq!(pinned.metadata().unwrap(), b"genesis");
        assert!(pinned.get(b"accounts", b"alice").unwrap().is_none());
        assert!(db.head().is_err());
        assert!(db.snapshot(None).is_err());
        assert!(db.version_for_block(id(0)).is_err());
        assert!(db.select(g, g).is_err());
        assert!(db
            .commit(Some(g), Some(g), id(2), b"wrong path", true, |_| panic!(
                "replaced writer ran callback"
            ))
            .is_err());
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        // Renaming preserves the inode: a different path cannot bypass its lease.
        assert_eq!(
            StateDatabase::open(&moved, id(99)).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(db);
        let db = StateDatabase::open(&moved, id(99)).unwrap();
        assert_eq!(db.head().unwrap(), Some(g));
        assert!(db.version_for_block(id(1)).unwrap().is_none());
        assert!(db
            .snapshot(None)
            .unwrap()
            .get(b"accounts", b"alice")
            .unwrap()
            .is_none());
        assert_eq!(pinned.metadata().unwrap(), b"genesis");
        drop(db);
        drop(pinned);
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn independent_forks_pinned_reads_atomic_failure_and_reopen() {
        let d = dir();
        let p = d.join("state.redb");
        let db = StateDatabase::open(&p, id(99)).unwrap();
        let g = genesis(&db);
        let a = db
            .commit(Some(g), Some(g), id(1), b"branch-a", true, |w| {
                w.put(b"accounts", b"alice", b"one")
            })
            .unwrap();
        let pinned = db.snapshot(None).unwrap();
        let b = db
            .commit(
                Some(a.version.id),
                Some(g),
                id(2),
                b"branch-b",
                false,
                |w| w.put(b"accounts", b"bob", b"two"),
            )
            .unwrap();
        assert_eq!(db.head().unwrap(), Some(a.version.id));
        assert!(db
            .commit(Some(g), Some(g), id(3), b"stale", true, |_| panic!(
                "stale commit ran callback"
            ))
            .is_err());
        let nodes_before = db
            .db
            .begin_read()
            .unwrap()
            .open_table(NODES)
            .unwrap()
            .len()
            .unwrap();
        assert!(db
            .commit(
                Some(a.version.id),
                Some(a.version.id),
                id(4),
                b"aborted",
                true,
                |w| {
                    w.put(b"accounts", b"alice", b"bad")?;
                    Err(bad("injected execution failure"))
                }
            )
            .is_err());
        assert_eq!(
            db.db
                .begin_read()
                .unwrap()
                .open_table(NODES)
                .unwrap()
                .len()
                .unwrap(),
            nodes_before
        );
        assert!(db.version_for_block(id(4)).unwrap().is_none());
        db.select(a.version.id, b.version.id).unwrap();
        let current = db.snapshot(None).unwrap();
        assert_eq!(
            current.get(b"accounts", b"bob").unwrap(),
            Some(b"two".to_vec())
        );
        assert!(current.get(b"accounts", b"alice").unwrap().is_none());
        assert_eq!(
            pinned.get(b"accounts", b"alice").unwrap(),
            Some(b"one".to_vec())
        );
        assert!(pinned.get(b"accounts", b"bob").unwrap().is_none());
        drop(current);
        drop(pinned);
        drop(db);
        let db = StateDatabase::open(&p, id(99)).unwrap();
        assert_eq!(db.head().unwrap(), Some(b.version.id));
        assert_eq!(
            db.snapshot(None).unwrap().get(b"accounts", b"bob").unwrap(),
            Some(b"two".to_vec())
        );
        drop(db);
        assert!(StateDatabase::open(&p, id(100)).is_err());
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn child_storage_tries_commit_with_account_root_in_same_transaction() {
        let d = dir();
        let db = StateDatabase::open(&d.join("state.redb"), id(99)).unwrap();
        let g = genesis(&db);
        let next = db
            .commit(Some(g), Some(g), id(1), b"nested-storage", true, |w| {
                let mut storage = Trie::default();
                w.update_trie(&mut storage, b"slot", b"value")?;
                let root = w.persist_trie(&storage)?;
                w.put(b"accounts", b"contract", &root)?;
                Ok(())
            })
            .unwrap();
        let view = db.snapshot(Some(next.version.id)).unwrap();
        let storage: Hash = view
            .get(b"accounts", b"contract")
            .unwrap()
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            view.get_at(storage, b"slot").unwrap(),
            Some(b"value".to_vec())
        );
        drop(view);
        drop(db);
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn missing_cross_database_references_cannot_publish_a_version() {
        let d = dir();
        let db = StateDatabase::open(&d.join("state.redb"), id(99)).unwrap();
        let g = genesis(&db);
        assert!(db
            .commit(Some(g), Some(g), id(1), b"missing", true, |w| {
                let foreign = Trie::from_root(id(55));
                w.persist_trie(&foreign)?;
                w.attach_root(b"accounts", foreign.root())
            })
            .is_err());
        assert_eq!(db.head().unwrap(), Some(g));
        assert!(db.version_for_block(id(1)).unwrap().is_none());
        drop(db);
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn later_node_corruption_is_detected_by_fresh_reads_while_pinned_view_is_consistent() {
        let d = dir();
        let db = StateDatabase::open(&d.join("state.redb"), id(99)).unwrap();
        let g = genesis(&db);
        let a = db
            .commit(Some(g), Some(g), id(1), b"data", true, |w| {
                w.put(b"accounts", b"alice", b"one")
            })
            .unwrap();
        let pinned = db.snapshot(None).unwrap();
        assert!(pinned.get(b"accounts", b"alice").unwrap().is_some());
        {
            let tx = db.db.begin_write().unwrap();
            {
                let mut table = tx.open_table(NODES).unwrap();
                table
                    .insert(
                        a.version.roots[b"accounts".as_slice()].as_slice(),
                        b"damaged".as_slice(),
                    )
                    .unwrap();
            }
            tx.commit().unwrap();
        }
        assert!(db
            .snapshot(None)
            .unwrap()
            .get(b"accounts", b"alice")
            .is_err());
        assert_eq!(
            pinned.get(b"accounts", b"alice").unwrap(),
            Some(b"one".to_vec())
        );
        drop(pinned);
        drop(db);
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn damaged_payload_cannot_be_selected_or_opened_as_complete_state() {
        let d = dir();
        let p = d.join("state.redb");
        let db = StateDatabase::open(&p, id(99)).unwrap();
        let g = genesis(&db);
        let a = db
            .commit(Some(g), Some(g), id(1), b"first", true, |w| {
                w.put(b"accounts", b"alice", b"one")
            })
            .unwrap();
        let b = db
            .commit(Some(a.version.id), Some(g), id(2), b"second", false, |w| {
                w.put(b"accounts", b"bob", b"two")
            })
            .unwrap();
        {
            let tx = db.db.begin_write().unwrap();
            {
                let mut t = tx.open_table(PAYLOADS).unwrap();
                t.insert(b.version.metadata_digest.as_slice(), b"damaged".as_slice())
                    .unwrap();
            }
            tx.commit().unwrap();
        }
        assert!(db.select(a.version.id, b.version.id).is_err());
        assert_eq!(db.head().unwrap(), Some(a.version.id));
        {
            let tx = db.db.begin_write().unwrap();
            {
                let mut t = tx.open_table(PAYLOADS).unwrap();
                t.insert(a.version.metadata_digest.as_slice(), b"damaged".as_slice())
                    .unwrap();
            }
            tx.commit().unwrap();
        }
        drop(db);
        assert!(StateDatabase::open(&p, id(99)).is_err());
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn commit_crash_child() {
        let Ok(d) = std::env::var("QUAI_STATE_DB_CHILD") else {
            return;
        };
        let db = StateDatabase::open(&Path::new(&d).join("state.redb"), id(99)).unwrap();
        let g = db.head().unwrap().unwrap();
        db.commit(Some(g), Some(g), id(1), b"committed", true, |w| {
            w.put(b"accounts", b"alice", b"one")
        })
        .unwrap();
        panic!("crash failpoint did not exit");
    }
    #[test]
    fn process_exit_recovers_old_or_complete_new_root_and_indexes() {
        for phase in ["before-commit", "after-commit"] {
            let d = dir();
            let db = StateDatabase::open(&d.join("state.redb"), id(99)).unwrap();
            let g = genesis(&db);
            drop(db);
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "state_db::tests::commit_crash_child"])
                .env("QUAI_STATE_DB_CHILD", &d)
                .env("QUAI_STATE_DB_FAULT", phase)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(74));
            let db = StateDatabase::open(&d.join("state.redb"), id(99)).unwrap();
            let view = db.snapshot(None).unwrap();
            if phase == "before-commit" {
                assert_eq!(view.version.id, g);
                assert!(db.version_for_block(id(1)).unwrap().is_none());
                assert!(view.get(b"accounts", b"alice").unwrap().is_none());
            } else {
                assert_eq!(view.version.height, 1);
                assert_eq!(db.version_for_block(id(1)).unwrap(), Some(view.version.id));
                assert_eq!(
                    view.get(b"accounts", b"alice").unwrap(),
                    Some(b"one".to_vec())
                );
            }
            drop(view);
            drop(db);
            fs::remove_dir_all(d).unwrap();
        }
    }
}
