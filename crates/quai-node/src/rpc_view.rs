//! `quai-node rpc`: a `quai_rpc::view::ChainView` over a QEX1 export and the
//! state of the latest replay checkpoint in the data directory.
//!
//! What is validated, and how:
//! - The checkpoint names the block at its height; the replay that wrote it
//!   checked every block up to there and every state root. At startup the
//!   view recomputes the hash of each zone block from genesis to the
//!   checkpoint, checks the parent links, and requires the block at the
//!   checkpoint height to be the checkpoint's block. The head is that block.
//! - The state is the checkpoint's state file; its roots must equal the head
//!   header's EVM, ETX and UTXO roots.
//! - Receipts come from the export (Go exports only); the replay compared
//!   them with the header receipt roots.
//! - Derived block fields: the order comes from the export's prime and region
//!   records (the replay checked them against the computed order); the total
//!   entropy of a block is its child's zone parent entropy (checked by the
//!   replay), and for the head it is computed natively. Uncles are classified
//!   with their proof-of-work, as go-quai does.
//!
//! "Latest" is the head. State at other blocks is served only when their
//! EVM root is the head's (or empty); otherwise the answer is go-quai's for
//! state that a pruned node no longer keeps.
use crate::checkpoint;
use crate::log::Log;
use crate::qex::{self, Context, Manifest, SegmentReader};
use crate::replay;
use crate::rpc_call;
use crate::spec::Chain;
use crate::store::{self, PastDb, StateStore};
use quai_consensus::{
    history::{self, History, Record},
    pow::{Algorithm, SharedCaches},
    share_rules::{self, Classification},
    verifier::{Verifier, PINNED_SCHEDULE},
};
use quai_execution::{
    historical::History as _,
    qi::{OutPoint, UtxoView},
    state_store::Commitment,
};
use quai_primitives::{transaction::Address, work::WorkObject, Hash, Header};
use quai_rpc::view::{
    AccountState, BlockData, CallError, CallRequest, CallResult, ChainView, LogData, OutpointData,
    ReceiptData, TxPosition, UncleKind, UtxoData, ViewError, U256,
};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// `types.EmptyRootHash`.
pub const EMPTY_ROOT: Hash = [
    0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6, 0x92, 0xc0, 0xf8, 0x6e,
    0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0, 0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
];
/// go-quai's default `WorkShareP2PThresholdDiff`.
const P2P_THRESHOLD: u16 = 7;
/// Decoded blocks kept in memory.
const BLOCK_CACHE: usize = 512;

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
    fn block_bytes(&self, height: u64) -> Result<Vec<u8>, ViewError> {
        let e = self.entry(height)?;
        self.read(e.segment, e.block_at, e.block_len)
    }
    fn entry(&self, height: u64) -> Result<&Entry, ViewError> {
        self.heights
            .get(height as usize)
            .ok_or_else(|| ViewError::Unavailable(format!("zone height {height}")))
    }
    fn record(&self, height: u64) -> Result<Record, ViewError> {
        Record::decode(&self.block_bytes(height)?)
            .map_err(|e| ViewError::Unavailable(format!("zone block {height}: {e}")))
    }
}
/// Ancestry reads for the native entropy computation.
struct Records(Arc<Index>);
impl history::Reader for Records {
    fn read(&self, id: &Hash) -> Result<Option<Arc<Record>>, quai_consensus::Error> {
        let Some(height) = self.0.by_hash.get(id) else {
            return Ok(None);
        };
        self.0
            .record(*height)
            .map(|r| Some(Arc::new(r)))
            .map_err(|_| quai_consensus::Error::Unavailable("export ancestry record"))
    }
}

/// The checkpoint state: the lazy World and UTXO set.
struct Checkpointed {
    state: quai_consensus::zone_execution::State,
    evm_root: Hash,
    /// Unspent outputs per address, when the set is small enough to scan.
    by_address: Option<HashMap<Address, Vec<OutpointData>>>,
    past: Mutex<PastDb>,
}

pub struct ExportView {
    chain: Arc<Chain>,
    index: Arc<Index>,
    head: u64,
    checkpointed: Checkpointed,
    caches: Arc<SharedCaches>,
    blocks: Mutex<(HashMap<u64, Arc<BlockData>>, VecDeque<u64>)>,
    kinds: Mutex<(HashMap<u64, Arc<Vec<UncleKind>>>, VecDeque<u64>)>,
    head_entropy: OnceLock<Result<U256, String>>,
}

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

