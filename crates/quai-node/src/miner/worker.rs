//! Pending header builder, workshare pool & LRU cache, PoW submission
//! verifier, and live miner control (`go-quai/core/worker.go`, `core/slice.go`,
//! `core/core.go`).
use super::auxpow::{
    aux_commitment, build_coinbase_tx, build_donor_header, default_fallback_template, hex_plain,
    parse_submitted_donor_block, POW_KAWPOW, POW_PROGPOW, POW_SCRYPT, POW_SHA_BCH, POW_SHA_BTC,
};
use super::template::{marshal_aux_pow_template, BlockTemplateParams};
use crate::spec::Chain;
use crypto_bigint::{Encoding as _, U256};
use prost::Message;
use quai_consensus::{
    auxiliary::{coinbase, Auxiliary, Quorum},
    donor,
    pow::{Algorithm, SharedCaches},
    rules,
    share_rules::{self, Classification, Triple},
    validation,
    verifier::PINNED_SCHEDULE,
};
use quai_primitives::{
    auxiliary as aux_codec, encode_hex,
    schema::{
        block::{
            ProtoAuxPow, ProtoAuxTemplate, ProtoHeader, ProtoPowShareDiffAndCount,
            ProtoTransactions, ProtoWorkObject, ProtoWorkObjectBlockView, ProtoWorkObjectBody,
            ProtoWorkObjectHeader, ProtoWorkObjectHeaders, ProtoWorkObjectShareView,
        },
        common::{ProtoAddress, ProtoHash, ProtoLocation},
    },
    spec::{self, FORKS},
    transaction::Address,
    work::{self, WorkObject},
    Hash, Header,
};
use quai_rpc::view::ChainView;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, RwLock,
};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

const PENDING_CACHE_CAP: usize = 1000;
const WORKSHARE_LRU_CAP: usize = 1000;

/// Miner runtime configuration (editable live via `miner_*` RPCs).
#[derive(Clone, Debug)]
pub struct MinerConfig {
    pub enabled: bool,
    pub primary_coinbase: Address,
    pub extra: Vec<u8>,
    pub gas_ceil: u64,
    pub gas_price: U256,
    pub lockup_byte: u8,
    pub miner_preference: f64,
    pub recommit_ms: u64,
    pub workshare_threshold: u16,
    pub workshare_p2p_threshold: u16,
    pub pool_tag: String,
    /// When true, allow local/solo mining with unsigned fallback AuxTemplates
    /// if no signed quorum template has been received yet (useful for local
    /// devnets and standalone Stratum testing).
    pub allow_unsigned_fallback: bool,
}

impl Default for MinerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            primary_coinbase: [0u8; 20],
            extra: b"quai-rust".to_vec(),
            gas_ceil: spec::STATE_CEIL,
            gas_price: U256::from_u64(1_000_000_000),
            lockup_byte: 0,
            miner_preference: 0.5,
            recommit_ms: 2000,
            workshare_threshold: spec::WORKSHARES_THRESHOLD_DIFF,
            workshare_p2p_threshold: spec::WORKSHARE_P2P_THRESHOLD_DIFF,
            pool_tag: "quai-rust".into(),
            allow_unsigned_fallback: true,
        }
    }
}

/// Outcome of `SubmitBlock` (`quai_submitShaBlock`, `quai_submitScryptBlock`, `quai_submitKawpowBlock`).
#[derive(Clone)]
pub struct SubmitOutcome {
    pub hash: Hash,
    pub number: u64,
    /// 0 = Sub, 1 = Valid workshare, 2 = Full Quai block.
    pub status: u64,
    pub classification: Classification,
    pub work_object: WorkObject,
}

/// Compact dump returned by `debug_getWorkshareLRUDump`.
#[derive(Clone, Debug)]
pub struct WorkshareLruDump {
    pub received: Vec<Value>,
    pub mined: Vec<Value>,
}

struct BoundedMap<K, V> {
    map: HashMap<K, V>,
    order: VecDeque<K>,
    cap: usize,
}
impl<K: Eq + std::hash::Hash + Clone, V> BoundedMap<K, V> {
    fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap,
        }
    }
    fn insert(&mut self, key: K, value: V) {
        if self.map.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }
    fn get(&self, key: &K) -> Option<&V> {
        self.map.get(key)
    }
    fn values_recent(&self, limit: usize) -> Vec<&V> {
        let iter = self.order.iter().rev().filter_map(|k| self.map.get(k));
        if limit == 0 {
            iter.collect()
        } else {
            iter.take(limit).collect()
        }
    }
}

struct WorkshareCaches {
    kawpow: BoundedMap<Hash, Header>,
    sha: BoundedMap<Hash, Header>,
    scrypt: BoundedMap<Hash, Header>,
    received: BoundedMap<Hash, Value>,
    mined: BoundedMap<Hash, Value>,
}
impl WorkshareCaches {
    fn new() -> Self {
        Self {
            kawpow: BoundedMap::new(WORKSHARE_LRU_CAP),
            sha: BoundedMap::new(WORKSHARE_LRU_CAP),
            scrypt: BoundedMap::new(WORKSHARE_LRU_CAP),
            received: BoundedMap::new(WORKSHARE_LRU_CAP),
            mined: BoundedMap::new(WORKSHARE_LRU_CAP),
        }
    }
    fn record(&mut self, header: &Header, mined: bool) {
        let Ok(hash) = header.block_hash() else {
            return;
        };
        let hw = header.wire();
        let pow_id = hw
            .aux_pow
            .as_deref()
            .and_then(|b| Auxiliary::decode(b).ok())
            .map_or(POW_PROGPOW, |a| a.id());
        match pow_id {
            POW_SHA_BTC | POW_SHA_BCH => self.sha.insert(hash, header.clone()),
            POW_SCRYPT => self.scrypt.insert(hash, header.clone()),
            _ => self.kawpow.insert(hash, header.clone()),
        }
        let number = share_rules::number(hw.number.as_deref().unwrap_or_default()).unwrap_or(0);
        let summary = json!({
            "hash": encode_hex(&hash),
            "number": format!("0x{number:x}"),
            "powId": pow_id,
            "time": hw.time.unwrap_or(0),
            "coinbase": hw.primary_coinbase.as_ref().map(|c| encode_hex(&c.value)).unwrap_or_default(),
        });
        if mined {
            self.mined.insert(hash, summary);
        } else {
            self.received.insert(hash, summary);
        }
    }
}

