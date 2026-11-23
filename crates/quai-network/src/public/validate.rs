//! go-quai v0.57.0 GossipSub validators (`p2p/node/pubsubManager/gossipsub.go`,
//! `ValidatorFunc`), built from the state-free checks of quai-consensus.
//!
//! The checks run in Go's order, so a message gets Go's verdict:
//! decoding, the work-object binding (`verifyWorkObjectBinding`: PoW ID, body
//! header hash, AuxPoW seal and Merkle commitments), the body sanity checks
//! (`SanityCheckWorkObject{Block,Header,Share}ViewBody`), AuxPoW signatures,
//! the proof-of-work (`VerifySeal`), the workshare thresholds and the AuxPoW
//! template times.
//!
//! Go compares some values with its current header. This node holds no
//! chain; it uses [`Heads`]: the highest block it accepted on each context
//! topic. With no head yet, Go's genesis rules apply: blocks are accepted
//! after the proof-of-work check, and workshares are ignored (more than 5
//! blocks from height 0). Go's `VerifyHeader` returns `ErrUnknownAncestor`
//! (accept) for a block whose parent it does not hold; this node holds no
//! parents, so that is always the outcome. go-quai v0.57.0 has no bad hashes
//! (`core.BadHashes` is empty), so those two checks never trigger.
use super::gossip::parse_topic;
use crate::TopicKind;
use crypto_bigint::{Encoding as _, U256};
use prost::Message;
use quai_consensus::{
    auxiliary::{Auxiliary, Quorum},
    pow::{Algorithm, SharedCaches},
    rules, share_rules, validation,
    verifier::{Verifier, PINNED_SCHEDULE as SCHEDULE},
};
use quai_primitives::{
    auxiliary as aux_codec,
    schema::{
        self,
        block::{
            ProtoWorkObject, ProtoWorkObjectBlockView, ProtoWorkObjectHeaderView,
            ProtoWorkObjectShareView,
        },
    },
    spec::{self, FORKS},
    transaction::Transaction,
    work::{self, WorkObject},
    Hash, Header,
};
use std::sync::{Arc, RwLock};

/// gossipsub.go `c_MaxWorkShareDist`.
pub const MAX_WORKSHARE_DISTANCE: u64 = 5;
/// params `MaxTxInWorkShare`.
pub const MAX_TX_IN_WORKSHARE: usize = 300;
/// params `MaxAllowableEntropyDist`.
pub const MAX_ALLOWABLE_ENTROPY_DIST: u64 = 40;
/// params `ShareDiffRelativeThreshold`, in percent.
pub const SHARE_DIFF_RELATIVE_THRESHOLD: u64 = 90;
/// params `AuxTemplateLivenessTime`, seconds.
pub const AUX_TEMPLATE_LIVENESS: u64 = 15;
/// params `AuxTemplateStaleTime`, seconds (10 minutes).
pub const AUX_TEMPLATE_STALE: u64 = 600;
/// The slice of this node.
pub const SLICE: [u8; 2] = [0, 0];
const ZONE: usize = 2;

/// A GossipSub validation result (`pubsub.ValidationResult`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Deliver and forward.
    Accept,
    /// Drop without forwarding; no penalty.
    Ignore(String),
    /// Drop without forwarding; the message is invalid.
    Reject(String),
}
impl Verdict {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Ignore(_) => "ignore",
            Self::Reject(_) => "reject",
        }
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Accept => None,
            Self::Ignore(r) | Self::Reject(r) => Some(r),
        }
    }
}
fn reject(why: impl std::fmt::Display) -> Verdict {
    Verdict::Reject(why.to_string())
}

/// What Go's validators read from the current header of a context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub hash: Hash,
    /// `NumberU64(ctx)`.
    pub number: u64,
    /// `IntrinsicLogEntropy(VerifySeal(head))`.
    pub intrinsic: U256,
    /// `ParentEntropy(ctx)`.
    pub parent_entropy: U256,
    /// `KawpowActivationHappened()`.
    pub kawpow: bool,
    /// `ShaDiffAndCount().Difficulty()`.
    pub sha_difficulty: U256,
    /// `ScryptDiffAndCount().Difficulty()`.
    pub scrypt_difficulty: U256,
}
/// The current header of each context, as far as this node knows it.
///
/// Once the node's own canonical chain populates a context via
/// [`Heads::set_canonical`], untrusted gossip can no longer overwrite it.
#[derive(Clone, Debug, Default)]
pub struct Heads {
    slots: [Option<Head>; 3],
    canonical: [bool; 3],
}
impl Heads {
    pub fn get(&self, ctx: usize) -> Option<&Head> {
        self.slots.get(ctx).and_then(Option::as_ref)
    }
    /// Anchor a context to the node's validated canonical head; untrusted
    /// gossip cannot overwrite or poison an anchored context.
    pub fn set_canonical(&mut self, ctx: usize, head: Head) {
        if let Some(slot) = self.slots.get_mut(ctx) {
            *slot = Some(head);
            self.canonical[ctx] = true;
        }
    }
    /// Keep the higher block of an unanchored context, refusing wrap-around
    /// heights and low-difficulty poisoning attempts.
    pub fn offer(&mut self, ctx: usize, head: Head) -> bool {
        let Some(slot) = self.slots.get_mut(ctx) else {
            return false;
        };
        if self.canonical[ctx] {
            return false;
        }
        if head.number >= u64::MAX - MAX_WORKSHARE_DISTANCE {
            return false;
        }
        if let Some(old) = slot.as_ref() {
            if old.number >= head.number {
                return false;
            }
            if head.intrinsic < old.intrinsic.wrapping_div(&U256::from_u8(2)) {
                return false;
            }
        }
        *slot = Some(head);
        true
    }
}

