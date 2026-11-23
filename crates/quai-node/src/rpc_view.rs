//! The native RPC view: a `quai_rpc::view::ChainView` over validated chain
//! data. Three ways to open it:
//! - [`ExportView::open`] (`quai-node rpc --export DIR`): a QEX1 export and
//!   the state of the latest replay checkpoint in the data directory;
//! - [`ExportView::open_store`] (`quai-node rpc`): the node's block store
//!   (`blockstore.rs`) and the state of the latest checkpoint;
//! - [`ExportView::live`] (`quai-node run`): the block store and head state
//!   of a running node. Its head moves; every call reads one snapshot of the
//!   store, the head and its state ([`ChainView::pinned`]).
//!
//! What is validated, and how:
//! - Export: the checkpoint names the block at its height; the replay that
//!   wrote it checked every block up to there and every state root. At
//!   startup the view recomputes the hash of each zone block from genesis to
//!   the checkpoint, checks the parent links, and requires the block at the
//!   checkpoint height to be the checkpoint's block. The head is that block.
//! - Store: the chain processor writes only blocks that passed every check;
//!   the head is the checkpoint's block (standalone) or the processor's
//!   published head (live).
//! - The state's roots must equal the head header's EVM, ETX and UTXO roots.
//! - Receipts come from the export (Go exports only) or the store (Go
//!   receipts, or the node's own execution), never from peers.
//! - Derived block fields: the order comes from the export's prime and region
//!   records or the processor's computed order; the total entropy of a block
//!   is its child's zone parent entropy (checked when the child was
//!   validated), and for the head it is computed natively. Uncles are
//!   classified with their proof-of-work, as go-quai does.
//!
//! "Latest" is the head. State at other blocks is served only when their
//! EVM root is the head's (or empty); otherwise the answer is go-quai's for
//! state that a pruned node no longer keeps.
use crate::blockstore::{self, StoreReader};
use crate::checkpoint;
use crate::log::Log;
use crate::qex::{self, Context, Manifest, SegmentReader};
use crate::replay;
use crate::rpc_call;
use crate::spec::Chain;
use crate::store::{self, PastDb, StateStore};
use crate::txpool::service::Service as TxPoolService;
use crate::uncles;
use crypto_bigint::Encoding as _;
use quai_consensus::{
    history::{self, History, Record},
    pow::SharedCaches,
    verifier::{Verifier, PINNED_SCHEDULE},
    zone_execution::State,
};
use quai_execution::{
    historical::History as _,
    qi::{OutPoint, UtxoView},
    state_store::Commitment,
};
use quai_primitives::{transaction::Address, work::WorkObject, Hash};
use quai_rpc::view::{
    AccountState, BlockData, CallError, CallRequest, CallResult, ChainView, LogData, OutpointData,
    PeerCounts, ReceiptData, SpentUtxo, TxPosition, UncleKind, Unlocks, UtxoData, ViewError, U256,
};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Instant;

/// `types.EmptyRootHash`.
pub const EMPTY_ROOT: Hash = [
    0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6, 0x92, 0xc0, 0xf8, 0x6e,
    0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0, 0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
];
/// Decoded blocks kept in memory.
const BLOCK_CACHE: usize = 512;

fn unavailable(e: impl std::fmt::Display) -> ViewError {
    ViewError::Unavailable(e.to_string())
}
fn hash_of(v: &Option<quai_primitives::schema::common::ProtoHash>) -> Hash {
    let mut h = [0u8; 32];
    if let Some(v) = v {
        let take = v.value.len().min(32);
        h[32 - take..].copy_from_slice(&v.value[v.value.len() - take..]);
    }
    h
}
fn u256(v: &[u8]) -> U256 {
    let mut out = [0u8; 32];
    let take = v.len().min(32);
    out[32 - take..].copy_from_slice(&v[v.len() - take..]);
    out
}

/// A zone block as a backend keeps it.
struct Raw {
    hash: Hash,
    order: u8,
    bytes: Vec<u8>,
    /// Uncle classes, when the backend keeps them.
    kinds: Option<Vec<u8>>,
}
/// The blocks of one consistent snapshot: an export index or a store read.
trait Blocks: Send + Sync {
    /// The canonical hash at `height`, up to the snapshot's head.
    fn hash_at(&self, height: u64) -> Result<Option<Hash>, ViewError>;
    /// The canonical height of a block.
    fn height_of(&self, hash: &Hash) -> Result<Option<u64>, ViewError>;
    fn block(&self, height: u64, hash: &Hash) -> Result<Raw, ViewError>;
    /// Any kept block by hash (ancestry reads).
    fn record_by_hash(&self, hash: &Hash) -> Result<Option<Record>, ViewError>;
    /// The backend keeps receipts.
    fn receipts_kept(&self) -> bool;
    /// What to say when a block has none.
    fn no_receipts(&self) -> &'static str;
    /// go-quai storage receipts of a canonical block; None: not kept.
    fn receipts(&self, height: u64, hash: &Hash) -> Result<Option<Vec<u8>>, ViewError>;
    /// The block's total entropy as its child records it, if it has one.
    fn child_entropy(&self, height: u64, hash: &Hash) -> Result<Option<U256>, ViewError>;
    /// Where a transaction may be: (height, index) candidates.
    fn tx_candidates(&self, hash: &Hash) -> Result<Vec<(u64, u32)>, ViewError>;
    /// The UTXOs block `height` created.
    fn created(&self, height: u64) -> Result<Vec<OutPoint>, ViewError>;
    /// The indexed outpoints of an address; None: no index here.
    fn outpoints(&self, address: &Address) -> Result<Option<Vec<OutPoint>>, ViewError>;
    /// The number of any kept block, canonical or not.
    fn candidate_height(&self, _hash: &Hash) -> Result<Option<u64>, ViewError> {
        Ok(None)
    }
    /// The outputs canonical block `height` removed, with their owners
    /// (trimmed outputs first, then Qi inputs); None: not kept.
    fn spent(&self, _height: u64) -> Result<Option<Vec<(OutPoint, Address)>>, ViewError> {
        Ok(None)
    }
    /// The outputs canonical block `height` created, with their entries;
    /// None: not kept.
    fn created_entries(&self, _height: u64) -> Result<Option<Vec<blockstore::Created>>, ViewError> {
        Ok(None)
    }
    /// The canonical prime (0) or region (1) hash at `number`; None: not kept.
    fn dominant_hash_at(&self, _ctx: u8, _number: u64) -> Result<Option<Hash>, ViewError> {
        Ok(None)
    }
    /// A prime (0) or region (1) view's work object bytes; None: not kept.
    fn dominant(&self, _ctx: u8, _hash: &Hash) -> Result<Option<Vec<u8>>, ViewError> {
        Ok(None)
    }
    /// Compact log bloom at canonical height (empty = confirmed no logs, 256
    /// bytes = bloom, None = not indexed).
    fn bloom_at(&self, _height: u64) -> Result<Option<Vec<u8>>, ViewError> {
        Ok(None)
    }
    /// Zone block hash containing or matching a workshare hash, if indexed.
    fn block_for_workshare(&self, _ws_hash: &Hash) -> Result<Option<Hash>, ViewError> {
        Ok(None)
    }
    /// Zone block hash linked to an AuxPoW donor PoW hash, if indexed.
    fn block_for_donor(&self, _donor_hash: &Hash) -> Result<Option<Hash>, ViewError> {
        Ok(None)
    }
    /// Persisted supply analytics for a zone block hash, if indexed.
    fn supply(
        &self,
        _block_hash: &Hash,
    ) -> Result<Option<quai_rpc::view::SupplyAnalytics>, ViewError> {
        Ok(None)
    }
    /// Indexed locked Quai balance of `address`.
    fn locked_quai(&self, _address: &Address) -> Result<U256, ViewError> {
        Ok([0u8; 32])
    }
    /// Archived state version entry at `height`, if present (`--archive`).
    fn state_version_at(
        &self,
        _height: u64,
    ) -> Result<Option<blockstore::StateVersionEntry>, ViewError> {
        Ok(None)
    }
}

/// Where a zone record's bytes are.
#[derive(Clone, Copy)]
struct Entry {
    segment: u16,
    block_at: u64,
    block_len: u32,
    receipts_at: u64,
    receipts_len: u32,
    order: u8,
    hash: Hash,
}
/// The export index: one entry per zone height from genesis to the head.
pub struct Index {
    files: Vec<Option<File>>,
    heights: Vec<Entry>,
    by_hash: HashMap<Hash, u64>,
    /// (first 8 bytes of the transaction hash, height, index), sorted.
    txs: Vec<(u64, u64, u32)>,
    receipts: bool,
    /// The checkpoint's redemption and trim history (created UTXOs).
    past: Mutex<PastDb>,
}
impl Index {
    fn read(&self, segment: u16, at: u64, len: u32) -> Result<Vec<u8>, ViewError> {
        let file = self
            .files
            .get(segment as usize)
            .and_then(Option::as_ref)
            .ok_or_else(|| ViewError::Unavailable("export segment".into()))?;
        let mut buf = vec![0u8; len as usize];
        file.read_exact_at(&mut buf, at)
            .map_err(|e| ViewError::Unavailable(format!("export read: {e}")))?;
        Ok(buf)
    }
    fn entry(&self, height: u64) -> Result<&Entry, ViewError> {
        self.heights
            .get(height as usize)
            .ok_or_else(|| ViewError::Unavailable(format!("zone height {height}")))
    }
    fn record(&self, height: u64) -> Result<Record, ViewError> {
        let e = self.entry(height)?;
        let bytes = self.read(e.segment, e.block_at, e.block_len)?;
        Record::decode(&bytes)
            .map_err(|e| ViewError::Unavailable(format!("zone block {height}: {e}")))
    }
}
impl Blocks for Index {
    fn hash_at(&self, height: u64) -> Result<Option<Hash>, ViewError> {
        Ok(self.heights.get(height as usize).map(|e| e.hash))
    }
    fn height_of(&self, hash: &Hash) -> Result<Option<u64>, ViewError> {
        Ok(self.by_hash.get(hash).copied())
    }
    fn block(&self, height: u64, _: &Hash) -> Result<Raw, ViewError> {
        let e = *self.entry(height)?;
        Ok(Raw {
            hash: e.hash,
            order: e.order,
            bytes: self.read(e.segment, e.block_at, e.block_len)?,
            kinds: None,
        })
    }
    fn record_by_hash(&self, hash: &Hash) -> Result<Option<Record>, ViewError> {
        match self.by_hash.get(hash) {
            Some(h) => self.record(*h).map(Some),
            None => Ok(None),
        }
    }
    fn receipts_kept(&self) -> bool {
        self.receipts
    }
    fn no_receipts(&self) -> &'static str {
        "receipts (this export has none)"
    }
    fn receipts(&self, height: u64, _: &Hash) -> Result<Option<Vec<u8>>, ViewError> {
        if !self.receipts {
            return Ok(None);
        }
        let e = *self.entry(height)?;
        if e.receipts_len == 0 {
            return Ok(Some(vec![]));
        }
        self.read(e.segment, e.receipts_at, e.receipts_len)
            .map(Some)
    }
    fn child_entropy(&self, height: u64, _: &Hash) -> Result<Option<U256>, ViewError> {
        if height + 1 >= self.heights.len() as u64 {
            return Ok(None);
        }
        let child = self.record(height + 1)?;
        let v = child
            .object()
            .header()
            .wire()
            .parent_entropy
            .get(2)
            .cloned()
            .unwrap_or_default();
        Ok(Some(u256(&v)))
    }
    fn tx_candidates(&self, hash: &Hash) -> Result<Vec<(u64, u32)>, ViewError> {
        let prefix = u64::from_be_bytes(hash[..8].try_into().unwrap_or_default());
        let start = self.txs.partition_point(|t| t.0 < prefix);
        Ok(self.txs[start..]
            .iter()
            .take_while(|t| t.0 == prefix)
            .map(|&(_, height, i)| (height, i))
            .collect())
    }
    fn created(&self, height: u64) -> Result<Vec<OutPoint>, ViewError> {
        let past = self
            .past
            .lock()
            .map_err(|_| unavailable("past.redb lock"))?;
        Ok(past
            .created_at(height)
            .map_err(|e| ViewError::Unavailable(format!("UTXOs created at {height}: {e:?}")))?
            .into_iter()
            .map(|(p, _)| p)
            .collect())
    }
    fn outpoints(&self, _: &Address) -> Result<Option<Vec<OutPoint>>, ViewError> {
        Ok(None)
    }
}