/// Optional callback invoked when a valid block or workshare should be
/// published to P2P gossip (wired by `node.rs` when `--p2p-publish` is set).
pub type PublishHook = Arc<dyn Fn(String, Vec<u8>) + Send + Sync>;

/// Optional callback invoked when a full Quai block is mined locally so the
/// node's processor can immediately ingest it.
pub type BlockSink = Arc<dyn Fn(Vec<u8>) + Send + Sync>;

/// Thread-safe Quai miner, pending header generator, AuxPoW template store and
/// workshare manager.
pub struct Miner {
    chain: Arc<Chain>,
    view: Arc<dyn ChainView>,
    caches: Arc<SharedCaches>,
    quorum: Quorum,
    config: RwLock<MinerConfig>,
    mining: AtomicBool,
    hashrate: AtomicU64,
    aux_templates: RwLock<HashMap<u8, ProtoAuxTemplate>>,
    pending_by_seal: Mutex<BoundedMap<Hash, WorkObject>>,
    pending_by_aux_key: Mutex<BoundedMap<Hash, WorkObject>>,
    workshares: Mutex<WorkshareCaches>,
    pending_header_tx: broadcast::Sender<Vec<u8>>,
    template_update_tx: broadcast::Sender<u8>,
    workshare_tx: broadcast::Sender<Arc<WorkObject>>,
    publish_hook: RwLock<Option<PublishHook>>,
    block_sink: RwLock<Option<BlockSink>>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn trim_be(v: U256) -> Vec<u8> {
    let b = v.to_be_bytes();
    b[b.iter().position(|x| *x != 0).unwrap_or(32)..].to_vec()
}

fn trim_u64(v: u64) -> Vec<u8> {
    let b = v.to_be_bytes();
    b[b.iter().position(|x| *x != 0).unwrap_or(8)..].to_vec()
}

fn aux_lookup_key(commitment: Hash, signature_time: u32) -> Hash {
    let mut buf = [0u8; 36];
    buf[..32].copy_from_slice(&commitment);
    buf[32..].copy_from_slice(&signature_time.to_be_bytes());
    quai_primitives::transaction::keccak(&buf)
}

impl Miner {
    pub fn new(
        chain: Arc<Chain>,
        view: Arc<dyn ChainView>,
        caches: Arc<SharedCaches>,
        config: MinerConfig,
    ) -> Arc<Self> {
        let enabled = config.enabled;
        let (pending_header_tx, _) = broadcast::channel(64);
        let (template_update_tx, _) = broadcast::channel(64);
        let (workshare_tx, _) = broadcast::channel(256);
        Arc::new(Self {
            chain,
            view,
            caches,
            quorum: Quorum::pinned(),
            config: RwLock::new(config),
            mining: AtomicBool::new(enabled),
            hashrate: AtomicU64::new(0),
            aux_templates: RwLock::new(HashMap::new()),
            pending_by_seal: Mutex::new(BoundedMap::new(PENDING_CACHE_CAP)),
            pending_by_aux_key: Mutex::new(BoundedMap::new(PENDING_CACHE_CAP)),
            workshares: Mutex::new(WorkshareCaches::new()),
            pending_header_tx,
            template_update_tx,
            workshare_tx,
            publish_hook: RwLock::new(None),
            block_sink: RwLock::new(None),
        })
    }

