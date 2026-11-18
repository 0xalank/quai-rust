//! Raw lockup records with pinned point reads and a bounded mutation overlay.
use crate::{
    lockup::{Key, Lockup},
    state_error::ReadError,
};
use quai_primitives::{
    mpt::{self, ReadNodes, Trie},
    Hash,
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, OnceLock},
};
pub(crate) trait Reader: ReadNodes + Send + Sync + std::fmt::Debug {
    fn identity(&self) -> Hash;
    fn get(&self, key: &Key) -> Result<Option<Lockup>, ReadError>;
    fn scan(
        &self,
        max_entries: usize,
        max_bytes: usize,
    ) -> Result<BTreeMap<Key, Lockup>, ReadError>;
    fn loaded(&self) -> usize;
}
#[derive(Clone, Debug)]
struct Base {
    root: Hash,
    reader: Arc<dyn Reader>,
}
impl PartialEq for Base {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root && self.reader.identity() == other.reader.identity()
    }
}
impl Eq for Base {}
#[derive(Clone, Debug, Default)]
pub struct LockupState {
    base: Option<Base>,
    values: Arc<BTreeMap<Key, Lockup>>,
    deleted: Arc<BTreeSet<Key>>,
    root_cache: Arc<OnceLock<Trie>>,
}
impl PartialEq for LockupState {
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base && self.values == other.values && self.deleted == other.deleted
    }
}
impl Eq for LockupState {}
impl LockupState {
    pub(crate) fn from_reader(root: Hash, reader: Arc<dyn Reader>) -> Self {
        Self {
            base: Some(Base { root, reader }),
            ..Default::default()
        }
    }
    pub fn is_lazy(&self) -> bool {
        self.base.is_some()
    }
    pub fn loaded_entries(&self) -> usize {
        self.base
            .as_ref()
            .map_or(self.values.len(), |b| b.reader.loaded())
    }
    pub fn get(&self, key: &Key) -> Result<Option<Lockup>, ReadError> {
        if self.deleted.contains(key) {
            return Ok(None);
        }
        if let Some(value) = self.values.get(key) {
            return Ok(Some(value.clone()));
        }
        if let Some(base) = &self.base {
            base.reader.get(key)
        } else {
            Ok(None)
        }
    }
    pub fn set(&mut self, key: Key, value: Option<Lockup>) -> Result<(), ReadError> {
        let limit = if self.base.is_some() {
            65_536
        } else {
            1_000_000
        };
        if self.values.len() + self.deleted.len() >= limit
            && !self.values.contains_key(&key)
            && !self.deleted.contains(&key)
        {
            return Err("lockup working bound".into());
        }
        self.get(&key)?;
        self.root_cache = Default::default();
        if let Some(value) = value {
            Arc::make_mut(&mut self.deleted).remove(&key);
            Arc::make_mut(&mut self.values).insert(key, value);
        } else {
            Arc::make_mut(&mut self.values).remove(&key);
            if self.base.is_some() {
                Arc::make_mut(&mut self.deleted).insert(key);
            }
        }
        Ok(())
    }
    pub fn materialize(&self) -> Result<Cow<'_, BTreeMap<Key, Lockup>>, ReadError> {
        let Some(base) = &self.base else {
            return Ok(Cow::Borrowed(&self.values));
        };
        let mut rows = base.reader.scan(1_000_000, 128 << 20)?;
        for key in self.deleted.iter() {
            rows.remove(key);
        }
        for (key, value) in self.values.iter() {
            rows.insert(*key, value.clone());
        }
        if rows.len() > 1_000_000 {
            return Err("lockup export bound".into());
        }
        Ok(Cow::Owned(rows))
    }
    pub(crate) fn memory_image(&self) -> Result<Self, ReadError> {
        Ok(Self {
            values: Arc::new(self.materialize()?.into_owned()),
            ..Default::default()
        })
    }
    pub fn is_empty(&self) -> Result<bool, ReadError> {
        if self.base.is_none() {
            Ok(self.values.is_empty())
        } else {
            Ok(self.root()? == mpt::empty_root())
        }
    }
    pub(crate) fn changed_keys(&self, other: &Self) -> Result<BTreeSet<Key>, ReadError> {
        match (&self.base, &other.base) {
            (None, None) => {}
            (Some(a), Some(b)) if a == b => {}
            _ => return Err("unrelated lockup bases".into()),
        };
        Ok(self
            .values
            .keys()
            .chain(other.values.keys())
            .chain(self.deleted.iter())
            .chain(other.deleted.iter())
            .copied()
            .collect())
    }
    pub fn root(&self) -> Result<Hash, ReadError> {
        if let Some(trie) = self.root_cache.get() {
            return Ok(trie.root());
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
        for (key, value) in self.values.iter() {
            let mut raw = key.raw().to_vec();
            raw.extend(value.encode());
            trie.update(nodes, &key.raw(), &raw).map_err(|e| {
                if self.base.is_some() {
                    ReadError::Unavailable(e.to_string())
                } else {
                    ReadError::Invalid(e.to_string())
                }
            })?;
        }
        for key in self.deleted.iter() {
            trie.update(nodes, &key.raw(), &[])
                .map_err(|e| ReadError::Unavailable(e.to_string()))?;
        }
        let root = trie.root();
        let _ = self.root_cache.set(trie);
        Ok(root)
    }
}
