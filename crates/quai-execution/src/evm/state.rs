//! Account snapshots with pinned read providers and write overlays.
//!
//! A `World` has two forms:
//!
//! - Lazy (`World::from_reader`, from `state_store::restore_sparse`). This is
//!   the form for block execution at mainnet scale. Its write overlay has no
//!   count or byte cap, because go-quai has none. Gas does not give a usable
//!   bound either: the state-scaled SSTORE and cold-access prices fall to zero
//!   when the parent state size and the contract size are at most one entry
//!   (params.CalculateGasWithStateScaling). The overlay also lives across
//!   blocks until the caller persists it. For example, value transfers to
//!   2,100 contracts with 32 KiB of code each cost less than 25,000,000 gas
//!   (9,000 + 2,600 + 100 per call at a 2^20-account state) and put more than
//!   64 MiB of account data into the overlay.
//! - In memory (`World::default`). This is a complete snapshot for tests,
//!   private chains, checkpoints and state images. `MAX_ACCOUNTS`,
//!   `MAX_STORAGE_SLOTS` and the aggregate limits in `checked_totals` bound the
//!   whole snapshot, not one block, and `validate` enforces them. A mainnet
//!   state is larger than these bounds, so mainnet replay must use the lazy form.
//!
//! The 32,768-byte code bound in both forms is go-quai's `params.NewMaxCodeSize`.
use super::word::{self, Word};
pub use crate::state_error::ReadError;
use quai_primitives::mpt::{self, ReadNodes, Trie};
use quai_primitives::{
    transaction::{keccak, Address},
    Hash,
};
use std::{borrow::Cow, collections::BTreeSet};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};
/// `World::proof`: Merkle proofs of an account and its storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldProof {
    /// The encoded account-trie nodes on the account's path, root first.
    pub account: Vec<Vec<u8>>,
    /// The storage of an existing account; None when it does not exist.
    pub storage: Option<StorageProof>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageProof {
    /// The account's storage root.
    pub root: Hash,
    /// keccak256 of the account's code.
    pub code_hash: Hash,
    /// For each requested key: its value and the encoded nodes on its path.
    pub keys: Vec<(Hash, Vec<Vec<u8>>)>,
}
/// Capacity of an in-memory snapshot and of explicit account exports.
pub const MAX_ACCOUNTS: usize = 65_536;
/// Per-account capacity of an in-memory snapshot and of storage exports.
pub const MAX_STORAGE_SLOTS: usize = 4096;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub nonce: u64,
    pub balance: Word,
    pub code: Vec<u8>,
    pub storage: BTreeMap<Hash, Hash>,
    pub storage_size: Word,
    pub backing: Option<StorageBase>,
}
impl Default for Account {
    fn default() -> Self {
        Self {
            nonce: 0,
            balance: Word::ZERO,
            code: vec![],
            storage: BTreeMap::new(),
            storage_size: Word::ZERO,
            backing: None,
        }
    }
}
fn magnitude(value: Word) -> Vec<u8> {
    let b = word::bytes(value);
    b[b.iter().position(|v| *v != 0).unwrap_or(32)..].to_vec()
}
pub fn rlp_bytes(value: &[u8]) -> Vec<u8> {
    if value.len() == 1 && value[0] < 128 {
        return value.to_vec();
    }
    let mut out = rlp_prefix(value.len(), 0x80, 0xb7);
    out.extend_from_slice(value);
    out
}
fn rlp_prefix(length: usize, short: u8, long: u8) -> Vec<u8> {
    if length < 56 {
        return vec![short + length as u8];
    }
    // Big-endian length without its leading zero bytes.
    let b = length.to_be_bytes();
    let b = &b[(length.leading_zeros() / 8) as usize..];
    let mut out = vec![long + b.len() as u8];
    out.extend_from_slice(b);
    out
}
fn rlp_list(values: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
    let body: Vec<u8> = values.into_iter().flatten().collect();
    let mut out = rlp_prefix(body.len(), 0xc0, 0xf7);
    out.extend(body);
    out
}
pub(crate) trait NodeReader: ReadNodes + Send + Sync + std::fmt::Debug {
    fn identity(&self) -> Hash;
    fn slots(
        &self,
        root: Hash,
        max_entries: usize,
        max_bytes: usize,
    ) -> Result<BTreeMap<Hash, Hash>, ReadError>;
}
#[derive(Clone, Debug)]
pub struct StorageBase {
    pub(crate) reader: Arc<dyn NodeReader>,
    pub(crate) root: Hash,
    pub(crate) count: u64,
}
impl PartialEq for StorageBase {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
            && self.count == other.count
            && self.reader.identity() == other.reader.identity()
    }
}
impl Eq for StorageBase {}
pub(crate) trait AccountReader: ReadNodes + Send + Sync + std::fmt::Debug {
    fn identity(&self) -> Hash;
    fn account(&self, address: &Address) -> Result<Option<Arc<Account>>, ReadError>;
    fn accounts(
        &self,
        max_entries: usize,
        max_bytes: usize,
    ) -> Result<BTreeMap<Address, Arc<Account>>, ReadError>;
    fn loaded(&self) -> usize;
}
fn trie_error(error: mpt::Error, stored: bool) -> ReadError {
    if stored {
        ReadError::Unavailable(error.to_string())
    } else {
        ReadError::Invalid(error.to_string())
    }
}
pub(crate) fn slot_rlp(value: &Hash) -> Vec<u8> {
    match value.iter().position(|v| *v != 0) {
        None => vec![],
        Some(start) => rlp_bytes(&value[start..]),
    }
}
impl Account {
    // No slot cap here: an account created in a lazy world has no backing yet,
    // and `World::validate` bounds in-memory snapshots.
    pub(crate) fn storage_trie(&self) -> Result<Trie, ReadError> {
        if let Some(base) = &self.backing {
            let mut trie = Trie::from_root(base.root);
            for (key, value) in &self.storage {
                trie.update(base.reader.as_ref(), key, &slot_rlp(value))
                    .map_err(|e| trie_error(e, true))?;
            }
            Ok(trie)
        } else {
            let pairs = self
                .storage
                .iter()
                .filter(|(_, v)| **v != [0; 32])
                .map(|(k, v)| (k.to_vec(), slot_rlp(v)))
                .collect::<Vec<_>>();
            if pairs.len() <= quai_primitives::trie::MAX_LIST_ITEMS {
                return Trie::from_secure_pairs(&pairs).map_err(|e| trie_error(e, false));
            }
            // The bulk builder stops at MAX_LIST_ITEMS entries. Path updates
            // give the same root for a larger new account in a lazy world.
            let mut trie = Trie::default();
            for (key, value) in &pairs {
                trie.update(&mpt::NoNodes, key, value)
                    .map_err(|e| trie_error(e, false))?;
            }
            Ok(trie)
        }
    }
    pub fn storage_root(&self) -> Result<Hash, ReadError> {
        Ok(self.storage_trie()?.root())
    }
    pub fn storage_len(&self) -> u64 {
        self.backing.as_ref().map_or_else(
            || self.storage.values().filter(|v| **v != [0; 32]).count() as u64,
            |b| b.count,
        )
    }
    pub fn materialize_storage(&self) -> Result<Cow<'_, BTreeMap<Hash, Hash>>, ReadError> {
        let Some(base) = &self.backing else {
            return Ok(Cow::Borrowed(&self.storage));
        };
        if self.storage_len() > MAX_STORAGE_SLOTS as u64 {
            return Err("storage export bound".into());
        }
        let mut rows = base.reader.slots(base.root, MAX_STORAGE_SLOTS, 32 << 20)?;
        for (key, value) in &self.storage {
            if *value == [0; 32] {
                rows.remove(key);
            } else {
                rows.insert(*key, *value);
            }
        }
        if rows.len() as u64 != self.storage_len() {
            return Err(ReadError::Unavailable("storage export count".into()));
        }
        Ok(Cow::Owned(rows))
    }
    pub fn consensus_rlp(&self) -> Result<Vec<u8>, ReadError> {
        if self.code.len() > 32768 {
            return Err("account code exceeds supported bound".into());
        }
        Ok(rlp_list([
            rlp_bytes(&magnitude(Word::from_u64(self.nonce))),
            rlp_bytes(&magnitude(self.balance)),
            rlp_bytes(&self.storage_root()?),
            rlp_bytes(&keccak(&self.code)),
            rlp_bytes(&magnitude(self.storage_size)),
        ]))
    }
    pub fn get(&self, key: Hash) -> Result<Hash, ReadError> {
        if let Some(value) = self.storage.get(&key) {
            return Ok(*value);
        }
        let Some(base) = &self.backing else {
            return Ok([0; 32]);
        };
        let raw = Trie::from_root(base.root)
            .get(base.reader.as_ref(), &key)
            .map_err(|e| trie_error(e, true))?;
        raw.map(|v| {
            crate::state_store::decode_slot(&v).map_err(|e| ReadError::Unavailable(e.to_string()))
        })
        .transpose()
        .map(|v| v.unwrap_or([0; 32]))
    }
    pub fn put(&mut self, key: Hash, value: Hash) -> Result<(), ReadError> {
        let old = self.get(key)?;
        if old == value {
            if self.backing.is_none() && value == [0; 32] {
                self.storage.remove(&key);
            }
            return Ok(());
        }
        if let Some(base) = &mut self.backing {
            base.count = if old == [0; 32] {
                base.count.checked_add(1).ok_or("storage count overflow")?
            } else if value == [0; 32] {
                base.count
                    .checked_sub(1)
                    .ok_or_else(|| ReadError::Unavailable("stored slot count underflow".into()))?
            } else {
                base.count
            };
            self.storage.insert(key, value);
        } else if value == [0; 32] {
            self.storage.remove(&key);
        } else {
            self.storage.insert(key, value);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Default)]