/// Build a gossip validator [`Head`] from a validated canonical chain record
/// and its verified proof-of-work hash.
pub fn head_from_record(
    ctx: usize,
    record: &quai_consensus::history::Record,
    pow: Hash,
) -> Result<Head, String> {
    let header = record.header();
    let body = record.object().header().wire();
    let intrinsic = rules::intrinsic(pow).map_err(|e| e.to_string())?;
    let parent_entropy = uint_of(
        body.parent_entropy
            .get(ctx)
            .map(Vec::as_slice)
            .unwrap_or_default(),
    )?;
    let number = record.number(ctx).map_err(|e| e.to_string())?;
    let prime = number_of(header.wire().prime_terminus_number.as_deref())?;
    Ok(Head {
        hash: record.id(),
        number,
        intrinsic,
        parent_entropy,
        kawpow: prime >= FORKS.kawpow,
        sha_difficulty: share_rules::Triple::from_header(header, true)
            .map_err(|e| e.to_string())?
            .difficulty,
        scrypt_difficulty: share_rules::Triple::from_header(header, false)
            .map_err(|e| e.to_string())?
            .difficulty,
    })
}

/// The verdict and what the validator learned.
#[derive(Clone, Debug)]
pub struct Checked {
    pub verdict: Verdict,
    pub kind: Option<TopicKind>,
    /// 0 prime, 1 region, 2 zone.
    pub context: usize,
    /// The work object hash, once decoding got that far.
    pub hash: Option<Hash>,
    /// The number in the topic's context, once known.
    pub number: Option<u64>,
    /// The PoW algorithm: 0 ProgPoW, 1 KAWPOW, 2 SHA_BTC, 3 SHA_BCH, 4 Scrypt.
    pub pow_id: Option<u8>,
}

/// One validator per thread; the heads are shared.
pub struct Validator {
    verifier: Verifier,
    caches: Arc<SharedCaches>,
    quorum: Quorum,
    heads: Arc<RwLock<Heads>>,
    /// Slice `[region, zone]` handled by this node.
    slice: [u8; 2],
    /// `--workshare.p2pthreshold`, go-quai default 7.
    p2p_threshold: u16,
}
pub(crate) fn number_of(bytes: Option<&[u8]>) -> Result<u64, String> {
    share_rules::number(bytes.unwrap_or_default()).map_err(|e| e.to_string())
}
fn uint_of(bytes: &[u8]) -> Result<U256, String> {
    share_rules::uint(bytes).map_err(|e| e.to_string())
}
fn context_of(ctx: usize) -> work::Context {
    match ctx {
        0 => work::Context::Prime,
        1 => work::Context::Region,
        _ => work::Context::Zone,
    }
}
fn decode_view<M: Message + Default>(name: &str, data: &[u8]) -> Result<M, Verdict> {
    schema::preflight(name, data).map_err(reject)?;
    M::decode(data).map_err(reject)
}
fn aux_id(header: &Header) -> Result<Option<Auxiliary>, Verdict> {
    header
        .wire()
        .aux_pow
        .as_ref()
        .map(|b| Auxiliary::decode(b))
        .transpose()
        .map_err(|e| reject(format!("auxpow: {e}")))
}

impl Validator {
    pub fn new(caches: Arc<SharedCaches>, heads: Arc<RwLock<Heads>>) -> Self {
        Self::with_slice(caches, heads, SLICE)
    }
    pub fn with_slice(
        caches: Arc<SharedCaches>,
        heads: Arc<RwLock<Heads>>,
        slice: [u8; 2],
    ) -> Self {
        Self {
            verifier: Verifier::with_shared_caches(caches.clone()),
            caches,
            quorum: Quorum::pinned(),
            heads,
            slice,
            p2p_threshold: spec::WORKSHARE_P2P_THRESHOLD_DIFF,
        }
    }
    pub fn slice(&self) -> [u8; 2] {
        self.slice
    }
    pub fn heads(&self) -> Heads {
        self.heads.read().map(|h| h.clone()).unwrap_or_default()
    }
    /// Validate one message of `topic` at Unix time `now`.
    pub fn validate(&mut self, topic: &str, data: &[u8], now: u64) -> Checked {
        let mut c = Checked {
            verdict: Verdict::Accept,
            kind: None,
            context: 0,
            hash: None,
            number: None,
            pow_id: None,
        };
        let t = match parse_topic(topic) {
            Ok(t) => t,
            Err(e) => {
                c.verdict = reject(format!("topic: {e}"));
                return c;
            }
        };
        c.kind = Some(t.kind);
        c.context = t.context();
        // Go deref's the backend of the topic location; another location panics
        // and the recovered validator rejects.
        if !self.slice.starts_with(&t.location) {
            c.verdict = reject("no backend for this location");
            return c;
        }
        let result = match t.kind {
            TopicKind::Blocks => self.block(&mut c, data, false),
            TopicKind::Headers => self.block(&mut c, data, true),
            TopicKind::Workshares => self.share(&mut c, data),
            TopicKind::AuxTemplate => aux_template(&self.quorum, data, now),
            TopicKind::Transactions => Err(reject("data type not supported")),
        };
        c.verdict = match result {
            Ok(v) | Err(v) => v,
        };
        c
    }