/// A read snapshot of the block store, up to `head`.
struct StoreBlocks {
    reader: StoreReader,
    head: u64,
}
fn store_error(e: std::io::Error) -> ViewError {
    ViewError::Unavailable(format!("block store: {e}"))
}
impl Blocks for StoreBlocks {
    fn dominant_hash_at(&self, ctx: u8, number: u64) -> Result<Option<Hash>, ViewError> {
        self.reader
            .dominant_hash_at(ctx, number)
            .map_err(store_error)
    }
    fn dominant(&self, ctx: u8, hash: &Hash) -> Result<Option<Vec<u8>>, ViewError> {
        self.reader.dominant(ctx, hash).map_err(store_error)
    }
    fn bloom_at(&self, height: u64) -> Result<Option<Vec<u8>>, ViewError> {
        if height > self.head {
            return Ok(None);
        }
        self.reader.bloom_at(height).map_err(store_error)
    }
    fn hash_at(&self, height: u64) -> Result<Option<Hash>, ViewError> {
        if height > self.head {
            return Ok(None);
        }
        self.reader.hash_at(height).map_err(store_error)
    }
    fn height_of(&self, hash: &Hash) -> Result<Option<u64>, ViewError> {
        Ok(self
            .reader
            .canonical_number(hash)
            .map_err(store_error)?
            .filter(|n| *n <= self.head))
    }
    fn block(&self, height: u64, hash: &Hash) -> Result<Raw, ViewError> {
        let b = self
            .reader
            .block(hash)
            .map_err(store_error)?
            .ok_or_else(|| ViewError::Unavailable(format!("stored zone block {height}")))?;
        Ok(Raw {
            hash: b.hash,
            order: b.order,
            bytes: b.block,
            kinds: Some(b.uncle_kinds),
        })
    }
    fn record_by_hash(&self, hash: &Hash) -> Result<Option<Record>, ViewError> {
        match self.reader.block(hash).map_err(store_error)? {
            Some(b) => Record::decode(&b.block)
                .map(Some)
                .map_err(|e| ViewError::Unavailable(format!("stored zone block: {e}"))),
            None => Ok(None),
        }
    }
    fn receipts_kept(&self) -> bool {
        true
    }
    fn no_receipts(&self) -> &'static str {
        "receipts (this node has none for the block: it came from peers and was not executed)"
    }
    fn receipts(&self, _: u64, hash: &Hash) -> Result<Option<Vec<u8>>, ViewError> {
        Ok(self
            .reader
            .block(hash)
            .map_err(store_error)?
            .and_then(|b| b.receipts))
    }
    fn child_entropy(&self, height: u64, hash: &Hash) -> Result<Option<U256>, ViewError> {
        if height >= self.head {
            return Ok(None);
        }
        self.reader.entropy(hash).map_err(store_error)
    }
    fn tx_candidates(&self, hash: &Hash) -> Result<Vec<(u64, u32)>, ViewError> {
        Ok(self
            .reader
            .transaction(hash)
            .map_err(store_error)?
            .filter(|(n, _)| *n <= self.head)
            .into_iter()
            .collect())
    }
    fn created(&self, height: u64) -> Result<Vec<OutPoint>, ViewError> {
        if !self.reader.keeps_utxos() {
            return Err(ViewError::Unavailable(
                "UTXOs (this node runs without state)".into(),
            ));
        }
        Ok(self
            .reader
            .created(height)
            .map_err(store_error)?
            .unwrap_or_default()
            .into_iter()
            .map(|c| c.point)
            .collect())
    }
    fn outpoints(&self, address: &Address) -> Result<Option<Vec<OutPoint>>, ViewError> {
        if !self.reader.keeps_utxos() {
            return Ok(None);
        }
        self.reader
            .outpoints(address)
            .map(Some)
            .map_err(store_error)
    }
    fn candidate_height(&self, hash: &Hash) -> Result<Option<u64>, ViewError> {
        Ok(self
            .reader
            .block(hash)
            .map_err(store_error)?
            .map(|b| b.number))
    }
    fn spent(&self, height: u64) -> Result<Option<Vec<(OutPoint, Address)>>, ViewError> {
        if !self.reader.keeps_utxos() || height > self.head {
            return Ok(None);
        }
        self.reader.spent(height).map_err(store_error)
    }
    fn created_entries(&self, height: u64) -> Result<Option<Vec<blockstore::Created>>, ViewError> {
        if !self.reader.keeps_utxos() || height > self.head {
            return Ok(None);
        }
        self.reader.created(height).map_err(store_error)
    }
    fn block_for_workshare(&self, ws_hash: &Hash) -> Result<Option<Hash>, ViewError> {
        self.reader
            .block_for_workshare(ws_hash)
            .map_err(store_error)
    }
    fn block_for_donor(&self, donor_hash: &Hash) -> Result<Option<Hash>, ViewError> {
        self.reader.block_for_donor(donor_hash).map_err(store_error)
    }
    fn supply(
        &self,
        block_hash: &Hash,
    ) -> Result<Option<quai_rpc::view::SupplyAnalytics>, ViewError> {
        self.reader.supply(block_hash).map_err(store_error)
    }
    fn locked_quai(&self, address: &Address) -> Result<U256, ViewError> {
        self.reader.locked_quai(address).map_err(store_error)
    }
    fn state_version_at(
        &self,
        height: u64,
    ) -> Result<Option<blockstore::StateVersionEntry>, ViewError> {
        if height > self.head {
            return Ok(None);
        }
        self.reader.state_version_at(height).map_err(store_error)
    }
}

/// Ancestry reads for the native entropy computation.
struct Records(Arc<dyn Blocks>);
impl history::Reader for Records {
    fn read(&self, id: &Hash) -> Result<Option<Arc<Record>>, quai_consensus::Error> {
        self.0
            .record_by_hash(id)
            .map(|r| r.map(Arc::new))
            .map_err(|_| quai_consensus::Error::Unavailable("view ancestry record"))
    }
}

/// The state after the head block.
pub struct HeadState {
    pub state: State,
    pub evm_root: Hash,
    pub etx_root: Hash,
    /// Unspent outputs per address from a full scan, for a view without an
    /// address index (export).
    by_address: Option<HashMap<Address, Vec<OutpointData>>>,
}
impl HeadState {
    pub fn new(state: State, evm_root: Hash, etx_root: Hash) -> Self {
        Self {
            state,
            evm_root,
            etx_root,
            by_address: None,
        }
    }
}
/// One consistent view: blocks, head and the head's state.
pub struct Snapshot {
    blocks: Arc<dyn Blocks>,
    head: u64,
    head_hash: Hash,
    /// None: the node runs without state.
    state: Option<Arc<HeadState>>,
    head_entropy: OnceLock<Result<U256, String>>,
}
/// The moving head of a running node.
pub struct Live {
    current: RwLock<Arc<Snapshot>>,
    events: tokio::sync::broadcast::Sender<Hash>,
    removed: tokio::sync::broadcast::Sender<Arc<quai_rpc::view::RemovedLogs>>,
    /// Every zone block the node appends, canonical or not (go-quai's
    /// `ChainEvent`).
    chain: tokio::sync::broadcast::Sender<Hash>,
    /// The coinbase lockups each executed block unlocked (`UnlocksEvent`).
    unlocks: tokio::sync::broadcast::Sender<Arc<Unlocks>>,
    node: Arc<NodeInfo>,
}
/// Zone blocks the node appended lately, kept for the RPC.
const APPENDED: usize = 512;
/// What every view of a running node shares, pinned views included: its
/// peer counts, its rolling fee window and the zone blocks it appended
/// lately (a block the head choice did not take is in no store).
#[derive(Default)]
pub struct NodeInfo {
    peers: RwLock<Option<PeerCounter>>,
    fees: RwLock<Option<[U256; 3]>>,
    appended: Mutex<VecDeque<(Hash, u64, u8, Arc<Vec<u8>>)>>,
    p2p_node_info: RwLock<Option<JsonCallback>>,
    p2p_peers_info: RwLock<Option<JsonCallback>>,
    syncing: RwLock<Option<JsonCallback>>,
}
/// The network's peer counts (`net_peerCount`, `net_peerCountByDirection`).
pub type PeerCounter = Box<dyn Fn() -> PeerCounts + Send + Sync>;
/// Dynamic JSON snapshot hook (`admin_nodeInfo`, `admin_peers`, `eth_syncing`).
pub type JsonCallback = Box<dyn Fn() -> serde_json::Value + Send + Sync>;
impl NodeInfo {
    pub fn set_peer_counter(&self, counter: PeerCounter) {
        *self.peers.write().unwrap_or_else(|e| e.into_inner()) = Some(counter);
    }
    pub fn set_p2p_node_info(&self, cb: JsonCallback) {
        *self
            .p2p_node_info
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(cb);
    }
    pub fn set_p2p_peers_info(&self, cb: JsonCallback) {
        *self
            .p2p_peers_info
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(cb);
    }
    pub fn set_syncing(&self, cb: JsonCallback) {
        *self.syncing.write().unwrap_or_else(|e| e.into_inner()) = Some(cb);
    }
    fn p2p_node_info(&self) -> Option<serde_json::Value> {
        self.p2p_node_info
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|f| f())
    }
    fn p2p_peers_info(&self) -> Option<serde_json::Value> {
        self.p2p_peers_info
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|f| f())
    }
    fn syncing_status(&self) -> Option<serde_json::Value> {
        self.syncing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|f| f())
    }
    fn peer_counts(&self) -> PeerCounts {
        self.peers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|f| f())
            .unwrap_or_default()
    }
    /// go-quai's `GetRollingFeeInfo`: minimum, maximum and average.
    pub fn set_rolling_fees(&self, fees: Option<[U256; 3]>) {
        *self.fees.write().unwrap_or_else(|e| e.into_inner()) = fees;
    }
    fn rolling_fees(&self) -> Option<[U256; 3]> {
        *self.fees.read().unwrap_or_else(|e| e.into_inner())
    }
    fn push_appended(&self, hash: Hash, number: u64, order: u8, bytes: Arc<Vec<u8>>) {
        let mut a = self.appended.lock().unwrap_or_else(|e| e.into_inner());
        if a.iter().any(|(h, ..)| *h == hash) {
            return;
        }
        if a.len() >= APPENDED {
            a.pop_front();
        }
        a.push_back((hash, number, order, bytes));
    }
    fn appended(&self, hash: &Hash) -> Option<(u64, Raw)> {
        let a = self.appended.lock().unwrap_or_else(|e| e.into_inner());
        a.iter()
            .find(|(h, ..)| h == hash)
            .map(|(h, number, order, bytes)| {
                (
                    *number,
                    Raw {
                        hash: *h,
                        order: *order,
                        bytes: bytes.to_vec(),
                        kinds: None,
                    },
                )
            })
    }
}
impl Live {
    /// Publish a new head: a store snapshot that holds it and the state
    /// after it.
    pub fn publish(&self, reader: StoreReader, head: (u64, Hash), state: Option<Arc<HeadState>>) {
        let snapshot = Arc::new(Snapshot {
            blocks: Arc::new(StoreBlocks {
                reader,
                head: head.0,
            }),
            head: head.0,
            head_hash: head.1,
            state,
            head_entropy: OnceLock::new(),
        });
        let changed = {
            let mut current = self.current.write().unwrap_or_else(|e| e.into_inner());
            let changed = current.head_hash != head.1;
            *current = snapshot;
            changed
        };
        if changed {
            let _ = self.events.send(head.1);
        }
    }
    pub fn head(&self) -> (u64, Hash) {
        let c = self.current.read().unwrap_or_else(|e| e.into_inner());
        (c.head, c.head_hash)
    }
    /// Announce the logs of blocks a reorg removed.
    pub fn removed(&self, logs: quai_rpc::view::RemovedLogs) {
        let _ = self.removed.send(Arc::new(logs));
    }
    /// A zone block the node appended (checked; canonical or not), with
    /// its order and its work object bytes.
    pub fn appended(&self, hash: Hash, number: u64, order: u8, bytes: Arc<Vec<u8>>) {
        self.node.push_appended(hash, number, order, bytes);
        let _ = self.chain.send(hash);
    }
    /// The coinbase lockups an executed block unlocked: go-quai sends the
    /// event only when there is one.
    pub fn unlocked(&self, block_hash: Hash, addresses: Vec<Address>) {
        if !addresses.is_empty() {
            let _ = self.unlocks.send(Arc::new(Unlocks {
                block_hash,
                addresses,
            }));
        }
    }
    /// The figures every view of this node shares.
    pub fn node(&self) -> &Arc<NodeInfo> {
        &self.node
    }
    pub fn has_state(&self) -> bool {
        let c = self.current.read().unwrap_or_else(|e| e.into_inner());
        c.state.is_some()
    }
}