    pub fn set_publish_hook(&self, hook: PublishHook) {
        *self.publish_hook.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    pub fn set_block_sink(&self, sink: BlockSink) {
        *self.block_sink.write().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    }

    pub fn chain(&self) -> &Arc<Chain> {
        &self.chain
    }

    pub fn view(&self) -> &Arc<dyn ChainView> {
        &self.view
    }

    pub fn location(&self) -> [u8; 2] {
        self.chain.location()
    }

    /// Schedule for decoding AuxPoW work objects on this chain: on `local`
    /// devnets AuxPoW is active from genesis (prime 0); on public networks it
    /// uses `PINNED_SCHEDULE` (`kawpow_fork = 3_050_000`).
    pub fn aux_schedule(&self) -> quai_primitives::Schedule {
        if self.chain.spec().environment == "local" {
            quai_primitives::Schedule {
                kawpow_fork: 0,
                ..PINNED_SCHEDULE
            }
        } else {
            PINNED_SCHEDULE
        }
    }

    pub fn is_mining(&self) -> bool {
        self.mining.load(Ordering::Relaxed)
    }

    pub fn start_mining(&self) {
        self.mining.store(true, Ordering::Relaxed);
        self.config
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .enabled = true;
    }

    pub fn stop_mining(&self) {
        self.mining.store(false, Ordering::Relaxed);
        self.config
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .enabled = false;
    }

    pub fn hashrate(&self) -> u64 {
        self.hashrate.load(Ordering::Relaxed)
    }

    pub fn set_hashrate(&self, rate: u64) {
        self.hashrate.store(rate, Ordering::Relaxed);
    }

    pub fn config(&self) -> MinerConfig {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn primary_coinbase(&self) -> Address {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .primary_coinbase
    }

    pub fn set_primary_coinbase(&self, addr: Address) {
        self.config
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .primary_coinbase = addr;
    }

    pub fn set_extra(&self, extra: Vec<u8>) -> Result<(), String> {
        if extra.len() > spec::MAXIMUM_EXTRA_DATA_SIZE {
            return Err(format!(
                "extra exceeds max length ({} > {})",
                extra.len(),
                spec::MAXIMUM_EXTRA_DATA_SIZE
            ));
        }
        self.config.write().unwrap_or_else(|e| e.into_inner()).extra = extra;
        Ok(())
    }

    pub fn set_gas_limit(&self, gas_ceil: u64) {
        self.config
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .gas_ceil = gas_ceil;
    }

    pub fn set_gas_price(&self, price: U256) {
        self.config
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .gas_price = price;
    }

    pub fn set_lockup_byte(&self, lock: u8) -> Result<(), String> {
        if lock as usize >= spec::LOCK_PERIODS.by_lock_byte.len() {
            return Err(format!("lockup byte {lock} out of range"));
        }
        self.config
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .lockup_byte = lock;
        Ok(())
    }

    pub fn set_miner_preference(&self, pref: f64) -> Result<(), String> {
        if !(0.0..=1.0).contains(&pref) || pref.is_nan() {
            return Err("miner preference must be between 0 and 1".into());
        }
        self.config
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .miner_preference = pref;
        Ok(())
    }

    pub fn set_recommit_interval(&self, ms: u64) {
        self.config
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .recommit_ms = ms.max(100);
    }

    pub fn workshare_threshold(&self) -> u16 {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .workshare_threshold
    }

    pub fn workshare_p2p_threshold(&self) -> u16 {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .workshare_p2p_threshold
    }

    pub fn set_workshare_p2p_threshold(&self, threshold: u16) -> Result<(), String> {
        if threshold > 64 {
            return Err("workshare p2p threshold out of range".into());
        }
        self.config
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .workshare_p2p_threshold = threshold;
        Ok(())
    }

    pub fn subscribe_pending_header(&self) -> broadcast::Receiver<Vec<u8>> {
        self.pending_header_tx.subscribe()
    }

    pub fn subscribe_template_updates(&self) -> broadcast::Receiver<u8> {
        self.template_update_tx.subscribe()
    }

    pub fn subscribe_workshares(&self) -> broadcast::Receiver<Arc<WorkObject>> {
        self.workshare_tx.subscribe()
    }

    /// Return the latest AuxTemplate stored for `pow_id`.
    pub fn best_aux_template(&self, pow_id: u8) -> Option<ProtoAuxTemplate> {
        let key = if pow_id == POW_SHA_BTC {
            POW_SHA_BCH
        } else {
            pow_id
        };
        self.aux_templates
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
    }

    /// Store a template without quorum verification (for local devnets/tests).
    pub fn inject_aux_template_unchecked(&self, template: ProtoAuxTemplate) {
        let pow_id = template.chain_id.unwrap_or(u32::from(POW_KAWPOW)) as u8;
        let key = if pow_id == POW_SHA_BTC {
            POW_SHA_BCH
        } else {
            pow_id
        };
        self.aux_templates
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, template);
        let _ = self.template_update_tx.send(key);
    }

    /// Verify and accept a signed `ProtoAuxTemplate` (`quai_submitAuxTemplate`).
    pub fn submit_aux_template(&self, raw: &[u8]) -> Result<(), String> {
        let t = aux_codec::canonical_template(raw)
            .map_err(|e| format!("failed to decode AuxTemplate: {e}"))?;
        let sigs = t.sigs.as_deref().unwrap_or_default();
        if sigs.is_empty() || !self.quorum.verify(aux_codec::template_hash(&t), sigs) {
            return Err("invalid signature on AuxTemplate (embedded)".into());
        }
        let now = unix_now();
        let signed_at = u64::from(t.signature_time.unwrap_or(0));
        if signed_at > now.saturating_add(15) || now > signed_at.saturating_add(15) {
            return Err("AuxTemplate timestamp is outside the relay window".into());
        }
        let pow_id = t.chain_id.unwrap_or(0) as u8;
        let key = if pow_id == POW_SHA_BTC {
            POW_SHA_BCH
        } else {
            pow_id
        };
        self.aux_templates
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, t);
        let _ = self.template_update_tx.send(key);
        if let Some(hook) = self
            .publish_hook
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            if let Ok(topic_gen) = self.chain.topic_genesis() {
                if let Ok(topic) = quai_network::topic(
                    &topic_gen,
                    &self.location(),
                    quai_network::TopicKind::AuxTemplate,
                ) {
                    hook(topic, raw.to_vec());
                }
            }
        }
        Ok(())
    }

    /// Return current subsidy chain heights from the stored AuxTemplates.
    pub fn subsidy_chain_height(&self) -> Result<Value, String> {
        if self.location() != [0, 0] {
            return Err(
                "cannot call GetSubsidyChainHeight in any other chain than cyprus-1".into(),
            );
        }
        let templates = self.aux_templates.read().unwrap_or_else(|e| e.into_inner());
        let mut map = serde_json::Map::new();
        if let Some(t) = templates.get(&POW_KAWPOW) {
            map.insert(
                "ravencoin".into(),
                Value::String(format!("0x{:x}", t.height.unwrap_or(0))),
            );
        }
        if let Some(t) = templates.get(&POW_SHA_BCH) {
            map.insert(
                "bch".into(),
                Value::String(format!("0x{:x}", t.height.unwrap_or(0))),
            );
        }
        if let Some(t) = templates.get(&POW_SCRYPT) {
            map.insert(
                "ltc".into(),
                Value::String(format!("0x{:x}", t.height.unwrap_or(0))),
            );
        }
        if map.is_empty() {
            return Err("no subsidy chain height found".into());
        }
        Ok(Value::Object(map))
    }

    /// Build a pending `WorkObject` extending the current canonical head for
    /// `pow_id` (`GetPendingHeader`).
    pub fn generate_pending_header(
        &self,
        pow_id: u8,
        coinbase_override: Option<Address>,
        extra_data: &[u8],
        lock_override: Option<u8>,
    ) -> Result<WorkObject, String> {
        let view = self.view.pinned().unwrap_or_else(|| self.view.clone());
        let head_num = view.head();
        let parent = view
            .block_by_number(head_num)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("head block #{head_num} not found"))?;