impl ExportView {
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
        let list = checkpoint::list(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let Some((height, path)) = list.first() else {
            return Err(format!(
                "no state checkpoint in {}: run `quai-node replay --state` first",
                dir.display()
            ));
        };
        let cp = checkpoint::read(path).map_err(|e| e.to_string())?;
        if cp.height != *height
            || cp.environment != chain.spec().environment
            || cp.genesis != chain.genesis_hash()
        {
            return Err(format!("checkpoint {height} is for another chain"));
        }
        let section = cp
            .state
            .clone()
            .ok_or_else(|| format!("checkpoint {height} has no state"))?;
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
        let index = Arc::new(build_index(export, &manifest, &chain, head)?);
        if index.heights[head as usize].hash != cp.block {
            return Err(format!(
                "the export block at {head} is not the checkpoint block {}",
                qex::hex(&cp.block)
            ));
        }
        // The checkpoint state and its binding to the head header.
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
        let head_record = index.record(head).map_err(|e| e.to_string())?;
        let body = head_record.object().header().wire();
        if section.evm != hash_of(&body.evm_root)
            || section.etx != hash_of(&body.etx_set_root)
            || section.utxo != hash_of(&body.utxo_root)
        {
            return Err(format!(
                "checkpoint {head} state roots differ from the head header"
            ));
        }
        let store = StateStore::open(
            &dir,
            store::profile(&chain.spec().environment, &chain.genesis_hash()),
            head,
            section.version,
            commitment,
        )
        .map_err(|e| format!("state {head}: {e}"))?;
        let state = store.restore().map_err(|e| format!("state {head}: {e}"))?;
        let past = PastDb::open(&dir.join("past.redb")).map_err(|e| format!("past.redb: {e}"))?;
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
        Ok(Self {
            chain,
            index,
            head,
            checkpointed: Checkpointed {
                state,
                evm_root: section.evm,
                by_address,
                past: Mutex::new(past),
            },
            caches: Arc::new(SharedCaches::new(2).map_err(|e| e.to_string())?),
            blocks: Mutex::new((HashMap::new(), VecDeque::new())),
            kinds: Mutex::new((HashMap::new(), VecDeque::new())),
            head_entropy: OnceLock::new(),
        })
    }

    fn load(&self, height: u64) -> Result<Arc<BlockData>, ViewError> {
        if let Ok(cache) = self.blocks.lock() {
            if let Some(b) = cache.0.get(&height) {
                return Ok(b.clone());
            }
        }
        let entry = *self.index.entry(height)?;
        let bytes = self
            .index
            .read(entry.segment, entry.block_at, entry.block_len)?;
        let object = WorkObject::decode_auxiliary(&bytes, PINNED_SCHEDULE).map_err(unavailable)?;
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
        let block = Arc::new(BlockData {
            hash: entry.hash,
            number: height,
            object,
            header,
            uncles,
            size: entry.block_len as u64,
            order: entry.order,
            tx_hashes,
            etx_hashes,
        });
        if let Ok(mut cache) = self.blocks.lock() {
            if cache.0.len() >= BLOCK_CACHE {
                if let Some(old) = cache.1.pop_front() {
                    cache.0.remove(&old);
                }
            }
            cache.0.insert(height, block.clone());
            cache.1.push_back(height);
        }
        Ok(block)
    }