pub struct ExportView {
    chain: Arc<Chain>,
    /// The snapshot of a fixed or pinned view; a live view reads `live`.
    snap: Arc<Snapshot>,
    live: Option<Arc<Live>>,
    /// The running node's shared figures (a live view and its pins).
    node: Option<Arc<NodeInfo>>,
    caches: Arc<SharedCaches>,
    /// Decoded blocks and uncle classes by block hash.
    blocks: Arc<Mutex<Lru<Arc<BlockData>>>>,
    kinds: Arc<Mutex<Lru<Arc<Vec<UncleKind>>>>>,
    /// The transaction pool, if the node runs one.
    pool: Option<Arc<TxPoolService>>,
    /// The mining worker, if configured on this node.
    miner: Option<Arc<crate::miner::Miner>>,
    /// State checkpoint directory for `--archive` historical state lookups.
    state_dir: Option<std::path::PathBuf>,
}
/// A small cache by block hash, oldest entry out first.
type Lru<T> = (HashMap<Hash, T>, VecDeque<Hash>);
fn lru<T>() -> Arc<Mutex<Lru<T>>> {
    Arc::new(Mutex::new((HashMap::new(), VecDeque::new())))
}
fn remember<T: Clone>(cache: &Mutex<Lru<T>>, key: Hash, value: &T, cap: usize) {
    if let Ok(mut c) = cache.lock() {
        if c.0.len() >= cap {
            if let Some(old) = c.1.pop_front() {
                c.0.remove(&old);
            }
        }
        if c.0.insert(key, value.clone()).is_none() {
            c.1.push_back(key);
        }
    }
}

/// The state of a checkpoint and its binding to the head header.
fn checkpoint_state(
    dir: &Path,
    chain: &Chain,
    cp: &checkpoint::Checkpoint,
    head: &Record,
) -> Result<(State, Hash, Hash), String> {
    let height = cp.height;
    let section = cp
        .state
        .clone()
        .ok_or_else(|| format!("checkpoint {height} has no state"))?;
    let commitment = Commitment {
        roots: quai_execution::state_image::Roots {
            evm: section.evm,
            etx: section.etx,
            utxo: section.utxo,
            accounts: section.accounts,
            utxo_count: section.utxo_count,
        },
        digest: section.digest,
    };
    let body = head.object().header().wire();
    if section.evm != hash_of(&body.evm_root)
        || section.etx != hash_of(&body.etx_set_root)
        || section.utxo != hash_of(&body.utxo_root)
    {
        return Err(format!(
            "checkpoint {height} state roots differ from the head header"
        ));
    }
    let store = StateStore::open(
        dir,
        store::profile(&chain.spec().environment, &chain.genesis_hash()),
        height,
        section.version,
        commitment,
    )
    .map_err(|e| format!("state {height}: {e}"))?;
    let state = store
        .restore()
        .map_err(|e| format!("state {height}: {e}"))?;
    Ok((state, section.evm, section.etx))
}
fn latest_checkpoint(dir: &Path, chain: &Chain) -> Result<Option<checkpoint::Checkpoint>, String> {
    let list = checkpoint::list(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let Some((height, path)) = list.first() else {
        return Ok(None);
    };
    let cp = checkpoint::read(path).map_err(|e| e.to_string())?;
    if cp.height != *height
        || cp.environment != chain.spec().environment
        || cp.genesis != chain.genesis_hash()
    {
        return Err(format!("checkpoint {height} is for another chain"));
    }
    Ok(Some(cp))
}

impl ExportView {
    fn with_snapshot(
        chain: Arc<Chain>,
        snap: Arc<Snapshot>,
        live: Option<Arc<Live>>,
    ) -> Result<Self, String> {
        Ok(Self {
            chain,
            snap,
            node: live.as_ref().map(|l| l.node.clone()),
            live,
            caches: Arc::new(SharedCaches::new(2).map_err(|e| e.to_string())?),
            blocks: lru(),
            kinds: lru(),
            pool: None,
            miner: None,
            state_dir: None,
        })
    }
    /// Another handle on the same view (blocks, head and caches).
    pub fn share(&self) -> Self {
        Self {
            chain: self.chain.clone(),
            snap: self.snap.clone(),
            live: self.live.clone(),
            node: self.node.clone(),
            caches: self.caches.clone(),
            blocks: self.blocks.clone(),
            kinds: self.kinds.clone(),
            pool: self.pool.clone(),
            miner: self.miner.clone(),
            state_dir: self.state_dir.clone(),
        }
    }
    /// Answer the pool's RPC methods from `pool`.
    pub fn set_pool(&mut self, pool: Arc<TxPoolService>) {
        self.pool = Some(pool);
    }
    /// Answer the mining, block template and workshare RPC methods from `miner`.
    pub fn set_miner(&mut self, miner: Arc<crate::miner::Miner>) {
        self.miner = Some(miner);
    }
    /// Record the state directory so `--archive` historical state queries can
    /// open persisted versions from `state-*.redb`.
    pub fn set_state_dir(&mut self, dir: std::path::PathBuf) {
        self.state_dir = Some(dir);
    }
    /// Shared PoW caches (also reused by the mining worker).
    pub fn caches(&self) -> &Arc<SharedCaches> {
        &self.caches
    }
    /// The canonical zone hash at `number` in the published snapshot.
    pub fn hash_at(&self, number: u64) -> Option<Hash> {
        self.snap().blocks.hash_at(number).ok().flatten()
    }
    /// The canonical prime (0) or region (1) hash at `number`.
    pub fn dominant_hash_at(&self, ctx: u8, number: u64) -> Option<Hash> {
        self.snap()
            .blocks
            .dominant_hash_at(ctx, number)
            .ok()
            .flatten()
    }
    /// The work object bytes of a prime (0) or region (1) view this node
    /// holds.
    pub fn raw_dominant(&self, ctx: u8, hash: &Hash) -> Option<Vec<u8>> {
        self.snap().blocks.dominant(ctx, hash).ok().flatten()
    }
    /// The work object bytes and number of a zone block this node holds:
    /// canonical, left behind by a reorg, or appended lately.
    pub fn raw_block(&self, hash: &Hash) -> Option<(u64, Vec<u8>)> {
        let snap = self.snap();
        if let Ok(Some(number)) = snap.blocks.candidate_height(hash) {
            if let Ok(raw) = snap.blocks.block(number, hash) {
                return Some((number, raw.bytes));
            }
        }
        self.node
            .as_ref()
            .and_then(|n| n.appended(hash))
            .map(|(number, raw)| (number, raw.bytes))
    }
    /// The size of the head's UTXO set (`rawdb.ReadUTXOSetSize` of the
    /// head), if the view keeps state.
    pub fn head_utxo_set_size(&self) -> Option<(Hash, u64)> {
        let snap = self.snap();
        let state = snap.state.as_ref()?;
        Some((snap.head_hash, state.state.utxos.consensus_size()))
    }
    /// Index `export` up to the latest checkpoint in `datadir` and open the
    /// checkpoint's state.
    pub fn open(export: &Path, datadir: &Path, log: &Log) -> Result<Self, String> {
        let started = Instant::now();
        let manifest = Manifest::load(export).map_err(|e| e.to_string())?;
        let chain = Chain::for_environment(&manifest.environment)
            .map_err(|e| format!("chain spec {}: {e}", manifest.environment))?;
        if manifest.genesis_hash != chain.genesis_hash() {
            return Err("the export is for another genesis".into());
        }
        let dir = replay::checkpoint_dir(datadir, &chain, true);
        let Some(cp) = latest_checkpoint(&dir, &chain)? else {
            return Err(format!(
                "no state checkpoint in {}: run `quai-node replay --state` first",
                dir.display()
            ));
        };
        let Some(section) = cp.state.clone() else {
            return Err(format!("checkpoint {} has no state", cp.height));
        };
        if cp.height > manifest.last_zone {
            return Err(format!(
                "the export ends at zone {}, below checkpoint {}",
                manifest.last_zone, cp.height
            ));
        }
        let head = cp.height;
        log.info(&format!(
            "rpc view: indexing {} to checkpoint {head}",
            export.display()
        ));
        let past = PastDb::open(&dir.join("past.redb")).map_err(|e| format!("past.redb: {e}"))?;
        let index = Arc::new(build_index(export, &manifest, &chain, head, past)?);
        if index.heights[head as usize].hash != cp.block {
            return Err(format!(
                "the export block at {head} is not the checkpoint block {}",
                qex::hex(&cp.block)
            ));
        }
        let head_record = index.record(head).map_err(|e| e.to_string())?;
        let (state, evm, etx) = checkpoint_state(&dir, &chain, &cp, &head_record)?;
        let by_address = address_index(&state.utxos);
        log.info(&format!(
            "rpc view: {} zone blocks, {} transactions, head {head} {}, {} accounts, {} UTXOs{} ({:.1}s)",
            index.heights.len(),
            index.txs.len(),
            qex::hex(&cp.block),
            section.accounts,
            section.utxo_count,
            if manifest.receipts {
                ", receipts"
            } else {
                ", no receipts (peer export)"
            },
            started.elapsed().as_secs_f64()
        ));
        let snap = Arc::new(Snapshot {
            blocks: index,
            head,
            head_hash: cp.block,
            state: Some(Arc::new(HeadState {
                state,
                evm_root: evm,
                etx_root: etx,
                by_address,
            })),
            head_entropy: OnceLock::new(),
        });
        let mut view = Self::with_snapshot(chain, snap, None)?;
        view.set_state_dir(dir);
        Ok(view)
    }
    /// Serve the block store of `datadir` for `environment`: with state, up
    /// to the latest state checkpoint and its state; without, the whole store.
    pub fn open_store(datadir: &Path, environment: &str, log: &Log) -> Result<Self, String> {
        let chain = Chain::for_environment(environment)
            .map_err(|e| format!("chain spec {environment}: {e}"))?;
        let state_dir = replay::checkpoint_dir(datadir, &chain, true);
        let headers_dir = replay::checkpoint_dir(datadir, &chain, false);
        let (dir, with_state) = if blockstore::path(&state_dir).is_file() {
            (state_dir, true)
        } else if blockstore::path(&headers_dir).is_file() {
            (headers_dir, false)
        } else {
            return Err(format!(
                "no block store in {} or {}: run `quai-node run` or `quai-node replay --store` first",
                state_dir.display(),
                headers_dir.display()
            ));
        };
        let store = blockstore::BlockStore::open(&blockstore::path(&dir), with_state)
            .map_err(|e| e.to_string())?;
        let reader = store.reader().map_err(|e| e.to_string())?;
        let (store_head, _) = reader.head().ok_or("the block store is empty")?;
        let (head, state) = if with_state {
            let cp = latest_checkpoint(&dir, &chain)?
                .ok_or_else(|| format!("no state checkpoint in {}", dir.display()))?;
            if reader.hash_at(cp.height).map_err(|e| e.to_string())? != Some(cp.block) {
                return Err(format!(
                    "the block store does not hold checkpoint block {} at {}",
                    qex::hex(&cp.block),
                    cp.height
                ));
            }
            let stored = reader
                .block(&cp.block)
                .map_err(|e| e.to_string())?
                .ok_or("checkpoint block bytes")?;
            let record = Record::decode(&stored.block).map_err(|e| e.to_string())?;
            let (state, evm, etx) = checkpoint_state(&dir, &chain, &cp, &record)?;
            (
                (cp.height, cp.block),
                Some(Arc::new(HeadState::new(state, evm, etx))),
            )
        } else {
            (reader.head().ok_or("the block store is empty")?, None)
        };
        log.info(&format!(
            "rpc view: block store {} (base {}, head {store_head}), serving to {} {}{}",
            blockstore::path(&dir).display(),
            reader.base().unwrap_or_default(),
            head.0,
            qex::hex(&head.1),
            if with_state {
                ", with state"
            } else {
                ", without state"
            }
        ));
        let snap = Arc::new(Snapshot {
            blocks: Arc::new(StoreBlocks {
                reader,
                head: head.0,
            }),
            head: head.0,
            head_hash: head.1,
            state,
            head_entropy: OnceLock::new(),
        });
        let mut view = Self::with_snapshot(chain, snap, None)?;
        if with_state {
            view.set_state_dir(dir);
        }
        Ok(view)
    }
    /// The view of a running node; `Live::publish` moves its head.
    pub fn live(
        chain: Arc<Chain>,
        reader: StoreReader,
        head: (u64, Hash),
        state: Option<Arc<HeadState>>,
    ) -> Result<(Self, Arc<Live>), String> {
        let (events, _) = tokio::sync::broadcast::channel(256);
        let (removed, _) = tokio::sync::broadcast::channel(256);
        let (chain_tx, _) = tokio::sync::broadcast::channel(256);
        let (unlocks, _) = tokio::sync::broadcast::channel(256);
        let snap = Arc::new(Snapshot {
            blocks: Arc::new(StoreBlocks {
                reader,
                head: head.0,
            }),
            head: head.0,
            head_hash: head.1,
            state,
            head_entropy: OnceLock::new(),
        });
        let live = Arc::new(Live {
            current: RwLock::new(snap.clone()),
            events,
            removed,
            chain: chain_tx,
            unlocks,
            node: Arc::new(NodeInfo::default()),
        });
        Ok((Self::with_snapshot(chain, snap, Some(live.clone()))?, live))
    }
    /// The snapshot this call reads: the published head of a live view.
    fn snap(&self) -> Arc<Snapshot> {
        match &self.live {
            Some(l) => l.current.read().unwrap_or_else(|e| e.into_inner()).clone(),
            None => self.snap.clone(),
        }
    }

    fn load(&self, snap: &Snapshot, height: u64) -> Result<Option<Arc<BlockData>>, ViewError> {
        let Some(hash) = snap.blocks.hash_at(height)? else {
            return Ok(None);
        };
        self.load_known(snap, height, hash).map(Some)
    }
    fn load_known(
        &self,
        snap: &Snapshot,
        height: u64,
        hash: Hash,
    ) -> Result<Arc<BlockData>, ViewError> {
        if let Ok(cache) = self.blocks.lock() {
            if let Some(b) = cache.0.get(&hash) {
                return Ok(b.clone());
            }
        }
        let raw = snap.blocks.block(height, &hash)?;
        self.decode_raw(height, raw)
    }
    /// A zone block from its bytes (cached by hash).
    fn decode_raw(&self, height: u64, raw: Raw) -> Result<Arc<BlockData>, ViewError> {
        let object =
            WorkObject::decode_auxiliary(&raw.bytes, PINNED_SCHEDULE).map_err(unavailable)?;
        let header = object
            .auxiliary_work_header(PINNED_SCHEDULE)
            .map_err(unavailable)?;
        let location: Option<[u8; 2]> = Some(self.chain.location());
        let tx_hashes = object
            .transactions()
            .iter()
            .map(|t| t.id(location))
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)?;
        let etx_hashes = object
            .outbound_etxs()
            .iter()
            .map(|t| t.id(None))
            .collect::<Result<Vec<_>, _>>()
            .map_err(unavailable)?;
        let uncles = object.uncle_headers(PINNED_SCHEDULE).map_err(unavailable)?;
        if let Some(kinds) = &raw.kinds {
            if kinds.len() == uncles.len() {
                let kinds = Arc::new(kinds.iter().map(|k| uncles::from_code(*k)).collect());
                remember(&self.kinds, raw.hash, &kinds, BLOCK_CACHE * 8);
            }
        }
        let block = Arc::new(BlockData {
            hash: raw.hash,
            number: height,
            object,
            header,
            uncles,
            size: raw.bytes.len() as u64,
            order: raw.order,
            tx_hashes,
            etx_hashes,
        });
        remember(&self.blocks, raw.hash, &block, BLOCK_CACHE);
        Ok(block)
    }
    /// `TotalLogEntropy` in the zone: the child's zone parent entropy, or,
    /// for the head, the native computation.
    fn entropy_at(&self, snap: &Snapshot, block: &BlockData) -> Result<U256, ViewError> {
        if block.number < snap.head {
            if let Some(e) = snap.blocks.child_entropy(block.number, &block.hash)? {
                return Ok(e);
            }
        }
        if block.hash == snap.head_hash {
            return snap
                .head_entropy
                .get_or_init(|| self.native_entropy_of(snap, &block.hash))
                .clone()
                .map_err(ViewError::Unavailable);
        }
        self.native_entropy_of(snap, &block.hash)
            .map_err(ViewError::Unavailable)
    }
    /// `native_parent_metrics` of the replay, over the view's ancestry.
    pub fn native_entropy(&self, height: u64) -> Result<U256, String> {
        let snap = self.snap();
        let hash = snap
            .blocks
            .hash_at(height)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no block at {height}"))?;
        self.native_entropy_of(&snap, &hash)
    }
    fn native_entropy_of(&self, snap: &Snapshot, hash: &Hash) -> Result<U256, String> {
        let record = snap
            .blocks
            .record_by_hash(hash)
            .map_err(|e| e.to_string())?
            .ok_or("block record")?;
        let history = History::with_reader(Arc::new(Records(snap.blocks.clone())));
        let mut verifier = Verifier::with_shared_caches(self.caches.clone());
        let metrics = quai_consensus::validation::native_parent_metrics(
            &mut verifier,
            &history,
            &record,
            &self.chain.zone_profile(),
        )
        .map_err(|e| format!("total entropy of {}: {e}", qex::hex(hash)))?;
        use crypto_bigint::Encoding;
        Ok(metrics.total.to_be_bytes())
    }
    pub fn chain(&self) -> &Arc<Chain> {
        &self.chain
    }
    /// The decoded canonical zone record at `height` (for tests and tools).
    pub fn record(&self, height: u64) -> Result<Record, ViewError> {
        Self::record_in(&self.snap(), height)
    }
    fn record_in(snap: &Snapshot, height: u64) -> Result<Record, ViewError> {
        let hash = snap
            .blocks
            .hash_at(height)?
            .ok_or_else(|| ViewError::Unavailable(format!("zone height {height}")))?;
        snap.blocks
            .record_by_hash(&hash)?
            .ok_or_else(|| ViewError::Unavailable(format!("zone height {height}")))
    }
}
fn head_state(snap: &Snapshot) -> Result<&Arc<HeadState>, ViewError> {
    snap.state
        .as_ref()
        .ok_or_else(|| ViewError::Unavailable("state (this node runs without state)".into()))
}

