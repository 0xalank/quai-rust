//! Transaction state with nested revisions. Original storage survives frame reverts;
//! balances, code, storage, refunds, transient storage and logs follow revisions.
use super::{
    state::{Account, World},
    word::Word,
    CapabilityError,
};
use quai_primitives::{
    receipt::Log,
    transaction::{AccessTuple, Address, Transaction},
    Hash,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicU64, Ordering},
};
static OWNERS: AtomicU64 = AtomicU64::new(1);

// The undo log has no length cap. Go's state journal (core/state/journal.go)
// grows with every change too; gas bounds the number of changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Revision {
    owner: u64,
    serial: u64,
}
enum Undo {
    Account(Address, Option<Account>),
    Storage(Address, Hash, Hash),
    Balance(Address, Word),
    Nonce(Address, u64),
    Code(Address, Vec<u8>),
    Size(Address, Word),
    Transient(Address, Hash, Option<Hash>),
    Suicided(Address, bool),
    Touch(Address, bool),
    Reinitialize(
        Address,
        Option<(Word, u64)>,
        Vec<(Hash, Hash)>,
        Option<(Word, u64)>,
    ),
}
struct Checkpoint {
    id: Revision,
    undo: usize,
    logs: usize,
    etxs: usize,
    refund: u64,
    deleted: std::sync::Arc<BTreeMap<crate::lockup::Key, crate::lockup::Lockup>>,
    deleted_hashes: usize,
}
pub struct Journal {
    pub(crate) deleted: std::sync::Arc<BTreeMap<crate::lockup::Key, crate::lockup::Lockup>>,
    pub(crate) deleted_hashes: Vec<Hash>,
    world: World,
    owner: u64,
    undo: Vec<Undo>,
    revisions: Vec<Checkpoint>,
    next_revision: u64,
    original: BTreeMap<(Address, Hash), Hash>,
    pub(crate) allow_calls: bool,
    pub(crate) etxs: Vec<Transaction>,
    pub(crate) transaction_hash: Option<Hash>,
    pub(crate) eligible_slices: Option<Hash>,
    pub(crate) access_list: Vec<AccessTuple>,
    pub(crate) refund: u64,
    pub(crate) logs: Vec<Log>,
    pub(crate) trace_hash: Hash,
    pub(crate) trace_steps: usize,
    pub(crate) trace_bytes: usize,
    initial_sizes: BTreeMap<Address, (Word, u64)>,
    transient: BTreeMap<(Address, Hash), Hash>,
    suicided: BTreeSet<Address>,
    touched: BTreeSet<Address>,
    sticky_touched: BTreeSet<Address>,
    pub(crate) delete_empty: bool,
    pub(crate) prime_number: Option<u64>,
    pub(crate) block_hashes: BTreeMap<u64, Hash>,
    pub(crate) addresses: BTreeSet<Address>,
    pub(crate) slots: BTreeSet<(Address, Hash)>,
}
impl Journal {
    pub fn new(world: World) -> Result<Self, CapabilityError> {
        world.validate().map_err(|error| {
            CapabilityError::state(error, CapabilityError::Limit("world snapshot"))
        })?;
        let owner = OWNERS
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| CapabilityError::Limit("journal identity"))?;
        let initial_sizes = BTreeMap::new();
        Ok(Self {
            owner,
            deleted: Default::default(),
            deleted_hashes: vec![],
            allow_calls: false,
            etxs: vec![],
            transaction_hash: None,
            eligible_slices: None,
            access_list: vec![],
            world,
            suicided: BTreeSet::new(),
            touched: BTreeSet::new(),
            sticky_touched: BTreeSet::new(),
            delete_empty: false,
            block_hashes: BTreeMap::new(),
            prime_number: None,
            initial_sizes,
            trace_hash: [0; 32],
            trace_steps: 0,
            trace_bytes: 0,
            undo: vec![],
            revisions: vec![],
            next_revision: 0,
            original: BTreeMap::new(),
            refund: 0,
            logs: vec![],
            transient: BTreeMap::new(),
            addresses: BTreeSet::new(),
            slots: BTreeSet::new(),
        })
    }
    pub fn world(&self) -> &World {
        &self.world
    }
    pub fn into_world(self) -> Result<World, CapabilityError> {
        if !self.revisions.is_empty() {
            return Err(CapabilityError::Context("unclosed state revisions"));
        }
        self.world.validate().map_err(|error| {
            CapabilityError::state(error, CapabilityError::Limit("world snapshot"))
        })?;
        Ok(self.world)
    }
    pub(crate) fn delete_lockup(
        &mut self,
        key: crate::lockup::Key,
        value: crate::lockup::Lockup,
    ) -> Result<(), CapabilityError> {
        self.world.set_lockup(key, None)?;
        self.deleted_hashes.push(value.commitment(key));
        std::sync::Arc::make_mut(&mut self.deleted).insert(key, value);
        Ok(())
    }
    pub fn snapshot(&mut self) -> Result<Revision, CapabilityError> {
        let id = Revision {
            owner: self.owner,
            serial: self.next_revision,
        };
        self.next_revision = self
            .next_revision
            .checked_add(1)
            .ok_or(CapabilityError::Limit("revision counter"))?;
        self.revisions.push(Checkpoint {
            id,
            undo: self.undo.len(),
            logs: self.logs.len(),
            etxs: self.etxs.len(),
            refund: self.refund,
            deleted: self.deleted.clone(),
            deleted_hashes: self.deleted_hashes.len(),
        });
        Ok(id)
    }
    pub fn commit(&mut self, id: Revision) -> Result<(), CapabilityError> {
        if self.revisions.last().map(|r| r.id) != Some(id) {
            return Err(CapabilityError::Context("revision commit order"));
        }
        self.revisions.pop();
        if self.revisions.is_empty() {
            self.undo.clear();
        }
        Ok(())
    }
    pub fn revert(&mut self, id: Revision) -> Result<(), CapabilityError> {
        let index = self
            .revisions
            .iter()
            .position(|r| r.id == id)
            .ok_or(CapabilityError::Context("stale revision"))?;
        let checkpoint = &self.revisions[index];
        let offset = checkpoint.undo;
        let logs = checkpoint.logs;
        let etxs = checkpoint.etxs;
        let refund = checkpoint.refund;
        let deleted = checkpoint.deleted.clone();
        let deleted_hashes = checkpoint.deleted_hashes;
        if self.prime_number.is_some_and(|p| p >= 2_317_000) {
            for (k, v) in self.deleted.iter() {
                if !deleted.contains_key(k) {
                    self.world.set_lockup(*k, Some(v.clone()))?;
                }
            }
        }
        while self.undo.len() > offset {
            match self.undo.pop().unwrap() {
                Undo::Reinitialize(a, size, original, baseline) => {
                    self.world.set_size_baseline(a, baseline);
                    if let Some(v) = size {
                        self.initial_sizes.insert(a, v);
                    } else {
                        self.initial_sizes.remove(&a);
                    }
                    self.original.retain(|(address, _), _| *address != a);
                    for (k, v) in original {
                        self.original.insert((a, k), v);
                    }
                }
                Undo::Account(a, Some(v)) => {
                    self.world.insert(a, v).map_err(|error| {
                        CapabilityError::state(error, CapabilityError::Context("account undo"))
                    })?;
                }
                Undo::Account(a, None) => {
                    self.world.remove(&a)?;
                }
                Undo::Storage(a, k, v) => {
                    self.world
                        .account_mut(&a)?
                        .unwrap()
                        .put(k, v)
                        .map_err(|error| {
                            CapabilityError::state(error, CapabilityError::Context("storage undo"))
                        })?;
                }
                Undo::Balance(a, v) => self.world.account_mut(&a)?.unwrap().balance = v,
                Undo::Nonce(a, v) => self.world.account_mut(&a)?.unwrap().nonce = v,
                Undo::Code(a, v) => self.world.account_mut(&a)?.unwrap().code = v,
                Undo::Size(a, v) => self.world.account_mut(&a)?.unwrap().storage_size = v,
                Undo::Suicided(a, true) => {
                    self.suicided.insert(a);
                }
                Undo::Suicided(a, false) => {
                    self.suicided.remove(&a);
                }
                Undo::Touch(a, true) => {
                    self.touched.insert(a);
                }
                Undo::Touch(a, false) => {
                    self.touched.remove(&a);
                }
                Undo::Transient(a, k, Some(v)) => {
                    self.transient.insert((a, k), v);
                }
                Undo::Transient(a, k, None) => {
                    self.transient.remove(&(a, k));
                }
            }
        }
        self.logs.truncate(logs);
        self.etxs.truncate(etxs);
        self.refund = refund;
        self.deleted = deleted;
        self.deleted_hashes.truncate(deleted_hashes);
        self.revisions.truncate(index);
        Ok(())
    }
    fn record(&mut self, change: Undo) -> Result<(), CapabilityError> {
        if self.revisions.is_empty() {
            return Err(CapabilityError::Context("mutation outside revision"));
        }
        self.undo.push(change);
        Ok(())
    }
    pub fn create(&mut self, a: Address) -> Result<(), CapabilityError> {
        self.touch(a)?;
        let previous = self.initial_sizes.get(&a).copied();
        let original = self
            .original
            .iter()
            .filter_map(|((address, k), v)| (*address == a).then_some((*k, *v)))
            .collect();
        self.record(Undo::Reinitialize(
            a,
            previous,
            original,
            self.world.size_baseline(a),
        ))?;
        self.initial_sizes.insert(
            a,
            (
                self.world
                    .account(&a)?
                    .map(|v| v.storage_size)
                    .unwrap_or(Word::ZERO),
                0,
            ),
        );
        self.world.set_size_baseline(
            a,
            Some((
                self.world
                    .account(&a)?
                    .map(|v| v.storage_size)
                    .unwrap_or(Word::ZERO),
                0,
            )),
        );
        self.original.retain(|(address, _), _| *address != a);
        let old = self.world.account(&a)?.map(|v| (*v).clone());
        self.record(Undo::Account(a, old.clone()))?;
        // Go CreateAccount preserves existing balance and storage size while replacing storage/code.
        let v = Account {
            balance: old.as_ref().map(|v| v.balance).unwrap_or(Word::ZERO),
            storage_size: old.as_ref().map(|v| v.storage_size).unwrap_or(Word::ZERO),
            ..Default::default()
        };
        self.world.insert(a, v).map_err(|error| {
            CapabilityError::state(error, CapabilityError::Limit("account creation"))
        })
    }
    pub fn set_balance(&mut self, a: Address, v: Word) -> Result<(), CapabilityError> {
        self.touch(a)?;
        if self.world.account(&a)?.is_none() {
            self.create(a)?;
        }
        let old = self.world.account(&a)?.unwrap().balance;
        self.record(Undo::Balance(a, old))?;
        self.world.account_mut(&a)?.unwrap().balance = v;
        Ok(())
    }
    pub fn transfer(
        &mut self,
        from: Address,
        to: Address,
        value: Word,
    ) -> Result<bool, CapabilityError> {
        let balance = self
            .world
            .account(&from)?
            .map(|a| a.balance)
            .unwrap_or(Word::ZERO);
        if balance < value {
            return Ok(false);
        }
        if value == Word::ZERO {
            if self.world.account(&to)?.is_some_and(|v| {
                v.nonce == 0
                    && v.balance == Word::ZERO
                    && v.code.is_empty()
                    && v.storage_size == Word::ZERO
            }) {
                self.touch_empty_balance(to)?;
            }
            return Ok(true);
        }
        if from == to {
            self.touch(from)?;
            return Ok(true);
        }
        let before = self
            .world
            .account(&to)?
            .map(|a| a.balance)
            .unwrap_or(Word::ZERO);
        let after = before.wrapping_add(&value);
        if after < before {
            return Err(CapabilityError::Context("balance overflow"));
        }
        self.set_balance(from, balance.wrapping_sub(&value))?;
        self.set_balance(to, after)?;
        Ok(true)
    }
    pub(crate) fn has_suicided(&self, a: Address) -> bool {
        self.suicided.contains(&a)
    }
    pub(crate) fn suicide(&mut self, a: Address) -> Result<(), CapabilityError> {
        let old = self.suicided.contains(&a);
        self.record(Undo::Suicided(a, old))?;
        self.suicided.insert(a);
        self.set_balance(a, Word::ZERO)
    }
    pub(crate) fn finalize_deletions(&mut self) -> Result<(), CapabilityError> {
        for a in self.suicided.clone() {
            let old = self.world.account(&a)?.map(|v| (*v).clone());
            self.record(Undo::Account(a, old))?;
            self.world.remove(&a)?;
        }
        Ok(())
    }
    pub fn set_nonce(&mut self, a: Address, v: u64) -> Result<(), CapabilityError> {
        self.touch(a)?;
        if self.world.account(&a)?.is_none() {
            self.create(a)?;
        }
        let old = self.world.account(&a)?.unwrap().nonce;
        self.record(Undo::Nonce(a, old))?;
        self.world.account_mut(&a)?.unwrap().nonce = v;
        Ok(())
    }
    pub fn set_code(&mut self, a: Address, v: Vec<u8>) -> Result<(), CapabilityError> {
        self.touch(a)?;
        if v.len() > 32768 {
            return Err(CapabilityError::Limit("code"));
        }
        if self.world.account(&a)?.is_none() {
            self.create(a)?;
        }
        let old = self.world.account(&a)?.unwrap().code.clone();
        self.record(Undo::Code(a, old))?;
        self.world.account_mut(&a)?.unwrap().code = v;
        Ok(())
    }
    pub fn original(&mut self, a: Address, k: Hash) -> Result<Hash, CapabilityError> {
        if let Some(value) = self.original.get(&(a, k)) {
            return Ok(*value);
        }
        let value = self
            .world
            .account(&a)?
            .map(|v| v.get(k))
            .transpose()?
            .unwrap_or([0; 32]);
        self.original.insert((a, k), value);
        Ok(value)
    }
    pub fn set_storage(&mut self, a: Address, k: Hash, v: Hash) -> Result<(), CapabilityError> {
        self.original(a, k)?;
        let old = self
            .world
            .account(&a)?
            .ok_or(CapabilityError::Context("storage account"))?
            .get(k)?;
        if old == v {
            return Ok(());
        }
        self.touch(a)?;
        self.record(Undo::Storage(a, k, old))?;
        self.world
            .account_mut(&a)?
            .unwrap()
            .put(k, v)
            .map_err(|error| CapabilityError::state(error, CapabilityError::Limit("storage slots")))
    }
    pub fn set_size(&mut self, a: Address, v: Word) -> Result<(), CapabilityError> {
        self.touch(a)?;
        let old = self
            .world
            .account(&a)?
            .ok_or(CapabilityError::Context("storage account"))?
            .storage_size;
        self.record(Undo::Size(a, old))?;
        self.world.account_mut(&a)?.unwrap().storage_size = v;
        Ok(())
    }
    pub(crate) fn finalize_sizes(&mut self) -> Result<(), CapabilityError> {
        self.finalize_deletions()?;
        if self.delete_empty {
            let dirty = self
                .touched
                .union(&self.sticky_touched)
                .copied()
                .collect::<Vec<_>>();
            for a in dirty {
                if self.world.account(&a)?.is_some_and(|v| {
                    v.nonce == 0
                        && v.balance == Word::ZERO
                        && v.code.is_empty()
                        && v.storage_size == Word::ZERO
                }) {
                    let old = self.world.account(&a)?.map(|v| (*v).clone());
                    self.record(Undo::Account(a, old))?;
                    self.world.remove(&a)?;
                }
            }
        }
        if self.world.in_block() {
            return Ok(());
        }
        let changes = self
            .initial_sizes
            .iter()
            .map(|(a, (size, count))| {
                let Some(account) = self.world.account(a)? else {
                    return Ok(None);
                };
                let slots = account.storage_len();
                let next = if slots >= *count {
                    let n = size.wrapping_add(&Word::from_u64(slots - count));
                    if n < *size {
                        return Err(CapabilityError::Context("storage size overflow"));
                    }
                    n
                } else {
                    let delta = Word::from_u64(count - slots);
                    if *size < delta {
                        return Err(CapabilityError::Context("storage size underflow"));
                    }
                    size.wrapping_sub(&delta)
                };
                Ok(Some((*a, next)))
            })
            .collect::<Result<Vec<_>, CapabilityError>>()?;
        let changes = changes.into_iter().flatten();
        for (a, v) in changes {
            if self.world.account(&a)?.unwrap().storage_size != v {
                self.set_size(a, v)?;
            }
        }
        Ok(())
    }
    fn touch(&mut self, a: Address) -> Result<(), CapabilityError> {
        if !self.initial_sizes.contains_key(&a) {
            let baseline = self
                .world
                .account(&a)?
                .map(|v| (v.storage_size, v.storage_len()))
                .unwrap_or((Word::ZERO, 0));
            self.initial_sizes.insert(a, baseline);
        }
        let previous = self.touched.contains(&a);
        self.record(Undo::Touch(a, previous))?;
        self.touched.insert(a);
        Ok(())
    }
    pub(crate) fn touch_empty_balance(&mut self, a: Address) -> Result<(), CapabilityError> {
        if self.world.account(&a)?.is_none() {
            self.create(a)?;
        }
        if self.world.account(&a)?.is_some_and(|v| {
            v.nonce == 0
                && v.balance == Word::ZERO
                && v.code.is_empty()
                && v.storage_size == Word::ZERO
        }) {
            self.touch(a)?;
            if a[..19].iter().all(|b| *b == 0) && a[19] == 3 {
                self.sticky_touched.insert(a);
            }
        }
        Ok(())
    }
    pub fn transient(&self, a: Address, k: Hash) -> Hash {
        self.transient.get(&(a, k)).copied().unwrap_or([0; 32])
    }
    pub fn set_transient(&mut self, a: Address, k: Hash, v: Hash) -> Result<(), CapabilityError> {
        let old = self.transient.get(&(a, k)).copied();
        self.record(Undo::Transient(a, k, old))?;
        if v == [0; 32] {
            self.transient.remove(&(a, k));
        } else {
            self.transient.insert((a, k), v);
        }
        Ok(())
    }
}