    /// `UncleWorkShareClassification`: the uncle's proof-of-work against the
    /// block target, then the workshare thresholds.
    fn classify(&self, h: &Header) -> UncleKind {
        let Ok(pow) = self.pow_hash(h) else {
            return UncleKind::Other;
        };
        match share_rules::classify(h, pow, P2P_THRESHOLD) {
            Ok(Classification::Block) => UncleKind::Block,
            Ok(Classification::Valid) => UncleKind::Workshare,
            _ => UncleKind::Other,
        }
    }
    /// The proof-of-work hash of a header (`ComputePowHash`, or the donor
    /// hash of an auxiliary proof). A wrong mix hash is an error, as in Go.
    fn pow_hash(&self, h: &Header) -> Result<Hash, quai_consensus::Error> {
        let w = h.wire();
        let height = share_rules::number(w.prime_terminus_number.as_deref().unwrap_or_default())?;
        if let Some(bytes) = &w.aux_pow {
            let aux = quai_consensus::auxiliary::Auxiliary::decode(bytes)?;
            return match aux.id() {
                1 => {
                    let (seal, nonce, donor, mix) = aux.kawpow_input()?;
                    let (actual, hash) = self
                        .caches
                        .get(Algorithm::Kawpow, donor)?
                        .compute(seal, nonce, donor)?;
                    if actual != mix {
                        return Err(quai_consensus::Error::Invalid("mix hash"));
                    }
                    Ok(hash)
                }
                2 | 3 => quai_consensus::donor::sha256d(aux.header()),
                _ => quai_consensus::donor::scrypt(aux.header()),
            };
        }
        let mix =
            hash_of(
                &w.mix_hash
                    .as_ref()
                    .map(|m| quai_primitives::schema::common::ProtoHash {
                        value: m.value.clone(),
                    }),
            );
        let (actual, hash) = self.caches.get(Algorithm::Progpow, height)?.compute(
            h.seal_hash(),
            w.nonce.unwrap_or_default(),
            height,
        )?;
        if actual != mix {
            return Err(quai_consensus::Error::Invalid("mix hash"));
        }
        Ok(hash)
    }
    /// `TotalLogEntropy` in the zone: the child's zone parent entropy, or,
    /// for the head, the native computation.
    fn entropy_at(&self, height: u64) -> Result<U256, ViewError> {
        if height < self.head {
            let child = self.index.record(height + 1)?;
            let v = child
                .object()
                .header()
                .wire()
                .parent_entropy
                .get(2)
                .cloned()
                .unwrap_or_default();
            let mut out = [0u8; 32];
            let take = v.len().min(32);
            out[32 - take..].copy_from_slice(&v[v.len() - take..]);
            return Ok(out);
        }
        self.head_entropy
            .get_or_init(|| self.native_entropy(height))
            .clone()
            .map_err(ViewError::Unavailable)
    }
    /// `native_parent_metrics` of the replay, over the export ancestry.
    pub fn native_entropy(&self, height: u64) -> Result<U256, String> {
        let record = self.index.record(height).map_err(|e| e.to_string())?;
        let history = History::with_reader(Arc::new(Records(self.index.clone())));
        let mut verifier = Verifier::with_shared_caches(self.caches.clone());
        let metrics = quai_consensus::validation::native_parent_metrics(
            &mut verifier,
            &history,
            &record,
            &self.chain.zone_profile(),
        )
        .map_err(|e| format!("total entropy of {height}: {e}"))?;
        use crypto_bigint::Encoding;
        Ok(metrics.total.to_be_bytes())
    }
    pub fn chain(&self) -> &Arc<Chain> {
        &self.chain
    }
    /// The decoded zone record at `height` (for tests and tools).
    pub fn record(&self, height: u64) -> Result<Record, ViewError> {
        self.index.record(height)
    }
}