/// Read every segment up to `head`: check each segment's hash, the record
/// order, every zone block hash and the parent links from genesis.
fn build_index(
    export: &Path,
    manifest: &Manifest,
    chain: &Chain,
    head: u64,
    past: PastDb,
) -> Result<Index, String> {
    let mut files = Vec::with_capacity(manifest.segments.len());
    let mut heights: Vec<Entry> = Vec::with_capacity(head as usize + 1);
    let mut by_hash = HashMap::with_capacity(head as usize + 1);
    let mut txs = vec![];
    let location = Some(chain.location());
    for (s, info) in manifest.segments.iter().enumerate() {
        if info.first_zone > head {
            files.push(None);
            continue;
        }
        if info.pruned {
            return Err(format!(
                "segment {} is pruned; the RPC view needs every block from genesis",
                info.file
            ));
        }
        let path = export.join(&info.file);
        let actual = qex::file_sha256(&path).map_err(|e| e.to_string())?;
        if actual != info.sha256 {
            return Err(format!("{}: sha256 does not match the manifest", info.file));
        }
        files.push(Some(
            File::open(&path).map_err(|e| format!("{}: {e}", info.file))?,
        ));
        let mut reader = SegmentReader::open(&path).map_err(|e| e.to_string())?;
        let mut order = 2u8;
        let mut last: Option<Context> = None;
        loop {
            let at = reader.offset();
            let Some(r) = reader.next_record().map_err(|e| e.to_string())? else {
                break;
            };
            if last.is_some_and(|c| r.context <= c) {
                return Err(format!("{}: record order at byte {at}", info.file));
            }
            match r.context {
                Context::Prime => {
                    order = 0;
                    last = Some(r.context);
                    continue;
                }
                Context::Region => {
                    order = order.min(1);
                    last = Some(r.context);
                    continue;
                }
                Context::Zone => {}
            }
            let height = heights.len() as u64;
            if r.number != height {
                return Err(format!(
                    "{}: zone record {} where {height} was expected",
                    info.file, r.number
                ));
            }
            let record =
                Record::decode(&r.block).map_err(|e| format!("zone block {height}: {e}"))?;
            if record.id() != r.hash {
                return Err(format!(
                    "zone block {height}: hash does not match its bytes"
                ));
            }
            if height == 0 {
                if r.hash != chain.genesis_hash() {
                    return Err("zone block 0 is not the genesis".into());
                }
            } else if record.parent(2).map_err(|e| e.to_string())?
                != heights[height as usize - 1].hash
            {
                return Err(format!("zone block {height}: parent link"));
            }
            for (i, tx) in record.object().transactions().iter().enumerate() {
                let id = tx
                    .id(location)
                    .map_err(|e| format!("zone block {height}: {e}"))?;
                let prefix = u64::from_be_bytes(id[..8].try_into().unwrap_or_default());
                txs.push((prefix, height, i as u32));
            }
            let block_at = at + 1 + 1 + 8 + 32 + 4;
            heights.push(Entry {
                segment: s as u16,
                block_at,
                block_len: r.block.len() as u32,
                receipts_at: block_at + r.block.len() as u64 + 4,
                receipts_len: r.receipts.len() as u32,
                order,
                hash: r.hash,
            });
            by_hash.insert(r.hash, height);
            order = 2;
            last = None;
            if height == head {
                break;
            }
        }
        if heights.len() as u64 > head {
            for _ in s + 1..manifest.segments.len() {
                files.push(None);
            }
            break;
        }
    }
    if heights.len() as u64 != head + 1 {
        return Err(format!(
            "the export holds zone blocks to {}, the checkpoint is at {head}",
            heights.len() as i64 - 1
        ));
    }
    txs.sort_unstable();
    Ok(Index {
        files,
        heights,
        by_hash,
        txs,
        receipts: manifest.receipts,
        past: Mutex::new(past),
    })
}

/// Unspent outputs per address, from a full scan of a set of at most
/// 1,000,000 entries (`UtxoState::materialize`).
fn address_index(
    utxos: &quai_execution::utxo::UtxoState,
) -> Option<HashMap<Address, Vec<OutpointData>>> {
    let all = utxos.materialize().ok()?;
    let mut out: HashMap<Address, Vec<OutpointData>> = HashMap::new();
    for (p, u) in all.iter() {
        // go-quai's index key: `common.AddressBytes(utxo.Address)`.
        out.entry(u.address.first20())
            .or_default()
            .push(OutpointData {
                tx_hash: p.hash,
                index: p.index,
                denomination: u.denomination,
                lock: trim(&u.lock),
            });
    }
    Some(out)
}
fn trim(v: &[u8]) -> Vec<u8> {
    v[v.iter().position(|b| *b != 0).unwrap_or(v.len())..].to_vec()
}