        let cfg = self.config();
        let coinbase = match coinbase_override {
            Some(c) if c != [0u8; 20] => {
                if !validation::internal_address(&c, &self.location()) {
                    return Err("out of scope or invalid coinbase address".into());
                }
                c
            }
            _ => {
                if cfg.primary_coinbase != [0u8; 20] {
                    cfg.primary_coinbase
                } else {
                    let mut fallback = [0u8; 20];
                    fallback[0] = self.chain.spec().address_prefix();
                    fallback[19] = 1;
                    fallback
                }
            }
        };
        let lock = lock_override.unwrap_or(cfg.lockup_byte);
        if lock as usize >= spec::LOCK_PERIODS.by_lock_byte.len() {
            return Err(format!("lockup byte {lock} out of range"));
        }

        let pw = parent.header.wire();
        let pb = parent.object.header().wire();
        let now = unix_now().max(pw.time.unwrap_or(0).saturating_add(1));
        let next_num = head_num.saturating_add(1);
        let parent_prime =
            share_rules::number(pw.prime_terminus_number.as_deref().unwrap_or_default())
                .unwrap_or(0);
        let parent_diff = share_rules::uint(pw.difficulty.as_deref().unwrap_or_default())
            .unwrap_or_else(|_| U256::from_u128(self.chain.spec().header.minimum_difficulty))
            .max(U256::ONE);

        // Include recent pending workshares matching `parent_num` (within inclusion depth)
        let uncles: Vec<ProtoWorkObjectHeader> = {
            let ws = self.workshares.lock().unwrap_or_else(|e| e.into_inner());
            let (max_total, max_sha, max_scrypt) = FORKS.workshare_counts(parent_prime);
            let mut chosen = Vec::new();
            let mut sha_n = 0usize;
            let mut scrypt_n = 0usize;
            for h in ws
                .kawpow
                .values_recent(max_total)
                .into_iter()
                .chain(ws.sha.values_recent(max_sha))
                .chain(ws.scrypt.values_recent(max_scrypt))
            {
                if chosen.len() >= max_total {
                    break;
                }
                let h_num = share_rules::number(h.wire().number.as_deref().unwrap_or_default())
                    .unwrap_or(0);
                if h_num > next_num || next_num.saturating_sub(h_num) > 4 {
                    continue;
                }
                let id = h
                    .wire()
                    .aux_pow
                    .as_deref()
                    .and_then(|b| Auxiliary::decode(b).ok())
                    .map_or(0, |a| a.id());
                if matches!(id, POW_SHA_BTC | POW_SHA_BCH) {
                    if sha_n >= max_sha {
                        continue;
                    }
                    sha_n += 1;
                } else if id == POW_SCRYPT {
                    if scrypt_n >= max_scrypt {
                        continue;
                    }
                    scrypt_n += 1;
                }
                if let Ok(pw) = ProtoWorkObjectHeader::decode(h.encode().as_slice()) {
                    chosen.push(pw);
                }
            }
            chosen
        };

        // Select pending Quai transactions from the pool (respecting gas limit)
        let gas_limit = pb
            .gas_limit
            .unwrap_or(self.chain.spec().genesis.gas_limit)
            .min(cfg.gas_ceil.max(spec::MIN_GAS_LIMIT));
        let mut proto_txs = Vec::new();
        let mut tx_models = Vec::new();
        let mut gas_accum = 0u64;
        if let Some(content) = view.pool_content(None) {
            for (_addr, txs) in content.pending {
                for ptx in txs {
                    let g = match ptx.tx.kind() {
                        quai_primitives::transaction::Kind::Quai(q) => q.gas,
                        quai_primitives::transaction::Kind::External(e) => e.gas,
                        quai_primitives::transaction::Kind::Qi(_) => 0,
                    };
                    if gas_accum.saturating_add(g) > gas_limit {
                        continue;
                    }
                    gas_accum += g;
                    let raw_tx = ptx.tx.encode();
                    if let Ok(pt) =
                        quai_primitives::schema::block::ProtoTransaction::decode(raw_tx.as_slice())
                    {
                        proto_txs.push(pt);
                        tx_models.push(ptx.tx);
                    }
                }
            }
        }
        let tx_root = work::transactions_root(&tx_models).unwrap_or(crate::rpc_view::EMPTY_ROOT);
        let uncle_root = work::uncles_root(&ProtoWorkObjectHeaders {
            wo_headers: uncles.clone(),
        });

        // Build body Header extending parent
        let mut next_body_header = pb.clone();
        if next_body_header.parent_hash.len() == 3 {
            next_body_header.parent_hash[2] = ProtoHash {
                value: parent.hash.to_vec(),
            };
        }
        if next_body_header.number.len() == 3 {
            next_body_header.number[2] = trim_u64(next_num);
        }
        next_body_header.gas_limit = Some(gas_limit);
        next_body_header.gas_used = Some(0);
        next_body_header.uncle_hash = Some(ProtoHash {
            value: uncle_root.to_vec(),
        });
        let base_fee = view.calc_base_fee(&parent).unwrap_or([0u8; 32]);
        let bf_trimmed = &base_fee[base_fee.iter().position(|b| *b != 0).unwrap_or(32)..];
        next_body_header.base_fee = Some(bf_trimmed.to_vec());
        next_body_header.extra = Some(if extra_data.is_empty() {
            cfg.extra.clone()
        } else {
            extra_data[..extra_data.len().min(spec::MAXIMUM_EXTRA_DATA_SIZE)].to_vec()
        });
        if let Ok(total_entropy) = view.total_entropy(&parent) {
            if next_body_header.parent_entropy.len() == 3 {
                let trimmed =
                    &total_entropy[total_entropy.iter().position(|b| *b != 0).unwrap_or(32)..];
                next_body_header.parent_entropy[2] = trimmed.to_vec();
            }
        }
        let body_header_hash = {
            let encoded = next_body_header.encode_to_vec();
            quai_primitives::transaction::keccak(&encoded)
        };