    /// `verifyWorkObjectBinding`: PoW ID, body header hash, AuxPoW binding.
    fn binding(object: &WorkObject, header: &Header, workshare: bool) -> Result<(), Verdict> {
        let w = header.wire();
        let prime = number_of(w.prime_terminus_number.as_deref()).map_err(reject)?;
        let aux = aux_id(header)?;
        if !rules::pow_id_valid(prime, aux.as_ref().map(Auxiliary::id), workshare) {
            return Err(reject("pow id is not valid for this prime terminus"));
        }
        if !workshare {
            let committed = &w.header_hash.as_ref().ok_or(reject("header hash"))?.value;
            if committed.as_slice() != object.header().hash() {
                return Err(reject("header hash mismatch"));
            }
        }
        if let Some(aux) = aux.filter(|_| prime >= FORKS.kawpow) {
            aux.verify_binding(header.seal_hash())
                .map_err(|e| reject(format!("auxpow binding: {e}")))?;
        }
        Ok(())
    }
    /// `VerifySeal`: the proof-of-work hash of a block header, which must meet
    /// the header difficulty (the ProgPoW or KAWPOW engine, as Go selects it).
    fn seal(&self, header: &Header) -> Result<Hash, String> {
        let w = header.wire();
        let difficulty = w.difficulty.as_deref().unwrap_or_default();
        if difficulty.iter().all(|b| *b == 0) {
            return Err("invalid difficulty".into());
        }
        let proof = match &w.aux_pow {
            Some(bytes) => {
                let aux = Auxiliary::decode(bytes).map_err(|e| e.to_string())?;
                let (seal, nonce, height, mix) = aux.kawpow_input().map_err(|e| e.to_string())?;
                self.caches
                    .get(Algorithm::Kawpow, height)
                    .and_then(|c| c.verify(seal, nonce, height, mix, difficulty))
            }
            None => {
                let prime = number_of(w.prime_terminus_number.as_deref())?;
                let mix: Hash = w
                    .mix_hash
                    .as_ref()
                    .map(|m| m.value.as_slice())
                    .unwrap_or_default()
                    .try_into()
                    .map_err(|_| "mix hash")?;
                self.caches.get(Algorithm::Progpow, prime).and_then(|c| {
                    c.verify(
                        header.seal_hash(),
                        w.nonce.unwrap_or_default(),
                        prime,
                        mix,
                        difficulty,
                    )
                })
            }
        };
        proof.map(|p| p.hash()).map_err(|e| e.to_string())
    }
    /// `ConvertToTemplate().VerifySignature()`: the composite MuSig2 signature
    /// of the AuxPoW template by 2 of the 3 quorum keys.
    fn signature(&self, aux: &Auxiliary) -> bool {
        !aux.signature().is_empty()
            && aux.template().is_ok_and(|t| {
                self.quorum
                    .verify(aux_codec::template_hash(&t), aux.signature())
            })
    }