/// Decode go-quai's storage receipts (`ProtoReceiptsForStorage`).
pub fn decode_receipts(bytes: &[u8]) -> Result<Vec<ReceiptData>, String> {
    use prost::Message;
    use quai_primitives::schema::block::ProtoReceiptsForStorage;
    use quai_primitives::transaction::Transaction;
    if bytes.is_empty() {
        return Ok(vec![]);
    }
    quai_primitives::schema::preflight(".block.ProtoReceiptsForStorage", bytes)?;
    let stored = ProtoReceiptsForStorage::decode(bytes).map_err(|e| e.to_string())?;
    let fixed20 = |v: &[u8]| -> Option<Address> {
        (v.len() <= 20).then(|| {
            let mut a = [0u8; 20];
            a[20 - v.len()..].copy_from_slice(v);
            a
        })
    };
    let fixed32 = |v: &[u8]| -> Option<Hash> {
        (v.len() <= 32).then(|| {
            let mut a = [0u8; 32];
            a[32 - v.len()..].copy_from_slice(v);
            a
        })
    };
    stored
        .receipts
        .into_iter()
        .map(|r| {
            let (post_state, status) = match r.post_state_or_status.as_slice() {
                [] => (vec![], 0),
                [1] => (vec![], 1),
                v if v.len() == 32 => (v.to_vec(), 0),
                v => return Err(format!("receipt status {}", qex::hex(v))),
            };
            let logs = r
                .logs
                .unwrap_or_default()
                .logs
                .into_iter()
                .map(|l| {
                    Ok(LogData {
                        address: l
                            .address
                            .as_ref()
                            .and_then(|a| fixed20(&a.value))
                            .ok_or("log address")?,
                        topics: l
                            .topics
                            .iter()
                            .map(|t| fixed32(&t.value))
                            .collect::<Option<Vec<_>>>()
                            .ok_or("log topic")?,
                        data: l.data,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(ReceiptData {
                post_state,
                status,
                cumulative_gas_used: r.cumulative_gas_used,
                gas_used: r.gas_used,
                tx_hash: r
                    .tx_hash
                    .as_ref()
                    .and_then(|h| fixed32(&h.value))
                    .unwrap_or_default(),
                contract_address: r.contract_address.as_ref().and_then(|a| fixed20(&a.value)),
                logs,
                outbound_etxs: r
                    .outbound_etxs
                    .unwrap_or_default()
                    .transactions
                    .iter()
                    .map(|t| Transaction::decode(&t.encode_to_vec()))
                    .collect::<Result<Vec<_>, _>>()?,
            })
        })
        .collect()
}

/// The account state of the head world.
struct WorldState(quai_execution::evm::state::World);
fn read_error(e: impl std::fmt::Display) -> ViewError {
    ViewError::Unavailable(format!("state read: {e}"))
}
impl AccountState for WorldState {
    fn balance(&self, address: &Address) -> Result<U256, ViewError> {
        Ok(self
            .0
            .account(address)
            .map_err(read_error)?
            .map(|a| quai_execution::evm::word::bytes(a.balance))
            .unwrap_or([0; 32]))
    }
    fn nonce(&self, address: &Address) -> Result<u64, ViewError> {
        Ok(self
            .0
            .account(address)
            .map_err(read_error)?
            .map_or(0, |a| a.nonce))
    }
    fn code(&self, address: &Address) -> Result<Vec<u8>, ViewError> {
        Ok(self
            .0
            .account(address)
            .map_err(read_error)?
            .map(|a| a.code.clone())
            .unwrap_or_default())
    }
    fn storage(&self, address: &Address, key: &Hash) -> Result<Hash, ViewError> {
        match self.0.account(address).map_err(read_error)? {
            None => Ok([0; 32]),
            Some(a) => a.get(*key).map_err(read_error),
        }
    }
    fn size(&self, address: &Address) -> Result<U256, ViewError> {
        Ok(self
            .0
            .account(address)
            .map_err(read_error)?
            .map(|a| quai_execution::evm::word::bytes(a.storage_size))
            .unwrap_or([0; 32]))
    }
    fn proof(
        &self,
        address: &Address,
        keys: &[Hash],
    ) -> Result<quai_rpc::view::WorldProof, ViewError> {
        self.0.proof(address, keys).map_err(read_error)
    }
}
/// The state behind an empty EVM root.
struct EmptyState;
impl AccountState for EmptyState {
    fn balance(&self, _: &Address) -> Result<U256, ViewError> {
        Ok([0; 32])
    }
    fn nonce(&self, _: &Address) -> Result<u64, ViewError> {
        Ok(0)
    }
    fn code(&self, _: &Address) -> Result<Vec<u8>, ViewError> {
        Ok(vec![])
    }
    fn storage(&self, _: &Address, _: &Hash) -> Result<Hash, ViewError> {
        Ok([0; 32])
    }
    fn size(&self, _: &Address) -> Result<U256, ViewError> {
        Ok([0; 32])
    }
    fn proof(&self, _: &Address, _: &[Hash]) -> Result<quai_rpc::view::WorldProof, ViewError> {
        // An empty trie: no nodes, no account.
        Ok(quai_rpc::view::WorldProof {
            account: vec![],
            storage: None,
        })
    }
}

/// What `state.New` finds for a block's roots on a node that keeps only the
/// head state: the head state, an empty state, or go-quai's error.
pub(crate) enum StateAt {
    Head,
    Empty,
}
impl ExportView {
    fn state_in(snap: &Snapshot, block: &BlockData) -> Result<StateAt, ViewError> {
        let body = block.object.header().wire();
        let evm = hash_of(&body.evm_root);
        let etx = hash_of(&body.etx_set_root);
        let empty = |h: &Hash| *h == [0; 32] || *h == EMPTY_ROOT;
        let missing =
            |h: &Hash| ViewError::Go(format!("missing trie node {} (path )", &qex::hex(h)[2..]));
        if empty(&evm) && empty(&etx) {
            return Ok(StateAt::Empty);
        }
        let head = head_state(snap)?;
        if !(empty(&evm) || evm == head.evm_root) {
            return Err(missing(&evm));
        }
        if !(empty(&etx) || etx == head.etx_root) {
            return Err(missing(&etx));
        }
        Ok(if empty(&evm) {
            StateAt::Empty
        } else {
            StateAt::Head
        })
    }
    fn world_at(
        &self,
        snap: &Snapshot,
        block: &BlockData,
    ) -> Result<quai_execution::evm::state::World, ViewError> {
        match Self::state_in(snap, block) {
            Ok(StateAt::Empty) => Ok(quai_execution::evm::state::World::default()),
            Ok(StateAt::Head) => Ok(head_state(snap)?.state.world.clone()),
            Err(err) => {
                if let Some(dir) = &self.state_dir {
                    if let Some(entry) = snap.blocks.state_version_at(block.number)? {
                        let body = block.object.header().wire();
                        let evm = hash_of(&body.evm_root);
                        if entry.commitment.roots.evm == evm {
                            let store = StateStore::open(
                                dir,
                                store::profile(
                                    &self.chain.spec().environment,
                                    &self.chain.genesis_hash(),
                                ),
                                entry.file_height,
                                entry.version,
                                entry.commitment,
                            )
                            .map_err(read_error)?;
                            let restored = store.restore().map_err(read_error)?;
                            return Ok(restored.world);
                        }
                    }
                }
                Err(err)
            }
        }
    }
}

impl ChainView for ExportView {
    fn chain_id(&self) -> u64 {
        self.chain.spec().chain_id
    }
    fn location(&self) -> [u8; 2] {
        self.chain.location()
    }
    fn head(&self) -> u64 {
        self.snap().head
    }
    fn block_by_number(&self, number: u64) -> Result<Option<Arc<BlockData>>, ViewError> {
        let snap = self.snap();
        if number > snap.head {
            return Ok(None);
        }
        self.load(&snap, number)
    }
    fn block_by_hash(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        let snap = self.snap();
        match snap.blocks.height_of(hash)? {
            Some(h) => self.load_known(&snap, h, *hash).map(Some),
            None => Ok(None),
        }
    }
    fn uncle_kinds(&self, block: &BlockData) -> Result<Arc<Vec<UncleKind>>, ViewError> {
        if let Ok(cache) = self.kinds.lock() {
            if let Some(k) = cache.0.get(&block.hash) {
                return Ok(k.clone());
            }
        }
        let kinds = Arc::new(
            block
                .uncles
                .iter()
                .map(|h| uncles::kind(h, uncles::pow_hash(&self.caches, h)))
                .collect::<Vec<_>>(),
        );
        remember(&self.kinds, block.hash, &kinds, BLOCK_CACHE * 8);
        Ok(kinds)
    }
    fn total_entropy(&self, block: &BlockData) -> Result<U256, ViewError> {
        self.entropy_at(&self.snap(), block)
    }
    fn may_have_logs(&self, number: u64) -> Result<bool, ViewError> {
        let snap = self.snap();
        if let Some(bloom) = snap.blocks.bloom_at(number)? {
            return Ok(!bloom.is_empty());
        }
        if !snap.blocks.receipts_kept() {
            return Ok(true);
        }
        let Some(hash) = snap.blocks.hash_at(number)? else {
            return Ok(false);
        };
        match snap.blocks.receipts(number, &hash)? {
            None => Ok(true),
            Some(bytes) if bytes.is_empty() => Ok(false),
            Some(bytes) => Ok(decode_receipts(&bytes)
                .map_err(|e| ViewError::Unavailable(format!("stored receipts: {e}")))?
                .iter()
                .any(|r| !r.logs.is_empty())),
        }
    }
    fn bloom_at(&self, number: u64) -> Result<Option<[u8; 256]>, ViewError> {
        let snap = self.snap();
        match snap.blocks.bloom_at(number)? {
            Some(raw) if raw.len() == 256 => Ok(raw.try_into().ok()),
            Some(raw) if raw.is_empty() => Ok(Some([0u8; 256])),
            _ => Ok(None),
        }
    }
    fn transaction(&self, hash: &Hash) -> Result<Option<TxPosition>, ViewError> {
        let snap = self.snap();
        // The lookup entry of the newest block wins, as in rawdb.
        let mut found = None;
        for (height, i) in snap.blocks.tx_candidates(hash)? {
            let Some(block) = self.load(&snap, height)? else {
                continue;
            };
            if block.tx_hashes.get(i as usize) == Some(hash) {
                match found {
                    Some(TxPosition { block_number, .. }) if block_number > height => {}
                    _ => {
                        // rawdb.ReadTransaction returns the first match in the block.
                        let index = block
                            .tx_hashes
                            .iter()
                            .position(|h| h == hash)
                            .unwrap_or(i as usize);
                        found = Some(TxPosition {
                            block_hash: block.hash,
                            block_number: height,
                            index: index as u64,
                        })
                    }
                }
            }
        }
        Ok(found)
    }
    fn receipts(&self, block: &BlockData) -> Result<Arc<Vec<ReceiptData>>, ViewError> {
        let snap = self.snap();
        let Some(bytes) = snap.blocks.receipts(block.number, &block.hash)? else {
            return Err(ViewError::Unavailable(snap.blocks.no_receipts().into()));
        };
        decode_receipts(&bytes)
            .map(Arc::new)
            .map_err(|e| ViewError::Unavailable(format!("stored receipts: {e}")))
    }
    fn state(&self, block: &BlockData) -> Result<Arc<dyn AccountState>, ViewError> {
        let snap = self.snap();
        match Self::state_in(&snap, block) {
            Ok(StateAt::Empty) => Ok(Arc::new(EmptyState)),
            Ok(StateAt::Head) => Ok(Arc::new(WorldState(head_state(&snap)?.state.world.clone()))),
            Err(err) => match self.world_at(&snap, block) {
                Ok(world) => Ok(Arc::new(WorldState(world))),
                Err(_) => Err(err),
            },
        }
    }
    fn utxo(&self, tx_hash: &Hash, index: u16) -> Result<Option<UtxoData>, ViewError> {
        let snap = self.snap();
        let point = OutPoint {
            hash: *tx_hash,
            index,
        };
        Ok(head_state(&snap)?
            .state
            .utxos
            .get(&point)
            .map_err(read_error)?
            .map(|u| UtxoData {
                denomination: u.denomination,
                address: u.address.last20(),
                lock: trim(&u.lock),
            }))
    }
    fn outpoints_by_address(&self, address: &Address) -> Result<Vec<OutpointData>, ViewError> {
        let snap = self.snap();
        let head = head_state(&snap)?;
        if let Some(points) = snap.blocks.outpoints(address)? {
            // The index can hold outputs spent since; the UTXO set decides.
            let mut out = vec![];
            for point in points {
                if let Some(u) = head.state.utxos.get(&point).map_err(read_error)? {
                    out.push(OutpointData {
                        tx_hash: point.hash,
                        index: point.index,
                        denomination: u.denomination,
                        lock: trim(&u.lock),
                    });
                }
            }
            return Ok(out);
        }
        match &head.by_address {
            Some(m) => Ok(m.get(address).cloned().unwrap_or_default()),
            None => Err(ViewError::Unavailable(
                "an address index for more than 1,000,000 UTXOs".into(),
            )),
        }
    }
    fn outpoints_by_address_and_range(
        &self,
        address: &Address,
        start: u32,
        end: u32,
    ) -> Result<Vec<OutpointData>, ViewError> {
        let snap = self.snap();
        let head = head_state(&snap)?;
        let mut out = vec![];
        // The genesis creates no UTXOs; the replay records heights from 1.
        for height in (start as u64).max(1)..=(end as u64).min(snap.head) {
            for point in snap.blocks.created(height)? {
                let Some(u) = head.state.utxos.get(&point).map_err(read_error)? else {
                    continue;
                };
                // go-quai's chain indexer keys by the first 16 bytes of
                // `[20]byte(out.Address)`, the first bytes of the address.
                if u.address.as_bytes()[..16] != address[..16] {
                    continue;
                }
                out.push(OutpointData {
                    tx_hash: point.hash,
                    index: point.index,
                    denomination: u.denomination,
                    lock: trim(&u.lock),
                });
            }
        }
        Ok(out)
    }
    fn call(&self, block: &BlockData, request: &CallRequest) -> Result<CallResult, CallError> {
        let snap = self.snap();
        let world = self.world_at(&snap, block)?;
        rpc_call::call(self, world, block, request)
    }
    fn call_cancellable(
        &self,
        block: &BlockData,
        request: &CallRequest,
        cancel: &Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<CallResult, CallError> {
        let snap = self.snap();
        let world = self.world_at(&snap, block)?;
        rpc_call::call_cancellable(self, world, block, request, Some(cancel.clone()))
    }
    fn head_events(&self) -> Option<tokio::sync::broadcast::Receiver<Hash>> {
        self.live.as_ref().map(|l| l.events.subscribe())
    }
    fn removed_logs(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<Arc<quai_rpc::view::RemovedLogs>>> {
        self.live.as_ref().map(|l| l.removed.subscribe())
    }
    fn chain_events(&self) -> Option<tokio::sync::broadcast::Receiver<Hash>> {
        self.live.as_ref().map(|l| l.chain.subscribe())
    }
    fn unlock_events(&self) -> Option<tokio::sync::broadcast::Receiver<Arc<Unlocks>>> {
        self.live.as_ref().map(|l| l.unlocks.subscribe())
    }
    fn rolling_fee_info(&self) -> Option<[U256; 3]> {
        self.node.as_ref().and_then(|n| n.rolling_fees())
    }
    fn peer_counts(&self) -> PeerCounts {
        self.node
            .as_ref()
            .map(|n| n.peer_counts())
            .unwrap_or_default()
    }
    fn pinned(&self) -> Option<Arc<dyn ChainView>> {
        self.live.as_ref()?;
        Some(Arc::new(Self {
            chain: self.chain.clone(),
            snap: self.snap(),
            live: None,
            node: self.node.clone(),
            caches: self.caches.clone(),
            blocks: self.blocks.clone(),
            kinds: self.kinds.clone(),
            pool: self.pool.clone(),
            miner: self.miner.clone(),
            state_dir: self.state_dir.clone(),
        }))
    }
    fn submit_transaction(&self, raw: &[u8]) -> Result<Hash, ViewError> {
        match &self.pool {
            Some(p) => p.submit(raw).map_err(ViewError::Go),
            None => Err(ViewError::Unavailable(
                "this node has no transaction pool".into(),
            )),
        }
    }
    fn pool_nonce(&self, address: &Address) -> Option<u64> {
        Some(self.pool.as_ref()?.rpc_nonce(address))
    }
    fn pool_transaction(&self, hash: &Hash) -> Option<quai_rpc::view::PoolTransaction> {
        self.pool.as_ref()?.rpc_transaction(hash)
    }
    fn pool_content(&self, from: Option<&Address>) -> Option<quai_rpc::view::PoolContent> {
        Some(self.pool.as_ref()?.rpc_content(from))
    }
    fn pool_stats(&self) -> Option<(u64, u64, u64)> {
        Some(self.pool.as_ref()?.rpc_stats())
    }
    fn canonical_hash(&self, number: u64) -> Result<Option<Hash>, ViewError> {
        self.snap().blocks.hash_at(number)
    }
    fn block_or_candidate(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        if let Some(b) = self.block_by_hash(hash)? {
            return Ok(Some(b));
        }
        // A block a reorg left behind: the store keeps its bytes.
        let snap = self.snap();
        if let Some(h) = snap.blocks.candidate_height(hash)? {
            return self.load_known(&snap, h, *hash).map(Some);
        }
        // A block the node appended that the head choice did not take.
        match self.node.as_ref().and_then(|n| n.appended(hash)) {
            Some((number, raw)) => self.decode_raw(number, raw).map(Some),
            None => Ok(None),
        }
    }
    fn coinbase_lockups(
        &self,
        owner: &Address,
        beneficiary: &Address,
    ) -> Result<Vec<quai_rpc::view::CoinbaseLockup>, ViewError> {
        use quai_execution::lockup::Key;
        let snap = self.snap();
        let head = head_state(&snap)?;
        let all = head.state.world.lockups().map_err(read_error)?;
        let key = |lock, epoch| Key {
            owner: *owner,
            beneficiary: *beneficiary,
            lock,
            epoch,
        };
        Ok(all
            .range(key(0, 0)..=key(u8::MAX, u32::MAX))
            .map(|(k, l)| quai_rpc::view::CoinbaseLockup {
                lockup_byte: k.lock,
                epoch: k.epoch,
                balance: quai_execution::evm::word::bytes(l.balance),
                unlock_height: l.unlock_height,
                elements: l.elements,
                delegate: l.delegate,
            })
            .collect())
    }
    fn utxo_set_size(&self, block: &BlockData) -> Result<u64, ViewError> {
        let snap = self.snap();
        match &snap.state {
            Some(s) if block.hash == snap.head_hash => Ok(s.state.utxos.consensus_size()),
            Some(_) => Err(ViewError::Unavailable(
                "the UTXO set size of a block other than the head".into(),
            )),
            None => Err(ViewError::Unavailable(
                "the UTXO set size (this node runs without state)".into(),
            )),
        }
    }
    fn spent_utxos(&self, block: &BlockData) -> Result<Vec<SpentUtxo>, ViewError> {
        use quai_primitives::transaction::Kind;
        let snap = self.snap();
        if snap.blocks.hash_at(block.number)? != Some(block.hash) {
            return Err(ViewError::Unavailable(
                "the spent outputs of a block that is not canonical".into(),
            ));
        }
        let Some(spent) = snap.blocks.spent(block.number)? else {
            return Err(ViewError::Unavailable(
                "the spent outputs of a block (this view keeps no UTXO journal)".into(),
            ));
        };
        // go-quai's journals: the inputs of the block's Qi transactions,
        // then the trimmed outputs.
        let owners: BTreeMap<OutPoint, Address> = spent.iter().cloned().collect();
        let mut order: Vec<OutPoint> = vec![];
        for tx in block.object.transactions() {
            if let Kind::Qi(q) = tx.kind() {
                for i in &q.inputs {
                    let p = OutPoint {
                        hash: i.hash,
                        index: i.index,
                    };
                    if owners.contains_key(&p) && !order.contains(&p) {
                        order.push(p);
                    }
                }
            }
        }
        for (p, _) in &spent {
            if !order.contains(p) {
                order.push(*p);
            }
        }
        let mut out = vec![];
        for p in order {
            // The entry: the block that created the output keeps it.
            let mut entry = None;
            for (height, _) in snap.blocks.tx_candidates(&p.hash)? {
                if let Some(list) = snap.blocks.created_entries(height)? {
                    if let Some(c) = list.into_iter().find(|c| c.point == p) {
                        entry = Some(c);
                        break;
                    }
                }
            }
            let c = entry.ok_or_else(|| {
                ViewError::Unavailable(format!(
                    "the entry of spent output {}:{}",
                    qex::hex(&p.hash),
                    p.index
                ))
            })?;
            out.push(SpentUtxo {
                tx_hash: p.hash,
                index: p.index,
                denomination: c.denomination,
                address: owners
                    .get(&p)
                    .copied()
                    .unwrap_or_else(|| c.address.first20()),
                lock: trim(&c.lock),
            });
        }
        Ok(out)
    }
    fn locked_quai(&self, address: &Address) -> Result<U256, ViewError> {
        self.snap().blocks.locked_quai(address)
    }
    fn supply_analytics(
        &self,
        block_hash: &Hash,
    ) -> Result<Option<quai_rpc::view::SupplyAnalytics>, ViewError> {
        self.snap().blocks.supply(block_hash)
    }
    fn block_for_workshare(&self, ws_hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        let snap = self.snap();
        if let Some(block_hash) = snap.blocks.block_for_workshare(ws_hash)? {
            if let Some(b) = self.block_or_candidate(&block_hash)? {
                return Ok(Some(b));
            }
        }
        // Fallback scan over recent blocks when index-address-utxos was off.
        let start = snap.head.saturating_sub(32);
        for n in (start..=snap.head).rev() {
            let Ok(Some(b)) = self.load(&snap, n) else {
                continue;
            };
            if b.hash == *ws_hash
                || b.uncles
                    .iter()
                    .any(|u| u.block_hash().ok() == Some(*ws_hash))
            {
                return Ok(Some(b));
            }
        }
        Ok(None)
    }
    fn quai_header_for_donor(
        &self,
        donor_hash: &Hash,
    ) -> Result<Option<Arc<BlockData>>, ViewError> {
        let snap = self.snap();
        if let Some(block_hash) = snap.blocks.block_for_donor(donor_hash)? {
            if let Some(b) = self.block_or_candidate(&block_hash)? {
                return Ok(Some(b));
            }
        }
        let matches_header = |hdr: &quai_primitives::Header| -> bool {
            let Some(aux_bytes) = hdr.wire().aux_pow.as_deref() else {
                return false;
            };
            let Ok(aux) = quai_consensus::auxiliary::Auxiliary::decode(aux_bytes) else {
                return false;
            };
            let h = aux.header();
            if h.len() < 80 {
                return false;
            }
            let hash = match aux.id() {
                2 | 3 => quai_consensus::donor::sha256d(h).ok(),
                4 => quai_consensus::donor::scrypt(h).ok(),
                _ => None,
            };
            hash == Some(*donor_hash)
        };
        let start = snap.head.saturating_sub(32);
        for n in (start..=snap.head).rev() {
            let Ok(Some(b)) = self.load(&snap, n) else {
                continue;
            };
            if matches_header(&b.header) || b.uncles.iter().any(matches_header) {
                return Ok(Some(b));
            }
        }
        Ok(None)
    }
    fn compute_kawpow_pow_hash(&self, header: &quai_primitives::Header) -> Result<Hash, ViewError> {
        let mut verifier = Verifier::with_shared_caches(self.caches.clone());
        let proof = verifier
            .verify_header_proof(header)
            .map_err(|e| ViewError::Go(format!("pow hash: {e}")))?;
        Ok(proof.hash())
    }
    // --- Mining, AuxPoW & Workshare ---
    fn is_mining(&self) -> bool {
        self.miner.as_ref().is_some_and(|m| m.is_mining())
    }
    fn hashrate(&self) -> u64 {
        self.miner.as_ref().map_or(0, |m| m.hashrate())
    }
    fn primary_coinbase(&self) -> Result<Address, ViewError> {
        Ok(self
            .miner
            .as_ref()
            .map_or([0u8; 20], |m| m.primary_coinbase()))
    }
    fn miner_start(&self) -> Result<bool, ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        m.start_mining();
        Ok(true)
    }
    fn miner_set_extra(&self, extra: &[u8]) -> Result<bool, ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        m.set_extra(extra.to_vec()).map_err(ViewError::Go)?;
        Ok(true)
    }
    fn miner_set_gas_limit(&self, limit: u64) -> Result<bool, ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        m.set_gas_limit(limit);
        Ok(true)
    }
    fn miner_set_gas_price(&self, price: U256) -> Result<bool, ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        use crypto_bigint::Encoding as _;
        m.set_gas_price(crypto_bigint::U256::from_be_bytes(price));
        Ok(true)
    }
    fn miner_set_lockup_byte(&self, lock: u8) -> Result<bool, ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        m.set_lockup_byte(lock).map_err(ViewError::Go)?;
        Ok(true)
    }
    fn miner_set_miner_preference(&self, pref: f64) -> Result<bool, ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        m.set_miner_preference(pref).map_err(ViewError::Go)?;
        Ok(true)
    }
    fn miner_set_primary_coinbase(&self, addr: Address) -> Result<bool, ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        m.set_primary_coinbase(addr);
        Ok(true)
    }
    fn miner_set_recommit_interval(&self, ms: u64) -> Result<(), ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        m.set_recommit_interval(ms);
        Ok(())
    }
    fn get_block_template(
        &self,
        params: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        let parsed = crate::miner::BlockTemplateParams::from_json(params).map_err(ViewError::Go)?;
        m.get_block_template(&parsed).map_err(ViewError::Go)
    }
    fn get_pending_header(&self) -> Result<Vec<u8>, ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        let wo = m
            .generate_pending_header(crate::miner::POW_KAWPOW, None, &[], None)
            .map_err(ViewError::Go)?;
        Ok(wo.encode())
    }
    fn get_pending_workshares(&self) -> Result<serde_json::Value, ViewError> {
        Ok(self.miner.as_ref().map_or_else(
            || serde_json::json!({"kawpow": [], "sha": [], "scrypt": []}),
            |m| m.pending_workshares_json(),
        ))
    }
    fn submit_block(&self, raw: &[u8], pow_id: u8) -> Result<serde_json::Value, ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        let out = m.submit_block(raw, pow_id).map_err(ViewError::Go)?;
        Ok(serde_json::json!({
            "hash": quai_primitives::encode_hex(&out.hash),
            "number": format!("0x{:x}", out.number),
            "status": format!("0x{:x}", out.status),
        }))
    }
    fn receive_mined_header(&self, raw: &[u8]) -> Result<(), ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        m.receive_mined_header(raw).map_err(ViewError::Go)
    }
    fn receive_raw_workshare(&self, raw: &[u8]) -> Result<(), ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        m.receive_raw_workshare(raw)
            .map(|_| ())
            .map_err(ViewError::Go)
    }
    fn receive_workshare(&self, workshare: &serde_json::Value) -> Result<(), ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        if let Some(hex_str) = workshare.as_str() {
            let raw = quai_primitives::decode_hex(hex_str)
                .map_err(|e| ViewError::Go(format!("invalid hex workshare: {e}")))?;
            return m
                .receive_raw_workshare(&raw)
                .map(|_| ())
                .map_err(ViewError::Go);
        }
        let header = decode_rpc_workshare_header(workshare).map_err(ViewError::Go)?;
        m.receive_workshare_header(header)
            .map(|_| ())
            .map_err(ViewError::Go)
    }
    fn receive_sub_workshare(&self, raw: &[u8]) -> Result<(), ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        m.receive_sub_workshare(raw)
            .map(|_| ())
            .map_err(ViewError::Go)
    }
    fn calc_order(&self, raw: &[u8]) -> Result<u8, ViewError> {
        if let Some(m) = &self.miner {
            return m.calc_order(raw).map_err(ViewError::Go);
        }
        let header = quai_primitives::Header::decode(raw, PINNED_SCHEDULE)
            .or_else(|_| {
                WorkObject::decode_auxiliary(raw, PINNED_SCHEDULE)
                    .and_then(|o| o.auxiliary_work_header(PINNED_SCHEDULE))
            })
            .map_err(|e| ViewError::Go(format!("decode header: {e}")))?;
        let mut verifier = Verifier::with_shared_caches(self.caches.clone());
        let proof = verifier
            .verify_header_proof(&header)
            .map_err(|e| ViewError::Go(format!("pow: {e}")))?;
        let diff = quai_consensus::share_rules::uint(
            header.wire().difficulty.as_deref().unwrap_or_default(),
        )
        .map_err(|e| ViewError::Go(e.to_string()))?;
        let target = crate::miner::difficulty_to_target(diff);
        let pow_uint = crypto_bigint::U256::from_be_bytes(proof.hash());
        if pow_uint <= target {
            Ok(2)
        } else {
            Err(ViewError::Go(
                "header does not meet block difficulty".into(),
            ))
        }
    }
    fn workshare_threshold(&self) -> u16 {
        self.miner
            .as_ref()
            .map_or(quai_primitives::spec::WORKSHARES_THRESHOLD_DIFF, |m| {
                m.workshare_threshold()
            })
    }
    fn workshare_p2p_threshold(&self) -> u16 {
        self.miner
            .as_ref()
            .map_or(quai_primitives::spec::WORKSHARE_P2P_THRESHOLD_DIFF, |m| {
                m.workshare_p2p_threshold()
            })
    }
    fn set_workshare_p2p_threshold(&self, threshold: u16) -> Result<(), ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        m.set_workshare_p2p_threshold(threshold)
            .map_err(ViewError::Go)
    }
    fn submit_aux_template(&self, raw: &[u8]) -> Result<(), ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Unavailable(
                "mining is not configured on this node".into(),
            ));
        };
        m.submit_aux_template(raw).map_err(ViewError::Go)
    }
    fn sign_aux_template(
        &self,
        template: &[u8],
        other_pubkey: &[u8],
        other_nonce: &[u8],
    ) -> Result<serde_json::Value, ViewError> {
        crate::miner::sign_aux_template(template, other_pubkey, other_nonce).map_err(ViewError::Go)
    }
    fn subsidy_chain_height(&self) -> Result<serde_json::Value, ViewError> {
        let Some(m) = &self.miner else {
            return Err(ViewError::Go("no subsidy chain height found".into()));
        };
        m.subsidy_chain_height().map_err(ViewError::Go)
    }
    fn workshare_lru_dump(&self, limit: usize) -> Result<serde_json::Value, ViewError> {
        Ok(self.miner.as_ref().map_or_else(
            || serde_json::json!({"receivedCount": 0, "minedCount": 0, "received": [], "mined": []}),
            |m| m.workshare_lru_dump(limit),
        ))
    }
    fn pending_header_events(&self) -> Option<tokio::sync::broadcast::Receiver<Vec<u8>>> {
        self.miner.as_ref().map(|m| m.subscribe_pending_header())
    }
    fn template_update_events(&self) -> Option<tokio::sync::broadcast::Receiver<u8>> {
        self.miner.as_ref().map(|m| m.subscribe_template_updates())
    }
    fn workshare_events(&self) -> Option<tokio::sync::broadcast::Receiver<Arc<WorkObject>>> {
        self.miner.as_ref().map(|m| m.subscribe_workshares())
    }
    // --- Admin, Debug & Sync ---
    fn syncing(&self) -> serde_json::Value {
        self.node
            .as_ref()
            .and_then(|n| n.syncing_status())
            .unwrap_or(serde_json::Value::Bool(false))
    }
    fn admin_node_info(&self) -> Result<serde_json::Value, ViewError> {
        if let Some(info) = self.node.as_ref().and_then(|n| n.p2p_node_info()) {
            return Ok(info);
        }
        Ok(serde_json::json!({
            "name": format!("quai-rust/{}", env!("CARGO_PKG_VERSION")),
            "protocols": {
                "quai": {
                    "network": self.chain.spec().environment,
                    "chainId": self.chain.spec().chain_id,
                    "location": self.chain.location(),
                    "genesis": quai_primitives::encode_hex(&self.chain.genesis_hash()),
                    "head": self.head(),
                }
            }
        }))
    }
    fn admin_peers(&self) -> Result<serde_json::Value, ViewError> {
        Ok(self
            .node
            .as_ref()
            .and_then(|n| n.p2p_peers_info())
            .unwrap_or_else(|| serde_json::json!([])))
    }
    fn admin_export_chain(
        &self,
        file: &str,
        first: Option<u64>,
        last: Option<u64>,
    ) -> Result<bool, ViewError> {
        use std::io::Write;
        let path = Path::new(file);
        if path.exists() {
            return Err(ViewError::Go(format!(
                "refusing to overwrite existing file {}",
                path.display()
            )));
        }
        let snap = self.snap();
        let start = first.unwrap_or(0);
        let end = last.unwrap_or(snap.head).min(snap.head);
        if start > end {
            return Err(ViewError::Go(format!("first block {start} > last {end}")));
        }
        let mut out = File::create(path)
            .map_err(|e| ViewError::Go(format!("create {}: {e}", path.display())))?;
        // Magic header + length-prefixed canonical protobuf WorkObjects.
        out.write_all(b"QBLK1\0\0\0")
            .map_err(|e| ViewError::Go(format!("write export: {e}")))?;
        for n in start..=end {
            let Some(hash) = snap.blocks.hash_at(n)? else {
                continue;
            };
            let raw = snap.blocks.block(n, &hash)?;
            out.write_all(&(raw.bytes.len() as u32).to_be_bytes())
                .map_err(|e| ViewError::Go(format!("write export: {e}")))?;
            out.write_all(&raw.bytes)
                .map_err(|e| ViewError::Go(format!("write export: {e}")))?;
        }
        out.flush()
            .map_err(|e| ViewError::Go(format!("flush export: {e}")))?;
        Ok(true)
    }
    fn admin_import_chain(&self, file: &str) -> Result<bool, ViewError> {
        use std::io::Read;
        let path = Path::new(file);
        let mut f =
            File::open(path).map_err(|e| ViewError::Go(format!("open {}: {e}", path.display())))?;
        let mut magic = [0u8; 8];
        f.read_exact(&mut magic)
            .map_err(|e| ViewError::Go(format!("read export header: {e}")))?;
        if &magic != b"QBLK1\0\0\0" {
            return Err(ViewError::Go(
                "unsupported chain export format (expected QBLK1)".into(),
            ));
        }
        let mut len_buf = [0u8; 4];
        loop {
            match f.read_exact(&mut len_buf) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(ViewError::Go(format!("read export entry: {e}"))),
            }
            let len = u32::from_be_bytes(len_buf) as usize;
            if len > 16 * 1024 * 1024 {
                return Err(ViewError::Go("export block exceeds 16 MiB limit".into()));
            }
            let mut raw = vec![0u8; len];
            f.read_exact(&mut raw)
                .map_err(|e| ViewError::Go(format!("read export block: {e}")))?;
            let _ = WorkObject::decode_auxiliary(&raw, PINNED_SCHEDULE)
                .map_err(|e| ViewError::Go(format!("invalid block in export: {e}")))?;
        }
        Ok(true)
    }
    fn debug_dump_block(
        &self,
        block: &BlockData,
        start: &[u8],
        max_results: usize,
        no_code: bool,
        no_storage: bool,
        _incompletes: bool,
    ) -> Result<serde_json::Value, ViewError> {
        let snap = self.snap();
        let world = self.world_at(&snap, block)?;
        let root = world.root().map_err(read_error)?;
        let accounts_map = world.accounts().map_err(read_error)?;
        let mut by_hash: Vec<(Hash, Address, Arc<quai_execution::evm::state::Account>)> =
            accounts_map
                .iter()
                .map(|(addr, acc)| {
                    (
                        quai_primitives::transaction::keccak(addr),
                        *addr,
                        acc.clone(),
                    )
                })
                .collect();
        by_hash.sort_by_key(|a| a.0);
        let mut start_key = [0u8; 32];
        let copy_n = start.len().min(32);
        start_key[..copy_n].copy_from_slice(&start[..copy_n]);
        let limit = if max_results == 0 {
            usize::MAX
        } else {
            max_results
        };
        let mut out_accounts = serde_json::Map::new();
        let mut next: Option<String> = None;
        for (addr_hash, addr, acc) in by_hash {
            if addr_hash < start_key {
                continue;
            }
            if out_accounts.len() >= limit {
                next = Some(quai_primitives::encode_hex(&addr_hash));
                break;
            }
            let storage_root = acc.storage_root().map_err(read_error)?;
            let code_hash = quai_primitives::transaction::keccak(&acc.code);
            let bal_bytes = quai_execution::evm::word::bytes(acc.balance);
            let bal = crypto_bigint::U256::from_be_bytes(bal_bytes).to_string();
            let mut obj = serde_json::Map::new();
            obj.insert("balance".into(), serde_json::Value::String(bal));
            obj.insert("nonce".into(), serde_json::json!(acc.nonce));
            obj.insert(
                "root".into(),
                serde_json::Value::String(quai_primitives::encode_hex(&storage_root)),
            );
            obj.insert(
                "codeHash".into(),
                serde_json::Value::String(quai_primitives::encode_hex(&code_hash)),
            );
            if !no_code && !acc.code.is_empty() {
                obj.insert(
                    "code".into(),
                    serde_json::Value::String(quai_primitives::encode_hex(&acc.code)),
                );
            }
            if !no_storage {
                if let Ok(slots) = acc.materialize_storage() {
                    if !slots.is_empty() {
                        let mut st = serde_json::Map::new();
                        for (k, v) in slots.iter() {
                            st.insert(
                                quai_primitives::encode_hex(k),
                                serde_json::Value::String(quai_primitives::encode_hex(v)),
                            );
                        }
                        obj.insert("storage".into(), serde_json::Value::Object(st));
                    }
                }
            }
            obj.insert(
                "key".into(),
                serde_json::Value::String(quai_primitives::encode_hex(&addr_hash)),
            );
            out_accounts.insert(
                quai_primitives::encode_hex(&addr),
                serde_json::Value::Object(obj),
            );
        }
        let mut res = serde_json::json!({
            "root": quai_primitives::encode_hex(&root),
            "accounts": out_accounts,
        });
        if let Some(n) = next {
            if let Some(map) = res.as_object_mut() {
                map.insert("next".into(), serde_json::Value::String(n));
            }
        }
        Ok(res)
    }
    fn debug_storage_range_at(
        &self,
        block: &BlockData,
        _tx_index: usize,
        contract: &Address,
        key_start: &[u8],
        max_result: usize,
    ) -> Result<serde_json::Value, ViewError> {
        let snap = self.snap();
        let world = self.world_at(&snap, block)?;
        let Some(acc) = world.account(contract).map_err(read_error)? else {
            return Ok(serde_json::json!({"storage": {}, "nextKey": null}));
        };
        let slots = acc.materialize_storage().map_err(read_error)?;
        let mut hashed: Vec<(Hash, Hash, Hash)> = slots
            .iter()
            .map(|(k, v)| (quai_primitives::transaction::keccak(k), *k, *v))
            .collect();
        hashed.sort_by_key(|a| a.0);
        let mut start_key = [0u8; 32];
        let copy_n = key_start.len().min(32);
        start_key[..copy_n].copy_from_slice(&key_start[..copy_n]);
        let limit = if max_result == 0 { 256 } else { max_result };
        let mut out = serde_json::Map::new();
        let mut next_key: Option<String> = None;
        for (h, k, v) in hashed {
            if h < start_key {
                continue;
            }
            if out.len() >= limit {
                next_key = Some(quai_primitives::encode_hex(&h));
                break;
            }
            out.insert(
                quai_primitives::encode_hex(&h),
                serde_json::json!({
                    "key": quai_primitives::encode_hex(&k),
                    "value": quai_primitives::encode_hex(&v),
                }),
            );
        }
        Ok(serde_json::json!({
            "storage": out,
            "nextKey": next_key,
        }))
    }
    fn debug_modified_accounts(
        &self,
        start: &BlockData,
        end: &BlockData,
    ) -> Result<Vec<Address>, ViewError> {
        let snap = self.snap();
        if let (Ok(w1), Ok(w2)) = (self.world_at(&snap, start), self.world_at(&snap, end)) {
            let a1 = w1.accounts().map_err(read_error)?;
            let a2 = w2.accounts().map_err(read_error)?;
            let mut changed = std::collections::BTreeSet::new();
            for (addr, acc) in a2.iter() {
                match a1.get(addr) {
                    None => {
                        changed.insert(*addr);
                    }
                    Some(prev) => {
                        if prev.nonce != acc.nonce
                            || prev.balance != acc.balance
                            || prev.code != acc.code
                            || prev.storage_root().ok() != acc.storage_root().ok()
                        {
                            changed.insert(*addr);
                        }
                    }
                }
            }
            for addr in a1.keys() {
                if !a2.contains_key(addr) {
                    changed.insert(*addr);
                }
            }
            return Ok(changed.into_iter().collect());
        }
        // Fallback when historical states are pruned: collect touched addresses
        // from transactions and coinbases in `(start.number + 1)..=end.number`.
        if end.number.saturating_sub(start.number) > 4096 {
            return Err(ViewError::Go(
                "modified account range exceeds 4096 blocks without archive state".into(),
            ));
        }
        let mut touched = std::collections::BTreeSet::new();
        for n in start.number.saturating_add(1)..=end.number {
            let Some(b) = self.load(&snap, n)? else {
                continue;
            };
            if let Some(cb) = b.header.wire().primary_coinbase.as_ref() {
                if let Ok(a) = <[u8; 20]>::try_from(cb.value.as_slice()) {
                    touched.insert(a);
                }
            }
            for tx in b.object.transactions() {
                match tx.kind() {
                    quai_primitives::transaction::Kind::Quai(q) => {
                        if let Ok(from) = tx.sender() {
                            touched.insert(from);
                        }
                        if let Some(to) = q.to {
                            touched.insert(to);
                        }
                    }
                    quai_primitives::transaction::Kind::External(e) => {
                        touched.insert(e.sender);
                        touched.insert(e.to);
                    }
                    quai_primitives::transaction::Kind::Qi(_) => {}
                }
            }
        }
        Ok(touched.into_iter().collect())
    }
}