        // Compute SHA/Scrypt share triples & targets
        let sha_triple = Triple::from_header(&parent.header, true).unwrap_or_default();
        let scrypt_triple = Triple::from_header(&parent.header, false).unwrap_or_default();
        let sha_diff = if sha_triple.difficulty == U256::ZERO {
            U256::from_u64(7_000_000_000_000_000)
        } else {
            sha_triple.difficulty
        };
        let scrypt_diff = if scrypt_triple.difficulty == U256::ZERO {
            U256::from_u64(700_000_000_000)
        } else {
            scrypt_triple.difficulty
        };

        let mut wo_header = ProtoWorkObjectHeader {
            header_hash: Some(ProtoHash {
                value: body_header_hash.to_vec(),
            }),
            parent_hash: Some(ProtoHash {
                value: parent.hash.to_vec(),
            }),
            number: Some(trim_u64(next_num)),
            difficulty: Some(trim_be(parent_diff)),
            prime_terminus_number: Some(pw.prime_terminus_number.clone().unwrap_or_default()),
            tx_hash: Some(ProtoHash {
                value: tx_root.to_vec(),
            }),
            location: Some(ProtoLocation {
                value: self.location().to_vec(),
            }),
            mix_hash: Some(ProtoHash {
                value: vec![0u8; 32],
            }),
            time: Some(now),
            nonce: Some(0),
            lock: Some(u32::from(lock)),
            primary_coinbase: Some(ProtoAddress {
                value: coinbase.to_vec(),
            }),
            data: extra_data[..extra_data.len().min(spec::MAXIMUM_EXTRA_DATA_SIZE)].to_vec(),
            aux_pow: None,
            sha_diff_and_count: Some(ProtoPowShareDiffAndCount {
                difficulty: Some(trim_be(sha_diff)),
                count: Some(trim_be(sha_triple.count)),
                uncled: Some(trim_be(sha_triple.uncled)),
            }),
            scrypt_diff_and_count: Some(ProtoPowShareDiffAndCount {
                difficulty: Some(trim_be(scrypt_diff)),
                count: Some(trim_be(scrypt_triple.count)),
                uncled: Some(trim_be(scrypt_triple.uncled)),
            }),
            sha_share_target: Some(
                pw.sha_share_target
                    .clone()
                    .unwrap_or_else(|| trim_u64(3 * (1u64 << 32))),
            ),
            scrypt_share_target: Some(
                pw.scrypt_share_target
                    .clone()
                    .unwrap_or_else(|| trim_u64(3 * (1u64 << 32))),
            ),
            kawpow_difficulty: Some(
                pw.kawpow_difficulty
                    .clone()
                    .unwrap_or_else(|| trim_be(parent_diff)),
            ),
        };

        // Attach AuxPoW when pow_id is KAWPOW, SHA, or Scrypt.
        let schedule = self.aux_schedule();
        let mut aux_key_opt = None;
        if matches!(pow_id, POW_KAWPOW | POW_SHA_BTC | POW_SHA_BCH | POW_SCRYPT) {
            let seal_header = Header::decode_auxiliary(&wo_header.encode_to_vec(), schedule)
                .map_err(|e| format!("encode pending header: {e}"))?;
            let quai_seal = seal_header.seal_hash();
            let tpl = match self.best_aux_template(pow_id) {
                Some(t) => t,
                None if cfg.allow_unsigned_fallback => {
                    default_fallback_template(pow_id, now as u32, next_num)
                }
                None => return Err(format!("no aux template available for powId {pow_id}")),
            };
            let aux_pow2 = tpl.aux_pow2.clone().unwrap_or_default();
            let commitment = aux_commitment(pow_id, &aux_pow2, quai_seal)?;
            let sig_time = tpl.signature_time.unwrap_or(now as u32);
            let donor_height = tpl.height.unwrap_or(next_num as u32).max(1);
            let coinbase_out = tpl.coinbase_out.as_deref().unwrap_or_default();
            let coinbase_tx =
                build_coinbase_tx(pow_id, donor_height, coinbase_out, commitment, sig_time);
            let merkle_root = coinbase::merkle_root(&coinbase_tx, &tpl.merkle_branch)
                .map_err(|e| format!("aux merkle root: {e}"))?;
            let mut prev_hash = [0u8; 32];
            if let Some(ph) = tpl.prev_hash.as_deref() {
                let n = ph.len().min(32);
                prev_hash[..n].copy_from_slice(&ph[..n]);
            }
            let donor_header = build_donor_header(
                pow_id,
                tpl.version.unwrap_or(0x2000_0000),
                prev_hash,
                merkle_root,
                now.max(u64::from(sig_time)) as u32,
                tpl.bits.unwrap_or(0x1f00_ffff),
                0,
                donor_height,
                [0u8; 32],
            );
            let proto_aux = ProtoAuxPow {
                chain_id: Some(u32::from(pow_id)),
                header: Some(donor_header),
                signature: tpl.sigs.clone(),
                merkle_branch: tpl.merkle_branch.clone(),
                transaction: Some(coinbase_tx),
                signature_time: Some(u64::from(sig_time)),
                auxpow2: if pow_id == POW_SCRYPT {
                    Some(aux_pow2)
                } else {
                    None
                },
            };
            wo_header.aux_pow = Some(proto_aux);
            aux_key_opt = Some(aux_lookup_key(commitment, sig_time));
        }

