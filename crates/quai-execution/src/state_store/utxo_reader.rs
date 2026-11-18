//! Pinned raw membership reader; MuHash/count metadata is kept independently.
use super::*;
use crate::qi::ReadError;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicUsize, Ordering},
};
const CACHE_ENTRIES: usize = 4096;
struct Cache {
    rows: BTreeMap<OutPoint, Option<Utxo>>,
    order: VecDeque<OutPoint>,
}
pub(super) struct Reader {
    snapshot: Arc<Mutex<Snapshot>>,
    identity: Hash,
    base_count: u64,
    cache: Mutex<Cache>,
    decoded: AtomicUsize,
}
impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UtxoReader")
            .field("identity", &self.identity)
            .field("base_count", &self.base_count)
            .finish()
    }
}
fn unavailable(v: impl std::fmt::Display) -> ReadError {
    ReadError::Unavailable(v.to_string())
}
fn decode(point: OutPoint, raw: &[u8]) -> Result<Utxo, ReadError> {
    if raw.len() < 56 || raw[..34] != point_key(&point) {
        return Err(unavailable("UTXO record key/width"));
    }
    let denomination = raw[34];
    let address = raw[35..55].try_into().unwrap();
    let len = raw[55] as usize;
    if denomination as usize >= crate::qi::DENOMINATIONS.len() || len > 32 || raw.len() != 56 + len
    {
        return Err(unavailable("UTXO record denomination/lock"));
    }
    Ok(Utxo {
        denomination,
        address,
        lock: raw[56..].to_vec(),
    })
}
impl Reader {
    pub fn new(
        snapshot: Arc<Mutex<Snapshot>>,
        view: &Snapshot,
        base_count: u64,
        digest: Hash,
    ) -> Arc<Self> {
        let mut h = Sha256::new();
        h.update(b"QUAI-PINNED-UTXO-1\n");
        h.update(digest);
        h.update(view.root(b"utxos"));
        Arc::new(Self {
            snapshot,
            identity: h.finalize().into(),
            base_count,
            cache: Mutex::new(Cache {
                rows: BTreeMap::new(),
                order: VecDeque::new(),
            }),
            decoded: AtomicUsize::new(0),
        })
    }
}
impl crate::utxo::Reader for Reader {
    fn identity(&self) -> Hash {
        self.identity
    }
    fn loaded(&self) -> usize {
        self.decoded.load(Ordering::Relaxed)
    }
    fn get(&self, point: &OutPoint) -> Result<Option<Utxo>, ReadError> {
        {
            let cache = self.cache.lock().map_err(unavailable)?;
            if let Some(v) = cache.rows.get(point) {
                return Ok(v.clone());
            }
        }
        let value = self
            .snapshot
            .lock()
            .map_err(unavailable)?
            .get(b"utxos", &point_key(point))
            .map_err(unavailable)?
            .map(|raw| decode(*point, &raw))
            .transpose()?;
        self.decoded
            .fetch_add(usize::from(value.is_some()), Ordering::Relaxed);
        let mut cache = self.cache.lock().map_err(unavailable)?;
        if !cache.rows.contains_key(point) {
            while cache.rows.len() >= CACHE_ENTRIES {
                let key = cache
                    .order
                    .pop_front()
                    .ok_or_else(|| unavailable("UTXO cache accounting"))?;
                cache.rows.remove(&key);
            }
            cache.order.push_back(*point);
            cache.rows.insert(*point, value.clone());
        }
        Ok(value)
    }
    fn scan(
        &self,
        max_entries: usize,
        max_bytes: usize,
    ) -> Result<BTreeMap<OutPoint, Utxo>, ReadError> {
        if self.base_count > max_entries as u64 {
            return Err(unavailable("UTXO export entry bound"));
        }
        let snapshot = self.snapshot.lock().map_err(unavailable)?;
        let mut out = BTreeMap::new();
        snapshot
            .scan(b"utxos", max_entries, max_bytes, &mut |key, value| {
                let raw = preimage(key, value, 34)?;
                let point = OutPoint {
                    hash: raw[..32].try_into().unwrap(),
                    index: u16::from_be_bytes(raw[32..].try_into().unwrap()),
                };
                let value = decode(point, value).map_err(bad)?;
                need(
                    out.insert(point, value).is_none(),
                    "duplicate UTXO export key",
                )
            })
            .map_err(unavailable)?;
        if out.len() as u64 != self.base_count {
            return Err(unavailable("UTXO export membership count"));
        }
        Ok(out)
    }
}
