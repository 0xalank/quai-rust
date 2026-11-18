//! Pinned ETX queue and economic metadata with bounded write overlays.
use crate::state_error::ReadError;
use quai_primitives::{
    mpt::{self, ReadNodes, Trie},
    transaction::{Kind, Transaction},
    Hash,
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, OnceLock},
};
/// Bounds of the in-memory ETX image (`base == None`: the snapshot image and
/// the private test chains). A lazy overlay over a state database has no
/// change, payload or buffer cap: go-quai's ETX set is unbounded, and the
/// overlay is written to the database at every commit.
pub const MAX_PENDING_ETXS: usize = 16_384;
const MAX_IMAGE_PAYLOAD: usize = 32 << 20;
const MAX_IMAGE_BUFFERS: usize = 64 << 20;
pub(crate) fn index_key(newest: bool) -> Vec<u8> {
    let mut key = vec![0; 4];
    key.extend([255; 28]);
    if !newest {
        key[31] = 254;
    }
    key
}
pub(crate) fn magnitude(index: u64) -> Vec<u8> {
    let bytes = index.to_be_bytes();
    bytes[bytes.iter().position(|b| *b != 0).unwrap_or(8)..].to_vec()
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub transaction: Arc<Transaction>,
    pub bytes: usize,
    pub envelope: Vec<u8>,
}
pub(crate) trait Reader: ReadNodes + Send + Sync + std::fmt::Debug {
    fn identity(&self) -> Hash;
    fn entry(&self, index: u64) -> Result<Option<Arc<Entry>>, ReadError>;
    fn auxiliary(&self, key: &[u8]) -> Result<Option<Vec<u8>>, ReadError>;
    fn auxiliary_image(
        &self,
        max_entries: usize,
        max_bytes: usize,
    ) -> Result<BTreeMap<Vec<u8>, Vec<u8>>, ReadError>;
    fn loaded(&self) -> usize;
}
#[derive(Clone, Debug)]
struct Base {
    reader: Arc<dyn Reader>,
    root: Hash,
}
impl PartialEq for Base {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root && self.reader.identity() == other.reader.identity()
    }
}
impl Eq for Base {}
#[derive(Clone, Debug, Default)]
pub struct EtxState {
    oldest: u64,
    newest: u64,
    base: Option<Base>,
    slots: Arc<BTreeMap<u64, Arc<Entry>>>,
    deleted: Arc<BTreeSet<u64>>,
    payload_bytes: usize,
    buffer_bytes: usize,
    auxiliary: Arc<BTreeMap<Vec<u8>, Vec<u8>>>,
    root_cache: Arc<OnceLock<Trie>>,
}
impl PartialEq for EtxState {
    fn eq(&self, other: &Self) -> bool {
        self.oldest == other.oldest
            && self.newest == other.newest
            && self.base == other.base
            && self.slots == other.slots
            && self.deleted == other.deleted
            && self.payload_bytes == other.payload_bytes
            && self.buffer_bytes == other.buffer_bytes
            && self.auxiliary == other.auxiliary
    }
}
impl Eq for EtxState {}
fn unavailable(error: impl std::fmt::Display) -> ReadError {
    ReadError::Unavailable(error.to_string())
}
impl EtxState {
    pub(crate) fn from_reader(
        oldest: u64,
        newest: u64,
        root: Hash,
        reader: Arc<dyn Reader>,
    ) -> Result<Self, ReadError> {
        if newest < oldest {
            return Err(unavailable("ETX queue interval"));
        }
        let cache = OnceLock::new();
        let _ = cache.set(Trie::from_root(root));
        Ok(Self {
            oldest,
            newest,
            base: Some(Base { reader, root }),
            root_cache: Arc::new(cache),
            ..Default::default()
        })
    }
    pub fn is_lazy(&self) -> bool {
        self.base.is_some()
    }
    pub fn loaded_entries(&self) -> usize {
        self.base
            .as_ref()
            .map_or(self.slots.len(), |b| b.reader.loaded())
    }
    pub fn oldest(&self) -> u64 {
        self.oldest
    }
    pub fn newest(&self) -> u64 {
        self.newest
    }
    pub fn len(&self) -> usize {
        usize::try_from(self.newest - self.oldest).unwrap_or(usize::MAX)
    }
    pub fn is_empty(&self) -> bool {
        self.newest == self.oldest
    }
    pub fn is_pristine(&self) -> Result<bool, ReadError> {
        Ok(self.oldest == 0 && self.newest == 0 && self.root()? == mpt::empty_root())
    }
    pub fn entries(&self) -> Result<Vec<(u64, Arc<Transaction>)>, ReadError> {
        if self.len() > MAX_PENDING_ETXS {
            return Err("ETX diagnostic entry limit".into());
        }
        let mut out = Vec::with_capacity(self.len());
        let mut bytes = 0usize;
        let mut buffers = 0usize;
        for index in self.oldest..self.newest {
            let entry = self
                .entry(index)?
                .ok_or_else(|| unavailable("missing queued ETX"))?;
            bytes = bytes
                .checked_add(entry.envelope.len())
                .ok_or("ETX diagnostic bytes")?;
            if bytes > 32 << 20 {
                return Err("ETX diagnostic bytes".into());
            }
            buffers = buffers
                .checked_add(entry.bytes)
                .ok_or("ETX diagnostic buffers")?;
            if buffers > 64 << 20 {
                return Err("ETX diagnostic buffer bound".into());
            }
            out.push((index, entry.transaction.clone()));
        }
        Ok(out)
    }
    pub fn auxiliary(&self) -> Result<Cow<'_, BTreeMap<Vec<u8>, Vec<u8>>>, ReadError> {
        let Some(base) = &self.base else {
            return Ok(Cow::Borrowed(&self.auxiliary));
        };
        let mut rows = base.reader.auxiliary_image(4096, 32 << 20)?;
        for (key, value) in self.auxiliary.iter() {
            if value.is_empty() {
                rows.remove(key);
            } else {
                rows.insert(key.clone(), value.clone());
            }
        }
        if rows.len() > 4096 || rows.values().map(Vec::len).sum::<usize>() > 32 << 20 {
            return Err("ETX auxiliary export limit".into());
        }
        Ok(Cow::Owned(rows))
    }
    pub fn auxiliary_value(&self, key: &[u8]) -> Result<Option<Vec<u8>>, ReadError> {
        if let Some(value) = self.auxiliary.get(key) {
            return Ok((!value.is_empty()).then(|| value.clone()));
        }
        if let Some(base) = &self.base {
            base.reader.auxiliary(key)
        } else {
            Ok(None)
        }
    }
    pub fn materialize(&self) -> Result<Self, ReadError> {
        let entries = self
            .entries()?
            .into_iter()
            .map(|(index, tx)| (index, (*tx).clone()))
            .collect();
        Self::from_image(
            self.oldest,
            self.newest,
            entries,
            self.auxiliary()?.into_owned(),
        )
    }
    pub(crate) fn from_image(
        oldest: u64,
        newest: u64,
        slots: Vec<(u64, Transaction)>,
        auxiliary: BTreeMap<Vec<u8>, Vec<u8>>,
    ) -> Result<Self, ReadError> {
        if newest.checked_sub(oldest) != Some(slots.len() as u64) || slots.len() > MAX_PENDING_ETXS
        {
            return Err("state image queue interval".into());
        }
        let mut out = Self {
            oldest,
            newest,
            ..Default::default()
        };
        for (offset, (index, tx)) in slots.into_iter().enumerate() {
            if index != oldest + offset as u64 || !matches!(tx.kind(), Kind::External(_)) {
                return Err("state image queue order/type".into());
            }
            let envelope = tx.rlp_envelope();
            out.payload_bytes = out
                .payload_bytes
                .checked_add(envelope.len())
                .ok_or("state image queue bytes")?;
            if out.payload_bytes > 32 << 20 {
                return Err("state image queue bytes".into());
            }
            let weight = tx
                .encode()
                .len()
                .checked_mul(2)
                .and_then(|n| n.checked_add(envelope.len()))
                .ok_or("ETX buffer bytes")?;
            out.buffer_bytes = out
                .buffer_bytes
                .checked_add(weight)
                .ok_or("ETX buffer bytes")?;
            if out.buffer_bytes > 64 << 20 {
                return Err("ETX buffer working bound".into());
            }
            Arc::make_mut(&mut out.slots).insert(
                index,
                Arc::new(Entry {
                    transaction: Arc::new(tx),
                    envelope,
                    bytes: weight,
                }),
            );
        }
        for (key, value) in auxiliary {
            out.set_auxiliary(key, value)?;
        }
        Ok(out)
    }
    pub fn set_auxiliary(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), ReadError> {
        if key.len() != 32
            || key == index_key(true)
            || key == index_key(false)
            || value.len() > 65_536
            || (self.auxiliary.len() >= 4096 && !self.auxiliary.contains_key(&key))
        {
            return Err("invalid/bounded ETX auxiliary entry".into());
        }
        self.auxiliary_value(&key)?;
        let removed = self.auxiliary.get(&key).map_or(0, Vec::len);
        let bytes = self
            .payload_bytes
            .checked_sub(removed)
            .and_then(|n| n.checked_add(value.len()))
            .ok_or("ETX byte overflow")?;
        let image = self.base.is_none();
        if image && bytes > MAX_IMAGE_PAYLOAD {
            return Err("ETX image payload exceeds 32 MiB".into());
        }
        let buffers = self
            .buffer_bytes
            .checked_sub(removed)
            .and_then(|n| n.checked_add(value.len()))
            .ok_or("ETX buffer bytes")?;
        if image && buffers > MAX_IMAGE_BUFFERS {
            return Err("ETX image buffer bound".into());
        }
        self.root_cache = Default::default();
        if value.is_empty() && self.base.is_none() {
            Arc::make_mut(&mut self.auxiliary).remove(&key);
        } else {
            Arc::make_mut(&mut self.auxiliary).insert(key, value);
        }
        self.payload_bytes = bytes;
        self.buffer_bytes = buffers;
        Ok(())
    }
    pub fn push(&mut self, inbound: &[Transaction]) -> Result<(), ReadError> {
        let image = self.base.is_none();
        if image
            && self
                .len()
                .checked_add(inbound.len())
                .is_none_or(|n| n > MAX_PENDING_ETXS)
        {
            return Err("ETX image queue capacity exceeded".into());
        }
        let next = self
            .newest
            .checked_add(inbound.len() as u64)
            .ok_or("ETX index overflow")?;
        if inbound
            .iter()
            .any(|tx| !matches!(tx.kind(), Kind::External(_)))
        {
            return Err("queue input is not an ETX".into());
        }
        let mut entries = Vec::with_capacity(inbound.len());
        let mut bytes = self.payload_bytes;
        let mut buffers = self.buffer_bytes;
        for tx in inbound {
            let envelope = tx.rlp_envelope();
            bytes = bytes
                .checked_add(envelope.len())
                .ok_or("ETX byte overflow")?;
            if image && bytes > MAX_IMAGE_PAYLOAD {
                return Err("ETX image payload exceeds 32 MiB".into());
            }
            let weight = tx
                .encode()
                .len()
                .checked_mul(2)
                .and_then(|n| n.checked_add(envelope.len()))
                .ok_or("ETX buffer bytes")?;
            buffers = buffers.checked_add(weight).ok_or("ETX buffer bytes")?;
            if image && buffers > MAX_IMAGE_BUFFERS {
                return Err("ETX image buffer bound".into());
            }
            entries.push(Arc::new(Entry {
                transaction: Arc::new(tx.clone()),
                envelope,
                bytes: weight,
            }));
        }
        if !entries.is_empty() {
            self.root_cache = Default::default();
        }
        for (offset, entry) in entries.into_iter().enumerate() {
            Arc::make_mut(&mut self.slots).insert(self.newest + offset as u64, entry);
        }
        self.newest = next;
        self.payload_bytes = bytes;
        self.buffer_bytes = buffers;
        Ok(())
    }
    pub(crate) fn entry(&self, index: u64) -> Result<Option<Arc<Entry>>, ReadError> {
        if index < self.oldest || index >= self.newest {
            return Ok(None);
        }
        if self.deleted.contains(&index) {
            return Err(unavailable("deleted ETX inside live interval"));
        }
        if let Some(entry) = self.slots.get(&index) {
            return Ok(Some(entry.clone()));
        }
        let entry = if let Some(base) = &self.base {
            base.reader.entry(index)?
        } else {
            None
        };
        if entry.is_none() {
            return Err(unavailable("missing ETX inside live queue interval"));
        }
        Ok(entry)
    }
    pub fn read(&self, index: u64) -> Result<Option<Arc<Transaction>>, ReadError> {
        Ok(self.entry(index)?.map(|entry| entry.transaction.clone()))
    }
    pub fn pop(&mut self) -> Result<Option<Transaction>, ReadError> {
        if self.oldest == self.newest {
            return Ok(None);
        }
        let entry = self
            .entry(self.oldest)?
            .ok_or_else(|| unavailable("missing queue head"))?;
        let next = self.oldest.checked_add(1).ok_or("ETX index overflow")?;
        if let Some(present) = Arc::make_mut(&mut self.slots).remove(&self.oldest) {
            self.buffer_bytes = self
                .buffer_bytes
                .checked_sub(present.bytes)
                .ok_or("ETX buffer accounting")?;
            self.payload_bytes = self
                .payload_bytes
                .checked_sub(present.envelope.len())
                .ok_or("ETX payload accounting")?;
        }
        if self.base.is_some() {
            Arc::make_mut(&mut self.deleted).insert(self.oldest);
        }
        self.root_cache = Default::default();
        self.oldest = next;
        Ok(Some(entry.transaction.as_ref().clone()))
    }
    pub(crate) fn changed_slots(&self, other: &Self) -> Result<BTreeSet<u64>, ReadError> {
        match (&self.base, &other.base) {
            (None, None) => {}
            (Some(a), Some(b)) if a == b => {}
            _ => return Err("unrelated ETX bases".into()),
        };
        Ok(self
            .slots
            .keys()
            .chain(other.slots.keys())
            .chain(self.deleted.iter())
            .chain(other.deleted.iter())
            .copied()
            .collect())
    }
    pub(crate) fn changed_auxiliary(&self, other: &Self) -> Result<BTreeSet<Vec<u8>>, ReadError> {
        match (&self.base, &other.base) {
            (None, None) => {}
            (Some(a), Some(b)) if a == b => {}
            _ => return Err("unrelated ETX bases".into()),
        };
        Ok(self
            .auxiliary
            .keys()
            .chain(other.auxiliary.keys())
            .cloned()
            .collect())
    }
    pub fn root(&self) -> Result<Hash, ReadError> {
        Ok(self.trie()?.root())
    }
    fn trie(&self) -> Result<Trie, ReadError> {
        if let Some(cached) = self.root_cache.get() {
            return Ok(cached.clone());
        }
        if self.base.is_none() {
            let mut entries = self.auxiliary.as_ref().clone();
            for (index, entry) in self.slots.iter() {
                entries.insert(magnitude(*index), entry.envelope.clone());
            }
            if self.oldest != 0 {
                entries.insert(index_key(false), magnitude(self.oldest));
            }
            if self.newest != 0 {
                entries.insert(index_key(true), magnitude(self.newest));
            }
            let trie = Trie::from_secure_pairs(&entries.into_iter().collect::<Vec<_>>())
                .map_err(|e| ReadError::Invalid(e.to_string()))?;
            let _ = self.root_cache.set(trie.clone());
            return Ok(trie);
        }
        let mut trie = if let Some(base) = &self.base {
            Trie::from_root(base.root)
        } else {
            Trie::default()
        };
        let nodes: &dyn ReadNodes = if let Some(base) = &self.base {
            base.reader.as_ref()
        } else {
            &mpt::NoNodes
        };
        for (index, entry) in self.slots.iter() {
            trie.update(nodes, &magnitude(*index), &entry.envelope)
                .map_err(|e| {
                    if self.base.is_some() {
                        unavailable(e)
                    } else {
                        ReadError::Invalid(e.to_string())
                    }
                })?;
        }
        for index in self.deleted.iter() {
            trie.update(nodes, &magnitude(*index), &[])
                .map_err(unavailable)?;
        }
        for (key, value) in self.auxiliary.iter() {
            trie.update(nodes, key, value).map_err(|e| {
                if self.base.is_some() {
                    unavailable(e)
                } else {
                    ReadError::Invalid(e.to_string())
                }
            })?;
        }
        for (key, value) in [
            (index_key(false), magnitude(self.oldest)),
            (index_key(true), magnitude(self.newest)),
        ] {
            trie.update(nodes, &key, &value).map_err(|e| {
                if self.base.is_some() {
                    unavailable(e)
                } else {
                    ReadError::Invalid(e.to_string())
                }
            })?;
        }
        let _ = self.root_cache.set(trie.clone());
        Ok(trie)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use quai_primitives::trie;
    #[test]
    fn empty_state_is_trie_root_and_metadata_is_preserved() {
        let mut state = EtxState::default();
        assert_eq!(state.root().unwrap(), trie::ordered_root(&[]).unwrap());
        assert!(state.pop().unwrap().is_none());
        let mut key = vec![0; 4];
        key.extend([255; 27]);
        key.push(250);
        state.set_auxiliary(key.clone(), vec![1]).unwrap();
        let root = state.root().unwrap();
        assert_ne!(root, trie::ordered_root(&[]).unwrap());
        state.push(&[]).unwrap();
        assert_eq!(state.root().unwrap(), root);
        assert!(state.set_auxiliary(index_key(true), vec![9]).is_err());
        state.set_auxiliary(key, vec![]).unwrap();
        assert_eq!(state.root().unwrap(), trie::ordered_root(&[]).unwrap());
    }
}