    /// Block views (`blocks`) and header views (`headers`) of a context.
    fn block(
        &mut self,
        c: &mut Checked,
        data: &[u8],
        header_view: bool,
    ) -> Result<Verdict, Verdict> {
        let ctx = c.context;
        let wo: Option<ProtoWorkObject> = if header_view {
            decode_view::<ProtoWorkObjectHeaderView>(".block.ProtoWorkObjectHeaderView", data)?
                .work_object
        } else {
            decode_view::<ProtoWorkObjectBlockView>(".block.ProtoWorkObjectBlockView", data)?
                .work_object
        };
        let wo = wo
            .filter(|wo| wo.wo_header.as_ref().is_some_and(|h| h.location.is_some()))
            .ok_or(reject("missing work object location"))?;
        let object = WorkObject::decode_auxiliary(&wo.encode_to_vec(), SCHEDULE).map_err(reject)?;
        let header = object.auxiliary_work_header(SCHEDULE).map_err(reject)?;
        c.hash = Some(header.block_hash().map_err(reject)?);
        let body = object.header().wire();
        c.number = Some(if ctx == ZONE {
            number_of(header.wire().number.as_deref()).map_err(reject)?
        } else {
            number_of(body.number.get(ctx).map(Vec::as_slice)).map_err(reject)?
        });
        c.pow_id = Some(aux_id(&header)?.map_or(0, |a| a.id()));
        // SanityCheckWorkObject{Block,Header}ViewBody.
        Self::binding(&object, &header, false)?;
        if header.wire().lock.unwrap_or_default() as usize >= spec::LOCK_PERIODS.by_lock_byte.len()
        {
            return Err(reject("work object header has invalid lockup byte"));
        }
        if ctx != ZONE {
            // No transactions, ETXs or uncles; a non-empty subordinate manifest
            // that matches the header; prime: the interlink root.
            object
                .verify_commitments(context_of(ctx))
                .map_err(|e| reject(format!("body: {e}")))?;
        } else {
            if header_view {
                let proto_body = wo.wo_body.as_ref();
                let txs = proto_body
                    .and_then(|b| b.transactions.as_ref())
                    .map_or(0, |t| t.transactions.len());
                if txs != 0 {
                    return Err(reject("zone body has non zero transactions"));
                }
                if !object.manifest_hashes().is_empty() {
                    return Err(reject("zone body has non zero manifests"));
                }
                let links = proto_body
                    .and_then(|b| b.interlink_hashes.as_ref())
                    .map_or(0, |l| l.hashes.len());
                if links != 0 {
                    return Err(reject("zone body has non zero interlink hashes"));
                }
                let etx = object.outbound_root().map_err(reject)?;
                if body.outbound_etx_hash.as_ref().map(|h| h.value.as_slice()) != Some(&etx[..]) {
                    return Err(reject("outbound transaction hash mismatch"));
                }
            } else {
                object
                    .verify_commitments(work::Context::Zone)
                    .map_err(|e| reject(format!("body: {e}")))?;
            }
            let coinbase = header
                .wire()
                .primary_coinbase
                .as_ref()
                .map(|a| a.value.clone())
                .unwrap_or_default();
            let prime =
                number_of(header.wire().prime_terminus_number.as_deref()).map_err(reject)?;
            if coinbase.get(1).is_some_and(|b| *b > 127) && prime < FORKS.controller_kick_in {
                return Err(reject(
                    "work object primary coinbase is in qi ledger scope before controller kick in block",
                ));
            }
        }
        if let Some(aux) = aux_id(&header)? {
            if !self.signature(&aux) {
                return Err(reject("auxpow signature verification failed"));
            }
        }
        // ApplyPoWFilter.
        let pow = self
            .seal(&header)
            .map_err(|e| reject(format!("seal: {e}")))?;
        let intrinsic = rules::intrinsic(pow).map_err(reject)?;
        let entropy = |c: usize| {
            uint_of(
                body.parent_entropy
                    .get(c)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            )
        };
        let parent_s = entropy(ctx).map_err(reject)?;
        let candidate = Head {
            hash: c.hash.unwrap_or_default(),
            number: c.number.unwrap_or_default(),
            intrinsic,
            parent_entropy: parent_s,
            kawpow: number_of(header.wire().prime_terminus_number.as_deref()).map_err(reject)?
                >= FORKS.kawpow,
            sha_difficulty: share_rules::Triple::from_header(&header, true)
                .map_err(reject)?
                .difficulty,
            scrypt_difficulty: share_rules::Triple::from_header(&header, false)
                .map_err(reject)?
                .difficulty,
        };
        let head = self.heads().get(ctx).cloned();
        if let Some(head) = head {
            if intrinsic < head.intrinsic.wrapping_div(&U256::from_u8(2)) {
                return Ok(Verdict::Ignore(
                    "block intrinsic below half of the head".into(),
                ));
            }
            let broadcast = entropy(ZONE).map_err(reject)?;
            let max = head
                .intrinsic
                .saturating_mul(&U256::from_u64(MAX_ALLOWABLE_ENTROPY_DIST));
            if head.parent_entropy > broadcast.saturating_add(&max) {
                return Ok(Verdict::Ignore(
                    "block entropy too far below the head".into(),
                ));
            }
        }
        // VerifyHeader: the parent is unknown here, ErrUnknownAncestor: accept.
        if let Ok(mut heads) = self.heads.write() {
            heads.offer(ctx, candidate);
        }
        Ok(Verdict::Accept)
    }