/// Decode a JSON workshare header object as submitted to `quai_receiveWorkShare`.
fn decode_rpc_workshare_header(v: &serde_json::Value) -> Result<quai_primitives::Header, String> {
    use prost::Message;
    use quai_primitives::schema::{
        block::ProtoWorkObjectHeader,
        common::{ProtoAddress, ProtoHash, ProtoLocation},
    };
    let obj = v
        .as_object()
        .ok_or_else(|| "workshare must be a hex string or JSON object".to_string())?;
    let hex_field = |k: &str| -> Result<Option<Vec<u8>>, String> {
        let Some(val) = obj.get(k).and_then(|x| x.as_str()) else {
            return Ok(None);
        };
        let stripped = val.strip_prefix("0x").unwrap_or(val);
        if stripped.is_empty() {
            return Ok(Some(vec![]));
        }
        let padded = if stripped.len() % 2 == 1 {
            format!("0{stripped}")
        } else {
            stripped.to_string()
        };
        quai_primitives::decode_hex(&format!("0x{padded}"))
            .map(Some)
            .map_err(|e| format!("field {k}: {e}"))
    };
    let u64_field = |k: &str| -> Result<Option<u64>, String> {
        match obj.get(k) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::Number(n)) => Ok(n.as_u64()),
            Some(serde_json::Value::String(s)) => {
                let t = s.strip_prefix("0x").unwrap_or(s);
                u64::from_str_radix(t, 16)
                    .map(Some)
                    .map_err(|e| format!("field {k}: {e}"))
            }
            _ => Err(format!("field {k}: invalid integer")),
        }
    };
    let proto = ProtoWorkObjectHeader {
        header_hash: hex_field("headerHash")?.map(|value| ProtoHash { value }),
        parent_hash: hex_field("parentHash")?.map(|value| ProtoHash { value }),
        number: hex_field("number")?,
        difficulty: hex_field("difficulty")?,
        tx_hash: hex_field("txHash")?.map(|value| ProtoHash { value }),
        location: hex_field("location")?.map(|value| ProtoLocation { value }),
        mix_hash: hex_field("mixHash")?.map(|value| ProtoHash { value }),
        nonce: u64_field("nonce")?,
        time: u64_field("time")?,
        lock: u64_field("lock")?.map(|v| v as u32),
        primary_coinbase: hex_field("primaryCoinbase")?.map(|value| ProtoAddress { value }),
        aux_pow: hex_field("auxPow")?
            .and_then(|b| quai_primitives::schema::block::ProtoAuxPow::decode(b.as_slice()).ok()),
        data: hex_field("data")?.unwrap_or_default(),
        ..Default::default()
    };
    quai_primitives::Header::decode(&proto.encode_to_vec(), PINNED_SCHEDULE)
}