struct Footprint {
    code: usize,
    slots: usize,
}
impl Footprint {
    fn of(a: &Account) -> Self {
        Self {
            code: a.code.len(),
            slots: a.storage.len(),
        }
    }
    fn bytes(self) -> Result<usize, String> {
        self.slots
            .checked_mul(64)
            .and_then(|n| n.checked_add(self.code))
            .and_then(|n| n.checked_add(128))
            .ok_or_else(|| "snapshot size overflow".into())
    }
}
#[derive(Clone, Copy, Debug, Default)]
struct Totals {
    bytes: usize,
    slots: usize,
}
#[derive(Clone, Debug, Default)]
struct Commitment {
    trie: quai_primitives::mpt::Trie,
    totals: Totals,
}
#[derive(Clone, Debug, Default)]
pub struct World {
    accounts: Arc<BTreeMap<Address, Arc<Account>>>,
    base: Option<Arc<dyn AccountReader>>,
    deleted: Arc<BTreeSet<Address>>,
    account_count: u64,
    // Derived only: excluded from equality and local checkpoint encoding.
    commitment_base: Arc<Commitment>,
    dirty_accounts: Arc<BTreeMap<Address, Option<Footprint>>>,
    root_cache: Arc<OnceLock<Commitment>>,
    lockups: Arc<crate::lockup_state::LockupState>,
    etxs: Arc<crate::etx::EtxState>,
    in_block: bool,
    size_baselines: Arc<BTreeMap<Address, (Word, u64)>>,
}
impl PartialEq for World {
    fn eq(&self, other: &Self) -> bool {
        self.base.as_ref().map(|b| b.identity()) == other.base.as_ref().map(|b| b.identity())
            && self.deleted == other.deleted
            && self.account_count == other.account_count
            && self.accounts == other.accounts
            && self.lockups == other.lockups
            && self.etxs == other.etxs
            && self.in_block == other.in_block
            && self.size_baselines == other.size_baselines
    }
}
impl Eq for World {}
impl World {
    fn invalidate_account(&mut self, address: Address) -> Result<(), ReadError> {
        let previous = std::mem::take(&mut self.root_cache);
        if let Some(committed) = previous.get() {
            self.commitment_base = Arc::new(committed.clone());
            self.dirty_accounts = Default::default();
        }
        if !self.dirty_accounts.contains_key(&address) {
            let old = self.account(&address)?.as_ref().map(|a| Footprint::of(a));
            Arc::make_mut(&mut self.dirty_accounts).insert(address, old);
        }
        Ok(())
    }

