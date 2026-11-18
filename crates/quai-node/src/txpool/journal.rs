//! go-quai's journal of local transactions (`core/tx_journal.go`): local
//! transactions survive a restart. At start the pool loads the journal
//! (`AddLocals` in batches of 1,024) and rewrites it with what it kept;
//! every local addition is appended; every hour it is rewritten.
//!
//! go-quai writes an RLP stream (`transactions.rlp`); this journal holds
//! the same transactions as length-prefixed canonical protobufs (4-byte
//! big-endian length, then `ProtoEncode`), in the node's data directory.
use super::list::Tx;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// go-quai's `Rejournal` default.
pub const REJOURNAL: std::time::Duration = std::time::Duration::from_secs(3600);
/// `loadBatch`.
pub const LOAD_BATCH: usize = 1024;

pub struct Journal {
    path: PathBuf,
    writer: Mutex<Option<File>>,
}

impl Journal {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            writer: Mutex::new(None),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// The transactions of the journal, in order. A truncated record ends
    /// the read, as a broken RLP stream does in go-quai.
    pub fn read(&self) -> Vec<Vec<u8>> {
        let mut data = vec![];
        if File::open(&self.path)
            .and_then(|mut f| f.read_to_end(&mut data))
            .is_err()
        {
            return vec![];
        }
        let mut out = vec![];
        let mut p = 0;
        while p + 4 <= data.len() {
            let n = u32::from_be_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as usize;
            p += 4;
            let Some(record) = data.get(p..p + n) else {
                break;
            };
            out.push(record.to_vec());
            p += n;
        }
        out
    }
    fn record(tx: &Tx, out: &mut Vec<u8>) {
        out.extend_from_slice(&(tx.proto.len() as u32).to_be_bytes());
        out.extend_from_slice(&tx.proto);
    }
    /// Append local transactions (`insert`); nothing before the first
    /// rotation opened the journal.
    pub fn insert(&self, txs: &[Tx]) -> std::io::Result<()> {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let Some(f) = w.as_mut() else {
            return Ok(());
        };
        let mut buf = vec![];
        for tx in txs {
            Self::record(tx, &mut buf);
        }
        f.write_all(&buf)
    }
    /// Rewrite the journal with `txs` and keep it open for appending.
    pub fn rotate(&self, txs: &[Tx]) -> std::io::Result<()> {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        *w = None;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("new");
        let mut buf = vec![];
        for tx in txs {
            Self::record(tx, &mut buf);
        }
        let mut f = File::create(&tmp)?;
        f.write_all(&buf)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &self.path)?;
        *w = Some(OpenOptions::new().append(true).open(&self.path)?);
        Ok(())
    }
}
