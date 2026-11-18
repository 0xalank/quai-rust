//! Checked ETX/economic/lockup point reads from one pinned state transaction.
use super::*;
use crate::{etx::Entry, state_error::ReadError};
use quai_primitives::{mpt::ReadNodes, transaction::Kind};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicUsize, Ordering},
};
fn unavailable(v: impl std::fmt::Display) -> ReadError {
    ReadError::Unavailable(v.to_string())
}
struct QueueCache {
    rows: BTreeMap<u64, Option<Arc<Entry>>>,
    order: VecDeque<u64>,
    bytes: usize,
}
struct LockupCache {
    rows: BTreeMap<Key, Option<Lockup>>,
    order: VecDeque<Key>,
}
pub(super) struct Reader {
    snapshot: Arc<Mutex<Snapshot>>,
    identity: Hash,
    queue: Mutex<QueueCache>,
    lockups: Mutex<LockupCache>,
    queue_decoded: AtomicUsize,
    lockups_decoded: AtomicUsize,
}
impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuxiliaryStateReader")
            .field("identity", &self.identity)
            .finish()
    }
}
impl Reader {
    pub fn new(snapshot: Arc<Mutex<Snapshot>>, digest: Hash) -> Arc<Self> {
        let mut h = Sha256::new();
        h.update(b"QUAI-PINNED-AUX-2\n");
        h.update(digest);
        Arc::new(Self {
            snapshot,
            identity: h.finalize().into(),
            queue: Mutex::new(QueueCache {
                rows: BTreeMap::new(),
                order: VecDeque::new(),
                bytes: 0,
            }),
            lockups: Mutex::new(LockupCache {
                rows: BTreeMap::new(),
                order: VecDeque::new(),
            }),
            queue_decoded: AtomicUsize::new(0),
            lockups_decoded: AtomicUsize::new(0),
        })
    }
}
impl ReadNodes for Reader {
    fn get_node(&self, hash: &Hash) -> Result<Option<Vec<u8>>, mpt::Error> {
        self.snapshot
            .lock()
            .map_err(|e| mpt::Error::Backend(e.to_string()))?
            .node(hash)
            .map_err(|e| mpt::Error::Backend(e.to_string()))
    }
}
pub(super) fn lockup(key: Key, raw: &[u8]) -> io::Result<Lockup> {
    need(
        raw.len() >= 47 && raw[..47] == key.raw(),
        "lockup record key/width",
    )?;
    let value = Lockup::decode(&raw[47..]).map_err(bad)?;
    need(
        value.encode() == raw[47..],
        "noncanonical local lockup record",
    )?;
    Ok(value)
}
pub(super) fn key(hashed: Hash, raw: &[u8]) -> io::Result<Key> {
    let p = preimage(hashed, raw, 47)?;
    need(&p[..2] == b"cl", "lockup key format")?;
    Ok(Key {
        owner: p[2..22].try_into().unwrap(),
        beneficiary: p[22..42].try_into().unwrap(),
        lock: p[42],
        epoch: u32::from_be_bytes(p[43..47].try_into().unwrap()),
    })
}
impl crate::lockup_state::Reader for Reader {
    fn identity(&self) -> Hash {
        self.identity
    }
    fn loaded(&self) -> usize {
        self.lockups_decoded.load(Ordering::Relaxed)
    }
    fn get(&self, key: &Key) -> Result<Option<Lockup>, ReadError> {
        if let Some(value) = self.lockups.lock().map_err(unavailable)?.rows.get(key) {
            return Ok(value.clone());
        }
        let raw = self
            .snapshot
            .lock()
            .map_err(unavailable)?
            .get(b"lockups", &key.raw())
            .map_err(unavailable)?;
        let value = raw
            .map(|v| lockup(*key, &v).map_err(unavailable))
            .transpose()?;
        self.lockups_decoded
            .fetch_add(usize::from(value.is_some()), Ordering::Relaxed);
        let mut cache = self.lockups.lock().map_err(unavailable)?;
        if !cache.rows.contains_key(key) {
            while cache.rows.len() >= 4096 {
                if let Some(old) = cache.order.pop_front() {
                    cache.rows.remove(&old);
                }
            }
            cache.order.push_back(*key);
            cache.rows.insert(*key, value.clone());
        }
        Ok(value)
    }
    fn scan(
        &self,
        max_entries: usize,
        max_bytes: usize,
    ) -> Result<BTreeMap<Key, Lockup>, ReadError> {
        let snapshot = self.snapshot.lock().map_err(unavailable)?;
        let mut rows = BTreeMap::new();
        snapshot
            .scan(b"lockups", max_entries, max_bytes, &mut |hash, raw| {
                let key = key(hash, raw)?;
                let value = lockup(key, raw)?;
                need(rows.insert(key, value).is_none(), "duplicate lockup key")?;
                Ok(())
            })
            .map_err(unavailable)?;
        self.lockups_decoded
            .fetch_add(rows.len(), Ordering::Relaxed);
        Ok(rows)
    }
}
impl crate::etx::Reader for Reader {
    fn identity(&self) -> Hash {
        self.identity
    }
    fn loaded(&self) -> usize {
        self.queue_decoded.load(Ordering::Relaxed)
    }
    fn entry(&self, index: u64) -> Result<Option<Arc<Entry>>, ReadError> {
        if let Some(value) = self.queue.lock().map_err(unavailable)?.rows.get(&index) {
            return Ok(value.clone());
        }
        let snapshot = self.snapshot.lock().map_err(unavailable)?;
        let raw = snapshot
            .get(b"etx-data", &index.to_be_bytes())
            .map_err(unavailable)?;
        let value = if let Some(raw) = raw {
            if raw.len() < 8 || raw[..8] != index.to_be_bytes() {
                return Err(unavailable("ETX record index/width"));
            }
            let transaction = Transaction::decode(&raw[8..]).map_err(unavailable)?;
            if !matches!(transaction.kind(), Kind::External(_)) || transaction.encode() != raw[8..]
            {
                return Err(unavailable("ETX record type/encoding"));
            }
            let envelope = transaction.rlp_envelope();
            if snapshot
                .get(b"etx", &crate::etx::magnitude(index))
                .map_err(unavailable)?
                != Some(envelope.clone())
            {
                return Err(unavailable("ETX consensus envelope binding"));
            }
            let weight = (raw.len() - 8)
                .checked_mul(2)
                .and_then(|n| n.checked_add(envelope.len()))
                .ok_or_else(|| unavailable("queue record buffer width"))?;
            Some(Arc::new(Entry {
                transaction: Arc::new(transaction),
                envelope,
                bytes: weight,
            }))
        } else {
            if snapshot
                .get(b"etx", &crate::etx::magnitude(index))
                .map_err(unavailable)?
                .is_some()
            {
                return Err(unavailable("missing ETX record"));
            }
            None
        };
        drop(snapshot);
        self.queue_decoded
            .fetch_add(usize::from(value.is_some()), Ordering::Relaxed);
        let bytes = value.as_ref().map_or(0, |v| v.bytes);
        let mut cache = self.queue.lock().map_err(unavailable)?;
        if bytes <= 16 << 20 && !cache.rows.contains_key(&index) {
            while cache.rows.len() >= 1024 || cache.bytes + bytes > 16 << 20 {
                let old = cache
                    .order
                    .pop_front()
                    .ok_or_else(|| unavailable("queue cache accounting"))?;
                if let Some(Some(old)) = cache.rows.remove(&old) {
                    cache.bytes = cache
                        .bytes
                        .checked_sub(old.bytes)
                        .ok_or_else(|| unavailable("queue cache bytes"))?;
                }
            }
            cache.bytes += bytes;
            cache.order.push_back(index);
            cache.rows.insert(index, value.clone());
        }
        Ok(value)
    }
    fn auxiliary(&self, key: &[u8]) -> Result<Option<Vec<u8>>, ReadError> {
        let snapshot = self.snapshot.lock().map_err(unavailable)?;
        let raw = snapshot.get(b"etx-aux", key).map_err(unavailable)?;
        if let Some(raw) = raw {
            if raw.len() <= 32 || raw.len() > 32 + 65536 || &raw[..32] != key {
                return Err(unavailable("ETX auxiliary record width/key"));
            }
            let value = raw[32..].to_vec();
            if snapshot.get(b"etx", key).map_err(unavailable)? != Some(value.clone()) {
                return Err(unavailable("ETX auxiliary consensus binding"));
            }
            Ok(Some(value))
        } else {
            if snapshot.get(b"etx", key).map_err(unavailable)?.is_some() {
                return Err(unavailable("missing ETX auxiliary record"));
            }
            Ok(None)
        }
    }
    fn auxiliary_image(
        &self,
        max_entries: usize,
        max_bytes: usize,
    ) -> Result<BTreeMap<Vec<u8>, Vec<u8>>, ReadError> {
        let snapshot = self.snapshot.lock().map_err(unavailable)?;
        let mut rows = BTreeMap::new();
        snapshot
            .scan(b"etx-aux", max_entries, max_bytes, &mut |hash, raw| {
                let key = preimage(hash, raw, 32)?;
                need(
                    raw.len() > 32
                        && raw.len() <= 32 + 65536
                        && key != crate::etx::index_key(true)
                        && key != crate::etx::index_key(false),
                    "ETX auxiliary record width/key",
                )?;
                need(
                    snapshot.get(b"etx", key)?.as_deref() == Some(&raw[32..]),
                    "ETX auxiliary consensus binding",
                )?;
                need(
                    rows.insert(key.to_vec(), raw[32..].to_vec()).is_none(),
                    "duplicate auxiliary key",
                )?;
                Ok(())
            })
            .map_err(unavailable)?;
        Ok(rows)
    }
}
