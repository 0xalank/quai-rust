//! go-quai `core/tx_list.go`: the nonce-sorted transaction list of an
//! account (`txList`), the price-sorted heaps of remote transactions
//! (`txPricedList`, with `container/heap`'s algorithms so that ties break
//! as in Go), and the pool's lookup (`txLookup`, core/tx_pool.go). Also a
//! port of hashicorp golang-lru v2.0.5 `simplelru.LRU`, which the Qi pool,
//! the fee cache and the nonce cache use.
use super::big;
use super::tx::PoolTx;
use quai_primitives::Hash;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub type Tx = Arc<PoolTx>;

/// `txSortedMap`: transactions of one account by nonce. Go keeps a heap of
/// nonces beside a map; every observable result (Forward, Ready and Cap
/// order, Flatten) is the nonce order this map gives.
#[derive(Clone, Default)]
pub struct SortedMap {
    items: BTreeMap<u64, Tx>,
}
impl SortedMap {
    pub fn get(&self, nonce: u64) -> Option<&Tx> {
        self.items.get(&nonce)
    }
    pub fn put(&mut self, tx: Tx) {
        self.items.insert(tx.nonce(), tx);
    }
    /// Remove the transactions below `threshold`, in nonce order.
    pub fn forward(&mut self, threshold: u64) -> Vec<Tx> {
        let keep = self.items.split_off(&threshold);
        std::mem::replace(&mut self.items, keep)
            .into_values()
            .collect()
    }
    /// Remove the transactions for which `f` holds.
    pub fn filter(&mut self, f: impl Fn(&Tx) -> bool) -> Vec<Tx> {
        let drop: Vec<u64> = self
            .items
            .iter()
            .filter(|(_, t)| f(t))
            .map(|(n, _)| *n)
            .collect();
        drop.iter().filter_map(|n| self.items.remove(n)).collect()
    }
    /// Keep the `threshold` lowest nonces; the others, highest first.
    pub fn cap(&mut self, threshold: usize) -> Vec<Tx> {
        let mut drops = vec![];
        while self.items.len() > threshold {
            if let Some((_, tx)) = self.items.pop_last() {
                drops.push(tx);
            }
        }
        drops
    }
    pub fn remove(&mut self, nonce: u64) -> bool {
        self.items.remove(&nonce).is_some()
    }
    /// The consecutive run from the lowest nonce, if it is at most `start`.
    pub fn ready(&mut self, start: u64) -> Vec<Tx> {
        let Some((&first, _)) = self.items.first_key_value() else {
            return vec![];
        };
        if first > start {
            return vec![];
        }
        let mut ready = vec![];
        let mut next = first;
        while let Some(tx) = self.items.remove(&next) {
            ready.push(tx);
            next += 1;
        }
        ready
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn flatten(&self) -> Vec<Tx> {
        self.items.values().cloned().collect()
    }
    pub fn last(&self) -> Option<&Tx> {
        self.items.values().next_back()
    }
}

/// `txList`.
#[derive(Clone)]
pub struct TxList {
    pub strict: bool,
    pub txs: SortedMap,
    costcap: Vec<u8>,
    gascap: u64,
}
impl TxList {
    pub fn new(strict: bool) -> Self {
        Self {
            strict,
            txs: SortedMap::default(),
            costcap: vec![],
            gascap: 0,
        }
    }
    pub fn overlaps(&self, tx: &PoolTx) -> bool {
        self.txs.get(tx.nonce()).is_some()
    }
    /// Insert, or replace a transaction of the same nonce that is at least
    /// `price_bump` percent cheaper. Returns (inserted, replaced).
    pub fn add(&mut self, tx: Tx, price_bump: u64) -> (bool, Option<Tx>) {
        let old = self.txs.get(tx.nonce()).cloned();
        if let Some(old) = &old {
            if big::cmp(old.gas_price(), tx.gas_price()).is_ge() {
                return (false, None);
            }
            let threshold =
                big::divmod_u32(&big::mul_u64(old.gas_price(), 100 + price_bump), 100).0;
            if big::cmp(tx.gas_price(), &threshold).is_lt() {
                return (false, None);
            }
        }
        let cost = tx.cost();
        if big::cmp(&self.costcap, &cost).is_lt() {
            self.costcap = cost;
        }
        if self.gascap < tx.gas() {
            self.gascap = tx.gas();
        }
        self.txs.put(tx);
        (true, old)
    }
    pub fn forward(&mut self, threshold: u64) -> Vec<Tx> {
        self.txs.forward(threshold)
    }
    /// Remove what costs more than `cost_limit` or uses more gas than
    /// `gas_limit`; in a strict list also every later nonce (returned
    /// second).
    pub fn filter(&mut self, cost_limit: &[u8], gas_limit: u64) -> (Vec<Tx>, Vec<Tx>) {
        if big::cmp(&self.costcap, cost_limit).is_le() && self.gascap <= gas_limit {
            return (vec![], vec![]);
        }
        self.costcap = big::trim(cost_limit);
        self.gascap = gas_limit;
        let removed = self
            .txs
            .filter(|tx| tx.gas() > gas_limit || big::cmp(&tx.cost(), cost_limit).is_gt());
        if removed.is_empty() {
            return (vec![], vec![]);
        }
        let mut invalids = vec![];
        if self.strict {
            let lowest = removed.iter().map(|t| t.nonce()).min().unwrap_or(u64::MAX);
            invalids = self.txs.filter(|tx| tx.nonce() > lowest);
        }
        (removed, invalids)
    }
    pub fn cap(&mut self, threshold: usize) -> Vec<Tx> {
        self.txs.cap(threshold)
    }
    /// Remove one transaction; in a strict list also the later nonces.
    pub fn remove(&mut self, tx: &PoolTx) -> (bool, Vec<Tx>) {
        let nonce = tx.nonce();
        if !self.txs.remove(nonce) {
            return (false, vec![]);
        }
        if self.strict {
            return (true, self.txs.filter(|t| t.nonce() > nonce));
        }
        (true, vec![])
    }
    pub fn ready(&mut self, start: u64) -> Vec<Tx> {
        self.txs.ready(start)
    }
    pub fn len(&self) -> usize {
        self.txs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.txs.is_empty()
    }
    pub fn flatten(&self) -> Vec<Tx> {
        self.txs.flatten()
    }
}

/// `txLookup`: every Quai transaction of the pool, split into locals and
/// remotes, with the slot count.
#[derive(Default)]
pub struct Lookup {
    pub slots: usize,
    pub locals: HashMap<Hash, Tx>,
    pub remotes: HashMap<Hash, Tx>,
    /// Insertion order of the remotes, for `Reheap` (Go iterates its map in
    /// random order; equal prices then compare by this order).
    order: BTreeMap<u64, Hash>,
    seq: HashMap<Hash, u64>,
    next: u64,
}
pub const TX_SLOT_SIZE: u64 = 32 * 1024;
pub fn num_slots(tx: &PoolTx) -> usize {
    tx.size.div_ceil(TX_SLOT_SIZE) as usize
}
impl Lookup {
    pub fn get(&self, hash: &Hash) -> Option<&Tx> {
        self.locals.get(hash).or_else(|| self.remotes.get(hash))
    }
    pub fn get_remote(&self, hash: &Hash) -> Option<&Tx> {
        self.remotes.get(hash)
    }
    pub fn count(&self) -> usize {
        self.locals.len() + self.remotes.len()
    }
    pub fn add(&mut self, tx: Tx, local: bool) {
        self.slots += num_slots(&tx);
        let hash = tx.hash;
        if local {
            self.locals.insert(hash, tx);
        } else {
            self.remotes.insert(hash, tx);
            self.next += 1;
            if let Some(old) = self.seq.insert(hash, self.next) {
                self.order.remove(&old);
            }
            self.order.insert(self.next, hash);
        }
    }
    pub fn remove(&mut self, hash: &Hash) {
        let tx = match self.locals.remove(hash) {
            Some(t) => t,
            None => match self.remotes.remove(hash) {
                Some(t) => t,
                None => return,
            },
        };
        if let Some(s) = self.seq.remove(hash) {
            self.order.remove(&s);
        }
        self.slots -= num_slots(&tx);
    }
    /// Move the remotes of local senders to the locals.
    pub fn remote_to_locals(&mut self, is_local: impl Fn(&PoolTx) -> bool) -> usize {
        let moved: Vec<Hash> = self
            .remotes
            .iter()
            .filter(|(_, t)| is_local(t))
            .map(|(h, _)| *h)
            .collect();
        for h in &moved {
            if let Some(tx) = self.remotes.remove(h) {
                if let Some(s) = self.seq.remove(h) {
                    self.order.remove(&s);
                }
                self.locals.insert(*h, tx);
            }
        }
        moved.len()
    }
    /// Remotes in insertion order.
    pub fn remotes_ordered(&self) -> Vec<Tx> {
        self.order
            .values()
            .filter_map(|h| self.remotes.get(h).cloned())
            .collect()
    }
}

/// `priceHeap` with `container/heap`. `Less` is true for equal prices, as
/// in Go's `cmp`, which returns -1 then.
#[derive(Default, Clone)]
pub struct PriceHeap {
    pub list: Vec<Tx>,
}
fn price_cmp(a: &Tx, b: &Tx) -> std::cmp::Ordering {
    match big::cmp(a.gas_price(), b.gas_price()) {
        std::cmp::Ordering::Equal => std::cmp::Ordering::Less,
        o => o,
    }
}
impl PriceHeap {
    fn less(&self, i: usize, j: usize) -> bool {
        price_cmp(&self.list[i], &self.list[j]).is_lt()
    }
    fn up(&mut self, mut j: usize) {
        loop {
            let i = if j == 0 { 0 } else { (j - 1) / 2 };
            if i == j || !self.less(j, i) {
                break;
            }
            self.list.swap(i, j);
            j = i;
        }
    }
    fn down(&mut self, i0: usize, n: usize) -> bool {
        let mut i = i0;
        loop {
            let j1 = 2 * i + 1;
            if j1 >= n {
                break;
            }
            let mut j = j1;
            let j2 = j1 + 1;
            if j2 < n && self.less(j2, j1) {
                j = j2;
            }
            if !self.less(j, i) {
                break;
            }
            self.list.swap(i, j);
            i = j;
        }
        i > i0
    }
    pub fn init(&mut self) {
        let n = self.list.len();
        for i in (0..n / 2).rev() {
            self.down(i, n);
        }
    }
    pub fn push(&mut self, tx: Tx) {
        self.list.push(tx);
        let n = self.list.len();
        self.up(n - 1);
    }
    pub fn pop(&mut self) -> Option<Tx> {
        let n = self.list.len().checked_sub(1)?;
        self.list.swap(0, n);
        self.down(0, n);
        self.list.pop()
    }
}

/// `txPricedList`: remote transactions in an urgent and a floating heap.
#[derive(Default)]
pub struct PricedList {
    pub urgent: PriceHeap,
    pub floating: PriceHeap,
    pub stales: i64,
}
const URGENT_RATIO: usize = 4;
const FLOATING_RATIO: usize = 1;
impl PricedList {
    pub fn put(&mut self, tx: Tx, local: bool) {
        if !local {
            self.urgent.push(tx);
        }
    }
    pub fn removed(&mut self, count: usize, all: &Lookup) {
        self.stales += count as i64;
        if self.stales <= ((self.urgent.list.len() + self.floating.list.len()) / 4) as i64 {
            return;
        }
        self.reheap(all);
    }
    fn underpriced_for(heap: &mut PriceHeap, stales: &mut i64, all: &Lookup, tx: &Tx) -> bool {
        while let Some(head) = heap.list.first() {
            if all.get_remote(&head.hash).is_none() {
                *stales -= 1;
                heap.pop();
                continue;
            }
            break;
        }
        match heap.list.first() {
            None => false,
            Some(head) => price_cmp(head, tx).is_ge(),
        }
    }
    /// Go evaluates the three terms left to right and stops at the first
    /// false one, so the floating heap is cleaned only when the urgent test
    /// passes.
    pub fn underpriced(&mut self, tx: &Tx, all: &Lookup) -> bool {
        if !(Self::underpriced_for(&mut self.urgent, &mut self.stales, all, tx)
            || self.urgent.list.is_empty())
        {
            return false;
        }
        if !(Self::underpriced_for(&mut self.floating, &mut self.stales, all, tx)
            || self.floating.list.is_empty())
        {
            return false;
        }
        !self.urgent.list.is_empty() || !self.floating.list.is_empty()
    }
    /// Find the cheapest remotes for `slots` slots; `(drops, ok)`.
    pub fn discard(&mut self, mut slots: i64, force: bool, all: &Lookup) -> (Vec<Tx>, bool) {
        let mut drop = vec![];
        while slots > 0 {
            if self.urgent.list.len() * FLOATING_RATIO > self.floating.list.len() * URGENT_RATIO {
                let Some(tx) = self.urgent.pop() else { break };
                if all.get_remote(&tx.hash).is_none() {
                    self.stales -= 1;
                    continue;
                }
                self.floating.push(tx);
            } else {
                if self.floating.list.is_empty() {
                    break;
                }
                let Some(tx) = self.floating.pop() else { break };
                if all.get_remote(&tx.hash).is_none() {
                    self.stales -= 1;
                    continue;
                }
                slots -= num_slots(&tx) as i64;
                drop.push(tx);
            }
        }
        if slots > 0 && !force {
            for tx in drop {
                self.urgent.push(tx);
            }
            return (vec![], false);
        }
        (drop, true)
    }
    pub fn reheap(&mut self, all: &Lookup) {
        self.stales = 0;
        self.urgent.list = all.remotes_ordered();
        self.urgent.init();
        let floating = self.urgent.list.len() * FLOATING_RATIO / (URGENT_RATIO + FLOATING_RATIO);
        self.floating.list = Vec::with_capacity(floating);
        for _ in 0..floating {
            if let Some(tx) = self.urgent.pop() {
                self.floating.list.push(tx);
            }
        }
        self.floating.init();
    }
    pub fn set_base_fee(&mut self, all: &Lookup) {
        self.reheap(all);
    }
}

/// hashicorp golang-lru v2.0.5 `simplelru.LRU`: `get` and `add` move an
/// entry to the front, `peek` does not; when full, `add` drops the oldest.
pub struct Lru<K, V> {
    size: usize,
    items: HashMap<K, (V, u64)>,
    order: BTreeMap<u64, K>,
    next: u64,
}
impl<K: std::hash::Hash + Eq + Clone, V> Lru<K, V> {
    pub fn new(size: usize) -> Self {
        Self {
            size: size.max(1),
            items: HashMap::new(),
            order: BTreeMap::new(),
            next: 0,
        }
    }
    fn touch(&mut self, key: &K) {
        if let Some((_, s)) = self.items.get_mut(key) {
            self.order.remove(s);
            self.next += 1;
            *s = self.next;
            self.order.insert(self.next, key.clone());
        }
    }
    /// Returns true when an entry was evicted.
    pub fn add(&mut self, key: K, value: V) -> bool {
        if let Some(entry) = self.items.get_mut(&key) {
            entry.0 = value;
            self.touch(&key);
            return false;
        }
        self.next += 1;
        self.items.insert(key.clone(), (value, self.next));
        self.order.insert(self.next, key);
        if self.items.len() > self.size {
            self.remove_oldest();
            return true;
        }
        false
    }
    pub fn get(&mut self, key: &K) -> Option<&V> {
        if !self.items.contains_key(key) {
            return None;
        }
        self.touch(key);
        self.items.get(key).map(|(v, _)| v)
    }
    pub fn peek(&self, key: &K) -> Option<&V> {
        self.items.get(key).map(|(v, _)| v)
    }
    pub fn peek_mut(&mut self, key: &K) -> Option<&mut V> {
        self.items.get_mut(key).map(|(v, _)| v)
    }
    pub fn contains(&self, key: &K) -> bool {
        self.items.contains_key(key)
    }
    /// `ContainsOrAdd`: add when absent, without touching a present entry.
    pub fn contains_or_add(&mut self, key: K, value: V) -> bool {
        if self.items.contains_key(&key) {
            return true;
        }
        self.add(key, value);
        false
    }
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let (v, s) = self.items.remove(key)?;
        self.order.remove(&s);
        Some(v)
    }
    pub fn remove_oldest(&mut self) -> Option<(K, V)> {
        let (_, key) = self.order.pop_first()?;
        let (v, _) = self.items.remove(&key)?;
        Some((key, v))
    }
    pub fn oldest(&self) -> Option<(&K, &V)> {
        let (_, key) = self.order.first_key_value()?;
        self.items.get(key).map(|(v, _)| (key, v))
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    /// Keys from the oldest to the newest.
    pub fn keys(&self) -> Vec<K> {
        self.order.values().cloned().collect()
    }
    /// Values from the oldest to the newest.
    pub fn values(&self) -> Vec<&V> {
        self.order
            .values()
            .filter_map(|k| self.items.get(k).map(|(v, _)| v))
            .collect()
    }
}
