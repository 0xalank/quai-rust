//! Durable scratch execution checkpoints. This store is deliberately separate
//! from the frozen RPC store and confers no consensus or canonical-chain authority.
//! A future admission layer must validate execution/consensus before publication.
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
pub type Id = [u8; 32];
const MAGIC: &[u8] = b"QUAI-PRIVATE-EXECUTION-JOURNAL-1\n";
const MAX_BYTES: usize = 64 << 20;
const MAX_DEPTH: usize = 65536;
fn hash(b: &[u8]) -> Id {
    Sha256::digest(b).into()
}
fn hex(id: &Id) -> String {
    id.iter().map(|v| format!("{v:02x}")).collect()
}
fn error(s: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, s)
}
fn read_bounded(path: &Path, max: usize) -> std::io::Result<Vec<u8>> {
    let f = File::open(path)?;
    if f.metadata()?.len() > max as u64 {
        return Err(error("checkpoint length limit"));
    }
    let mut b = vec![];
    f.take(max as u64 + 1).read_to_end(&mut b)?;
    if b.len() > max {
        return Err(error("checkpoint length limit"));
    }
    Ok(b)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub block: Id,
    pub parent: Option<Id>,
    pub height: u64,
    pub payload: Vec<u8>,
}
#[derive(Clone, Copy)]
struct Metadata {
    parent: Option<Id>,
    height: u64,
}
impl Checkpoint {
    fn encode(&self) -> std::io::Result<Vec<u8>> {
        if self.payload.len() > MAX_BYTES - 160 {
            return Err(error("checkpoint payload limit"));
        }
        let mut b = MAGIC.to_vec();
        b.extend(self.block);
        b.push(u8::from(self.parent.is_some()));
        b.extend(self.parent.unwrap_or([0; 32]));
        b.extend(self.height.to_be_bytes());
        b.extend((self.payload.len() as u64).to_be_bytes());
        b.extend(&self.payload);
        Ok(b)
    }
    fn parts(b: &[u8]) -> std::io::Result<(Metadata, &[u8])> {
        let n = MAGIC.len();
        if b.len() < n + 81 || !b.starts_with(MAGIC) || b[n + 32] > 1 {
            return Err(error("checkpoint format"));
        }
        let height = u64::from_be_bytes(crate::slice::array(&b[n + 65..n + 73])?);
        let len = u64::from_be_bytes(crate::slice::array(&b[n + 73..n + 81])?);
        if len as u128 != (b.len() - n - 81) as u128 {
            return Err(error("checkpoint payload length"));
        }
        let raw: Id = crate::slice::array(&b[n + 33..n + 65])?;
        if b[n + 32] == 0 && raw != [0; 32] {
            return Err(error("noncanonical checkpoint parent"));
        }
        Ok((
            Metadata {
                parent: if b[n + 32] == 1 { Some(raw) } else { None },
                height,
            },
            &b[n + 81..],
        ))
    }
    fn decode(b: &[u8]) -> std::io::Result<Self> {
        let (m, payload) = Self::parts(b)?;
        Ok(Self {
            block: crate::slice::array(&b[MAGIC.len()..MAGIC.len() + 32])?,
            parent: m.parent,
            height: m.height,
            payload: payload.to_vec(),
        })
    }
}
pub struct ScratchJournal {
    directory: PathBuf,
    profile: Id,
    head: Option<Id>,
    _lock: File,
    poisoned: bool,
}
impl Drop for ScratchJournal {
    fn drop(&mut self) {
        // Closing only this descriptor is insufficient when a concurrent fork
        // briefly inherits the same open-file description. Explicit unlock
        // releases the advisory writer lease before any duplicate closes.
        let _ = self._lock.unlock();
    }
}
impl ScratchJournal {
    /// The caller chooses a dedicated private directory and a frozen profile hash.
    /// Kernel file locking is released on process death; stale PID files are absent.
    pub fn open(directory: &Path, profile: Id) -> std::io::Result<Self> {
        fs::create_dir_all(directory)?;
        let marker = directory.join("PRIVATE-EXECUTION");
        if !marker.exists() {
            if fs::read_dir(directory)?.next().is_some() {
                return Err(error("refusing non-private store directory"));
            }
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)?;
            f.write_all(MAGIC)?;
            f.write_all(&profile)?;
            f.sync_all()?;
            File::open(directory)?.sync_all()?;
        }
        let expected = [MAGIC, profile.as_slice()].concat();
        if read_bounded(&marker, expected.len())? != expected {
            return Err(error("private store profile mismatch"));
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("LOCK"))?;
        lock.try_lock()
            .map_err(|_| error("private store already has a writer"))?;
        fs::create_dir_all(directory.join("objects"))?;
        let mut s = Self {
            directory: directory.to_path_buf(),
            profile,
            head: None,
            _lock: lock,
            poisoned: false,
        };
        s.head = s.read_head()?;
        s.verify_lineage(s.head)?;
        Ok(s)
    }
    pub fn profile(&self) -> Id {
        self.profile
    }
    pub fn head(&self) -> Option<Id> {
        self.head
    }
    fn read_head(&self) -> std::io::Result<Option<Id>> {
        let p = self.directory.join("HEAD");
        if !p.exists() {
            return Ok(None);
        }
        let b = read_bounded(&p, 64)?;
        if b.len() != 64 || b[32..] != hash(&b[..32]) {
            return Err(error("head checksum"));
        }
        Ok(Some(crate::slice::array(&b[..32])?))
    }
    pub fn load(&self, id: Id) -> std::io::Result<Checkpoint> {
        let b = read_bounded(&self.directory.join("objects").join(hex(&id)), MAX_BYTES)?;
        if hash(&b) != id {
            return Err(error("checkpoint checksum"));
        }
        Checkpoint::decode(&b)
    }
    fn metadata(&self, id: Id, memo: &mut BTreeMap<Id, Metadata>) -> std::io::Result<Metadata> {
        if let Some(value) = memo.get(&id) {
            return Ok(*value);
        }
        let b = read_bounded(&self.directory.join("objects").join(hex(&id)), MAX_BYTES)?;
        if hash(&b) != id {
            return Err(error("checkpoint checksum"));
        }
        let (value, _) = Checkpoint::parts(&b)?;
        memo.insert(id, value);
        Ok(value)
    }
    /// This memo exists only for one traversal operation. Every object in the
    /// union is read and checksummed; nothing is trusted across public calls.
    /// Metadata extraction borrows payload bytes rather than cloning snapshots.
    fn ancestry(
        &self,
        mut id: Option<Id>,
        memo: &mut BTreeMap<Id, Metadata>,
    ) -> std::io::Result<Vec<Id>> {
        let mut height = None;
        let mut seen = BTreeSet::new();
        let mut ancestry = vec![];
        while let Some(current) = id {
            if !seen.insert(current) || seen.len() > MAX_DEPTH {
                return Err(error("checkpoint ancestry bound"));
            }
            let v = self.metadata(current, memo)?;
            if height.is_some_and(|h| v.height.checked_add(1) != Some(h)) {
                return Err(error("checkpoint height discontinuity"));
            }
            height = Some(v.height);
            ancestry.push(current);
            id = v.parent;
            if id.is_none() && v.height != 0 {
                return Err(error("checkpoint ancestry lacks genesis"));
            }
        }
        Ok(ancestry)
    }
    fn verify_lineage(&self, id: Option<Id>) -> std::io::Result<()> {
        self.ancestry(id, &mut BTreeMap::new()).map(|_| ())
    }
    fn replace(&self, path: &Path, b: &[u8]) -> std::io::Result<()> {
        let temp = path.with_extension("pending");
        if temp.exists() {
            fs::remove_file(&temp)?;
        }
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        f.write_all(b)?;
        f.sync_all()?;
        fs::rename(temp, path)?;
        let dir = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "journal path without a directory",
            )
        })?;
        File::open(dir)?.sync_all()
    }
    /// Saves an immutable candidate. Its parent must already exist in this private
    /// store. Content-addressing detects corrupt snapshots before any head change.
    pub fn stage(&mut self, v: &Checkpoint) -> std::io::Result<Id> {
        if self.poisoned {
            return Err(error("reopen store after write failure"));
        }
        if let Some(parent) = v.parent {
            let p = self.metadata(parent, &mut BTreeMap::new())?;
            if p.height.checked_add(1) != Some(v.height) {
                return Err(error("candidate height"));
            }
        } else if v.height != 0 {
            return Err(error("genesis checkpoint height"));
        }
        let b = v.encode()?;
        let id = hash(&b);
        let path = self.directory.join("objects").join(hex(&id));
        if path.exists() {
            if read_bounded(&path, MAX_BYTES)? != b {
                return Err(error("candidate object collision/corruption"));
            }
            return Ok(id);
        }
        self.poisoned = true;
        self.replace(&path, &b)?;
        self.poisoned = false;
        Ok(id)
    }
    /// Atomic compare-and-swap of the private execution head. Fork choice is the
    /// caller's responsibility; this primitive does not select a network chain.
    pub fn publish(&mut self, expected: Option<Id>, next: Id) -> std::io::Result<()> {
        if self.poisoned {
            return Err(error("reopen store after write failure"));
        }
        if self.head != expected || self.read_head()? != expected {
            return Err(error("stale execution head"));
        }
        self.reorg_path(next)?;
        let mut b = next.to_vec();
        b.extend(hash(&next));
        self.poisoned = true;
        self.replace(&self.directory.join("HEAD"), &b)?;
        self.head = Some(next);
        self.poisoned = false;
        Ok(())
    }
    /// Branch switch plan: old-tip checkpoints to undo, then new checkpoints in
    /// forward order. The immutable snapshots make rollback independent of RAM.
    pub fn reorg_path(&self, next: Id) -> std::io::Result<(Vec<Id>, Vec<Id>)> {
        let mut memo = BTreeMap::new();
        let mut old = self.ancestry(self.head, &mut memo)?;
        let mut new = self.ancestry(Some(next), &mut memo)?;
        let positions = old
            .iter()
            .enumerate()
            .map(|(n, id)| (*id, n))
            .collect::<std::collections::BTreeMap<_, _>>();
        for (new_position, v) in new.iter().enumerate() {
            if let Some(n) = positions.get(v) {
                old.truncate(*n);
                new.truncate(new_position);
                new.reverse();
                return Ok((old, new));
            }
        }
        if self.head.is_some() {
            return Err(error("unrelated private genesis"));
        }
        new.reverse();
        Ok((old, new))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn dir(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "quai-journal-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&p).unwrap();
        p
    }
    fn checkpoint(parent: Option<Id>, height: u64, n: u8) -> Checkpoint {
        Checkpoint {
            block: [n; 32],
            parent,
            height,
            payload: vec![n; 17],
        }
    }
    #[test]
    fn restart_and_reorg_keep_immutable_branches() {
        let d = dir("fork");
        let mut s = ScratchJournal::open(&d, [1; 32]).unwrap();
        let g = s.stage(&checkpoint(None, 0, 0)).unwrap();
        s.publish(None, g).unwrap();
        let a = s.stage(&checkpoint(Some(g), 1, 1)).unwrap();
        s.publish(Some(g), a).unwrap();
        let a2 = s.stage(&checkpoint(Some(a), 2, 2)).unwrap();
        s.publish(Some(a), a2).unwrap();
        let b = s.stage(&checkpoint(Some(g), 1, 3)).unwrap();
        let b2 = s.stage(&checkpoint(Some(b), 2, 4)).unwrap();
        assert_eq!(s.reorg_path(b2).unwrap(), (vec![a2, a], vec![b, b2]));
        assert!(s.publish(Some(g), b2).is_err());
        s.publish(Some(a2), b2).unwrap();
        drop(s);
        let s = ScratchJournal::open(&d, [1; 32]).unwrap();
        assert_eq!(s.head(), Some(b2));
        assert_eq!(s.load(a2).unwrap().payload, vec![2; 17]);
        drop(s);
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn excludes_other_writers_and_wrong_profile() {
        let d = dir("lock");
        let s = ScratchJournal::open(&d, [1; 32]).unwrap();
        assert!(ScratchJournal::open(&d, [1; 32]).is_err());
        assert!(ScratchJournal::open(&d, [2; 32]).is_err());
        drop(s);
        assert!(ScratchJournal::open(&d, [1; 32]).is_ok());
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn drop_unlocks_an_inherited_descriptor_before_reopen() {
        let d = dir("inherited-lock");
        let s = ScratchJournal::open(&d, [1; 32]).unwrap();
        let inherited = s._lock.try_clone().unwrap();
        drop(s);
        let reopened = ScratchJournal::open(&d, [1; 32]).unwrap();
        drop(reopened);
        drop(inherited);
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn corrupt_committed_object_fails_closed() {
        let d = dir("corrupt");
        let mut s = ScratchJournal::open(&d, [1; 32]).unwrap();
        let g = s.stage(&checkpoint(None, 0, 0)).unwrap();
        s.publish(None, g).unwrap();
        drop(s);
        fs::write(d.join("objects").join(hex(&g)), b"truncated").unwrap();
        assert!(ScratchJournal::open(&d, [1; 32]).is_err());
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn crash_child() {
        let Ok(d) = std::env::var("QUAI_JOURNAL_CRASH_DIR") else {
            return;
        };
        let mut s = ScratchJournal::open(Path::new(&d), [1; 32]).unwrap();
        let old = s.head().unwrap();
        let next = s.stage(&checkpoint(Some(old), 1, 2)).unwrap();
        if std::env::var("QUAI_JOURNAL_CRASH_PHASE").unwrap() == "published" {
            s.publish(Some(old), next).unwrap();
        }
        std::process::exit(72);
    }
    #[test]
    fn process_exit_releases_lock_and_recovers_committed_boundary() {
        for phase in ["staged", "published"] {
            let d = dir(phase);
            let mut s = ScratchJournal::open(&d, [1; 32]).unwrap();
            let g = s.stage(&checkpoint(None, 0, 0)).unwrap();
            s.publish(None, g).unwrap();
            drop(s);
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "journal::tests::crash_child"])
                .env("QUAI_JOURNAL_CRASH_DIR", &d)
                .env("QUAI_JOURNAL_CRASH_PHASE", phase)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(72));
            let s = ScratchJournal::open(&d, [1; 32]).unwrap();
            let head = s.head().unwrap();
            assert_eq!(
                s.load(head).unwrap().height,
                u64::from(phase == "published")
            );
            drop(s);
            fs::remove_dir_all(d).unwrap();
        }
    }
    #[test]
    fn incomplete_temporary_files_do_not_replace_head() {
        let d = dir("partial");
        let mut s = ScratchJournal::open(&d, [1; 32]).unwrap();
        let g = s.stage(&checkpoint(None, 0, 0)).unwrap();
        s.publish(None, g).unwrap();
        drop(s);
        fs::write(d.join("HEAD.pending"), b"torn").unwrap();
        let s = ScratchJournal::open(&d, [1; 32]).unwrap();
        assert_eq!(s.head(), Some(g));
        drop(s);
        fs::write(d.join("HEAD"), b"torn").unwrap();
        assert!(ScratchJournal::open(&d, [1; 32]).is_err());
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn head_switch_rechecks_ancestors_after_prior_success_without_cross_call_cache() {
        let d = dir("fresh-integrity");
        let mut s = ScratchJournal::open(&d, [1; 32]).unwrap();
        let g = s.stage(&checkpoint(None, 0, 0)).unwrap();
        let a = s.stage(&checkpoint(Some(g), 1, 1)).unwrap();
        let b = s.stage(&checkpoint(Some(g), 1, 2)).unwrap();
        s.publish(None, a).unwrap();
        assert_eq!(s.reorg_path(b).unwrap(), (vec![a], vec![b]));
        let previous = fs::read(d.join("HEAD")).unwrap();
        fs::write(
            d.join("objects").join(hex(&g)),
            b"corrupted after verification",
        )
        .unwrap();
        assert!(s.publish(Some(a), b).is_err());
        assert_eq!(s.head(), Some(a));
        assert_eq!(fs::read(d.join("HEAD")).unwrap(), previous);
        assert!(s.reorg_path(b).is_err());
        drop(s);
        fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn branch_selection_rejects_unrelated_genesis_without_replacing_head() {
        let d = dir("unrelated-genesis");
        let mut s = ScratchJournal::open(&d, [1; 32]).unwrap();
        let g = s.stage(&checkpoint(None, 0, 0)).unwrap();
        let other = s.stage(&checkpoint(None, 0, 1)).unwrap();
        s.publish(None, g).unwrap();
        assert!(s.publish(Some(g), other).is_err());
        assert_eq!(s.head(), Some(g));
        s.publish(Some(g), g).unwrap();
        drop(s);
        let s = ScratchJournal::open(&d, [1; 32]).unwrap();
        assert_eq!(s.head(), Some(g));
        drop(s);
        fs::remove_dir_all(d).unwrap();
    }
}