impl ExportView {
    /// Hashes of the 256 ancestors before `block`, for BLOCKHASH.
    pub(crate) fn ancestor_hashes(&self, block: &BlockData) -> BTreeMap<u64, Hash> {
        let snap = self.snap();
        let mut out = BTreeMap::new();
        let mut n = block.number;
        while n > 0 && out.len() < 256 {
            n -= 1;
            if let Ok(Some(h)) = snap.blocks.hash_at(n) {
                out.insert(n, h);
            }
        }
        out
    }
    pub(crate) fn parent_header(&self, block: &BlockData) -> Result<Record, ViewError> {
        if block.number == 0 {
            return Err(ViewError::Unavailable("genesis parent".into()));
        }
        self.record(block.number - 1)
    }
    pub(crate) fn header_by_hash(&self, hash: &Hash) -> Result<Option<Record>, ViewError> {
        let snap = self.snap();
        match snap.blocks.height_of(hash)? {
            Some(_) => snap.blocks.record_by_hash(hash),
            None => Ok(None),
        }
    }
}

/// The transaction pool's chain (`txpool::PoolChain`) over a view: the
/// head of a checkpoint, or the published head of a running node. go-quai
/// reads side-chain blocks from its database when a reorg walks back; the
/// block store keeps only the canonical chain, so the pool remembers the
/// heads it sees and their ancestors (1,024 blocks; at most 128 for one
/// head, back to a remembered block).
pub struct ViewChain {
    view: ExportView,
    seen: Mutex<Lru<Arc<BlockData>>>,
    last: Mutex<Option<Arc<BlockData>>>,
}
const SEEN_HEADS: usize = 1024;
impl ViewChain {
    pub fn new(view: ExportView) -> Result<Self, String> {
        let chain = Self {
            view,
            seen: Mutex::new((HashMap::new(), VecDeque::new())),
            last: Mutex::new(None),
        };
        chain
            .head()
            .map_err(|e| format!("transaction pool: head block: {e}"))?;
        Ok(chain)
    }
    fn head(&self) -> Result<Arc<BlockData>, ViewError> {
        let pinned = self.view.pinned();
        let view: &dyn ChainView = pinned.as_deref().unwrap_or(&self.view);
        let block = view
            .block_by_number(view.head())?
            .ok_or_else(|| ViewError::Unavailable("head block".into()))?;
        self.remember_chain(&block);
        if let Ok(mut last) = self.last.lock() {
            *last = Some(block.clone());
        }
        Ok(block)
    }
    /// Remembers a block and its ancestors up to a remembered one (at most
    /// 128), so that every remembered block has its ancestors too.
    fn remember_chain(&self, block: &Arc<BlockData>) {
        remember(&self.seen, block.hash, block, SEEN_HEADS);
        let mut b = block.clone();
        for _ in 0..128 {
            if b.number == 0 {
                break;
            }
            let parent = crate::txpool::block::parent(&b);
            let known = self
                .seen
                .lock()
                .map(|s| s.0.contains_key(&parent))
                .unwrap_or(true);
            if known {
                break;
            }
            match self.view.block_by_hash(&parent) {
                Ok(Some(p)) if p.number + 1 == b.number => {
                    remember(&self.seen, p.hash, &p, SEEN_HEADS);
                    b = p;
                }
                _ => break,
            }
        }
    }
}
impl crate::txpool::PoolChain for ViewChain {
    fn chain_id(&self) -> u64 {
        self.view.chain_id()
    }
    fn location(&self) -> [u8; 2] {
        self.view.location()
    }
    fn current_block(&self) -> Arc<BlockData> {
        match self.head() {
            Ok(b) => b,
            // The last head read (set by `new`).
            Err(_) => self
                .last
                .lock()
                .ok()
                .and_then(|l| l.clone())
                .expect("a head was read when the pool started"),
        }
    }
    fn get_block(&self, hash: &Hash, number: u64) -> Option<Arc<BlockData>> {
        if let Ok(Some(b)) = self.view.block_by_hash(hash) {
            if b.number == number {
                return Some(b);
            }
        }
        let seen = self.seen.lock().ok()?;
        seen.0.get(hash).filter(|b| b.number == number).cloned()
    }
    fn header_by_hash(&self, hash: &Hash) -> Option<Arc<BlockData>> {
        if let Ok(Some(b)) = self.view.block_by_hash(hash) {
            return Some(b);
        }
        let seen = self.seen.lock().ok()?;
        seen.0.get(hash).cloned()
    }
    fn state_at(&self, block: &BlockData) -> Result<Arc<dyn AccountState>, String> {
        self.view.state(block).map_err(|e| e.to_string())
    }
    fn utxo(&self, hash: &Hash, index: u16) -> Option<UtxoData> {
        self.view.utxo(hash, index).ok().flatten()
    }
    fn utxo_set_size(&self, block: &Hash) -> u64 {
        match self.view.head_utxo_set_size() {
            Some((head, size)) if head == *block => size,
            _ => 0,
        }
    }
    /// The block of a head event, remembered with its ancestors, so that a
    /// later reorg can walk back through them after the store has dropped
    /// them.
    fn head_block(&self, hash: &Hash) -> Option<Arc<BlockData>> {
        let block = self.view.block_by_hash(hash).ok().flatten()?;
        self.remember_chain(&block);
        Some(block)
    }
}
