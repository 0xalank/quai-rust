//! On-demand ancestry/confirmation reads from one immutable local version.
//! Every lookup proves that its record belongs to this version's ancestry.
use super::{bad, decode, require, PrivateChain, KEYED_MAGIC};
use crate::{
    confirmation::{self, delta::Delta},
    history::{self, Record},
    placement, Error,
};
use quai_execution::{qi::OutPoint, state_image, state_store};
use quai_primitives::{
    transaction::{Kind, Transaction},
    Hash,
};
use quai_store::{journal::Checkpoint, state_db::Snapshot};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex, Weak,
};
fn unavailable(_: impl std::fmt::Display) -> Error {
    Error::Unavailable("persisted branch ancestry")
}
pub(super) struct Loaded {
    pub reference: state_image::Reference,
    pub records: [Option<Arc<Record>>; 3],
    pub delta: Option<Delta>,
    pub receipts: Vec<Vec<u8>>,
    pub created: Vec<(OutPoint, u8)>,
    pub deleted: Vec<Hash>,
    bytes: usize,
}
/// Only locally decoded immutable metadata is retained. The cache owns no
/// database transaction, provider, execution state, peer hints or authority.
pub(super) struct DecodeMemo {
    cache: history::Cache<Loaded>,
    decodes: AtomicUsize,
    reuses: AtomicUsize,
}
impl DecodeMemo {
    pub fn new() -> Self {
        Self {
            cache: history::Cache::new(32, 16 << 20),
            decodes: AtomicUsize::new(0),
            reuses: AtomicUsize::new(0),
        }
    }
    fn get(
        &self,
        profile: Hash,
        version: Hash,
        load: impl FnOnce() -> Result<Arc<Loaded>, Error>,
    ) -> Result<Arc<Loaded>, Error> {
        use sha2::{Digest, Sha256};
        let key: Hash = Sha256::new()
            .chain_update(profile)
            .chain_update(version)
            .finalize()
            .into();
        let mut decoded = false;
        let value = self
            .cache
            .get(
                &key,
                || {
                    let value = load()?;
                    decoded = true;
                    self.decodes.fetch_add(1, Ordering::Relaxed);
                    Ok(Some(value))
                },
                |v| v.bytes,
            )?
            .ok_or(Error::Unavailable("decoded history memo"))?;
        if !decoded {
            self.reuses.fetch_add(1, Ordering::Relaxed);
        }
        Ok(value)
    }
    #[cfg(test)]
    pub(super) fn get_for_test(
        &self,
        profile: Hash,
        version: Hash,
        value: &Loaded,
        bytes: usize,
    ) -> Result<Arc<Loaded>, Error> {
        self.get(profile, version, || {
            Ok(Arc::new(Loaded {
                reference: value.reference.clone(),
                records: value.records.clone(),
                delta: value.delta.clone(),
                receipts: value.receipts.clone(),
                created: value.created.clone(),
                deleted: value.deleted.clone(),
                bytes,
            }))
        })
    }
    pub fn stats(&self) -> (usize, usize, usize, usize) {
        (
            self.decodes.load(Ordering::Relaxed),
            self.reuses.load(Ordering::Relaxed),
            self.cache.len(),
            self.cache.logical_bytes(),
        )
    }
}
pub(super) struct Reader {
    snapshot: Arc<Mutex<Snapshot>>,
    profile: Arc<PrivateChain>,
    block: Hash,
    height: u64,
    cache: Mutex<std::collections::VecDeque<(Hash, Weak<Loaded>)>>,
    memo: Arc<DecodeMemo>,
    loaded: AtomicUsize,
}
impl Reader {
    pub fn new(
        snapshot: Snapshot,
        profile: &PrivateChain,
        memo: Arc<DecodeMemo>,
    ) -> std::io::Result<Arc<Self>> {
        let own = PrivateChain::hierarchy_for_fingerprint(profile.fingerprint).map_err(bad)?;
        require(
            own.fingerprint == profile.fingerprint,
            "indexed hierarchy profile",
        )?;
        let block = snapshot.version().block;
        let height = snapshot.version().height;
        Ok(Arc::new(Self {
            snapshot: Arc::new(Mutex::new(snapshot)),
            profile: own,
            block,
            height,
            cache: Mutex::new(std::collections::VecDeque::new()),
            memo,
            loaded: AtomicUsize::new(0),
        }))
    }
    pub fn loaded_records(&self) -> usize {
        self.loaded.load(Ordering::Relaxed)
    }
    pub fn lookup(&self, id: &Hash) -> Result<Option<Arc<Loaded>>, Error> {
        if let Some(value) = self
            .cache
            .lock()
            .map_err(unavailable)?
            .iter()
            .find_map(|(key, value)| if key == id { value.upgrade() } else { None })
        {
            return Ok(Some(value));
        }
        let (version, payload) = {
            let snapshot = self.snapshot.lock().map_err(unavailable)?;
            let Some(version) = snapshot.version_for_block(*id).map_err(unavailable)? else {
                return Ok(None);
            };
            if version.height > self.height
                || snapshot.ancestor(version.height).map_err(unavailable)?.id != version.id
            {
                return Ok(None);
            }
            let payload = snapshot.metadata_for(version.id).map_err(unavailable)?;
            (version, payload)
        };
        // Every new pinned reader checks current membership and payload bytes
        // before consulting the cross-reader decoded memo. A memo hit is not
        // an integrity, ancestry or execution-state result.
        let value = self.memo.get(self.profile.fingerprint, version.id, || {
            if !payload.starts_with(KEYED_MAGIC) {
                return Err(Error::Invalid("indexed branch metadata format"));
            }
            let bytes = payload.len();
            let cp = Checkpoint {
                block: version.block,
                parent: version.parent,
                height: version.height,
                payload,
            };
            let d = decode(&cp, &self.profile).map_err(unavailable)?;
            let delta = if version.height == 0 {
                if !d.hierarchy_delta.is_empty() {
                    return Err(Error::Invalid("genesis hierarchy delta"));
                }
                None
            } else {
                Some(Delta::decode(d.hierarchy_delta, &d.record)?)
            };
            let order = delta.as_ref().map_or(0, |d| d.order);
            let zone = Arc::new(d.record);
            let mut records: [Option<Arc<Record>>; 3] = std::array::from_fn(|_| None);
            records[2] = Some(zone.clone());
            if version.height == 0 {
                for slot in &mut records[..2] {
                    *slot = Some(zone.clone());
                }
            } else {
                if d.views.len() != 2usize.saturating_sub(order as usize) {
                    return Err(Error::Invalid("indexed coincident context count"));
                }
                for (c, bytes) in d.views {
                    let r = Arc::new(Record::decode(&bytes)?);
                    if c > 1
                        || c < order
                        || r.id() != zone.id()
                        || r.header().encode() != zone.header().encode()
                        || r.object().header().encode() != zone.object().header().encode()
                    {
                        return Err(Error::Invalid("indexed coincident context binding"));
                    }
                    records[c as usize] = Some(r);
                }
            }
            Ok(Arc::new(Loaded {
                reference: d.reference,
                records,
                delta,
                receipts: d.receipts,
                created: d.created,
                deleted: d.deleted,
                bytes,
            }))
        })?;
        self.loaded.fetch_add(1, Ordering::Relaxed);
        let mut cache = self.cache.lock().map_err(unavailable)?;
        if let Some(i) = cache.iter().position(|(key, _)| key == id) {
            cache.remove(i);
        }
        while cache.len() >= 32 {
            cache.pop_front();
        }
        cache.push_back((*id, Arc::downgrade(&value)));
        Ok(Some(value))
    }
    pub fn tip(&self) -> Result<Arc<Loaded>, Error> {
        self.lookup(&self.block)?
            .ok_or(Error::Unavailable("indexed branch tip"))
    }
    pub fn restore_state(&self, loaded: &Loaded) -> std::io::Result<state_image::Restored> {
        let (world, utxos) = state_store::restore_sparse(
            self.snapshot.clone(),
            &state_store::Commitment {
                roots: loaded.reference.roots.clone(),
                digest: loaded.reference.digest,
            },
        )?;
        Ok(state_image::Restored { world, utxos })
    }
    fn at_height(&self, height: u64) -> Result<Arc<Loaded>, Error> {
        let block = {
            let snapshot = self.snapshot.lock().map_err(unavailable)?;
            snapshot.ancestor(height).map_err(unavailable)?.block
        };
        self.lookup(&block)?
            .ok_or(Error::Unavailable("indexed historical block"))
    }
    pub fn incoming_at(
        &self,
        height: u64,
    ) -> Result<Vec<Transaction>, quai_execution::settlement::Error> {
        let d = self.at_height(height).map_err(|_| {
            quai_execution::settlement::Error::Invalid("missing private redemption ancestry")
        })?;
        Ok(d.records[2]
            .as_ref()
            .unwrap()
            .object()
            .transactions()
            .iter()
            .filter(|t| matches!(t.kind(), Kind::External(_)))
            .cloned()
            .collect())
    }
    pub fn created_at(
        &self,
        height: u64,
    ) -> Result<Vec<(OutPoint, u8)>, quai_execution::settlement::Error> {
        Ok(self
            .at_height(height)
            .map_err(|_| {
                quai_execution::settlement::Error::Invalid("missing private trim ancestry")
            })?
            .created
            .clone())
    }
}
impl history::Reader for Reader {
    fn read(&self, id: &Hash) -> Result<Option<Arc<Record>>, Error> {
        Ok(self.lookup(id)?.and_then(|v| v.records[2].clone()))
    }
}
impl confirmation::Reader for Reader {
    fn heads(&self) -> Result<[Hash; 3], Error> {
        Ok(self
            .tip()?
            .delta
            .as_ref()
            .map_or([self.profile.genesis[2].id(); 3], |d| d.heads))
    }
    fn record(&self, c: usize, id: &Hash) -> Result<Option<Arc<Record>>, Error> {
        if c > 2 {
            return Err(Error::Invalid("indexed context"));
        }
        Ok(self.lookup(id)?.and_then(|v| v.records[c].clone()))
    }
    fn snapshot(&self, c: usize, id: &Hash) -> Result<Option<Arc<placement::Snapshot>>, Error> {
        if c > 2 {
            return Err(Error::Invalid("indexed snapshot context"));
        }
        let Some(value) = self.lookup(id)? else {
            return Ok(None);
        };
        if let Some(d) = &value.delta {
            return Ok(d.snapshots[c].clone().map(Arc::new));
        }
        let p = placement::Profile {
            location: self.profile.location[..c].to_vec(),
            genesis: std::collections::BTreeSet::from([self.profile.genesis[2].id()]),
            default_genesis: None,
            minimum_difficulty: crate::rules::Uint::from_u64(2),
            duration: 5,
            gas_ceiling: self.profile.gas_limit,
            p2p_threshold: 7,
        };
        Ok(Some(Arc::new(placement::bootstrap_candidate(
            &self.profile.genesis[2],
            &p,
        )?)))
    }
    fn order(&self, id: &Hash) -> Result<Option<u8>, Error> {
        Ok(self
            .lookup(id)?
            .map(|v| v.delta.as_ref().map_or(0, |d| d.order)))
    }
    fn region_rollup(&self, id: &Hash) -> Result<Option<Arc<Vec<Transaction>>>, Error> {
        Ok(self.lookup(id)?.and_then(|v| match &v.delta {
            None => Some(Arc::new(vec![])),
            Some(d) if d.order <= 1 => Some(d.region.clone()),
            _ => None,
        }))
    }
    fn confirmed(&self, id: &Hash) -> Result<Option<Arc<Vec<Transaction>>>, Error> {
        Ok(self.lookup(id)?.map(|v| {
            v.delta
                .as_ref()
                .map_or_else(|| Arc::new(vec![]), |d| d.confirmed.clone())
        }))
    }
}
