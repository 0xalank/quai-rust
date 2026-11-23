//! Bounded header store and cryptographically verified partial state cache for
//! `quai-node light`.
//!
//! Keeps only recent validated headers (default 4,096 — a few MB on disk) plus
//! an LRU of accounts, storage slots, bytecode, and block tx/receipt payloads
//! that have been cryptographically verified against those headers' roots.
use crate::spec::Chain;
use crypto_bigint::Encoding as _;
use quai_consensus::{
    history::{History, Record},
    rules,
    share_rules::{self, Classification},
    verifier::PINNED_SCHEDULE,
};
use quai_primitives::{
    receipt::Receipt,
    transaction::{Address, Transaction},
    Hash, Header,
};
use quai_rpc::view::{BlockData, ReceiptData, UncleKind, U256};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::broadcast;

/// Default maximum canonical headers retained by a light node.
pub const DEFAULT_MAX_HEADERS: usize = 4_096;
/// Default maximum verified accounts cached in memory.
pub const DEFAULT_ACCOUNT_CACHE: usize = 4_096;
/// Default maximum verified storage slots cached in memory.
pub const DEFAULT_SLOT_CACHE: usize = 16_384;
/// Default maximum verified bytecode blobs cached in memory.
pub const DEFAULT_CODE_CACHE: usize = 1_024;
/// Default maximum verified block tx/receipt bodies cached in memory.
pub const DEFAULT_TX_BLOCK_CACHE: usize = 256;

const CANONICAL_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("light_canonical");
const HEADER_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("light_headers");
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("light_meta");

/// Cryptographically verified account state proven against a block's `evmRoot`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedAccount {
    pub exists: bool,
    pub nonce: u64,
    pub balance: U256,
    pub storage_root: Hash,
    pub code_hash: Hash,
    pub storage_size: U256,
    /// Raw account-trie proof nodes returned by `quai_getProof`.
    pub account_proof: Vec<Vec<u8>>,
}

/// Cryptographically verified storage slot proven against an account's `storageHash`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedSlot {
    pub value: Hash,
    pub proof: Vec<Vec<u8>>,
}

/// Cryptographically verified block transaction/receipt payload proven against
/// the header's `tx_hash` and `receipt_hash`.
#[derive(Clone, Debug)]
pub struct VerifiedBlockPayload {
    pub transactions: Vec<Transaction>,
    pub tx_hashes: Vec<Hash>,
    pub receipts: Option<Arc<Vec<ReceiptData>>>,
}

struct LruMap<K, V> {
    map: HashMap<K, V>,
    order: VecDeque<K>,
    cap: usize,
}

