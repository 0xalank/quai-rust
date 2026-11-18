//! Storage selection is private to native branch admission. The keyed format
//! never interprets a peer image and never migrates an old journal implicitly.
use super::{bad, require, PrivateChain};
use quai_store::{
    journal::{Checkpoint, Id, ScratchJournal},
    state_db::{Snapshot, StateDatabase},
};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
const MARKER: &[u8] = b"QUAI-PRIVATE-KEYED-3\n";
pub(super) enum Store {
    Images(ScratchJournal),
    Keyed(Keyed),
}
pub(super) struct Keyed {
    pub database: StateDatabase,
    path: PathBuf,
    identity: (u64, u64),
    head: Option<Id>,
}
fn identity(path: &Path) -> io::Result<(u64, u64)> {
    let m = fs::symlink_metadata(path)?;
    require(
        m.is_file() && !m.file_type().is_symlink(),
        "execution database file type",
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
            "keyed private writer file identity requires Unix",
        ))
    }
}
impl Keyed {
    fn open(dir: &Path, profile: &PrivateChain) -> io::Result<Self> {
        if !dir.exists() {
            fs::create_dir_all(dir)?;
        }
        require(
            fs::symlink_metadata(dir)?.is_dir()
                && !fs::symlink_metadata(dir)?.file_type().is_symlink(),
            "execution database directory",
        )?;
        let marker = dir.join("STATE-PROFILE");
        let path = dir.join("state.redb");
        let expected = [MARKER, profile.fingerprint.as_slice()].concat();
        if marker.exists() {
            require(
                fs::read(&marker)? == expected && path.exists(),
                "execution database profile or missing data; no automatic migration",
            )?;
        } else {
            require(
                fs::read_dir(dir)?.next().is_none(),
                "refusing execution database migration",
            )?;
            use std::io::Write;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)?;
            f.write_all(&expected)?;
            f.sync_all()?;
            fs::File::open(dir)?.sync_all()?;
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            require(
                matches!(
                    entry.file_name().to_str(),
                    Some("STATE-PROFILE" | "state.redb")
                ),
                "unexpected execution database entry",
            )?;
        }
        let database = StateDatabase::open(&path, profile.fingerprint)?;
        let file_identity = identity(&path)?;
        let head = database.head()?;
        Ok(Self {
            database,
            path,
            identity: file_identity,
            head,
        })
    }
    pub fn check_file(&self) -> io::Result<()> {
        require(
            identity(&self.path)? == self.identity,
            "execution database file replaced",
        )
    }
    pub fn snapshot(&self, id: Id) -> io::Result<Snapshot> {
        self.check_file()?;
        self.database.snapshot(Some(id))
    }
}
impl Store {
    pub fn images(dir: &Path, profile: &PrivateChain) -> io::Result<Self> {
        Ok(Self::Images(ScratchJournal::open(
            dir,
            profile.fingerprint,
        )?))
    }
    pub fn keyed(dir: &Path, profile: &PrivateChain) -> io::Result<Self> {
        Ok(Self::Keyed(Keyed::open(dir, profile)?))
    }
    pub fn head(&self) -> Option<Id> {
        match self {
            Self::Images(j) => j.head(),
            Self::Keyed(k) => k.head,
        }
    }
    pub fn durable_head(&self) -> io::Result<Option<Id>> {
        match self {
            Self::Images(j) => Ok(j.head()),
            Self::Keyed(k) => {
                k.check_file()?;
                k.database.head()
            }
        }
    }
    pub fn backend(&self) -> &'static str {
        match self {
            Self::Images(_) => "complete-images",
            Self::Keyed(_) => "incremental-trie-database",
        }
    }
    pub fn load(&self, id: Id) -> io::Result<Checkpoint> {
        match self {
            Self::Images(j) => j.load(id),
            Self::Keyed(k) => {
                let snapshot = k.snapshot(id)?;
                let version = snapshot.version();
                require(
                    k.database.version_for_block(version.block)? == Some(id),
                    "execution database block index",
                )?;
                let payload = snapshot.metadata()?;
                require(
                    payload.starts_with(super::KEYED_MAGIC),
                    "execution database payload format",
                )?;
                Ok(Checkpoint {
                    block: version.block,
                    parent: version.parent,
                    height: version.height,
                    payload,
                })
            }
        }
    }
    pub fn publish(&mut self, expected: Option<Id>, next: Id) -> io::Result<()> {
        match self {
            Self::Images(j) => j.publish(expected, next),
            Self::Keyed(k) => {
                k.check_file()?;
                require(k.head == expected, "stale keyed execution head")?;
                k.database.select_optional(expected, next)?;
                k.head = Some(next);
                Ok(())
            }
        }
    }
    pub fn reorg_path(&self, next: Id) -> io::Result<(Vec<Id>, Vec<Id>)> {
        match self {
            Self::Images(j) => j.reorg_path(next),
            Self::Keyed(k) => {
                let head = k
                    .head
                    .ok_or_else(|| bad("missing execution database head"))?;
                let mut a = k.snapshot(head)?.version().clone();
                let mut b = k.snapshot(next)?.version().clone();
                let mut detached = vec![];
                let mut attached = vec![];
                while a.height > b.height {
                    detached.push(a.id);
                    a = k
                        .snapshot(a.parent.ok_or_else(|| bad("missing detached parent"))?)?
                        .version()
                        .clone();
                }
                while b.height > a.height {
                    attached.push(b.id);
                    b = k
                        .snapshot(b.parent.ok_or_else(|| bad("missing attached parent"))?)?
                        .version()
                        .clone();
                }
                while a.id != b.id {
                    detached.push(a.id);
                    attached.push(b.id);
                    a = k
                        .snapshot(a.parent.ok_or_else(|| bad("missing reorg ancestor"))?)?
                        .version()
                        .clone();
                    b = k
                        .snapshot(b.parent.ok_or_else(|| bad("missing reorg ancestor"))?)?
                        .version()
                        .clone();
                }
                attached.reverse();
                Ok((detached, attached))
            }
        }
    }
}