/// Read every segment up to `head`: check each segment's hash, the record
/// order, every zone block hash and the parent links from genesis.
fn build_index(
    export: &Path,
    manifest: &Manifest,
    chain: &Chain,
    head: u64,
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
        out.entry(u.address).or_default().push(OutpointData {
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
}

/// What `state.New` finds for a block's roots on a node that keeps only the
/// head state: the head state, an empty state, or go-quai's error.
pub(crate) enum StateAt {
    Head,
    Empty,
}
impl ExportView {
    pub(crate) fn state_at(&self, block: &BlockData) -> Result<StateAt, ViewError> {
        let body = block.object.header().wire();
        let evm = hash_of(&body.evm_root);
        let etx = hash_of(&body.etx_set_root);
        let head_etx = self
            .index
            .record(self.head)?
            .object()
            .header()
            .wire()
            .etx_set_root
            .clone();
        let empty = |h: &Hash| *h == [0; 32] || *h == EMPTY_ROOT;
        let missing =
            |h: &Hash| ViewError::Go(format!("missing trie node {} (path )", &qex::hex(h)[2..]));
        if !(empty(&evm) || evm == self.checkpointed.evm_root) {
            return Err(missing(&evm));
        }
        if !(empty(&etx) || etx == hash_of(&head_etx)) {
            return Err(missing(&etx));
        }
        Ok(if empty(&evm) {
            StateAt::Empty
        } else {
            StateAt::Head
        })
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
        self.head
    }
    fn block_by_number(&self, number: u64) -> Result<Option<Arc<BlockData>>, ViewError> {
        if number > self.head {
            return Ok(None);
        }
        self.load(number).map(Some)
    }
    fn block_by_hash(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        match self.index.by_hash.get(hash) {
            Some(h) => self.load(*h).map(Some),
            None => Ok(None),
        }
    }
    fn uncle_kinds(&self, block: &BlockData) -> Result<Arc<Vec<UncleKind>>, ViewError> {
        if let Ok(cache) = self.kinds.lock() {
            if let Some(k) = cache.0.get(&block.number) {
                return Ok(k.clone());
            }
        }
        let kinds = Arc::new(
            block
                .uncles
                .iter()
                .map(|h| self.classify(h))
                .collect::<Vec<_>>(),
        );
        if let Ok(mut cache) = self.kinds.lock() {
            if cache.0.len() >= BLOCK_CACHE * 8 {
                if let Some(old) = cache.1.pop_front() {
                    cache.0.remove(&old);
                }
            }
            cache.0.insert(block.number, kinds.clone());
            cache.1.push_back(block.number);
        }
        Ok(kinds)
    }
    fn total_entropy(&self, block: &BlockData) -> Result<U256, ViewError> {
        self.entropy_at(block.number)
    }
    fn may_have_logs(&self, number: u64) -> Result<bool, ViewError> {
        if !self.index.receipts {
            return Ok(true);
        }
        let e = *self.index.entry(number)?;
        if e.receipts_len == 0 {
            return Ok(false);
        }
        let bytes = self.index.read(e.segment, e.receipts_at, e.receipts_len)?;
        Ok(decode_receipts(&bytes)
            .map_err(|e| ViewError::Unavailable(format!("stored receipts: {e}")))?
            .iter()
            .any(|r| !r.logs.is_empty()))
    }
    fn transaction(&self, hash: &Hash) -> Result<Option<TxPosition>, ViewError> {
        let prefix = u64::from_be_bytes(hash[..8].try_into().unwrap_or_default());
        let start = self.index.txs.partition_point(|t| t.0 < prefix);
        // The lookup entry of the newest block wins, as in rawdb.
        let mut found = None;
        for &(p, height, i) in &self.index.txs[start..] {
            if p != prefix {
                break;
            }
            let block = self.load(height)?;
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
        if !self.index.receipts {
            return Err(ViewError::Unavailable(
                "receipts (this export has none)".into(),
            ));
        }
        let e = *self.index.entry(block.number)?;
        if e.receipts_len == 0 {
            return Ok(Arc::new(vec![]));
        }
        let bytes = self.index.read(e.segment, e.receipts_at, e.receipts_len)?;
        decode_receipts(&bytes)
            .map(Arc::new)
            .map_err(|e| ViewError::Unavailable(format!("stored receipts: {e}")))
    }
    fn state(&self, block: &BlockData) -> Result<Arc<dyn AccountState>, ViewError> {
        Ok(match self.state_at(block)? {
            StateAt::Empty => Arc::new(EmptyState),
            StateAt::Head => Arc::new(WorldState(self.checkpointed.state.world.clone())),
        })
    }
    fn utxo(&self, tx_hash: &Hash, index: u16) -> Result<Option<UtxoData>, ViewError> {
        let point = OutPoint {
            hash: *tx_hash,
            index,
        };
        Ok(self
            .checkpointed
            .state
            .utxos
            .get(&point)
            .map_err(read_error)?
            .map(|u| UtxoData {
                denomination: u.denomination,
                address: u.address,
                lock: trim(&u.lock),
            }))
    }
    fn outpoints_by_address(&self, address: &Address) -> Result<Vec<OutpointData>, ViewError> {
        match &self.checkpointed.by_address {
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
        let past = self
            .checkpointed
            .past
            .lock()
            .map_err(|_| unavailable("past.redb lock"))?;
        let mut out = vec![];
        for height in start as u64..=(end as u64).min(self.head) {
            let created = past
                .created_at(height)
                .map_err(|e| ViewError::Unavailable(format!("UTXOs created at {height}: {e:?}")))?;
            for (point, _) in created {
                let Some(u) = self
                    .checkpointed
                    .state
                    .utxos
                    .get(&point)
                    .map_err(read_error)?
                else {
                    continue;
                };
                if u.address[..16] != address[..16] {
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
        let world = match self.state_at(block)? {
            StateAt::Head => self.checkpointed.state.world.clone(),
            StateAt::Empty => quai_execution::evm::state::World::default(),
        };
        rpc_call::call(self, world, block, request)
    }
}

impl ExportView {
    /// Hashes of the 256 ancestors before `block`, for BLOCKHASH.
    pub(crate) fn ancestor_hashes(&self, block: &BlockData) -> BTreeMap<u64, Hash> {
        let mut out = BTreeMap::new();
        let mut n = block.number;
        while n > 0 && out.len() < 256 {
            n -= 1;
            if let Ok(e) = self.index.entry(n) {
                out.insert(n, e.hash);
            }
        }
        out
    }
    pub(crate) fn parent_header(&self, block: &BlockData) -> Result<Record, ViewError> {
        if block.number == 0 {
            return Err(ViewError::Unavailable("genesis parent".into()));
        }
        self.index.record(block.number - 1)
    }
    pub(crate) fn header_by_hash(&self, hash: &Hash) -> Result<Option<Record>, ViewError> {
        match self.index.by_hash.get(hash) {
            Some(h) => self.index.record(*h).map(Some),
            None => Ok(None),
        }
    }
}
