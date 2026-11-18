//! A canonical generation pins one catalog transaction. Local receipt hashes
//! bind its native frontier and execution version; peers never supply these rows.
use super::*;
use crate::history::Cache;
use std::sync::atomic::AtomicUsize;
pub(super) struct Loaded {
    pub entry: Arc<Entry>,
    pub bundle: BlockBundle,
    bytes: usize,
}
pub(super) struct Source {
    snapshot: Mutex<CatalogSnapshot>,
    policy: Hash,
    cache: Cache<Loaded>,
    loaded: AtomicUsize,
}
impl Source {
    pub fn new(snapshot: CatalogSnapshot, policy: Hash) -> Arc<Self> {
        Arc::new(Self {
            snapshot: Mutex::new(snapshot),
            policy,
            cache: Cache::new(128, 16 << 20),
            loaded: AtomicUsize::new(0),
        })
    }
    pub fn head(&self) -> io::Result<CatalogHead> {
        self.snapshot
            .lock()
            .map_err(bad)?
            .head()
            .ok_or_else(|| bad("empty canonical admission catalog"))
    }
    pub fn loads(&self) -> usize {
        self.loaded.load(Ordering::Relaxed)
    }
    pub fn by_block(&self, block: Hash) -> io::Result<Option<Arc<Loaded>>> {
        let id = self.snapshot.lock().map_err(bad)?.id_for_block(block)?;
        id.map(|id| self.load(id)).transpose()
    }
    pub fn load(&self, id: Hash) -> io::Result<Arc<Loaded>> {
        self.cache
            .get(
                &id,
                || {
                    let snapshot = self
                        .snapshot
                        .lock()
                        .map_err(|_| crate::Error::Unavailable("canonical catalog lock"))?;
                    let receipt = snapshot
                        .get(id)
                        .map_err(|_| crate::Error::Unavailable("canonical receipt"))?;
                    let bytes = receipt.payload.len();
                    let stored = decode(&receipt.payload, self.policy)
                        .map_err(|_| crate::Error::Invalid("canonical receipt payload"))?;
                    let record = stored
                        .bundle
                        .checked()
                        .map_err(|_| crate::Error::Invalid("canonical receipt block"))?;
                    let parent = receipt
                        .parent
                        .map(|p| snapshot.header(p))
                        .transpose()
                        .map_err(|_| crate::Error::Unavailable("canonical receipt parent"))?;
                    let valid = stored.seq == receipt.sequence
                        && stored.checkpoint == receipt.checkpoint
                        && record.id() == receipt.block
                        && record.number(2)? == receipt.height
                        && stored.parent == parent.as_ref().map(|p| p.checkpoint)
                        && parent.as_ref().is_none_or(|p| {
                            p.height.checked_add(1) == Some(receipt.height)
                                && p.sequence < receipt.sequence
                        })
                        && stored.frontier.tips()[&vec![0, 0]].id == receipt.block
                        && stored.frontier.tips()[&vec![0, 0]].numbers == record.numbers;
                    if !valid {
                        return Err(crate::Error::Invalid("canonical receipt/version binding"));
                    }
                    self.loaded.fetch_add(1, Ordering::Relaxed);
                    Ok(Some(Arc::new(Loaded {
                        entry: Arc::new(Entry {
                            receipt: id,
                            parent: receipt.parent,
                            seq: stored.seq,
                            checkpoint: receipt.checkpoint,
                            block: receipt.block,
                            height: receipt.height,
                            roots: stored.roots,
                            frontier: stored.frontier,
                        }),
                        bundle: stored.bundle,
                        bytes,
                    })))
                },
                |v| v.bytes,
            )
            .map_err(bad)?
            .ok_or_else(|| bad("missing canonical receipt"))
    }
    pub fn ancestor(&self, id: Hash, height: u64) -> io::Result<Option<Arc<Entry>>> {
        let receipt = self.snapshot.lock().map_err(bad)?.ancestor(id, height)?;
        receipt
            .map(|r| self.load(r.id).map(|v| v.entry.clone()))
            .transpose()
    }
}