    pub fn etxs(&self) -> &crate::etx::EtxState {
        &self.etxs
    }
    pub fn etxs_mut(&mut self) -> &mut crate::etx::EtxState {
        Arc::make_mut(&mut self.etxs)
    }
    pub fn begin_block(&mut self) {
        self.in_block = true;
        self.size_baselines = Default::default();
    }
    pub(crate) fn in_block(&self) -> bool {
        self.in_block
    }
    pub(crate) fn size_baseline(&self, a: Address) -> Option<(Word, u64)> {
        self.size_baselines.get(&a).copied()
    }
    pub(crate) fn set_size_baseline(&mut self, a: Address, v: Option<(Word, u64)>) {
        if !self.in_block {
            return;
        }
        let map = Arc::make_mut(&mut self.size_baselines);
        if let Some(v) = v {
            map.insert(a, v);
        } else {
            map.remove(&a);
        }
    }
    fn mark_size(&mut self, a: Address) -> Result<(), ReadError> {
        if self.in_block && !self.size_baselines.contains_key(&a) {
            let old = self
                .account(&a)?
                .map(|v| (v.storage_size, v.storage_len()))
                .unwrap_or((Word::ZERO, 0));
            self.set_size_baseline(a, Some(old));
        }
        Ok(())
    }
    pub fn finish_block(&mut self) -> Result<(), ReadError> {
        let baselines = std::mem::take(&mut self.size_baselines);
        self.in_block = false;
        for (a, (size, count)) in baselines.iter() {
            if self.account(a)?.is_some_and(|v| {
                v.nonce == 0
                    && v.balance == Word::ZERO
                    && v.code.is_empty()
                    && v.storage_size == Word::ZERO
            }) {
                self.remove(a)?;
                continue;
            }
            if let Some(v) = self.account_mut(a)? {
                let slots = v.storage_len();
                let next = if slots >= *count {
                    let n = size.wrapping_add(&Word::from_u64(slots - count));
                    if n < *size {
                        return Err("storage size overflow".into());
                    }
                    n
                } else {
                    let n = Word::from_u64(count - slots);
                    if n > *size {
                        return Err("storage size underflow".into());
                    }
                    size.wrapping_sub(&n)
                };
                v.storage_size = next;
            }
        }
        Ok(())
    }

