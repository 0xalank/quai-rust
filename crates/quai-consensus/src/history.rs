//! Bounded ancestry snapshots. Imported records are candidates, never chain authority.
use crate::{rules::Uint, share_rules, verifier::PINNED_SCHEDULE, Error};
use quai_primitives::{work::WorkObject, Hash, Header};
use std::{collections::BTreeMap, sync::Arc};
pub const MAX_RECORDS: usize = 4096;
pub const MAX_HISTORY_BYTES: usize = 32 << 20;
#[derive(Clone)]
pub struct Record {
    pub(crate) object: WorkObject,
    pub(crate) header: Header,
    pub(crate) id: Hash,
    pub(crate) numbers: [u64; 3],
    pub(crate) parents: [Hash; 3],
    pub(crate) prime: u64,
    bytes: Vec<u8>,
}
impl Record {
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let object = WorkObject::decode_auxiliary(bytes, PINNED_SCHEDULE)
            .map_err(|_| Error::Unsupported("complete ancestry work-object domain"))?;
        let header = object
            .auxiliary_work_header(PINNED_SCHEDULE)
            .map_err(|_| Error::Unsupported("ancestry work header"))?;
        let b = object.header().wire();
        let h = header.wire();
        let numbers = [
            share_rules::number(&b.number[0])?,
            share_rules::number(&b.number[1])?,
            share_rules::number(h.number.as_deref().unwrap_or_default())?,
        ];
        let parents = [
            b.parent_hash[0].value.as_slice().try_into().unwrap(),
            b.parent_hash[1].value.as_slice().try_into().unwrap(),
            h.parent_hash
                .as_ref()
                .unwrap()
                .value
                .as_slice()
                .try_into()
                .unwrap(),
        ];
        let prime = share_rules::number(h.prime_terminus_number.as_deref().unwrap_or_default())?;
        let id = header
            .block_hash()
            .map_err(|_| Error::Unsupported("ancestry block hash"))?;
        let bytes = object.encode();
        Ok(Self {
            object,
            header,
            id,
            numbers,
            parents,
            prime,
            bytes,
        })
    }
    /// The all-height binding required before order lookup or calculation.
    pub fn verify_binding(&self) -> Result<(), Error> {
        if self.header.wire().header_hash.as_ref().unwrap().value != self.object.header().hash() {
            return Err(Error::Invalid("body-header commitment"));
        }
        let aux = self
            .header
            .wire()
            .aux_pow
            .as_ref()
            .map(|b| crate::auxiliary::Auxiliary::decode(b))
            .transpose()?;
        if !crate::rules::pow_id_valid(self.prime, aux.as_ref().map(|a| a.id()), false) {
            return Err(Error::Invalid("work-object proof ID"));
        }
        if self.prime >= crate::rules::KAWPOW_FORK {
            if let Some(a) = aux {
                a.verify_binding(self.header.seal_hash())?;
            }
        }
        Ok(())
    }
    pub fn id(&self) -> Hash {
        self.id
    }
    pub fn header(&self) -> &Header {
        &self.header
    }
    pub fn object(&self) -> &WorkObject {
        &self.object
    }
    pub fn parent(&self, context: usize) -> Result<Hash, Error> {
        self.parents
            .get(context)
            .copied()
            .ok_or(Error::Invalid("parent context"))
    }
    pub fn number(&self, context: usize) -> Result<u64, Error> {
        self.numbers
            .get(context)
            .copied()
            .ok_or(Error::Invalid("node context"))
    }
    pub fn prime(&self) -> u64 {
        self.prime
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn difficulty(&self) -> Result<Uint, Error> {
        share_rules::uint(self.header.wire().difficulty.as_deref().unwrap())
    }
    pub fn uncles(&self) -> Result<Vec<Header>, Error> {
        self.object
            .uncle_headers(PINNED_SCHEDULE)
            .map_err(|_| Error::Unsupported("ancestry uncle codec"))
    }
}
/// A reader is bound to one immutable branch version. None means the key is
/// outside that version; unavailable/corrupt known records must return errors.
pub trait Reader: Send + Sync {
    fn read(&self, id: &Hash) -> Result<Option<Arc<Record>>, Error>;
}
pub(crate) struct Cache<T> {
    values: std::sync::Mutex<(
        BTreeMap<Hash, (Arc<T>, usize)>,
        std::collections::VecDeque<Hash>,
        usize,
    )>,
    entries: usize,
    bytes: usize,
}
impl<T> Cache<T> {
    pub fn new(entries: usize, bytes: usize) -> Self {
        Self {
            values: Default::default(),
            entries,
            bytes,
        }
    }
    pub fn get(
        &self,
        id: &Hash,
        load: impl FnOnce() -> Result<Option<Arc<T>>, Error>,
        size: impl FnOnce(&T) -> usize,
    ) -> Result<Option<Arc<T>>, Error> {
        {
            let cache = self
                .values
                .lock()
                .map_err(|_| Error::Unavailable("ancestry cache"))?;
            if let Some((value, _)) = cache.0.get(id) {
                return Ok(Some(value.clone()));
            }
        }
        let Some(value) = load()? else {
            return Ok(None);
        };
        let bytes = size(&value);
        if self.entries == 0 || bytes > self.bytes {
            return Ok(Some(value));
        }
        let mut cache = self
            .values
            .lock()
            .map_err(|_| Error::Unavailable("ancestry cache"))?;
        if cache.0.contains_key(id) {
            return Ok(Some(value));
        }
        while cache.0.len() >= self.entries
            || cache.2.checked_add(bytes).is_none_or(|n| n > self.bytes)
        {
            let key = cache
                .1
                .pop_front()
                .ok_or(Error::Unavailable("ancestry cache accounting"))?;
            if let Some((_, n)) = cache.0.remove(&key) {
                cache.2 -= n;
            }
        }
        cache.0.insert(*id, (value.clone(), bytes));
        cache.1.push_back(*id);
        cache.2 += bytes;
        Ok(Some(value))
    }
    pub fn len(&self) -> usize {
        self.values.lock().map(|c| c.0.len()).unwrap_or(0)
    }
    pub fn logical_bytes(&self) -> usize {
        self.values.lock().map(|c| c.2).unwrap_or(0)
    }
}
/// Borrow the current candidate while retaining ownership when walking stored ancestors.
pub(crate) enum Cursor<'a> {
    Borrowed(&'a Record),
    Owned(Arc<Record>),
}
impl<'a> From<&'a Record> for Cursor<'a> {
    fn from(v: &'a Record) -> Self {
        Self::Borrowed(v)
    }
}
impl From<Arc<Record>> for Cursor<'_> {
    fn from(v: Arc<Record>) -> Self {
        Self::Owned(v)
    }
}
impl std::ops::Deref for Cursor<'_> {
    type Target = Record;
    fn deref(&self) -> &Record {
        match self {
            Self::Borrowed(v) => v,
            Self::Owned(v) => v,
        }
    }
}
pub struct History {
    records: BTreeMap<Hash, Arc<Record>>,
    bytes: usize,
    source: Option<Arc<dyn Reader>>,
    cache: Cache<Record>,
}
impl Default for History {
    fn default() -> Self {
        Self {
            records: BTreeMap::new(),
            bytes: 0,
            source: None,
            cache: Cache::new(512, 16 << 20),
        }
    }
}
impl History {
    pub fn with_reader(reader: Arc<dyn Reader>) -> Self {
        Self {
            source: Some(reader),
            ..Self::default()
        }
    }
    fn find(&self, id: &Hash) -> Result<Option<Arc<Record>>, Error> {
        if let Some(record) = self.records.get(id) {
            return Ok(Some(record.clone()));
        }
        let Some(source) = &self.source else {
            return Ok(None);
        };
        self.cache.get(
            id,
            || {
                let value = source.read(id)?;
                if value.as_ref().is_some_and(|v| v.id() != *id) {
                    return Err(Error::Invalid("ancestry reader key binding"));
                }
                Ok(value)
            },
            |v| v.bytes().len(),
        )
    }
    pub fn cached_records(&self) -> usize {
        self.cache.len()
    }
    pub(crate) fn check_insert(&self, record: &Record) -> Result<(), Error> {
        if let Some(old) = self.find(&record.id)? {
            if old.bytes != record.bytes {
                return Err(Error::Invalid("conflicting ancestry record"));
            }
            return Ok(());
        }
        if self.records.len() == MAX_RECORDS
            || self
                .bytes
                .checked_add(record.bytes.len())
                .is_none_or(|n| n > MAX_HISTORY_BYTES)
        {
            return Err(Error::ResourceLimit("ancestry snapshot"));
        }
        Ok(())
    }
    pub fn insert(&mut self, record: Record) -> Result<Hash, Error> {
        self.check_insert(&record)?;
        let id = record.id;
        if self.records.contains_key(&id) {
            return Ok(id);
        }
        self.bytes += record.bytes.len();
        self.records.insert(id, Arc::new(record));
        Ok(id)
    }
    pub fn get(&self, id: &Hash) -> Result<Arc<Record>, Error> {
        self.find(id)?.ok_or(Error::Unavailable("ancestry record"))
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    pub fn encoded_bytes(&self) -> usize {
        self.bytes
    }
    /// Pinned zone-chain distance. Missing records are unavailable. Values that
    /// imply a nonmonotonic/overflowed history are outside the qualified domain.
    pub fn workshare_distance(&self, block: &Record, share: &Header) -> Result<u64, Error> {
        let depth = share_rules::inclusion_depth(block.prime);
        let mut ancestors = BTreeMap::new();
        let mut current = Cursor::from(block);
        for _ in 0..depth {
            let parent = self.get(&current.parents[2])?;
            ancestors.insert(parent.id, parent.numbers[2]);
            current = parent.into();
        }
        let mut parent: Hash = share
            .wire()
            .parent_hash
            .as_ref()
            .unwrap()
            .value
            .as_slice()
            .try_into()
            .unwrap();
        let mut distance = 0u64;
        loop {
            let record = self.get(&parent)?;
            if let Some(number) = ancestors.get(&record.id) {
                let delta = block.numbers[2]
                    .checked_sub(*number)
                    .and_then(|n| n.checked_sub(1))
                    .ok_or(Error::Unsupported("nonmonotonic ancestry distance"))?;
                distance = distance
                    .checked_add(delta)
                    .ok_or(Error::Unsupported("ancestry distance overflow"))?;
                break;
            }
            distance += 1;
            if distance > depth as u64 {
                break;
            }
            parent = record.parents[2];
        }
        if distance > depth as u64 {
            return Err(Error::Invalid("workshare inclusion distance"));
        }
        Ok(distance)
    }
}

#[cfg(test)]
mod indexed_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Synthetic {
        seed: Record,
        reads: AtomicUsize,
        fail: Option<u64>,
    }
    fn id(n: u64) -> Hash {
        let mut h = [0; 32];
        h[24..].copy_from_slice(&n.to_be_bytes());
        h
    }
    impl Reader for Synthetic {
        fn read(&self, key: &Hash) -> Result<Option<Arc<Record>>, Error> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let n = u64::from_be_bytes(key[24..].try_into().unwrap());
            if key[..24] != [0; 24] || n >= 6000 {
                return Ok(None);
            }
            if self.fail == Some(n) {
                return Err(Error::Unavailable("synthetic missing record"));
            }
            // Synthetic candidate records exercise lookup/cache bounds, not block admission.
            let mut r = self.seed.clone();
            r.id = *key;
            r.numbers[2] = n;
            r.parents[2] = id(n.saturating_sub(1));
            Ok(Some(Arc::new(r)))
        }
    }
    #[test]
    fn indexed_candidate_reader_has_bounded_cache_without_a_total_history_cap() {
        let seed = crate::private_profile::PrivateChain::reviewed_hierarchy()
            .unwrap()
            .genesis[2]
            .clone();
        let source = Arc::new(Synthetic {
            seed: seed.clone(),
            reads: AtomicUsize::new(0),
            fail: None,
        });
        let history = History::with_reader(source.clone());
        for n in 0..6000 {
            assert_eq!(history.get(&id(n)).unwrap().number(2).unwrap(), n);
        }
        assert!(history.cached_records() <= 512);
        assert_eq!(history.len(), 0);
        let before = source.reads.load(Ordering::Relaxed);
        assert_eq!(history.get(&id(5999)).unwrap().id(), id(5999));
        assert_eq!(source.reads.load(Ordering::Relaxed), before);
        assert!(history.get(&id(6000)).is_err());
        let fresh = History::with_reader(Arc::new(Synthetic {
            seed,
            reads: AtomicUsize::new(0),
            fail: Some(5999),
        }));
        assert!(matches!(fresh.get(&id(5999)), Err(Error::Unavailable(_))));
    }
}
