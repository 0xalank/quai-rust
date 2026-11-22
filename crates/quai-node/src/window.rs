//! Bounded ancestry window for the replay. It keeps the recent records and
//! append snapshots of each context and serves them to the consensus
//! `History` and `CandidateIndex` through their reader traits. A lookup that
//! falls outside the window is reported by the consensus code as unavailable;
//! the replay then stops with that error instead of guessing.
use quai_consensus::{
    history::{self, Record},
    placement::{self, Snapshot},
    Error,
};
use quai_primitives::Hash;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};

/// Records kept per context, newest last.
pub const KEEP: [usize; 3] = [256, 256, 1024];
/// Zone records are also limited by encoded size, but never below this count.
pub const MIN_ZONE: usize = 64;
pub const MAX_ZONE_BYTES: usize = 512 << 20;
/// Zone views of the latest prime blocks stay available regardless of age:
/// expansion and base-fee rules read the prime terminus and its prime parent.
pub const PRIME_VIEWS: usize = 8;

#[derive(Default)]
struct Inner {
    records: [HashMap<Hash, Arc<Record>>; 3],
    snapshots: [HashMap<Hash, Arc<Snapshot>>; 3],
    order: [VecDeque<Hash>; 3],
    zone_bytes: usize,
    prime_views: VecDeque<(Hash, Arc<Record>)>,
    genesis: Option<(Arc<Record>, [Arc<Snapshot>; 3])>,
}
#[derive(Default)]
pub struct Window {
    inner: RwLock<Inner>,
}
impl Window {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    // A writer that panicked leaves whole entries: the window stays usable.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }
    /// The trusted genesis stays in every context for the whole replay.
    pub fn set_genesis(&self, genesis: Arc<Record>, snapshots: [Arc<Snapshot>; 3]) {
        let mut w = self.write();
        w.genesis = Some((genesis, snapshots));
    }
    /// Add a checked record and its append snapshot in one context.
    pub fn insert(&self, context: usize, record: Arc<Record>, snapshot: Arc<Snapshot>) {
        let mut w = self.write();
        let id = record.id();
        if context == 2 {
            w.zone_bytes += record.bytes().len();
        }
        w.records[context].insert(id, record);
        w.snapshots[context].insert(id, snapshot);
        w.order[context].push_back(id);
        loop {
            let len = w.order[context].len();
            let over = len > KEEP[context]
                || (context == 2 && w.zone_bytes > MAX_ZONE_BYTES && len > MIN_ZONE);
            if !over {
                break;
            }
            let Some(old) = w.order[context].pop_front() else {
                break;
            };
            if let Some(r) = w.records[context].remove(&old) {
                if context == 2 {
                    w.zone_bytes -= r.bytes().len();
                }
            }
            w.snapshots[context].remove(&old);
        }
    }
    /// Keep the zone view of a prime-coincident block beyond the zone window.
    pub fn pin_prime_view(&self, record: Arc<Record>) {
        let mut w = self.write();
        w.prime_views.push_back((record.id(), record));
        while w.prime_views.len() > PRIME_VIEWS {
            w.prime_views.pop_front();
        }
    }
    fn record(&self, context: usize, id: &Hash) -> Option<Arc<Record>> {
        let w = self.read();
        if let Some(r) = w.records[context].get(id) {
            return Some(r.clone());
        }
        if let Some((g, _)) = &w.genesis {
            if g.id() == *id {
                return Some(g.clone());
            }
        }
        if context == 2 {
            return w
                .prime_views
                .iter()
                .find(|(h, _)| h == id)
                .map(|(_, r)| r.clone());
        }
        None
    }
    fn snapshot(&self, context: usize, id: &Hash) -> Option<Arc<Snapshot>> {
        let w = self.read();
        if let Some(s) = w.snapshots[context].get(id) {
            return Some(s.clone());
        }
        match &w.genesis {
            Some((g, s)) if g.id() == *id => Some(s[context].clone()),
            _ => None,
        }
    }
    pub fn len(&self, context: usize) -> usize {
        self.read().order[context].len()
    }
    /// Records and snapshots per context, oldest first, and the pinned prime
    /// views, for a checkpoint. The genesis is not included.
    #[allow(clippy::type_complexity)]
    pub fn export(&self) -> ([Vec<(Arc<Record>, Arc<Snapshot>)>; 3], Vec<Arc<Record>>) {
        let w = self.read();
        let windows = std::array::from_fn(|c| {
            w.order[c]
                .iter()
                .filter_map(|id| {
                    Some((
                        w.records[c].get(id)?.clone(),
                        w.snapshots[c].get(id)?.clone(),
                    ))
                })
                .collect()
        });
        let views = w.prime_views.iter().map(|(_, r)| r.clone()).collect();
        (windows, views)
    }
    pub fn history_reader(self: &Arc<Self>, context: usize) -> Arc<dyn history::Reader> {
        Arc::new(View {
            window: self.clone(),
            context,
        })
    }
    pub fn index_reader(self: &Arc<Self>, context: usize) -> Arc<dyn placement::Reader> {
        Arc::new(View {
            window: self.clone(),
            context,
        })
    }
}
struct View {
    window: Arc<Window>,
    context: usize,
}
impl history::Reader for View {
    fn read(&self, id: &Hash) -> Result<Option<Arc<Record>>, Error> {
        Ok(self.window.record(self.context, id))
    }
}
impl placement::Reader for View {
    fn read(&self, id: &Hash) -> Result<Option<Arc<Snapshot>>, Error> {
        Ok(self.window.snapshot(self.context, id))
    }
}