    /// Workshare views (`worksharev2`), zone only.
    fn share(&mut self, c: &mut Checked, data: &[u8]) -> Result<Verdict, Verdict> {
        let view =
            decode_view::<ProtoWorkObjectShareView>(".block.ProtoWorkObjectShareView", data)?;
        let wo = view
            .work_object
            .filter(|wo| wo.wo_header.as_ref().is_some_and(|h| h.location.is_some()))
            .ok_or(reject("missing work object location"))?;
        let header = Header::decode_auxiliary(
            &wo.wo_header
                .as_ref()
                .map(Message::encode_to_vec)
                .unwrap_or_default(),
            SCHEDULE,
        )
        .map_err(reject)?;
        c.hash = Some(header.block_hash().map_err(reject)?);
        let w = header.wire();
        let number = number_of(w.number.as_deref()).map_err(reject)?;
        c.number = Some(number);
        let aux = aux_id(&header)?;
        c.pow_id = Some(aux.as_ref().map_or(0, Auxiliary::id));
        let body = wo
            .wo_body
            .as_ref()
            .ok_or(reject("missing work object body"))?;
        let txs = body
            .transactions
            .as_ref()
            .map(|t| {
                t.transactions
                    .iter()
                    .map(|tx| Transaction::decode(&tx.encode_to_vec()))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
            .map_err(reject)?
            .unwrap_or_default();
        // SanityCheckWorkObjectShareViewBody.
        if c.context != ZONE {
            return Err(reject("work object shares dont exist in non zone chains"));
        }
        let prime = number_of(w.prime_terminus_number.as_deref()).map_err(reject)?;
        if !rules::pow_id_valid(prime, aux.as_ref().map(Auxiliary::id), true) {
            return Err(reject(
                "workshare pow id is not valid for this prime terminus",
            ));
        }
        if w.lock.unwrap_or_default() as usize >= spec::LOCK_PERIODS.by_lock_byte.len() {
            return Err(reject("work object header has invalid lockup byte"));
        }
        // validateUncleData, the early lockup byte and the Qi coinbase rule.
        validation::data_policy(&header, prime, true)
            .map_err(|e| reject(format!("header data: {e}")))?;
        let nonempty = |n: usize, what: &str| {
            if n == 0 {
                Ok(())
            } else {
                Err(reject(format!("zone body has non zero {what}")))
            }
        };
        nonempty(
            body.outbound_etxs
                .as_ref()
                .map_or(0, |t| t.transactions.len()),
            "outbound etxs",
        )?;
        nonempty(
            body.manifest.as_ref().map_or(0, |m| m.manifest.len()),
            "manifests",
        )?;
        nonempty(
            body.interlink_hashes.as_ref().map_or(0, |l| l.hashes.len()),
            "interlink hashes",
        )?;
        nonempty(
            body.uncles.as_ref().map_or(0, |u| u.wo_headers.len()),
            "uncles",
        )?;
        let root = work::transactions_root(&txs).map_err(reject)?;
        if w.tx_hash.as_ref().map(|h| h.value.as_slice()) != Some(&root[..]) {
            return Err(reject("transaction root hash mismatch"));
        }
        // Distance from the current zone header.
        let head = self.heads().get(ZONE).cloned();
        let head_number = head.as_ref().map_or(0, |h| h.number);
        if number.saturating_add(MAX_WORKSHARE_DISTANCE) < head_number {
            return Ok(Verdict::Ignore("workshare too far behind the head".into()));
        }
        if number > head_number.saturating_add(MAX_WORKSHARE_DISTANCE) {
            return Ok(Verdict::Ignore(
                "workshare too far ahead of the head".into(),
            ));
        }
        if txs.len() > MAX_TX_IN_WORKSHARE {
            return Err(reject(format!(
                "workshare contains {} transactions",
                txs.len()
            )));
        }
        // The AuxPoW checks (signature time, seal and Merkle commitments,
        // coinbase outpoint, quorum signature with Go's exception for SHA and
        // Scrypt shares paid outside the chain) and the share work.
        let pow = self
            .verifier
            .precompute_inclusion_work(&header, &self.slice)
            .map_err(|e| reject(format!("workshare: {e}")))?;
        let id = aux.as_ref().map(Auxiliary::id);
        if prime >= FORKS.kawpow && matches!(id, Some(2..=4)) {
            let Some(head) = head else {
                return Ok(Verdict::Ignore("no head after the KAWPOW fork".into()));
            };
            if !head.kawpow {
                return Ok(Verdict::Ignore("head is before the KAWPOW fork".into()));
            }
            let sha = id != Some(4);
            let share_diff = share_rules::Triple::from_header(&header, sha)
                .map_err(reject)?
                .difficulty;
            if share_diff == U256::ZERO {
                return Err(reject("workshare has invalid share difficulty"));
            }
            let head_diff = if sha {
                head.sha_difficulty
            } else {
                head.scrypt_difficulty
            };
            let threshold = head_diff
                .saturating_mul(&U256::from_u64(SHARE_DIFF_RELATIVE_THRESHOLD))
                .wrapping_div(&U256::from_u64(100));
            if share_diff < threshold {
                return Err(reject("workshare difficulty is below 90% of the head"));
            }
            // pow <= 2^256/difficulty, as in VerifySeal.
            if !quai_primitives::meets_blake3_target(&pow, &share_diff.to_be_bytes())
                .map_err(reject)?
            {
                return Err(reject("workshare hash exceeds target"));
            }
        } else {
            let class = share_rules::classify(&header, pow, self.p2p_threshold).map_err(reject)?;
            if class == share_rules::Classification::Invalid {
                return Err(reject("workshare has less entropy than the p2p threshold"));
            }
            let is_workshare = matches!(
                class,
                share_rules::Classification::Valid | share_rules::Classification::Block
            );
            if !is_workshare && txs.is_empty() {
                return Err(reject("sub-share without transactions"));
            }
            let Some(head) = head else {
                return Ok(Verdict::Accept);
            };
            let intrinsic = rules::intrinsic(pow).map_err(reject)?;
            if intrinsic < head.intrinsic.wrapping_div(&U256::from_u8(2)) {
                return Ok(Verdict::Ignore(
                    "workshare intrinsic below half of the head".into(),
                ));
            }
        }
        Ok(Verdict::Accept)
    }
}

/// AuxPoW templates (`auxtemplate`).
fn aux_template(quorum: &Quorum, data: &[u8], now: u64) -> Result<Verdict, Verdict> {
    let t = aux_codec::canonical_template(data).map_err(reject)?;
    let sigs = t.sigs.clone().unwrap_or_default();
    if sigs.is_empty() || !quorum.verify(aux_codec::template_hash(&t), &sigs) {
        return Err(reject("aux template signature verification failed"));
    }
    let signed = u64::from(t.signature_time.unwrap_or_default());
    if now.saturating_add(AUX_TEMPLATE_LIVENESS) < signed {
        return Err(reject("aux template signature time is in the future"));
    }
    if now > signed.saturating_add(AUX_TEMPLATE_STALE) {
        return Err(reject("aux template is stale"));
    }
    if now > signed.saturating_add(AUX_TEMPLATE_LIVENESS) {
        return Ok(Verdict::Ignore(
            "aux template is older than the liveness time".into(),
        ));
    }
    Ok(Verdict::Accept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::public::COLOSSEUM_TOPIC_GENESIS;
    use quai_primitives::{decode_hex, schema::block::ProtoTransactions};

    /// Mainnet gossip captured from the colosseum boot peers (2026-11-18).
    const CAPTURE: &str = include_str!("../../tests/data/mainnet-gossip.jsonl");
    struct Sample {
        topic: String,
        received: u64,
        data: Vec<u8>,
    }
    fn samples(suffix: &str) -> Vec<Sample> {
        CAPTURE
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|v| v["topic"].as_str().unwrap().ends_with(suffix))
            .map(|v| Sample {
                topic: v["topic"].as_str().unwrap().to_string(),
                received: v["received_unix"].as_u64().unwrap(),
                data: decode_hex(v["data"].as_str().unwrap()).unwrap(),
            })
            .collect()
    }
    fn validator() -> Validator {
        Validator::new(
            Arc::new(SharedCaches::new(4).unwrap()),
            Arc::new(RwLock::new(Heads::default())),
        )
    }
    fn tname(location: &str, kind: &str) -> String {
        format!("{COLOSSEUM_TOPIC_GENESIS}/{location}/{kind}")
    }

    #[test]
    fn capture_has_every_go_topic_with_data() {
        for suffix in [
            "//blocks",
            "/0/blocks",
            "/0,0/blocks",
            "/0,0/headers",
            "/0,0/worksharev2",
            "/0,0/auxtemplate",
        ] {
            assert!(!samples(suffix).is_empty(), "{suffix}");
        }
    }

    #[test]
    fn aux_templates_follow_go_signature_and_time_rules() {
        let quorum = Quorum::pinned();
        for s in samples("/0,0/auxtemplate") {
            let t = aux_codec::canonical_template(&s.data).unwrap();
            let signed = u64::from(t.signature_time.unwrap());
            assert_eq!(aux_template(&quorum, &s.data, signed), Ok(Verdict::Accept));
            assert!(matches!(
                aux_template(&quorum, &s.data, signed + 16),
                Ok(Verdict::Ignore(_))
            ));
            assert!(matches!(
                aux_template(&quorum, &s.data, signed - 16),
                Err(Verdict::Reject(_))
            ));
            // A changed field breaks the quorum signature.
            let mut bad = t.clone();
            bad.bits = Some(bad.bits.unwrap() ^ 1);
            assert!(matches!(
                aux_template(&quorum, &bad.encode_to_vec(), signed),
                Err(Verdict::Reject(_))
            ));
            let mut unsigned = t.clone();
            unsigned.sigs = Some(vec![]);
            assert!(matches!(
                aux_template(&quorum, &unsigned.encode_to_vec(), signed),
                Err(Verdict::Reject(_))
            ));
            // Through the topic dispatch as well.
            let mut v = validator();
            assert_eq!(
                v.validate(&s.topic, &s.data, signed).verdict,
                Verdict::Accept
            );
            assert!(s.received > 0);
        }
    }

    #[test]
    fn transaction_topic_and_foreign_locations_are_rejected_as_in_go() {
        let mut v = validator();
        let data = &samples("/0,0/blocks")[0].data;
        for topic in [
            tname("0,0", "transactions"),
            tname("1,0", "blocks"),
            tname("0,0", "unknown"),
        ] {
            assert!(
                matches!(v.validate(&topic, data, 0).verdict, Verdict::Reject(_)),
                "{topic}"
            );
        }
        // A workshare on a region topic: Go's share sanity check refuses it.
        let share = &samples("/0,0/worksharev2")[0].data;
        assert!(matches!(
            v.validate(&tname("0", "worksharev2"), share, 0).verdict,
            Verdict::Reject(_)
        ));
        // Garbage, and a block view on the share topic.
        assert!(matches!(
            v.validate(&tname("0,0", "blocks"), &[1, 2, 3], 0).verdict,
            Verdict::Reject(_)
        ));
        assert!(matches!(
            v.validate(&tname("0,0", "worksharev2"), data, 0).verdict,
            Verdict::Reject(_)
        ));
    }

    /// KAWPOW checks generate an 80 MB epoch cache; run them in release.
    #[test]
    #[cfg_attr(debug_assertions, ignore)]
    fn live_blocks_headers_and_shares_get_go_verdicts() {
        let mut v = validator();
        // With no head (Go's genesis rules) every block passes after its PoW.
        for suffix in ["//blocks", "/0/blocks", "/0,0/blocks", "/0,0/headers"] {
            for s in samples(suffix) {
                let c = v.validate(&s.topic, &s.data, s.received);
                assert_eq!(c.verdict, Verdict::Accept, "{} {:?}", s.topic, c);
                assert_eq!(c.pow_id, Some(1));
            }
        }
        let head = v.heads().get(ZONE).cloned().unwrap();
        // Shares: within 5 of the head and above 90% of its share difficulty.
        let mut accepted = 0;
        for s in samples("/0,0/worksharev2") {
            let c = v.validate(&s.topic, &s.data, s.received);
            match &c.verdict {
                Verdict::Accept => accepted += 1,
                Verdict::Ignore(why) => assert!(why.contains("far"), "{why}"),
                Verdict::Reject(why) => panic!("{why}"),
            }
            assert!(matches!(c.pow_id, Some(2..=4)));
        }
        assert!(accepted > 0);
        // A share 6 blocks behind the head is ignored, as in Go.
        let mut far = v.heads();
        let mut ahead = head.clone();
        ahead.number += 50;
        far.offer(ZONE, ahead);
        let mut w = Validator::new(
            Arc::new(SharedCaches::new(2).unwrap()),
            Arc::new(RwLock::new(far)),
        );
        let s = &samples("/0,0/worksharev2")[0];
        assert!(matches!(
            w.validate(&s.topic, &s.data, s.received).verdict,
            Verdict::Ignore(_)
        ));
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore)]
    fn go_rejected_block_variants_are_rejected() {
        let s = &samples("/0,0/blocks")[0];
        let view = ProtoWorkObjectBlockView::decode(s.data.as_slice()).unwrap();
        let wo = view.work_object.clone().unwrap();
        let encode = |wo: ProtoWorkObject| {
            ProtoWorkObjectBlockView {
                work_object: Some(wo),
            }
            .encode_to_vec()
        };
        let mut cases: Vec<(&str, Vec<u8>)> = vec![];
        // A wrong nonce in the donor header: the KAWPOW mix no longer matches.
        let mut bad = wo.clone();
        let h = bad.wo_header.as_mut().unwrap();
        let aux = h.aux_pow.as_mut().unwrap();
        aux.header.as_mut().unwrap()[80] ^= 1;
        cases.push(("seal", encode(bad)));
        // A body header field changes: the header hash no longer binds it.
        let mut bad = wo.clone();
        let b = bad.wo_body.as_mut().unwrap().header.as_mut().unwrap();
        b.gas_used = Some(b.gas_used.unwrap_or_default() + 1);
        cases.push(("header hash", encode(bad)));
        // An extra transaction: the transaction root no longer matches.
        let mut bad = wo.clone();
        let txs = bad
            .wo_body
            .as_mut()
            .unwrap()
            .transactions
            .get_or_insert_with(ProtoTransactions::default);
        if let Some(tx) = txs.transactions.first().cloned() {
            txs.transactions.push(tx);
        } else {
            txs.transactions.push(Default::default());
        }
        cases.push(("transactions", encode(bad)));
        // No location, and a lockup byte above 3.
        let mut bad = wo.clone();
        bad.wo_header.as_mut().unwrap().location = None;
        cases.push(("location", encode(bad)));
        let mut bad = wo.clone();
        bad.wo_header.as_mut().unwrap().lock = Some(4);
        cases.push(("lock", encode(bad)));
        let mut v = validator();
        for (what, data) in cases {
            let c = v.validate(&s.topic, &data, s.received);
            assert!(
                matches!(c.verdict, Verdict::Reject(_)),
                "{what}: {:?}",
                c.verdict
            );
        }
        // A zone block on the region topic: a dominant body has no transactions.
        let c = v.validate(&tname("0", "blocks"), &s.data, s.received);
        assert!(matches!(c.verdict, Verdict::Reject(_)), "{:?}", c.verdict);
        // The untouched block still passes.
        assert_eq!(
            v.validate(&s.topic, &s.data, s.received).verdict,
            Verdict::Accept
        );
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore)]
    fn go_rejected_share_variants_are_rejected() {
        let mut v = validator();
        let blocks = samples("/0,0/blocks");
        for b in &blocks {
            v.validate(&b.topic, &b.data, b.received);
        }
        let s = samples("/0,0/worksharev2")
            .into_iter()
            .find(|s| v.validate(&s.topic, &s.data, s.received).verdict == Verdict::Accept)
            .unwrap();
        let view = ProtoWorkObjectShareView::decode(s.data.as_slice()).unwrap();
        let wo = view.work_object.clone().unwrap();
        let encode = |wo: ProtoWorkObject| {
            ProtoWorkObjectShareView {
                work_object: Some(wo),
            }
            .encode_to_vec()
        };
        // A transaction that the share's transaction root does not commit to.
        let block = ProtoWorkObjectBlockView::decode(blocks[0].data.as_slice()).unwrap();
        let tx = block
            .work_object
            .unwrap()
            .wo_body
            .unwrap()
            .transactions
            .unwrap()
            .transactions[0]
            .clone();
        let mut bad = wo.clone();
        bad.wo_body.as_mut().unwrap().transactions = Some(ProtoTransactions {
            transactions: vec![tx.clone()],
        });
        let c = v.validate(&s.topic, &encode(bad), s.received);
        assert!(
            matches!(&c.verdict, Verdict::Reject(r) if r.contains("transaction root")),
            "{:?}",
            c.verdict
        );
        // 301 transactions with a matching root: over MaxTxInWorkShare.
        let many = vec![tx; MAX_TX_IN_WORKSHARE + 1];
        let decoded: Vec<Transaction> = many
            .iter()
            .map(|t| Transaction::decode(&t.encode_to_vec()).unwrap())
            .collect();
        let mut bad = wo.clone();
        bad.wo_body.as_mut().unwrap().transactions = Some(ProtoTransactions { transactions: many });
        bad.wo_header.as_mut().unwrap().tx_hash =
            Some(quai_primitives::schema::common::ProtoHash {
                value: work::transactions_root(&decoded).unwrap().to_vec(),
            });
        let c = v.validate(&s.topic, &encode(bad), s.received);
        assert!(
            matches!(&c.verdict, Verdict::Reject(r) if r.contains("transactions")),
            "{:?}",
            c.verdict
        );
        // A changed coinbase: the AuxPoW seal commitment no longer matches.
        let mut bad = wo.clone();
        let h = bad.wo_header.as_mut().unwrap();
        h.time = Some(h.time.unwrap() + 1);
        let c = v.validate(&s.topic, &encode(bad), s.received);
        assert!(matches!(c.verdict, Verdict::Reject(_)), "{:?}", c.verdict);
        // Empty header data: validateUncleData.
        let mut bad = wo.clone();
        bad.wo_header.as_mut().unwrap().data = vec![];
        let c = v.validate(&s.topic, &encode(bad), s.received);
        assert!(matches!(c.verdict, Verdict::Reject(_)), "{:?}", c.verdict);
    }