        let proto_wo = ProtoWorkObject {
            wo_header: Some(wo_header),
            wo_body: Some(ProtoWorkObjectBody {
                header: Some(next_body_header),
                transactions: Some(ProtoTransactions {
                    transactions: proto_txs,
                }),
                uncles: Some(ProtoWorkObjectHeaders { wo_headers: uncles }),
                outbound_etxs: Some(ProtoTransactions {
                    transactions: vec![],
                }),
                manifest: None,
                interlink_hashes: None,
            }),
            tx: None,
        };
        let encoded = proto_wo.encode_to_vec();
        let wo = WorkObject::decode_auxiliary(&encoded, schedule)
            .map_err(|e| format!("decode built work object: {e}"))?;
        let final_hdr = wo
            .auxiliary_work_header(schedule)
            .map_err(|e| format!("pending work header: {e}"))?;

        {
            let mut by_seal = self
                .pending_by_seal
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            by_seal.insert(final_hdr.seal_hash(), wo.clone());
        }
        if let Some(aux_key) = aux_key_opt {
            let mut by_aux = self
                .pending_by_aux_key
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            by_aux.insert(aux_key, wo.clone());
        }
        let _ = self.pending_header_tx.send(encoded);
        Ok(wo)
    }

    /// Answer `quai_getBlockTemplate`.
    pub fn get_block_template(&self, params: &BlockTemplateParams) -> Result<Value, String> {
        let wo = self.generate_pending_header(params.pow_id, params.coinbase, &[], None)?;
        marshal_aux_pow_template(
            &wo,
            params.pow_id,
            &params.extra_nonce1,
            &params.extra_nonce2,
            params.extra_nonce2_len,
            &params.extra_data,
        )
    }

    /// Verify and submit a mined donor block (`quai_submitShaBlock`,
    /// `quai_submitScryptBlock`, `quai_submitKawpowBlock` & Stratum `mining.submit`).
    pub fn submit_block(&self, raw: &[u8], pow_id: u8) -> Result<SubmitOutcome, String> {
        let parsed = parse_submitted_donor_block(raw, pow_id)?;
        let aux_key = aux_lookup_key(parsed.coinbase.commitment, parsed.coinbase.signature_time);
        let pending = {
            let by_aux = self
                .pending_by_aux_key
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            by_aux.get(&aux_key).cloned()
        }
        .ok_or_else(|| "no matching pending block found for submitted donor block".to_string())?;

        // Reconstruct WorkObject with the solved AuxPoW donor header & coinbase tx.
        let mut proto_wo = ProtoWorkObject::decode(pending.encode().as_slice())
            .map_err(|e| format!("decode cached work object: {e}"))?;
        let wo_header = proto_wo
            .wo_header
            .as_mut()
            .ok_or_else(|| "cached work object missing header".to_string())?;
        let existing_aux = wo_header
            .aux_pow
            .as_ref()
            .ok_or_else(|| "cached work object missing auxpow".to_string())?;
        let merkle_branch = existing_aux.merkle_branch.clone();
        let computed_merkle = coinbase::merkle_root(&parsed.coinbase_tx, &merkle_branch)
            .map_err(|e| format!("coinbase merkle root: {e}"))?;
        if computed_merkle.as_slice() != &parsed.header[36..68] {
            return Err("submitted donor header merkle root does not match coinbase".into());
        }
        let solved_aux = ProtoAuxPow {
            chain_id: Some(u32::from(pow_id)),
            header: Some(parsed.header.clone()),
            signature: existing_aux.signature.clone(),
            merkle_branch,
            transaction: Some(parsed.coinbase_tx.clone()),
            signature_time: existing_aux.signature_time,
            auxpow2: existing_aux.auxpow2.clone(),
        };
        wo_header.aux_pow = Some(solved_aux);
        wo_header.time = Some(u64::from(parsed.ntime));

        let schedule = self.aux_schedule();
        let solved_bytes = proto_wo.encode_to_vec();
        let solved_wo = WorkObject::decode_auxiliary(&solved_bytes, schedule)
            .map_err(|e| format!("encode solved work object: {e}"))?;
        let solved_header = solved_wo
            .auxiliary_work_header(schedule)
            .map_err(|e| format!("solved header: {e}"))?;

        // Compute real PoW hash for the donor algorithm
        let pow_hash = self.compute_aux_pow_hash(pow_id, &parsed.header)?;
        let classification =
            share_rules::classify(&solved_header, pow_hash, self.workshare_p2p_threshold())
                .map_err(|e| format!("classify workshare: {e}"))?;

        // For pre-kawpow or local test chains with SHA/Scrypt where `prime < FORKS.kawpow`,
        // classify against the header's SHA/Scrypt difficulty directly so local Stratum testing
        // works seamlessly at any block height.
        let classification = if classification == Classification::Invalid
            && matches!(pow_id, POW_SHA_BTC | POW_SHA_BCH | POW_SCRYPT)
        {
            let triple =
                Triple::from_header(&solved_header, pow_id != POW_SCRYPT).unwrap_or_default();
            if triple.difficulty > U256::ZERO
                && quai_primitives::meets_blake3_target(&pow_hash, &triple.difficulty.to_be_bytes())
                    .unwrap_or(false)
            {
                Classification::Valid
            } else {
                Classification::Sub
            }
        } else {
            classification
        };

        if classification == Classification::Invalid {
            return Err("submitted block does not meet workshare or subshare target".into());
        }

        let hash = solved_header
            .block_hash()
            .map_err(|e| format!("solved hash: {e}"))?;
        let number =
            share_rules::number(solved_header.wire().number.as_deref().unwrap_or_default())
                .unwrap_or(0);

        self.record_and_broadcast(&solved_wo, &solved_header, classification, true);

        let status = match classification {
            Classification::Sub => 0,
            Classification::Valid => 1,
            Classification::Block => 2,
            Classification::Invalid => 0,
        };
        Ok(SubmitOutcome {
            hash,
            number,
            status,
            classification,
            work_object: solved_wo,
        })
    }

    /// Compute the donor PoW hash directly (SHA-256d, Scrypt, or KAWPOW).
    pub fn compute_aux_pow_hash(&self, pow_id: u8, donor_header: &[u8]) -> Result<Hash, String> {
        match pow_id {
            POW_SHA_BTC | POW_SHA_BCH => donor::sha256d(donor_header).map_err(|e| e.to_string()),
            POW_SCRYPT => donor::scrypt(donor_header).map_err(|e| e.to_string()),
            POW_KAWPOW => {
                if donor_header.len() != 120 {
                    return Err("KAWPOW donor header must be 120 bytes".into());
                }
                let seal = coinbase::double_sha(&donor_header[..80]);
                let height = u32::from_le_bytes([
                    donor_header[76],
                    donor_header[77],
                    donor_header[78],
                    donor_header[79],
                ]) as u64;
                let nonce = u64::from_le_bytes([
                    donor_header[80],
                    donor_header[81],
                    donor_header[82],
                    donor_header[83],
                    donor_header[84],
                    donor_header[85],
                    donor_header[86],
                    donor_header[87],
                ]);
                let mut mix = [0u8; 32];
                mix.copy_from_slice(&donor_header[88..120]);
                let cache = self
                    .caches
                    .get(Algorithm::Kawpow, height)
                    .map_err(|e| e.to_string())?;
                cache
                    .hash_with_mix(seal, nonce, height, mix)
                    .map_err(|e| format!("KAWPOW verify: {e}"))
            }
            _ => Err(format!("unsupported aux powId {pow_id}")),
        }
    }

    fn record_and_broadcast(
        &self,
        wo: &WorkObject,
        header: &Header,
        class: Classification,
        mined: bool,
    ) {
        if matches!(class, Classification::Valid | Classification::Block) {
            self.workshares
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .record(header, mined);
            let _ = self.workshare_tx.send(Arc::new(wo.clone()));
        }
        if class == Classification::Block {
            if let Some(sink) = self
                .block_sink
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                sink(wo.encode());
            }
        }
        if let Some(hook) = self
            .publish_hook
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            if let Ok(topic_gen) = self.chain.topic_genesis() {
                if class == Classification::Block {
                    if let Ok(proto_wo) = ProtoWorkObject::decode(wo.encode().as_slice()) {
                        let view = ProtoWorkObjectBlockView {
                            work_object: Some(proto_wo),
                        };
                        if let Ok(topic) = quai_network::topic(
                            &topic_gen,
                            &self.location(),
                            quai_network::TopicKind::Blocks,
                        ) {
                            hook(topic, view.encode_to_vec());
                        }
                    }
                } else if class == Classification::Valid {
                    if let Ok(proto_wo) = ProtoWorkObject::decode(wo.encode().as_slice()) {
                        let view = ProtoWorkObjectShareView {
                            work_object: Some(proto_wo),
                        };
                        if let Ok(topic) = quai_network::topic(
                            &topic_gen,
                            &self.location(),
                            quai_network::TopicKind::Workshares,
                        ) {
                            hook(topic, view.encode_to_vec());
                        }
                    }
                }
            }
        }
    }

    /// Answer `quai_receiveMinedHeader`.
    pub fn receive_mined_header(&self, raw: &[u8]) -> Result<(), String> {
        let schedule = self.aux_schedule();
        let wo = WorkObject::decode_auxiliary(raw, schedule)
            .map_err(|e| format!("decode mined header: {e}"))?;
        let header = wo
            .auxiliary_work_header(schedule)
            .map_err(|e| format!("work header: {e}"))?;
        let pow_hash = self.verify_work_header_pow(&header)?;
        let class = share_rules::classify(&header, pow_hash, self.workshare_p2p_threshold())
            .map_err(|e| format!("classify mined header: {e}"))?;
        if class == Classification::Invalid {
            return Err("mined header failed proof-of-work target".into());
        }
        // Attach cached body if the caller sent only header view
        let full_wo = {
            let by_seal = self
                .pending_by_seal
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            by_seal.get(&header.seal_hash()).cloned().unwrap_or(wo)
        };
        self.record_and_broadcast(&full_wo, &header, class, true);
        Ok(())
    }

    /// Answer `quai_receiveRawWorkShare`.
    pub fn receive_raw_workshare(&self, raw: &[u8]) -> Result<Classification, String> {
        let header = Header::decode_auxiliary(raw, self.aux_schedule())
            .map_err(|e| format!("decode workshare header: {e}"))?;
        self.receive_workshare_header(header)
    }

    /// Answer `quai_receiveWorkShare`.
    pub fn receive_workshare_header(&self, header: Header) -> Result<Classification, String> {
        let pow_hash = self.verify_work_header_pow(&header)?;
        let class = share_rules::classify(&header, pow_hash, self.workshare_p2p_threshold())
            .map_err(|e| format!("classify workshare: {e}"))?;
        if class == Classification::Invalid {
            return Err("workshare failed proof-of-work verification".into());
        }
        let proto_wo = ProtoWorkObject {
            wo_header: ProtoWorkObjectHeader::decode(header.encode().as_slice()).ok(),
            wo_body: Some(ProtoWorkObjectBody {
                header: Some(ProtoHeader::default()),
                transactions: Some(ProtoTransactions {
                    transactions: vec![],
                }),
                uncles: None,
                outbound_etxs: None,
                manifest: None,
                interlink_hashes: None,
            }),
            tx: None,
        };
        if let Ok(wo) = WorkObject::decode_auxiliary(&proto_wo.encode_to_vec(), self.aux_schedule())
        {
            self.record_and_broadcast(&wo, &header, class, false);
        }
        Ok(class)
    }

    /// Answer `workshare_receiveSubWorkshare`.
    pub fn receive_sub_workshare(&self, raw: &[u8]) -> Result<Classification, String> {
        let schedule = self.aux_schedule();
        let wo = WorkObject::decode_auxiliary(raw, schedule)
            .map_err(|e| format!("decode sub-workshare: {e}"))?;
        let header = wo
            .auxiliary_work_header(schedule)
            .map_err(|e| format!("sub-workshare header: {e}"))?;
        let pow_hash = self.verify_work_header_pow(&header)?;
        let class = share_rules::classify(&header, pow_hash, self.workshare_p2p_threshold())
            .map_err(|e| format!("classify sub-workshare: {e}"))?;
        match class {
            Classification::Valid | Classification::Block => {
                self.record_and_broadcast(&wo, &header, class, false);
                Ok(class)
            }
            Classification::Sub => {
                // Submit embedded tx (if any) to the node's txpool
                for tx in wo.transactions() {
                    let _ = self.view.submit_transaction(&tx.encode());
                }
                Ok(class)
            }
            Classification::Invalid => Err("sub-workshare failed proof-of-work threshold".into()),
        }
    }

    /// Answer `quai_calcOrder`.
    pub fn calc_order(&self, raw: &[u8]) -> Result<u8, String> {
        let wo = WorkObject::decode_auxiliary(raw, PINNED_SCHEDULE)
            .map_err(|e| format!("decode work object: {e}"))?;
        let header = wo
            .auxiliary_work_header(PINNED_SCHEDULE)
            .map_err(|e| format!("work header: {e}"))?;
        let pow_hash = self.verify_work_header_pow(&header)?;
        let diff = share_rules::uint(header.wire().difficulty.as_deref().unwrap_or_default())
            .map_err(|e| e.to_string())?;
        let b = wo.header().wire();
        let arr = |v: &[Vec<u8>]| -> [U256; 3] {
            [
                v.first()
                    .and_then(|x| share_rules::uint(x).ok())
                    .unwrap_or(U256::ZERO),
                v.get(1)
                    .and_then(|x| share_rules::uint(x).ok())
                    .unwrap_or(U256::ZERO),
                v.get(2)
                    .and_then(|x| share_rules::uint(x).ok())
                    .unwrap_or(U256::ZERO),
            ]
        };
        let input = rules::EntropyInput {
            hash: pow_hash,
            difficulty: diff.max(U256::ONE),
            expansion: b.expansion_number.unwrap_or(0) as u8,
            parent_entropy: arr(&b.parent_entropy),
            parent_delta: arr(&b.parent_delta_entropy),
            parent_uncled_delta: arr(&b.parent_uncled_delta_entropy),
            uncled_entropy: share_rules::uint(b.uncled_entropy.as_deref().unwrap_or_default())
                .unwrap_or(U256::ZERO),
            postfork_share_count: Some(
                wo.uncle_headers(PINNED_SCHEDULE)
                    .map(|u| u.len() as u32)
                    .unwrap_or(0),
            ),
            node_context: 2,
        };
        let entropy = rules::entropy(&input).map_err(|e| e.to_string())?;
        Ok(entropy.order)
    }

    fn verify_work_header_pow(&self, header: &Header) -> Result<Hash, String> {
        let hw = header.wire();
        if let Some(aux_bytes) = hw.aux_pow.as_deref() {
            let aux = Auxiliary::decode(aux_bytes).map_err(|e| e.to_string())?;
            return self.compute_aux_pow_hash(aux.id(), aux.header());
        }
        let prime = share_rules::number(hw.prime_terminus_number.as_deref().unwrap_or_default())
            .unwrap_or(0);
        let mix: Hash = hw
            .mix_hash
            .as_ref()
            .map(|m| m.value.as_slice())
            .unwrap_or_default()
            .try_into()
            .map_err(|_| "invalid ProgPoW mixHash".to_string())?;
        let cache = self
            .caches
            .get(Algorithm::Progpow, prime)
            .map_err(|e| e.to_string())?;
        cache
            .hash_with_mix(header.seal_hash(), hw.nonce.unwrap_or(0), prime, mix)
            .map_err(|e| format!("ProgPoW verify: {e}"))
    }

    /// Answer `quai_getPendingWorkShares`: grouped by algorithm (`kawpow`, `sha`, `scrypt`).
    pub fn pending_workshares_json(&self) -> Value {
        let ws = self.workshares.lock().unwrap_or_else(|e| e.into_inner());
        let fmt = |list: Vec<&Header>| -> Vec<Value> {
            list.into_iter()
                .map(|h| {
                    let hw = h.wire();
                    let num = share_rules::number(hw.number.as_deref().unwrap_or_default())
                        .unwrap_or(0);
                    json!({
                        "hash": h.block_hash().map(|x| encode_hex(&x)).unwrap_or_default(),
                        "parentHash": hw.parent_hash.as_ref().map(|x| encode_hex(&x.value)).unwrap_or_default(),
                        "number": format!("0x{num:x}"),
                        "difficulty": format!("0x{}", hex_plain(hw.difficulty.as_deref().unwrap_or(&[0]))),
                        "primaryCoinbase": hw.primary_coinbase.as_ref().map(|x| encode_hex(&x.value)).unwrap_or_default(),
                        "time": format!("0x{:x}", hw.time.unwrap_or(0)),
                        "lock": format!("0x{:x}", hw.lock.unwrap_or(0)),
                    })
                })
                .collect()
        };
        json!({
            "kawpow": fmt(ws.kawpow.values_recent(32)),
            "sha": fmt(ws.sha.values_recent(32)),
            "scrypt": fmt(ws.scrypt.values_recent(32)),
        })
    }

    /// Answer `debug_getWorkshareLRUDump`.
    pub fn workshare_lru_dump(&self, limit: usize) -> Value {
        let ws = self.workshares.lock().unwrap_or_else(|e| e.into_inner());
        let received: Vec<Value> = ws
            .received
            .values_recent(limit)
            .into_iter()
            .cloned()
            .collect();
        let mined: Vec<Value> = ws.mined.values_recent(limit).into_iter().cloned().collect();
        json!({
            "receivedCount": received.len(),
            "minedCount": mined.len(),
            "received": received,
            "mined": mined,
        })
    }
}