    pub fn lockups(
        &self,
    ) -> Result<Cow<'_, BTreeMap<crate::lockup::Key, crate::lockup::Lockup>>, ReadError> {
        self.lockups.materialize()
    }
    pub fn lockup(
        &self,
        key: &crate::lockup::Key,
    ) -> Result<Option<crate::lockup::Lockup>, ReadError> {
        self.lockups.get(key)
    }
    pub fn lockups_is_empty(&self) -> Result<bool, ReadError> {
        self.lockups.is_empty()
    }
    pub fn lazy_lockups(&self) -> bool {
        self.lockups.is_lazy()
    }
    pub fn loaded_lockups(&self) -> usize {
        self.lockups.loaded_entries()
    }
    pub(crate) fn lockup_state(&self) -> &crate::lockup_state::LockupState {
        &self.lockups
    }
    pub(crate) fn set_lockup_reader(
        &mut self,
        root: Hash,
        reader: Arc<dyn crate::lockup_state::Reader>,
    ) {
        self.lockups = Arc::new(crate::lockup_state::LockupState::from_reader(root, reader));
    }
    pub fn set_lockup(
        &mut self,
        key: crate::lockup::Key,
        value: Option<crate::lockup::Lockup>,
    ) -> Result<(), ReadError> {
        Arc::make_mut(&mut self.lockups).set(key, value)
    }
    pub(crate) fn replace_lockups(&mut self, other: &World) {
        self.lockups = other.lockups.clone();
    }

    fn checked_totals(&self) -> Result<Totals, ReadError> {
        // A lazy overlay has no count or byte cap (see the module documentation).
        // `insert`, `Journal::set_code` and `consensus_rlp` check the code size.
        if self.base.is_some() {
            return Ok(Totals::default());
        }
        if self.accounts.len() + self.deleted.len() > MAX_ACCOUNTS {
            return Err("account count exceeds snapshot bound".into());
        }
        if let Some(cached) = self.root_cache.get() {
            return Ok(cached.totals);
        }
        let mut total = self.commitment_base.totals;
        for (address, old) in self.dirty_accounts.iter() {
            if let Some(old) = old {
                total.bytes = total
                    .bytes
                    .checked_sub(old.bytes()?)
                    .ok_or("snapshot size accounting")?;
                total.slots = total
                    .slots
                    .checked_sub(old.slots)
                    .ok_or("slot count accounting")?;
            }
            if let Some(current) = self.accounts.get(address) {
                let next = Footprint::of(current);
                if next.code > 32768 || next.slots > MAX_STORAGE_SLOTS {
                    return Err("account exceeds snapshot bound".into());
                }
                total.bytes = total
                    .bytes
                    .checked_add(next.bytes()?)
                    .ok_or("snapshot size overflow")?;
                total.slots = total
                    .slots
                    .checked_add(next.slots)
                    .ok_or("slot count overflow")?;
            }
        }
        if total.bytes > 32 << 20 || total.slots > 65_536 {
            return Err("aggregate account snapshot bound exceeded".into());
        }
        Ok(total)
    }
    pub fn validate(&self) -> Result<(), ReadError> {
        self.checked_totals().map(|_| ())
    }
    pub(crate) fn from_reader(reader: Arc<dyn AccountReader>, root: Hash, count: u64) -> Self {
        Self {
            base: Some(reader),
            account_count: count,
            commitment_base: Arc::new(Commitment {
                trie: Trie::from_root(root),
                totals: Totals::default(),
            }),
            ..Default::default()
        }
    }
    pub fn is_lazy(&self) -> bool {
        self.base.is_some()
    }
    pub fn loaded_accounts(&self) -> usize {
        self.base
            .as_ref()
            .map_or(self.accounts.len(), |r| r.loaded())
    }
    pub fn is_empty(&self) -> Result<bool, ReadError> {
        Ok(self.account_count == 0
            && self.root()? == mpt::empty_root()
            && self.lockups.is_empty()?
            && self.etxs.is_pristine()?
            && !self.in_block
            && self.size_baselines.is_empty())
    }
    pub fn account_count(&self) -> u64 {
        self.account_count
    }
    pub fn insert(&mut self, address: Address, account: Account) -> Result<(), ReadError> {
        let exists = self.account(&address)?.is_some();
        let snapshot = self.base.is_none();
        if (snapshot && !exists && self.account_count == MAX_ACCOUNTS as u64)
            || (snapshot && account.storage.len() > MAX_STORAGE_SLOTS)
            || account.code.len() > 32768
        {
            return Err("account working bound exceeded".into());
        }
        self.mark_size(address)?;
        self.invalidate_account(address)?;
        if !exists {
            self.account_count = self
                .account_count
                .checked_add(1)
                .ok_or("account count overflow")?;
        }
        Arc::make_mut(&mut self.deleted).remove(&address);
        Arc::make_mut(&mut self.accounts).insert(address, Arc::new(account));
        Ok(())
    }
    pub fn account(&self, address: &Address) -> Result<Option<Arc<Account>>, ReadError> {
        if self.deleted.contains(address) {
            return Ok(None);
        }
        if let Some(account) = self.accounts.get(address) {
            return Ok(Some(account.clone()));
        }
        if let Some(reader) = &self.base {
            reader.account(address)
        } else {
            Ok(None)
        }
    }
    pub(crate) fn account_mut(
        &mut self,
        address: &Address,
    ) -> Result<Option<&mut Account>, ReadError> {
        let Some(account) = self.account(address)? else {
            return Ok(None);
        };
        self.mark_size(*address)?;
        self.invalidate_account(*address)?;
        let entry = Arc::make_mut(&mut self.accounts)
            .entry(*address)
            .or_insert(account);
        Ok(Some(Arc::make_mut(entry)))
    }
    pub(crate) fn remove(&mut self, address: &Address) -> Result<Option<Account>, ReadError> {
        let Some(account) = self.account(address)? else {
            return Ok(None);
        };
        self.invalidate_account(*address)?;
        self.account_count = self
            .account_count
            .checked_sub(1)
            .ok_or("account count underflow")?;
        Arc::make_mut(&mut self.accounts).remove(address);
        if self.base.is_some() {
            Arc::make_mut(&mut self.deleted).insert(*address);
        }
        Ok(Some(
            Arc::try_unwrap(account).unwrap_or_else(|v| (*v).clone()),
        ))
    }
    /// Explicit bounded export; native execution and persistence use touched keys.
    pub fn accounts(&self) -> Result<Cow<'_, BTreeMap<Address, Arc<Account>>>, ReadError> {
        let Some(reader) = &self.base else {
            return Ok(Cow::Borrowed(&self.accounts));
        };
        if self.account_count > MAX_ACCOUNTS as u64 {
            return Err("account export bound".into());
        }
        let mut rows = reader.accounts(MAX_ACCOUNTS, 32 << 20)?;
        for address in self.deleted.iter() {
            rows.remove(address);
        }
        for (address, account) in self.accounts.iter() {
            rows.insert(*address, account.clone());
        }
        if rows.len() as u64 != self.account_count {
            return Err(ReadError::Unavailable("account export count".into()));
        }
        Ok(Cow::Owned(rows))
    }
    /// Bounded legacy/diagnostic image, never needed for native execution.
    pub fn materialize(&self) -> Result<Self, ReadError> {
        let mut world = Self::default();
        let mut export_bytes = 0u64;
        let mut export_slots = 0u64;
        for (address, account) in self.accounts()?.iter() {
            let slots = account.storage_len();
            export_slots = export_slots
                .checked_add(slots)
                .ok_or("account export slot count")?;
            export_bytes = export_bytes
                .checked_add(slots.checked_mul(64).ok_or("account export slot bytes")?)
                .and_then(|n| n.checked_add(account.code.len() as u64 + 128))
                .ok_or("account export bytes")?;
            if slots > MAX_STORAGE_SLOTS as u64 || export_slots > 65_536 || export_bytes > 32 << 20
            {
                return Err("account export working bound".into());
            }
            let mut account = (**account).clone();
            let storage = account.materialize_storage()?.into_owned();
            account.storage = storage;
            account.backing = None;
            world.insert(*address, account)?;
        }
        world.lockups = Arc::new(self.lockups.memory_image()?);
        world.etxs = Arc::new(self.etxs.materialize()?);
        world.in_block = self.in_block;
        world.size_baselines = self.size_baselines.clone();
        world.validate()?;
        Ok(world)
    }
    pub(crate) fn changed_accounts(&self, other: &Self) -> Result<BTreeSet<Address>, ReadError> {
        match (&self.base, &other.base) {
            (None, None) => Ok(self
                .accounts
                .keys()
                .chain(other.accounts.keys())
                .copied()
                .collect()),
            (Some(a), Some(b)) if a.identity() == b.identity() => Ok(self
                .accounts
                .keys()
                .chain(other.accounts.keys())
                .chain(self.deleted.iter())
                .chain(other.deleted.iter())
                .copied()
                .collect()),
            _ => Err("unrelated account bases".into()),
        }
    }
    pub fn root(&self) -> Result<Hash, ReadError> {
        if let Some(cached) = self.root_cache.get() {
            return Ok(cached.trie.root());
        }
        let totals = self.checked_totals()?;
        let trie = if self.base.is_none() && self.commitment_base.trie.is_empty() {
            let pairs = self
                .accounts
                .iter()
                .map(|(address, account)| Ok((address.to_vec(), account.consensus_rlp()?)))
                .collect::<Result<Vec<_>, ReadError>>()?;
            quai_primitives::mpt::Trie::from_secure_pairs(&pairs)
                .map_err(|e| trie_error(e, false))?
        } else {
            let mut trie = self.commitment_base.trie.clone();
            for address in self.dirty_accounts.keys() {
                let value = self
                    .accounts
                    .get(address)
                    .map(|a| a.consensus_rlp())
                    .transpose()?
                    .unwrap_or_default();
                if let Some(reader) = &self.base {
                    trie.update(reader.as_ref(), address, &value)
                        .map_err(|e| trie_error(e, true))?;
                } else {
                    trie.update(&mpt::NoNodes, address, &value)
                        .map_err(|e| trie_error(e, false))?;
                }
            }
            trie
        };
        let root = trie.root();
        let _ = self.root_cache.set(Commitment { trie, totals });
        Ok(root)
    }
    /// go-quai `StateDB.GetProof` and `GetStorageProof` (go-ethereum
    /// `Trie.Prove`): the trie nodes on the path to `address`, root first,
    /// and for an existing account its storage root and the nodes on the
    /// path to each key. None for the storage of a missing account
    /// (`StorageTrie` is nil).
    pub fn proof(&self, address: &Address, keys: &[Hash]) -> Result<WorldProof, ReadError> {
        self.root()?;
        let trie = &self
            .root_cache
            .get()
            .ok_or_else(|| ReadError::Unavailable("state commitment".into()))?
            .trie;
        let account = match &self.base {
            Some(reader) => trie
                .prove(reader.as_ref(), address)
                .map_err(|e| trie_error(e, true))?,
            None => trie
                .prove(&mpt::NoNodes, address)
                .map_err(|e| trie_error(e, false))?,
        };
        let storage = match self.account(address)? {
            None => None,
            Some(a) => {
                let storage = a.storage_trie()?;
                let proofs = keys
                    .iter()
                    .map(|key| {
                        let proof = match &a.backing {
                            Some(base) => storage
                                .prove(base.reader.as_ref(), key)
                                .map_err(|e| trie_error(e, true))?,
                            None => storage
                                .prove(&mpt::NoNodes, key)
                                .map_err(|e| trie_error(e, false))?,
                        };
                        Ok((a.get(*key)?, proof))
                    })
                    .collect::<Result<Vec<_>, ReadError>>()?;
                Some(StorageProof {
                    root: storage.root(),
                    code_hash: keccak(&a.code),
                    keys: proofs,
                })
            }
        };
        Ok(WorldProof { account, storage })
    }
}
#[cfg(test)]
mod cached_commitment_tests {
    use super::*;
    use crate::evm::bytes;
    use quai_primitives::trie;
    fn slow(world: &World) -> Hash {
        trie::secure_root(
            &world
                .accounts
                .iter()
                .map(|(a, v)| (a.to_vec(), v.consensus_rlp().unwrap()))
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }
    #[test]
    fn unchanged_snapshots_share_derived_commitments_and_writes_detach_them() {
        let mut world = World::default();
        world
            .insert(
                [1; 20],
                Account {
                    nonce: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        let mut fork = world.clone();
        assert!(Arc::ptr_eq(&world.root_cache, &fork.root_cache));
        assert!(world.root_cache.get().is_none());
        let parent_root = fork.root().unwrap();
        assert!(world.root_cache.get().is_some());
        assert_eq!(world.root().unwrap(), parent_root);
        fork.account_mut(&[1; 20]).unwrap().unwrap().balance = Word::ONE;
        assert!(!Arc::ptr_eq(&world.root_cache, &fork.root_cache));
        assert_eq!(world.root().unwrap(), parent_root);
        assert_eq!(fork.root().unwrap(), slow(&fork));
        assert_ne!(fork.root().unwrap(), parent_root);
        world.account_mut(&[1; 20]).unwrap().unwrap().nonce = 2;
        assert_eq!(world.root().unwrap(), slow(&world));
        assert_ne!(world.root().unwrap(), fork.root().unwrap());
    }
    #[test]
    fn incremental_validation_preserves_aggregate_slot_limits_after_failed_roots() {
        let mut world = World::default();
        let storage = (0..4096u64)
            .map(|i| (bytes(Word::from_u64(i)), bytes(Word::from_u64(i + 1))))
            .collect::<BTreeMap<_, _>>();
        for i in 1..=16u8 {
            world
                .insert(
                    [i; 20],
                    Account {
                        nonce: 1,
                        storage: storage.clone(),
                        storage_size: Word::from_u64(4096),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        world
            .insert(
                [17; 20],
                Account {
                    nonce: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        let before = world.root().unwrap();
        let pinned = world.clone();
        world
            .account_mut(&[17; 20])
            .unwrap()
            .unwrap()
            .put([1; 32], [1; 32])
            .unwrap();
        assert!(world.validate().is_err());
        assert!(world.root().is_err());
        world
            .account_mut(&[17; 20])
            .unwrap()
            .unwrap()
            .put([1; 32], [0; 32])
            .unwrap();
        assert_eq!(world.root().unwrap(), before);
        world.remove(&[1; 20]).unwrap();
        world
            .account_mut(&[17; 20])
            .unwrap()
            .unwrap()
            .put([1; 32], [1; 32])
            .unwrap();
        assert_eq!(world.root().unwrap(), slow(&world));
        assert_eq!(pinned.root().unwrap(), before);
    }
    #[test]
    fn roots_survive_shared_snapshots_storage_size_finalization_and_replacement() {
        let mut world = World::default();
        world
            .insert(
                [1; 20],
                Account {
                    nonce: 1,
                    balance: Word::ONE,
                    ..Default::default()
                },
            )
            .unwrap();
        let initial = world.root().unwrap();
        assert_eq!(initial, slow(&world));
        let parent = world.clone();
        let mut other = world.clone();
        other.account_mut(&[1; 20]).unwrap().unwrap().balance = Word::from_u64(2);
        assert_ne!(other.root().unwrap(), initial);
        assert_eq!(other.root().unwrap(), slow(&other));
        assert_eq!(parent.root().unwrap(), initial);
        other.begin_block();
        other
            .account_mut(&[1; 20])
            .unwrap()
            .unwrap()
            .put([2; 32], [3; 32])
            .unwrap();
        let pending = other.root().unwrap();
        other.finish_block().unwrap();
        assert_ne!(other.root().unwrap(), pending);
        assert_eq!(other.root().unwrap(), slow(&other));
        assert_eq!(parent.root().unwrap(), initial);
        other.remove(&[1; 20]).unwrap();
        assert_eq!(other.root().unwrap(), slow(&other));
        other
            .insert(
                [1; 20],
                Account {
                    nonce: 1,
                    balance: Word::ONE,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(other.root().unwrap(), initial);
        assert_eq!(other.root().unwrap(), slow(&other));
        // Cache population and invalidation cannot change semantic equality.
        assert_eq!(world, parent);
        world.account_mut(&[1; 20]).unwrap().unwrap().nonce = 1;
        assert_eq!(world, parent);
        assert_eq!(world.root().unwrap(), initial);
    }
}