impl<K: Eq + std::hash::Hash + Clone, V: Clone> LruMap<K, V> {
    fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }
    fn get(&self, key: &K) -> Option<V> {
        self.map.get(key).cloned()
    }
    fn insert(&mut self, key: K, val: V) {
        if self.map.insert(key.clone(), val).is_none() {
            self.order.push_back(key);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct StoredHeader {
    pub block: Arc<BlockData>,
    pub record: Record,
    pub total_entropy: U256,
    pub raw: Vec<u8>,
}

struct HeaderIndex {
    head_number: u64,
    head_hash: Hash,
    head_entropy: U256,
    canonical: HashMap<u64, Hash>,
    canonical_order: VecDeque<u64>,
    by_hash: HashMap<Hash, StoredHeader>,
    by_hash_order: VecDeque<Hash>,
    tx_index: HashMap<Hash, (Hash, u64, u64)>,
    max_headers: usize,
}

struct PartialStateCache {
    accounts: LruMap<(Hash, Address), VerifiedAccount>,
    slots: LruMap<(Hash, Hash), VerifiedSlot>,
    codes: LruMap<Hash, Vec<u8>>,
    payloads: LruMap<Hash, Arc<VerifiedBlockPayload>>,
}

/// Thread-safe bounded header store + verified partial state cache.
pub struct LightHeaderStore {
    chain: Arc<Chain>,
    db: Option<Database>,
    index: RwLock<HeaderIndex>,
    cache: Mutex<PartialStateCache>,
    head_events: broadcast::Sender<Hash>,
    chain_events: broadcast::Sender<Hash>,
}

impl LightHeaderStore {
    /// Open an in-memory-only light store seeded with the chain's genesis block.
    pub fn in_memory(chain: Arc<Chain>, max_headers: usize) -> Result<Arc<Self>, String> {
        Self::build(chain, None, max_headers)
    }

    /// Open a persistent light header store backed by `<path>` (typically
    /// `<datadir>/light/<env>/headers.redb`).
    pub fn open(chain: Arc<Chain>, path: &Path, max_headers: usize) -> Result<Arc<Self>, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create light store dir {}: {e}", parent.display()))?;
        }
        let db = Database::create(path)
            .map_err(|e| format!("open light store {}: {e}", path.display()))?;
        let tx = db
            .begin_write()
            .map_err(|e| format!("light store init: {e}"))?;
        {
            let _ = tx.open_table(CANONICAL_TABLE);
            let _ = tx.open_table(HEADER_TABLE);
            let _ = tx.open_table(META_TABLE);
        }
        tx.commit()
            .map_err(|e| format!("light store commit: {e}"))?;
        Self::build(chain, Some(db), max_headers)
    }

    fn build(
        chain: Arc<Chain>,
        db: Option<Database>,
        max_headers: usize,
    ) -> Result<Arc<Self>, String> {
        let max_headers = max_headers.max(64);
        let (head_events, _) = broadcast::channel(256);
        let (chain_events, _) = broadcast::channel(256);
        let store = Arc::new(Self {
            chain: chain.clone(),
            db,
            index: RwLock::new(HeaderIndex {
                head_number: 0,
                head_hash: chain.genesis_hash(),
                head_entropy: [0u8; 32],
                canonical: HashMap::new(),
                canonical_order: VecDeque::new(),
                by_hash: HashMap::new(),
                by_hash_order: VecDeque::new(),
                tx_index: HashMap::new(),
                max_headers,
            }),
            cache: Mutex::new(PartialStateCache {
                accounts: LruMap::new(DEFAULT_ACCOUNT_CACHE),
                slots: LruMap::new(DEFAULT_SLOT_CACHE),
                codes: LruMap::new(DEFAULT_CODE_CACHE),
                payloads: LruMap::new(DEFAULT_TX_BLOCK_CACHE),
            }),
            head_events,
            chain_events,
        });

        // Seed genesis header.
        let genesis_rec = chain.genesis();
        let genesis_raw = genesis_rec.object().encode();
        store.insert_verified_raw(&genesis_raw, 0, [0u8; 32], true)?;

        // Reload persisted headers (if any).
        store.reload_from_db()?;
        Ok(store)
    }

    fn reload_from_db(&self) -> Result<(), String> {
        let Some(db) = &self.db else {
            return Ok(());
        };
        let rd = db.begin_read().map_err(|e| format!("light db read: {e}"))?;
        let Ok(canon) = rd.open_table(CANONICAL_TABLE) else {
            return Ok(());
        };
        let Ok(headers) = rd.open_table(HEADER_TABLE) else {
            return Ok(());
        };
        let mut entries: Vec<(u64, Hash, Vec<u8>, u8, U256)> = Vec::new();
        let iter = canon.iter().map_err(|e| format!("light db iter: {e}"))?;
        for row in iter {
            let (k, v) = row.map_err(|e| format!("light db row: {e}"))?;
            let num = k.value();
            let hash_bytes = v.value();
            if hash_bytes.len() != 32 {
                continue;
            }
            let mut hash = [0u8; 32];
            hash.copy_from_slice(hash_bytes);
            if let Ok(Some(blob)) = headers.get(hash.as_slice()) {
                let raw_blob = blob.value();
                if raw_blob.len() > 33 {
                    let order = raw_blob[0];
                    let mut ent = [0u8; 32];
                    ent.copy_from_slice(&raw_blob[1..33]);
                    entries.push((num, hash, raw_blob[33..].to_vec(), order, ent));
                }
            }
        }
        drop(headers);
        drop(canon);
        drop(rd);

        entries.sort_by_key(|e| e.0);
        for (_num, _hash, raw, order, ent) in entries {
            let _ = self.insert_verified_raw(&raw, order, ent, false);
        }
        Ok(())
    }

    pub fn chain(&self) -> &Arc<Chain> {
        &self.chain
    }

    pub fn head(&self) -> (u64, Hash) {
        let idx = self.index.read().unwrap_or_else(|e| e.into_inner());
        (idx.head_number, idx.head_hash)
    }

    pub fn head_events(&self) -> broadcast::Receiver<Hash> {
        self.head_events.subscribe()
    }

    pub fn chain_events(&self) -> broadcast::Receiver<Hash> {
        self.chain_events.subscribe()
    }

    /// Decode a raw `WorkObject` (header view or full block) into `BlockData`.
    pub fn decode_block_data(
        &self,
        raw: &[u8],
        order: u8,
    ) -> Result<(Arc<BlockData>, Record), String> {
        let record = Record::decode(raw).map_err(|e| format!("decode light header: {e}"))?;
        let object = record.object().clone();
        let header = record.header().clone();
        let hash = record.id();
        let number = record
            .number(2)
            .map_err(|e| format!("header number: {e}"))?;
        let uncles = object.uncle_headers(PINNED_SCHEDULE).unwrap_or_default();
        let tx_hashes = object
            .transactions()
            .iter()
            .filter_map(|t| t.id(None).ok())
            .collect();
        let etx_hashes = object
            .outbound_etxs()
            .iter()
            .filter_map(|t| t.id(None).ok())
            .collect();
        let block = Arc::new(BlockData {
            hash,
            number,
            object,
            header,
            uncles,
            size: raw.len() as u64,
            order,
            tx_hashes,
            etx_hashes,
        });
        Ok((block, record))
    }

    /// Insert a PoW- and consensus-verified header/block into the light store,
    /// applying PoEM total-entropy head choice and pruning entries beyond
    /// `max_headers`. Returns `true` if this header became the new canonical head.
    pub fn insert_verified_raw(
        &self,
        raw: &[u8],
        order: u8,
        total_entropy: U256,
        persist: bool,
    ) -> Result<bool, String> {
        let (block, record) = self.decode_block_data(raw, order)?;
        let hash = block.hash;
        let number = block.number;

        let mut became_head = false;
        let mut evicted_nums = Vec::new();
        let mut evicted_hashes = Vec::new();

        {
            let mut idx = self.index.write().unwrap_or_else(|e| e.into_inner());
            let stored = StoredHeader {
                block: block.clone(),
                record,
                total_entropy,
                raw: raw.to_vec(),
            };
            if !idx.by_hash.contains_key(&hash) {
                idx.by_hash_order.push_back(hash);
            }
            idx.by_hash.insert(hash, stored);

            for (i, txh) in block.tx_hashes.iter().enumerate() {
                idx.tx_index.insert(*txh, (hash, number, i as u64));
            }

            // PoEM head selection: adopt as canonical head if total entropy exceeds
            // current head (or ties at a greater height / initial genesis).
            let cur_e = crypto_bigint::U256::from_be_bytes(idx.head_entropy);
            let new_e = crypto_bigint::U256::from_be_bytes(total_entropy);
            let is_genesis_seed = number == 0 && idx.canonical.is_empty();
            if is_genesis_seed
                || new_e > cur_e
                || (new_e == cur_e && number > idx.head_number)
                || (idx.canonical.is_empty() && number >= idx.head_number)
            {
                became_head = true;
                // Walk parent chain to update canonical pointers on reorg.
                let mut chain_walk = vec![(number, hash)];
                let mut cur_hash = hash;
                while let Some(st) = idx.by_hash.get(&cur_hash) {
                    let parent_hash = match st.record.parent(2) {
                        Ok(p) => p,
                        Err(_) => break,
                    };
                    if st.block.number == 0 {
                        break;
                    }
                    let parent_num = st.block.number - 1;
                    if idx.canonical.get(&parent_num) == Some(&parent_hash) {
                        break;
                    }
                    if !idx.by_hash.contains_key(&parent_hash) {
                        break;
                    }
                    chain_walk.push((parent_num, parent_hash));
                    cur_hash = parent_hash;
                    if chain_walk.len() > 256 {
                        break;
                    }
                }
                for (n, h) in chain_walk.into_iter().rev() {
                    if idx.canonical.insert(n, h).is_none() {
                        idx.canonical_order.push_back(n);
                    }
                }
                idx.head_number = number;
                idx.head_hash = hash;
                idx.head_entropy = total_entropy;
            }

            // Enforce bounded header retention (always keep genesis #0).
            while idx.canonical_order.len() > idx.max_headers {
                if let Some(old_n) = idx.canonical_order.pop_front() {
                    if old_n == 0 {
                        idx.canonical_order.push_back(0);
                        if idx.canonical_order.len() <= idx.max_headers + 1 {
                            break;
                        }
                        continue;
                    }
                    idx.canonical.remove(&old_n);
                    evicted_nums.push(old_n);
                }
            }
            let max_by_hash = idx.max_headers.saturating_mul(2);
            while idx.by_hash_order.len() > max_by_hash {
                if let Some(old_h) = idx.by_hash_order.pop_front() {
                    if old_h == self.chain.genesis_hash() || old_h == idx.head_hash {
                        continue;
                    }
                    if let Some(removed) = idx.by_hash.remove(&old_h) {
                        for txh in &removed.block.tx_hashes {
                            idx.tx_index.remove(txh);
                        }
                    }
                    evicted_hashes.push(old_h);
                }
            }
        }

        if persist {
            self.persist_header(
                number,
                hash,
                order,
                total_entropy,
                raw,
                &evicted_nums,
                &evicted_hashes,
            )?;
        }

        let _ = self.chain_events.send(hash);
        if became_head {
            let _ = self.head_events.send(hash);
        }
        Ok(became_head)
    }

    #[allow(clippy::too_many_arguments)]
    fn persist_header(
        &self,
        number: u64,
        hash: Hash,
        order: u8,
        total_entropy: U256,
        raw: &[u8],
        evicted_nums: &[u64],
        evicted_hashes: &[Hash],
    ) -> Result<(), String> {
        let Some(db) = &self.db else {
            return Ok(());
        };
        let mut blob = Vec::with_capacity(33 + raw.len());
        blob.push(order);
        blob.extend_from_slice(&total_entropy);
        blob.extend_from_slice(raw);
        let tx = db
            .begin_write()
            .map_err(|e| format!("light db write: {e}"))?;
        {
            let mut canon = tx
                .open_table(CANONICAL_TABLE)
                .map_err(|e| format!("light canon table: {e}"))?;
            let mut headers = tx
                .open_table(HEADER_TABLE)
                .map_err(|e| format!("light headers table: {e}"))?;
            let _ = canon.insert(number, hash.as_slice());
            let _ = headers.insert(hash.as_slice(), blob.as_slice());
            for n in evicted_nums {
                let _ = canon.remove(*n);
            }
            for h in evicted_hashes {
                let _ = headers.remove(h.as_slice());
            }
        }
        tx.commit().map_err(|e| format!("light db commit: {e}"))
    }

    pub fn block_by_number(&self, number: u64) -> Option<Arc<BlockData>> {
        let idx = self.index.read().unwrap_or_else(|e| e.into_inner());
        let h = idx.canonical.get(&number)?;
        idx.by_hash.get(h).map(|s| s.block.clone())
    }

    pub fn canonical_hash(&self, number: u64) -> Option<Hash> {
        let idx = self.index.read().unwrap_or_else(|e| e.into_inner());
        idx.canonical.get(&number).copied()
    }

    pub fn block_by_hash(&self, hash: &Hash) -> Option<Arc<BlockData>> {
        let idx = self.index.read().unwrap_or_else(|e| e.into_inner());
        idx.by_hash.get(hash).map(|s| s.block.clone())
    }

    pub fn stored_by_hash(&self, hash: &Hash) -> Option<StoredHeader> {
        let idx = self.index.read().unwrap_or_else(|e| e.into_inner());
        idx.by_hash.get(hash).cloned()
    }

    pub fn total_entropy(&self, hash: &Hash) -> Option<U256> {
        let idx = self.index.read().unwrap_or_else(|e| e.into_inner());
        idx.by_hash.get(hash).map(|s| s.total_entropy)
    }

    pub fn tx_position(&self, tx_hash: &Hash) -> Option<(Hash, u64, u64)> {
        let idx = self.index.read().unwrap_or_else(|e| e.into_inner());
        idx.tx_index.get(tx_hash).copied()
    }

    /// Build a small consensus `History` containing `anchor` and its recent
    /// ancestors (up to `depth`) for `validation::derive_next_header_rules` or
    /// `validation::verify_workshare`.
    pub fn history_window(&self, anchor: &Hash, depth: usize) -> History {
        let mut h = History::default();
        let idx = self.index.read().unwrap_or_else(|e| e.into_inner());
        let mut cur = *anchor;
        for _ in 0..depth.max(1) {
            let Some(st) = idx.by_hash.get(&cur) else {
                break;
            };
            let _ = h.insert(st.record.clone());
            match st.record.parent(2) {
                Ok(p) if p != cur => cur = p,
                _ => break,
            }
        }
        h
    }

    /// Compute PoEM total entropy of a candidate header extending `parent`.
    pub fn compute_next_entropy(
        &self,
        parent: &StoredHeader,
        header: &Header,
        pow_hash: Hash,
    ) -> (u8, U256) {
        let diff = share_rules::uint(header.wire().difficulty.as_deref().unwrap_or_default())
            .unwrap_or(crypto_bigint::U256::ONE)
            .max(crypto_bigint::U256::ONE);
        let b = parent.block.object.header().wire();
        let arr = |v: &[Vec<u8>]| -> [crypto_bigint::U256; 3] {
            [
                v.first()
                    .and_then(|x| share_rules::uint(x).ok())
                    .unwrap_or(crypto_bigint::U256::ZERO),
                v.get(1)
                    .and_then(|x| share_rules::uint(x).ok())
                    .unwrap_or(crypto_bigint::U256::ZERO),
                v.get(2)
                    .and_then(|x| share_rules::uint(x).ok())
                    .unwrap_or(crypto_bigint::U256::ZERO),
            ]
        };
        let input = rules::EntropyInput {
            hash: pow_hash,
            difficulty: diff,
            expansion: b.expansion_number.unwrap_or(0) as u8,
            parent_entropy: arr(&b.parent_entropy),
            parent_delta: arr(&b.parent_delta_entropy),
            parent_uncled_delta: arr(&b.parent_uncled_delta_entropy),
            uncled_entropy: share_rules::uint(b.uncled_entropy.as_deref().unwrap_or_default())
                .unwrap_or(crypto_bigint::U256::ZERO),
            postfork_share_count: None,
            node_context: 2,
        };
        if let Ok(ent) = rules::entropy(&input) {
            let prev = crypto_bigint::U256::from_be_bytes(parent.total_entropy);
            let total = prev.saturating_add(&ent.intrinsic);
            return (ent.order, total.to_be_bytes());
        }
        let prev = crypto_bigint::U256::from_be_bytes(parent.total_entropy);
        (
            2,
            prev.saturating_add(&crypto_bigint::U256::ONE).to_be_bytes(),
        )
    }

    // --- Verified Partial State Cache Accessors ---

    pub fn get_verified_account(&self, evm_root: Hash, addr: &Address) -> Option<VerifiedAccount> {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .accounts
            .get(&(evm_root, *addr))
    }

    pub fn put_verified_account(&self, evm_root: Hash, addr: Address, acc: VerifiedAccount) {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .accounts
            .insert((evm_root, addr), acc);
    }

    pub fn get_verified_slot(&self, storage_root: Hash, key: &Hash) -> Option<VerifiedSlot> {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .slots
            .get(&(storage_root, *key))
    }

    pub fn put_verified_slot(&self, storage_root: Hash, key: Hash, slot: VerifiedSlot) {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .slots
            .insert((storage_root, key), slot);
    }

    pub fn get_verified_code(&self, code_hash: &Hash) -> Option<Vec<u8>> {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .codes
            .get(code_hash)
    }

    pub fn put_verified_code(&self, code_hash: Hash, code: Vec<u8>) {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .codes
            .insert(code_hash, code);
    }

    pub fn get_verified_payload(&self, block_hash: &Hash) -> Option<Arc<VerifiedBlockPayload>> {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .payloads
            .get(block_hash)
    }

    pub fn put_verified_payload(&self, block_hash: Hash, payload: Arc<VerifiedBlockPayload>) {
        {
            let mut idx = self.index.write().unwrap_or_else(|e| e.into_inner());
            if let Some(st) = idx.by_hash.get(&block_hash) {
                let num = st.block.number;
                for (i, txh) in payload.tx_hashes.iter().enumerate() {
                    idx.tx_index.insert(*txh, (block_hash, num, i as u64));
                }
            }
        }
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .payloads
            .insert(block_hash, payload);
    }

    /// Classify uncles of a stored block using the header's own PoW caches.
    pub fn classify_uncles(
        &self,
        block: &BlockData,
        caches: &quai_consensus::pow::SharedCaches,
    ) -> Vec<UncleKind> {
        block
            .uncles
            .iter()
            .map(|u| {
                let Ok(pow_hash) = crate::uncles::pow_hash(caches, u) else {
                    return UncleKind::Other;
                };
                match share_rules::classify(u, pow_hash, self.chain.spec().header.p2p_threshold) {
                    Ok(Classification::Block) => UncleKind::Block,
                    Ok(Classification::Valid) => UncleKind::Workshare,
                    _ => UncleKind::Other,
                }
            })
            .collect()
    }
}

/// Convert raw consensus `Receipt` list into RPC `ReceiptData` given block tx hashes.
pub fn receipts_to_rpc(receipts: &[Receipt], tx_hashes: &[Hash]) -> Vec<ReceiptData> {
    let mut prev_gas = 0u64;
    let mut out = Vec::with_capacity(receipts.len());
    for (i, r) in receipts.iter().enumerate() {
        let gas_used = r.cumulative_gas_used.saturating_sub(prev_gas);
        prev_gas = r.cumulative_gas_used;
        let tx_hash = tx_hashes.get(i).copied().unwrap_or([0u8; 32]);
        out.push(ReceiptData {
            post_state: r.post_state.clone(),
            status: r.status,
            cumulative_gas_used: r.cumulative_gas_used,
            gas_used,
            tx_hash,
            contract_address: None,
            logs: r
                .logs
                .iter()
                .map(|l| quai_rpc::view::LogData {
                    address: l.address,
                    topics: l.topics.clone(),
                    data: l.data.clone(),
                })
                .collect(),
            outbound_etxs: r.outbound_etxs.clone(),
        });
    }
    out
}
