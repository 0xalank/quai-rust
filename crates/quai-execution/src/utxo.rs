//! Native UTXO state: immutable membership base plus a bounded write overlay.
//! Raw membership and the legacy MuHash/count are independent state components.
use crate::{
    muhash::MuHash,
    qi::{OutPoint, ReadError, Utxo, UtxoView, DENOMINATIONS},
};
use quai_primitives::{transaction::keccak, trie, Hash};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
pub const MAX_UTXOS: usize = 1_000_000;
pub(crate) trait Reader: Send + Sync + std::fmt::Debug {
    fn identity(&self) -> Hash;
    fn get(&self, point: &OutPoint) -> Result<Option<Utxo>, ReadError>;
    fn scan(
        &self,
        max_entries: usize,
        max_bytes: usize,
    ) -> Result<BTreeMap<OutPoint, Utxo>, ReadError>;
    fn loaded(&self) -> usize;
}
#[derive(Clone, Debug)]
enum Base {
    Memory(Arc<BTreeMap<OutPoint, Utxo>>),
    Stored(Arc<dyn Reader>),
}
impl Default for Base {
    fn default() -> Self {
        Self::Memory(Default::default())
    }
}
impl PartialEq for Base {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Memory(a), Self::Memory(b)) => a == b,
            (Self::Stored(a), Self::Stored(b)) => a.identity() == b.identity(),
            _ => false,
        }
    }
}
impl Eq for Base {}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UtxoState {
    base: Base,
    changes: Arc<BTreeMap<OutPoint, Option<Utxo>>>,
    raw_count: u64,
    committed_size: u64,
    set: MuHash,
}
fn magnitude(v: &[u8]) -> &[u8] {
    &v[v.iter().position(|b| *b != 0).unwrap_or(v.len())..]
}
pub fn utxo_hash(point: OutPoint, value: &Utxo) -> Hash {
    let denomination = if value.denomination == 0 {
        vec![]
    } else {
        vec![value.denomination]
    };
    keccak(&trie::list([
        trie::bytes(&point.hash),
        trie::bytes(&point.index.to_be_bytes()),
        trie::list([
            trie::bytes(&denomination),
            trie::bytes(&value.address),
            trie::bytes(magnitude(&value.lock)),
        ]),
    ]))
}
impl UtxoView for UtxoState {
    fn get(&self, p: &OutPoint) -> Result<Option<Cow<'_, Utxo>>, ReadError> {
        if let Some(value) = self.changes.get(p) {
            return Ok(value.as_ref().map(Cow::Borrowed));
        }
        match &self.base {
            Base::Memory(m) => Ok(m.get(p).map(Cow::Borrowed)),
            Base::Stored(r) => Ok(r.get(p)?.map(Cow::Owned)),
        }
    }
}
impl UtxoState {
    pub(crate) fn from_image(
        entries: BTreeMap<OutPoint, Utxo>,
        accumulator: &[u8],
        committed_size: u64,
    ) -> Result<Self, String> {
        if entries.len() > MAX_UTXOS
            || entries
                .values()
                .any(|v| v.denomination as usize >= DENOMINATIONS.len() || v.lock.len() > 32)
        {
            return Err("state image UTXO domain".into());
        }
        Ok(Self {
            raw_count: entries.len() as u64,
            base: Base::Memory(Arc::new(entries)),
            committed_size,
            set: MuHash::from_bytes(accumulator)?,
            changes: Default::default(),
        })
    }
    pub(crate) fn from_reader(
        reader: Arc<dyn Reader>,
        raw_count: u64,
        accumulator: &[u8],
        committed_size: u64,
    ) -> Result<Self, String> {
        Ok(Self {
            base: Base::Stored(reader),
            changes: Default::default(),
            raw_count,
            committed_size,
            set: MuHash::from_bytes(accumulator)?,
        })
    }
    pub fn is_lazy(&self) -> bool {
        matches!(self.base, Base::Stored(_))
    }
    pub fn loaded_entries(&self) -> usize {
        match &self.base {
            Base::Stored(r) => r.loaded(),
            _ => 0,
        }
    }
    pub fn raw_len(&self) -> u64 {
        self.raw_count
    }
    pub fn consensus_size(&self) -> u64 {
        self.committed_size
    }
    pub fn len(&self) -> usize {
        self.committed_size as usize
    }
    pub fn is_empty(&self) -> bool {
        self.committed_size == 0
    }
    pub fn root(&self) -> Hash {
        self.set.hash()
    }
    pub fn accumulator(&self) -> [u8; 384] {
        self.set.serialize()
    }
    /// Explicit bounded export for legacy images and diagnostics. Native state
    /// persistence uses changed_points and never enumerates the stored base.
    pub fn materialize(&self) -> Result<Cow<'_, BTreeMap<OutPoint, Utxo>>, ReadError> {
        if self.raw_count > MAX_UTXOS as u64 {
            return Err("UTXO materialization entry bound".into());
        }
        let mut entries = match &self.base {
            Base::Memory(m) => return Ok(Cow::Borrowed(m)),
            Base::Stored(r) => r.scan(MAX_UTXOS, 128 << 20)?,
        };
        for (key, value) in self.changes.iter() {
            if let Some(value) = value {
                entries.insert(*key, value.clone());
            } else {
                entries.remove(key);
            }
        }
        if entries.len() as u64 != self.raw_count {
            return Err(ReadError::Unavailable(
                "UTXO membership count mismatch".into(),
            ));
        }
        Ok(Cow::Owned(entries))
    }
    pub(crate) fn changed_points(&self, other: &Self) -> Result<BTreeSet<OutPoint>, ReadError> {
        match (&self.base, &other.base) {
            (Base::Memory(a), Base::Memory(b)) => {
                if Arc::ptr_eq(a, b) {
                    Ok(BTreeSet::new())
                } else {
                    Ok(a.keys().chain(b.keys()).copied().collect())
                }
            }
            (Base::Stored(a), Base::Stored(b)) if a.identity() == b.identity() => Ok(self
                .changes
                .keys()
                .chain(other.changes.keys())
                .copied()
                .collect()),
            _ => Err("unrelated lazy UTXO bases".into()),
        }
    }
    fn valid(v: &Utxo) -> Result<(), ReadError> {
        if v.lock.len() > 32 || v.denomination as usize >= DENOMINATIONS.len() {
            Err("UTXO encoding".into())
        } else {
            Ok(())
        }
    }
    fn reserve(&self, points: impl IntoIterator<Item = OutPoint>) -> Result<(), ReadError> {
        if matches!(self.base, Base::Stored(_)) {
            let new = points
                .into_iter()
                .filter(|p| !self.changes.contains_key(p))
                .collect::<BTreeSet<_>>();
            if self
                .changes
                .len()
                .checked_add(new.len())
                .is_none_or(|n| n > MAX_UTXOS)
            {
                return Err("UTXO write overlay capacity".into());
            }
        }
        Ok(())
    }
    fn put_known(&mut self, p: OutPoint, v: Option<Utxo>) {
        match &mut self.base {
            Base::Memory(m) => {
                if let Some(v) = v {
                    Arc::make_mut(m).insert(p, v);
                } else {
                    Arc::make_mut(m).remove(&p);
                }
            }
            Base::Stored(_) => {
                Arc::make_mut(&mut self.changes).insert(p, v);
            }
        }
    }
    pub fn add_auxiliary_hash(&mut self, h: Hash) {
        self.set.add(&h);
        self.committed_size = self.committed_size.wrapping_add(1);
    }
    pub fn remove_auxiliary_hash(&mut self, h: Hash) {
        self.set.remove(&h);
        self.committed_size = self.committed_size.wrapping_sub(1);
    }
    pub fn create_raw(&mut self, p: OutPoint, v: Utxo) -> Result<(), ReadError> {
        Self::valid(&v)?;
        let exists = self.get(&p)?.is_some();
        self.reserve([p])?;
        let count = if exists {
            self.raw_count
        } else {
            self.raw_count
                .checked_add(1)
                .ok_or("UTXO raw count overflow")?
        };
        self.add_auxiliary_hash(utxo_hash(p, &v));
        self.put_known(p, Some(v));
        self.raw_count = count;
        Ok(())
    }
    pub(crate) fn trim_raw(&mut self, p: OutPoint, hash: Hash) -> Result<(), ReadError> {
        let exists = self.get(&p)?.is_some();
        self.reserve([p])?;
        let count = if exists {
            self.raw_count
                .checked_sub(1)
                .ok_or("UTXO raw count underflow")?
        } else {
            self.raw_count
        };
        if exists {
            self.put_known(p, None);
        }
        self.raw_count = count;
        self.remove_auxiliary_hash(hash);
        Ok(())
    }
    pub fn insert(&mut self, p: OutPoint, v: Utxo) -> Result<(), ReadError> {
        Self::valid(&v)?;
        if self.get(&p)?.is_some() {
            return Err("UTXO already exists".into());
        }
        if matches!(self.base, Base::Memory(_)) && self.raw_count >= MAX_UTXOS as u64 {
            return Err("UTXO state bounds or ledger".into());
        }
        self.reserve([p])?;
        let count = self
            .raw_count
            .checked_add(1)
            .ok_or("UTXO raw count overflow")?;
        self.add_auxiliary_hash(utxo_hash(p, &v));
        self.put_known(p, Some(v));
        self.raw_count = count;
        Ok(())
    }
    pub fn remove(&mut self, p: &OutPoint) -> Result<Utxo, ReadError> {
        let v = self.get(p)?.ok_or("missing UTXO")?.into_owned();
        self.reserve([*p])?;
        let count = self
            .raw_count
            .checked_sub(1)
            .ok_or("UTXO raw count underflow")?;
        self.put_known(*p, None);
        self.raw_count = count;
        self.remove_auxiliary_hash(utxo_hash(*p, &v));
        Ok(v)
    }
    pub fn apply_delta(
        &mut self,
        removed: &[OutPoint],
        added: &[(OutPoint, Utxo)],
    ) -> Result<(), ReadError> {
        let spent = removed.iter().copied().collect::<BTreeSet<_>>();
        if spent.len() != removed.len() {
            return Err("missing or duplicate spent UTXO".into());
        }
        let mut values = Vec::with_capacity(removed.len());
        for p in removed {
            values.push(
                self.get(p)?
                    .ok_or("missing or duplicate spent UTXO")?
                    .into_owned(),
            );
        }
        let created = added.iter().map(|(p, _)| *p).collect::<BTreeSet<_>>();
        if created.len() != added.len() {
            return Err("invalid or duplicate created UTXO".into());
        }
        for (p, v) in added {
            Self::valid(v)?;
            if !spent.contains(p) && self.get(p)?.is_some() {
                return Err("invalid or duplicate created UTXO".into());
            }
        }
        let count = self
            .raw_count
            .checked_sub(removed.len() as u64)
            .and_then(|n| n.checked_add(added.len() as u64))
            .ok_or("UTXO raw count overflow")?;
        if matches!(self.base, Base::Memory(_)) && count > MAX_UTXOS as u64 {
            return Err("UTXO state capacity".into());
        }
        self.reserve(removed.iter().copied().chain(added.iter().map(|(p, _)| *p)))?;
        for (p, v) in removed.iter().zip(values) {
            self.put_known(*p, None);
            self.remove_auxiliary_hash(utxo_hash(*p, &v));
        }
        for (p, v) in added {
            self.put_known(*p, Some(v.clone()));
            self.add_auxiliary_hash(utxo_hash(*p, v));
        }
        self.raw_count = count;
        Ok(())
    }
    pub fn from_entries(
        entries: impl IntoIterator<Item = (OutPoint, Utxo)>,
    ) -> Result<Self, ReadError> {
        let mut s = Self::default();
        for (p, v) in entries {
            s.insert(p, v)?;
        }
        Ok(s)
    }
}