    /// fixtures/mainnet/qex1-0-8: mainnet zone blocks 0 to 8 with their prime
    /// and region records, exported from go-quai v0.57.0 (QEX1).
    const QEX_SLICE: &[u8] =
        include_bytes!("../../../../fixtures/mainnet/qex1-0-8/segments/0000000000-0000000008.qex");
    struct Record {
        context: usize,
        number: u64,
        hash: Hash,
        block: Vec<u8>,
    }
    fn take<'a>(b: &mut &'a [u8], n: usize) -> &'a [u8] {
        let (head, tail) = b.split_at(n);
        *b = tail;
        head
    }
    /// docs/mainnet-export-format.md: kind, context, number, hash, block,
    /// receipts.
    fn qex_records() -> Vec<Record> {
        let mut b = QEX_SLICE.strip_prefix(b"QEX1\n").unwrap();
        let mut out = vec![];
        while !b.is_empty() {
            assert_eq!(take(&mut b, 1), [1]);
            let context = take(&mut b, 1)[0] as usize;
            let number = u64::from_be_bytes(take(&mut b, 8).try_into().unwrap());
            let hash: Hash = take(&mut b, 32).try_into().unwrap();
            let n = u32::from_be_bytes(take(&mut b, 4).try_into().unwrap()) as usize;
            let block = take(&mut b, n).to_vec();
            let n = u32::from_be_bytes(take(&mut b, 4).try_into().unwrap()) as usize;
            take(&mut b, n);
            out.push(Record {
                context,
                number,
                hash,
                block,
            });
        }
        out
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "ProgPoW verification needs a release build"
    )]
    fn fixture_slice_blocks_get_go_verdicts() {
        let records = qex_records();
        assert_eq!(records.len(), 17);
        let as_view = |block: &[u8]| ProtoWorkObjectBlockView {
            work_object: Some(ProtoWorkObject::decode(block).unwrap()),
        };
        // Fresh validators hold no head and share one PoW cache.
        let caches = Arc::new(SharedCaches::new(4).unwrap());
        let validator = || Validator::new(caches.clone(), Arc::new(RwLock::new(Heads::default())));
        let mut checked = 0;
        for r in records.iter().filter(|r| r.number > 0) {
            let topic = tname(["", "0", "0,0"][r.context], "blocks");
            let view = as_view(&r.block);
            // No head: Go's genesis rule accepts a block whose proof-of-work
            // is valid.
            let c = validator().validate(&topic, &view.encode_to_vec(), 0);
            assert_eq!(c.verdict, Verdict::Accept, "{} {}", r.context, r.number);
            assert_eq!(c.hash, Some(r.hash), "{} {}", r.context, r.number);
            assert_eq!(c.number, Some(r.number));
            // A wrong nonce breaks the ProgPoW seal: Go rejects it.
            let mut bad = view.clone();
            let h = bad
                .work_object
                .as_mut()
                .unwrap()
                .wo_header
                .as_mut()
                .unwrap();
            h.nonce = Some(h.nonce.unwrap_or_default() ^ 1);
            let c = validator().validate(&topic, &bad.encode_to_vec(), 0);
            assert!(
                matches!(c.verdict, Verdict::Reject(_)),
                "{} {}: {:?}",
                r.context,
                r.number,
                c.verdict
            );
            // A zone block on a dominant topic, or a dominant body on the
            // zone topic, fails the body rules of that context.
            let other = tname(["0,0", "0,0", "0"][r.context], "blocks");
            let c = validator().validate(&other, &view.encode_to_vec(), 0);
            assert!(
                matches!(c.verdict, Verdict::Reject(_)),
                "{} {}: {:?}",
                r.context,
                r.number,
                c.verdict
            );
            checked += 1;
        }
        assert_eq!(checked, 14);
        // With a live mainnet head, Go's PoW filter ignores these old blocks:
        // their intrinsic difficulty is far below half of the head's.
        let mut v = validator();
        let live = &samples("/0,0/blocks")[0];
        assert_eq!(
            v.validate(&live.topic, &live.data, live.received).verdict,
            Verdict::Accept
        );
        let r = records
            .iter()
            .find(|r| r.context == ZONE && r.number == 8)
            .unwrap();
        let c = v.validate(
            &tname("0,0", "blocks"),
            &as_view(&r.block).encode_to_vec(),
            0,
        );
        assert!(matches!(c.verdict, Verdict::Ignore(_)), "{:?}", c.verdict);
    }
}
