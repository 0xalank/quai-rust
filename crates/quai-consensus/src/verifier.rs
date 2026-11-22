//! Proof-to-header binding and bounded body commitments. Full admission fails closed.
use crate::{
    fields,
    pow::{Algorithm, LightCache},
    rules, Error, VerifiedPow,
};
use quai_primitives::{
    work::{Context, WorkObject},
    Hash, Header, Schedule,
};
use std::collections::VecDeque;
use std::sync::Arc;

const WORK_MEMO_ENTRIES: usize = 512;
const WORK_MEMO_BYTES: usize = 4 * 1024 * 1024;
struct WorkEntry {
    key: Vec<u8>,
    value: VerifiedWork,
}
#[derive(Debug)]
struct ProofEntry {
    key: Vec<u8>,
    value: VerifiedPow,
}
/// Bounded, process-local successful header proofs. Only a native verifier can
/// populate this value. It cannot be decoded from a peer or authorize a block,
/// ancestry, body, state transition or canonical publication.
#[derive(Default)]
pub struct HeaderProofMemo {
    quorum_keys: Option<[[u8; 33]; 3]>,
    entries: VecDeque<ProofEntry>,
    cache: Option<Arc<LightCache>>,
}
impl std::fmt::Debug for HeaderProofMemo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeaderProofMemo")
            .field("entries", &self.entries.len())
            .field("epoch_cache", &self.cache.as_ref().map(|cache| cache.key()))
            .finish()
    }
}
/// Diagnostic logical cache accounting, not allocator usage or RSS. Misses
/// include rejected requests; only successful inclusion work enters the memo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkMemoStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: usize,
    pub logical_bytes: usize,
    pub entry_limit: usize,
    pub logical_byte_limit: usize,
}
/// The header codec schedule of the go-quai fork schedule.
pub const PINNED_SCHEDULE: Schedule = quai_primitives::spec::FORKS.header_schedule();
/// Successful inclusion work computed by a local native verifier. Like
/// `HeaderProofMemo`, only a verifier can fill it and it never carries strong
/// block proofs, failures or values decoded from a peer.
#[derive(Default)]
pub struct InclusionWorkMemo {
    quorum_keys: Option<[[u8; 33]; 3]>,
    entries: VecDeque<WorkEntry>,
}
impl std::fmt::Debug for InclusionWorkMemo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InclusionWorkMemo")
            .field("entries", &self.entries.len())
            .finish()
    }
}
impl InclusionWorkMemo {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
#[derive(Default)]
pub struct Verifier {
    cache: Option<Arc<LightCache>>,
    key: Option<(Algorithm, u64)>,
    shared: Option<Arc<crate::pow::SharedCaches>>,
    quorum: Option<crate::auxiliary::Quorum>,
    work: VecDeque<WorkEntry>,
    work_bytes: usize,
    work_hits: u64,
    work_misses: u64,
    full_proof_inclusion_reuses: u64,
    proofs: VecDeque<ProofEntry>,
    proof_bytes: usize,
    proof_hits: u64,
    proof_misses: u64,
}
impl Verifier {
    /// Quorum keys come from a trusted chain specification, never a proof payload.
    pub fn with_trusted_quorum(quorum: crate::auxiliary::Quorum) -> Self {
        Self {
            quorum: Some(quorum),
            ..Self::default()
        }
    }
    pub fn with_cache(cache: LightCache) -> Self {
        let key = cache.key();
        Self {
            cache: Some(Arc::new(cache)),
            key: Some(key),
            ..Self::default()
        }
    }
    /// Take epoch caches from a store shared with other local verifiers, so
    /// parallel workers generate each epoch once. Proof rules are unchanged.
    pub fn with_shared_caches(shared: Arc<crate::pow::SharedCaches>) -> Self {
        Self {
            shared: Some(shared),
            ..Self::default()
        }
    }
    /// Successful full proofs reused for weaker inclusion work. This counts
    /// avoided work computations, not runtime, heap, or canonical admissions.
    pub fn full_proof_inclusion_reuses(&self) -> u64 {
        self.full_proof_inclusion_reuses
    }
    pub fn work_memo_stats(&self) -> WorkMemoStats {
        WorkMemoStats {
            hits: self.work_hits,
            misses: self.work_misses,
            entries: self.work.len(),
            logical_bytes: self.work_bytes,
            entry_limit: WORK_MEMO_ENTRIES,
            logical_byte_limit: WORK_MEMO_BYTES,
        }
    }
    /// Logical accounting for successfully verified full header proofs, kept
    /// separate from weaker inclusion work. No allocator/RSS claim is implied.
    pub fn header_proof_memo_stats(&self) -> WorkMemoStats {
        WorkMemoStats {
            hits: self.proof_hits,
            misses: self.proof_misses,
            entries: self.proofs.len(),
            logical_bytes: self.proof_bytes,
            entry_limit: WORK_MEMO_ENTRIES,
            logical_byte_limit: WORK_MEMO_BYTES,
        }
    }
    /// Move successful strong proofs to another local verifier. Share the
    /// immutable, locally generated epoch cache rather than duplicating it.
    /// Inclusion-only work and failures are never exported. The original
    /// verifier retains the same cache for the next acquisition window.
    pub fn take_header_proof_memo(&mut self) -> HeaderProofMemo {
        let quorum_keys = if self.proofs.is_empty() {
            None
        } else {
            Some(
                *self
                    .quorum
                    .get_or_insert_with(crate::auxiliary::Quorum::pinned)
                    .keys(),
            )
        };
        self.proof_bytes = 0;
        HeaderProofMemo {
            quorum_keys,
            entries: std::mem::take(&mut self.proofs),
            cache: if quorum_keys.is_some() {
                self.cache.clone()
            } else {
                None
            },
        }
    }
    /// Reuse only locally produced proofs under identical immutable quorum
    /// keys. Exact normalized header bytes remain the lookup key, and ordinary
    /// validation still applies on every use. Foreign-quorum hints are ignored.
    pub fn reuse_header_proof_memo(&mut self, memo: HeaderProofMemo) -> usize {
        if memo.entries.is_empty() {
            return 0;
        }
        let keys = self
            .quorum
            .get_or_insert_with(crate::auxiliary::Quorum::pinned)
            .keys();
        if memo.quorum_keys.as_ref() != Some(keys) {
            return 0;
        }
        if let Some(cache) = memo.cache {
            self.key = Some(cache.key());
            self.cache = Some(cache);
        }
        let mut reused = 0;
        for entry in memo.entries {
            if self.proofs.iter().any(|old| old.key == entry.key) {
                continue;
            }
            self.remember_header_proof(entry.key, entry.value);
            reused += 1;
        }
        reused
    }
    fn remember_header_proof(&mut self, key: Vec<u8>, value: VerifiedPow) {
        let cost = key.len() + std::mem::size_of::<ProofEntry>();
        if cost > WORK_MEMO_BYTES {
            return;
        }
        while self.proofs.len() >= WORK_MEMO_ENTRIES || self.proof_bytes + cost > WORK_MEMO_BYTES {
            let Some(old) = self.proofs.pop_front() else {
                break;
            };
            self.proof_bytes -= old.key.len() + std::mem::size_of::<ProofEntry>();
        }
        self.proof_bytes += cost;
        self.proofs.push_back(ProofEntry { key, value });
    }
    fn remember_work(&mut self, key: Vec<u8>, value: VerifiedWork) {
        let cost = key.len() + std::mem::size_of::<WorkEntry>();
        if cost > WORK_MEMO_BYTES {
            return;
        }
        while self.work.len() >= WORK_MEMO_ENTRIES || self.work_bytes + cost > WORK_MEMO_BYTES {
            let Some(old) = self.work.pop_front() else {
                break;
            };
            self.work_bytes -= old.key.len() + std::mem::size_of::<WorkEntry>();
        }
        self.work_bytes += cost;
        self.work.push_back(WorkEntry { key, value });
    }
    fn cache(&mut self, algorithm: Algorithm, height: u64) -> Result<&LightCache, Error> {
        algorithm.sizes(height)?;
        let key = (algorithm, height / algorithm.epoch_length());
        if self.key != Some(key) {
            self.cache = None;
            self.key = None;
            self.cache = Some(match &self.shared {
                Some(shared) => shared.get(algorithm, height)?,
                None => Arc::new(LightCache::new(algorithm, height)?),
            });
            self.key = Some(key);
        }
        self.cache
            .as_deref()
            .ok_or(Error::Unavailable("proof-of-work cache"))
    }
    /// Compute and remember the native inclusion work of one share for a node
    /// scope. Returns the work hash. Nothing here admits the share: inclusion
    /// rules still run when a block includes it.
    pub fn precompute_inclusion_work(
        &mut self,
        header: &Header,
        node_location: &[u8],
    ) -> Result<Hash, Error> {
        Ok(self.work_for_inclusion(header, node_location)?.hash())
    }
    /// Move successful inclusion work to another local verifier. Strong block
    /// proofs travel only through `take_header_proof_memo`.
    pub fn take_inclusion_work_memo(&mut self) -> InclusionWorkMemo {
        let quorum_keys = if self.work.is_empty() {
            None
        } else {
            Some(
                *self
                    .quorum
                    .get_or_insert_with(crate::auxiliary::Quorum::pinned)
                    .keys(),
            )
        };
        self.work_bytes = 0;
        InclusionWorkMemo {
            quorum_keys,
            entries: std::mem::take(&mut self.work),
        }
    }
    /// Reuse inclusion work made by a local verifier under identical trusted
    /// quorum keys. The memo key still binds the node scope and exact header
    /// bytes, and inclusion policy still applies on every use.
    pub fn reuse_inclusion_work_memo(&mut self, memo: InclusionWorkMemo) -> usize {
        if memo.entries.is_empty() {
            return 0;
        }
        let keys = self
            .quorum
            .get_or_insert_with(crate::auxiliary::Quorum::pinned)
            .keys();
        if memo.quorum_keys.as_ref() != Some(keys) {
            return 0;
        }
        let mut reused = 0;
        for entry in memo.entries {
            if self.work.iter().any(|old| old.key == entry.key) {
                continue;
            }
            self.remember_work(entry.key, entry.value);
            reused += 1;
        }
        reused
    }
    /// Verify a supported block header's native ProgPoW or signed KAWPOW proof.
    /// Auxiliary bindings/quorum verification precedes KAWPOW cache generation.
    pub fn verify_header_proof(&mut self, header: &Header) -> Result<VerifiedPow, Error> {
        // Exact normalized bytes bind every field and auxiliary signature, even
        // where historical block IDs/seals omit some fields. The quorum is
        // immutable for this verifier. Errors and inclusion-only work never enter.
        let key = header.encode();
        let hit = self
            .proofs
            .iter()
            .position(|v| v.key == key)
            .and_then(|index| self.proofs.remove(index));
        if let Some(entry) = hit {
            let proof = entry.value.clone();
            self.proofs.push_back(entry);
            self.proof_hits = self.proof_hits.saturating_add(1);
            return Ok(proof);
        }
        self.proof_misses = self.proof_misses.saturating_add(1);
        let proof = self.verify_header_proof_uncached(header)?;
        self.remember_header_proof(key, proof.clone());
        Ok(proof)
    }
    fn verify_header_proof_uncached(&mut self, header: &Header) -> Result<VerifiedPow, Error> {
        let wire = header.wire();
        let height = crate::share_rules::number(fields::bytes(
            &wire.prime_terminus_number,
            "missing prime terminus number",
        )?)?;
        if let Some(bytes) = &wire.aux_pow {
            self.verify_auxiliary_components(header, false)?;
            let aux = crate::auxiliary::Auxiliary::decode(bytes)?;
            let (seal, nonce, donor_height, mix) = aux.kawpow_input()?;
            return self.cache(Algorithm::Kawpow, donor_height)?.verify(
                seal,
                nonce,
                donor_height,
                mix,
                fields::bytes(&wire.difficulty, "missing difficulty")?,
            );
        }
        if !rules::pow_id_valid(height, None, false) {
            return Err(Error::Invalid("block proof-ID policy"));
        }
        let mix: Hash = fields::hash32(&wire.mix_hash, "missing mix hash")?;
        self.cache(Algorithm::Progpow, height)?.verify(
            header.seal_hash(),
            fields::some(wire.nonce, "missing nonce")?,
            height,
            mix,
            fields::bytes(&wire.difficulty, "missing difficulty")?,
        )
    }
    /// Admit signed auxiliary bindings for a block or workshare role. This does
    /// not apply workshare difficulty/count/history or establish PoW validity.
    pub fn verify_auxiliary_components(
        &mut self,
        header: &Header,
        workshare: bool,
    ) -> Result<crate::auxiliary::Admission, Error> {
        let w = header.wire();
        let height = crate::share_rules::number(fields::bytes(
            &w.prime_terminus_number,
            "missing prime terminus number",
        )?)?;
        let aux = crate::auxiliary::Auxiliary::decode(
            w.aux_pow
                .as_ref()
                .ok_or(Error::Invalid("missing auxiliary proof"))?,
        )?;
        if !rules::pow_id_valid(height, Some(aux.id()), workshare) {
            return Err(Error::Invalid("auxiliary proof-ID policy"));
        }
        let quorum = self
            .quorum
            .get_or_insert_with(crate::auxiliary::Quorum::pinned);
        aux.admit(
            header.seal_hash(),
            fields::some(w.time, "missing time")?,
            quorum,
        )
    }
    /// Recompute a share's native work and mix. The historical unpaid donor
    /// exception is derived from node scope here, never supplied by a proof.
    pub(crate) fn work_for_inclusion(
        &mut self,
        header: &Header,
        node_location: &[u8],
    ) -> Result<VerifiedWork, Error> {
        // Exact normalized header bytes include auxiliary signatures, difficulty
        // and every field omitted from some historical block identifiers. Node
        // scope affects the unpaid foreign-donor exception and is also bound.
        // Quorum is immutable for the lifetime of this verifier.
        let mut memo_key = (node_location.len() as u64).to_be_bytes().to_vec();
        memo_key.extend_from_slice(node_location);
        let header_bytes = header.encode();
        memo_key.extend_from_slice(&header_bytes);
        let hit = self
            .work
            .iter()
            .position(|entry| entry.key == memo_key)
            .and_then(|index| self.work.remove(index));
        if let Some(entry) = hit {
            let value = entry.value.clone();
            self.work.push_back(entry);
            self.work_hits = self.work_hits.saturating_add(1);
            return Ok(value);
        }
        self.work_misses = self.work_misses.saturating_add(1);
        let reused = self
            .proofs
            .iter()
            .position(|entry| entry.key == header_bytes)
            .and_then(|index| self.proofs.remove(index));
        let hash = if let Some(entry) = reused {
            // A successful block proof has already checked this exact header's
            // mix, target, auxiliary bindings and mandatory quorum signature.
            // Inclusion permits a weaker target and, only for foreign donor
            // work, a signature exception; it adds no stronger proof condition.
            // The converse is forbidden: inclusion work never fills `proofs`.
            let hash = entry.value.hash();
            self.proofs.push_back(entry);
            self.full_proof_inclusion_reuses = self.full_proof_inclusion_reuses.saturating_add(1);
            hash
        } else {
            let w = header.wire();
            let height = crate::share_rules::number(fields::bytes(
                &w.prime_terminus_number,
                "missing prime terminus number",
            )?)?;
            if let Some(bytes) = &w.aux_pow {
                let aux = crate::auxiliary::Auxiliary::decode(bytes)?;
                let external = !crate::validation::internal_address(
                    fields::hash(&w.primary_coinbase, "missing primary coinbase")?,
                    node_location,
                );
                let quorum = self
                    .quorum
                    .get_or_insert_with(crate::auxiliary::Quorum::pinned);
                aux.admit_inclusion(
                    header.seal_hash(),
                    fields::some(w.time, "missing time")?,
                    quorum,
                    external,
                )?;
                if aux.id() == 1 {
                    let (seal, nonce, donor_height, mix) = aux.kawpow_input()?;
                    let (actual, hash) = self.cache(Algorithm::Kawpow, donor_height)?.compute(
                        seal,
                        nonce,
                        donor_height,
                    )?;
                    if actual != mix {
                        return Err(Error::Invalid("mix hash"));
                    }
                    hash
                } else if matches!(aux.id(), 2 | 3) {
                    crate::donor::sha256d(aux.header())?
                } else {
                    crate::donor::scrypt(aux.header())?
                }
            } else {
                let mix: Hash = fields::hash32(&w.mix_hash, "missing mix hash")?;
                let (actual, hash) = self.cache(Algorithm::Progpow, height)?.compute(
                    header.seal_hash(),
                    fields::some(w.nonce, "missing nonce")?,
                    height,
                )?;
                if actual != mix {
                    return Err(Error::Invalid("mix hash"));
                }
                hash
            }
        };
        let value = VerifiedWork {
            header: header
                .block_hash()
                .map_err(|_| Error::Unsupported("share header hash"))?,
            hash,
        };
        self.remember_work(memo_key, value.clone());
        Ok(value)
    }
    /// The report confirms only the selected native components. It is never a
    /// validated block token and cannot publish chain state or serve validated RPC.
    pub fn inspect_components(
        &mut self,
        bytes: &[u8],
        context: Context,
    ) -> Result<ComponentReport, Error> {
        let object = WorkObject::decode_auxiliary(bytes, PINNED_SCHEDULE)
            .map_err(|_| Error::Unsupported("bounded canonical work-object domain"))?;
        object
            .verify_commitments(context)
            .map_err(|_| Error::Invalid("body commitments"))?;
        let header = object
            .auxiliary_work_header(PINNED_SCHEDULE)
            .map_err(|_| Error::Unsupported("work header domain"))?;
        let proof = self.verify_header_proof(&header)?;
        Ok(ComponentReport {
            proof,
            body_header_hash: object.header().hash(),
            fully_validated: false,
            pending: vec![
                "complete hierarchy placement and expansion/genesis initialization",
                "auxiliary share rewards and complete history qualification",
                "full header/uncle policy across hierarchy and expansions",
                "PoEM branch selection and canonical publication",
                "economic state transitions and receipts",
            ],
        })
    }
    /// Validate a block against an explicit parent context: the chain spec,
    /// the history windows and append snapshots of each context, the heads it
    /// must extend, the orders, rollups and confirmations of earlier blocks
    /// and the prime controller state. Runs every state-free rule
    /// (`block::check`, the prime controllers included) and, with a parent
    /// state, the execution with every state root (`block::execute`): the
    /// pipeline `quai-node replay` runs.
    pub fn validate_block<P: quai_execution::historical::History>(
        &mut self,
        parent: &crate::block::Parent<'_>,
        records: [Option<Arc<crate::history::Record>>; 3],
        state: Option<&mut crate::block::ParentState<'_, P>>,
    ) -> Result<crate::block::Validated, crate::block::Failure> {
        let checked = crate::block::check(self, parent, records)?;
        let executed = match state {
            Some(state) => Some(crate::block::execute(self, parent, state, &checked)?),
            None => None,
        };
        Ok(crate::block::Validated { checked, executed })
    }
}
#[derive(Clone, Debug)]
pub struct ComponentReport {
    pub proof: VerifiedPow,
    pub body_header_hash: Hash,
    pub fully_validated: bool,
    pub pending: Vec<&'static str>,
}

#[derive(Clone, Debug)]
pub(crate) struct VerifiedWork {
    header: Hash,
    hash: Hash,
}
impl VerifiedWork {
    pub fn header(&self) -> Hash {
        self.header
    }
    pub fn hash(&self) -> Hash {
        self.hash
    }
}

#[cfg(test)]
mod work_memo_tests {
    use super::*;
    use prost::Message;

    #[test]
    fn full_header_proof_can_supply_scope_bound_inclusion_work_without_another_kernel() {
        let c: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/admission.json"))
                .unwrap();
        let raw = quai_primitives::decode_hex(
            c["branches"][0]["vectors"][0]["work_object"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let record = crate::history::Record::decode(&raw).unwrap();
        let header = record.header();
        let mut v = Verifier::default();
        let proof = v.verify_header_proof(header).unwrap();
        // Proofs are immutable native facts. Removing the already generated
        // cache makes an accidental second compute visible in this test.
        v.cache = None;
        v.key = None;
        for scope in [[0, 0], [0, 1]] {
            let work = v.work_for_inclusion(header, &scope).unwrap();
            assert_eq!(work.hash(), proof.hash());
            assert_eq!(work.header(), header.block_hash().unwrap());
            assert!(v.cache.is_none());
        }
        assert_eq!(v.full_proof_inclusion_reuses(), 2);
        assert_eq!(v.work_memo_stats().entries, 2);
        assert_eq!(v.work_memo_stats().misses, 2);
        v.work_for_inclusion(header, &[0, 0]).unwrap();
        assert_eq!(v.work_memo_stats().hits, 1);
        assert_eq!(v.full_proof_inclusion_reuses(), 2);
        assert!(v.cache.is_none());
        let mut wire = header.wire().clone();
        wire.mix_hash.as_mut().unwrap().value[0] ^= 1;
        let bad = Header::decode(&wire.encode_to_vec(), PINNED_SCHEDULE).unwrap();
        assert!(v.work_for_inclusion(&bad, &[0, 0]).is_err());
        assert_eq!(v.full_proof_inclusion_reuses(), 2);
        assert_eq!(v.work_memo_stats().entries, 2);
    }

    #[test]
    fn full_proof_handoff_can_supply_inclusion_but_never_reverses_weak_role() {
        let c: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/admission.json"))
                .unwrap();
        let raw = quai_primitives::decode_hex(
            c["branches"][0]["vectors"][0]["work_object"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let record = crate::history::Record::decode(&raw).unwrap();
        let header = record.header();
        let mut producer = Verifier::default();
        let proof = producer.verify_header_proof(header).unwrap();
        let mut receiver = Verifier::default();
        assert_eq!(
            receiver.reuse_header_proof_memo(producer.take_header_proof_memo()),
            1
        );
        receiver.cache = None;
        receiver.key = None;
        assert_eq!(
            receiver.work_for_inclusion(header, &[0, 0]).unwrap().hash(),
            proof.hash()
        );
        assert_eq!(receiver.full_proof_inclusion_reuses(), 1);
        assert!(receiver.cache.is_none());
        let mut weak = Verifier::default();
        weak.work_for_inclusion(header, &[0, 0]).unwrap();
        assert_eq!(weak.full_proof_inclusion_reuses(), 0);
        assert_eq!(weak.header_proof_memo_stats().entries, 0);
        assert_eq!(
            receiver.reuse_header_proof_memo(weak.take_header_proof_memo()),
            0
        );
        assert_eq!(receiver.header_proof_memo_stats().entries, 1);
        // Existing high-difficulty/invalid-mix tests cover rejected weak work;
        // neither inclusion-only result nor failure can populate strong proofs.
    }

    #[test]
    fn local_header_proof_handoff_binds_exact_bytes_and_trusted_quorum() {
        let c: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/admission.json"))
                .unwrap();
        let raw = quai_primitives::decode_hex(
            c["branches"][0]["vectors"][0]["work_object"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let record = crate::history::Record::decode(&raw).unwrap();
        let header = record.header();
        let mut producer = Verifier::default();
        let proof = producer.verify_header_proof(header).unwrap();
        let mut consumer = Verifier::default();
        assert_eq!(
            consumer.reuse_header_proof_memo(producer.take_header_proof_memo()),
            1
        );
        assert_eq!(producer.header_proof_memo_stats().entries, 0);
        assert_eq!(producer.header_proof_memo_stats().logical_bytes, 0);
        let before = consumer.header_proof_memo_stats();
        assert_eq!(
            consumer.verify_header_proof(header).unwrap().hash(),
            proof.hash()
        );
        assert_eq!(consumer.header_proof_memo_stats().misses, before.misses);
        assert_eq!(consumer.header_proof_memo_stats().hits, before.hits + 1);
        // Future windows and uncached validation share one local allocation.
        assert!(Arc::ptr_eq(
            producer.cache.as_ref().unwrap(),
            consumer.cache.as_ref().unwrap()
        ));
        let mut wire = header.wire().clone();
        wire.mix_hash.as_mut().unwrap().value[0] ^= 1;
        let altered = Header::decode(&wire.encode_to_vec(), PINNED_SCHEDULE).unwrap();
        assert!(consumer.verify_header_proof(&altered).is_err());
        assert_eq!(consumer.header_proof_memo_stats().entries, 1);
        assert_eq!(consumer.header_proof_memo_stats().misses, before.misses + 1);
        // Identical bytes under different trusted keys must be recomputed.
        let mut keys = *crate::auxiliary::Quorum::pinned().keys();
        keys.swap(0, 1);
        let mut foreign = Verifier::with_trusted_quorum(
            crate::auxiliary::Quorum::from_trusted_keys(keys).unwrap(),
        );
        assert_eq!(
            foreign.reuse_header_proof_memo(consumer.take_header_proof_memo()),
            0
        );
        assert_eq!(foreign.header_proof_memo_stats().entries, 0);
        assert!(foreign.cache.is_none());
        foreign.verify_header_proof(header).unwrap();
        assert_eq!(foreign.header_proof_memo_stats().misses, 1);
        assert_eq!(foreign.header_proof_memo_stats().hits, 0);
        assert_eq!(
            foreign.reuse_header_proof_memo(HeaderProofMemo::default()),
            0
        );
    }

    #[test]
    fn header_proof_handoff_excludes_weak_work_and_preserves_both_memo_bounds() {
        let c: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/admission.json"))
                .unwrap();
        let raw = quai_primitives::decode_hex(
            c["branches"][0]["vectors"][0]["work_object"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let record = crate::history::Record::decode(&raw).unwrap();
        let mut producer = Verifier::default();
        producer
            .work_for_inclusion(record.header(), &[0, 0])
            .unwrap();
        let mut consumer = Verifier::default();
        assert_eq!(
            consumer.reuse_header_proof_memo(producer.take_header_proof_memo()),
            0
        );
        assert_eq!(producer.work_memo_stats().entries, 1);
        assert_eq!(consumer.header_proof_memo_stats().entries, 0);
        // Private test construction exercises eviction without expensive proofs;
        // production callers cannot manufacture memo entries or proof values.
        let proof = producer.verify_header_proof(record.header()).unwrap();
        for i in 0..WORK_MEMO_ENTRIES + 32 {
            producer.remember_header_proof(i.to_be_bytes().to_vec(), proof.clone());
        }
        let bytes = producer.header_proof_memo_stats().logical_bytes;
        assert_eq!(
            consumer.reuse_header_proof_memo(producer.take_header_proof_memo()),
            WORK_MEMO_ENTRIES
        );
        assert_eq!(
            consumer.header_proof_memo_stats().entries,
            WORK_MEMO_ENTRIES
        );
        assert_eq!(consumer.header_proof_memo_stats().logical_bytes, bytes);
        for i in 0..10 {
            producer.remember_header_proof(vec![i; WORK_MEMO_BYTES / 3], proof.clone());
        }
        consumer.reuse_header_proof_memo(producer.take_header_proof_memo());
        let stats = consumer.header_proof_memo_stats();
        assert!(stats.entries <= WORK_MEMO_ENTRIES && stats.logical_bytes <= WORK_MEMO_BYTES);
        assert_eq!(consumer.work_memo_stats().entries, 0);
    }

    #[test]
    fn exact_header_and_scope_are_bound_without_promoting_work_to_block_proof() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/zone-blocks.json"))
                .unwrap();
        let bytes =
            quai_primitives::decode_hex(corpus["vectors"][0]["work_object"].as_str().unwrap())
                .unwrap();
        let record = crate::history::Record::decode(&bytes).unwrap();
        let header = record.header();
        let mut verifier = Verifier::default();
        let first = verifier.work_for_inclusion(header, &[0, 0]).unwrap();
        assert_eq!(
            verifier.work_for_inclusion(header, &[0, 0]).unwrap().hash(),
            first.hash()
        );
        assert_eq!(verifier.work_memo_stats().hits, 1);
        assert_eq!(verifier.work_memo_stats().misses, 1);
        verifier.work_for_inclusion(header, &[0, 1]).unwrap();
        assert_eq!(verifier.work_memo_stats().misses, 2);

        let mut wire = header.wire().clone();
        wire.mix_hash.as_mut().unwrap().value[0] ^= 1;
        let bad = Header::decode(&wire.encode_to_vec(), PINNED_SCHEDULE).unwrap();
        for _ in 0..2 {
            assert!(matches!(
                verifier.work_for_inclusion(&bad, &[0, 0]),
                Err(Error::Invalid("mix hash"))
            ));
        }
        assert_eq!(verifier.work_memo_stats().entries, 2);
        assert_eq!(verifier.work_memo_stats().misses, 4);

        let mut wire = header.wire().clone();
        wire.difficulty = Some(vec![255; 32]);
        let weak = Header::decode(&wire.encode_to_vec(), PINNED_SCHEDULE).unwrap();
        let (mix, _) = verifier
            .cache(Algorithm::Progpow, 0)
            .unwrap()
            .compute(weak.seal_hash(), wire.nonce.unwrap(), 0)
            .unwrap();
        wire.mix_hash.as_mut().unwrap().value = mix.to_vec();
        let weak = Header::decode(&wire.encode_to_vec(), PINNED_SCHEDULE).unwrap();
        verifier.work_for_inclusion(&weak, &[0, 0]).unwrap();
        verifier.work_for_inclusion(&weak, &[0, 0]).unwrap();
        assert!(verifier.verify_header_proof(&weak).is_err());
    }

    #[test]
    fn positive_work_memo_is_bounded_by_entries_and_logical_bytes() {
        let value = VerifiedWork {
            header: [1; 32],
            hash: [2; 32],
        };
        let mut verifier = Verifier::default();
        for n in 0..WORK_MEMO_ENTRIES + 32 {
            verifier.remember_work(n.to_be_bytes().to_vec(), value.clone());
        }
        assert_eq!(verifier.work_memo_stats().entries, WORK_MEMO_ENTRIES);
        assert_eq!(verifier.work.front().unwrap().key, 32usize.to_be_bytes());
        for _ in 0..10 {
            verifier.remember_work(vec![0; WORK_MEMO_BYTES / 3], value.clone());
        }
        assert!(verifier.work_memo_stats().entries <= 2);
        assert!(verifier.work_memo_stats().logical_bytes <= WORK_MEMO_BYTES);
        let before = verifier.work_memo_stats();
        verifier.remember_work(vec![0; WORK_MEMO_BYTES + 1], value);
        assert_eq!(verifier.work_memo_stats(), before);
    }
    #[test]
    fn full_header_proof_memo_binds_bytes_rejects_mutation_and_never_promotes_weak_work() {
        let c: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/admission.json"))
                .unwrap();
        let raw = quai_primitives::decode_hex(
            c["branches"][0]["vectors"][0]["work_object"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let record = crate::history::Record::decode(&raw).unwrap();
        let header = record.header();
        let mut v = Verifier::default();
        let proof = v.verify_header_proof(header).unwrap();
        let again = v.verify_header_proof(header).unwrap();
        assert_eq!(proof.hash(), again.hash());
        assert_eq!(proof.seal(), again.seal());
        assert_eq!(v.header_proof_memo_stats().hits, 1);
        let mut wire = header.wire().clone();
        wire.mix_hash.as_mut().unwrap().value[0] ^= 1;
        let wrong = Header::decode(&wire.encode_to_vec(), PINNED_SCHEDULE).unwrap();
        assert!(v.verify_header_proof(&wrong).is_err());
        assert!(v.verify_header_proof(&wrong).is_err());
        assert_eq!(v.header_proof_memo_stats().entries, 1);
        assert_eq!(v.header_proof_memo_stats().misses, 3);
        let mut wire = header.wire().clone();
        wire.difficulty = Some(vec![255; 32]);
        let weak = Header::decode(&wire.encode_to_vec(), PINNED_SCHEDULE).unwrap();
        let (mix, _) = v
            .cache(Algorithm::Progpow, 0)
            .unwrap()
            .compute(weak.seal_hash(), wire.nonce.unwrap(), 0)
            .unwrap();
        wire.mix_hash.as_mut().unwrap().value = mix.to_vec();
        let weak = Header::decode(&wire.encode_to_vec(), PINNED_SCHEDULE).unwrap();
        v.work_for_inclusion(&weak, &[0, 0]).unwrap();
        assert!(v.verify_header_proof(&weak).is_err());
        assert_eq!(v.header_proof_memo_stats().entries, 1);
    }
    #[test]
    fn shared_epoch_caches_and_inclusion_handoff_keep_scope_and_role_bound() {
        let c: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/admission.json"))
                .unwrap();
        let raw = quai_primitives::decode_hex(
            c["branches"][0]["vectors"][0]["work_object"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let record = crate::history::Record::decode(&raw).unwrap();
        let header = record.header();
        assert!(crate::pow::SharedCaches::new(0).is_err());
        let shared = Arc::new(crate::pow::SharedCaches::new(2).unwrap());
        let mut producer = Verifier::with_shared_caches(shared.clone());
        let hash = producer.precompute_inclusion_work(header, &[0, 0]).unwrap();
        assert_eq!(producer.header_proof_memo_stats().entries, 0);
        let mut consumer = Verifier::with_shared_caches(shared.clone());
        assert_eq!(
            consumer.reuse_inclusion_work_memo(producer.take_inclusion_work_memo()),
            1
        );
        assert_eq!(producer.work_memo_stats().entries, 0);
        assert_eq!(producer.work_memo_stats().logical_bytes, 0);
        assert_eq!(
            consumer.work_for_inclusion(header, &[0, 0]).unwrap().hash(),
            hash
        );
        assert_eq!(consumer.work_memo_stats().hits, 1);
        assert!(consumer.cache.is_none());
        // Inclusion work never becomes a strong proof on the receiving side.
        assert_eq!(consumer.header_proof_memo_stats().entries, 0);
        // Another node scope is a separate entry; it reuses the shared epoch.
        consumer.work_for_inclusion(header, &[0, 1]).unwrap();
        assert_eq!(consumer.work_memo_stats().misses, 1);
        assert!(Arc::ptr_eq(
            producer.cache.as_ref().unwrap(),
            consumer.cache.as_ref().unwrap()
        ));
        assert_eq!(shared.epochs(), vec![(Algorithm::Progpow, 0)]);
        // Identical bytes under different trusted keys are not reused.
        let mut keys = *crate::auxiliary::Quorum::pinned().keys();
        keys.swap(0, 1);
        let mut foreign = Verifier::with_trusted_quorum(
            crate::auxiliary::Quorum::from_trusted_keys(keys).unwrap(),
        );
        let mut again = Verifier::with_shared_caches(shared);
        again.precompute_inclusion_work(header, &[0, 0]).unwrap();
        assert_eq!(
            foreign.reuse_inclusion_work_memo(again.take_inclusion_work_memo()),
            0
        );
        assert_eq!(foreign.work_memo_stats().entries, 0);
        assert_eq!(
            foreign.reuse_inclusion_work_memo(InclusionWorkMemo::default()),
            0
        );
    }
    #[test]
    fn full_header_proof_memo_is_bounded_and_has_no_inclusion_entries() {
        let proof = VerifiedPow {
            algorithm: Algorithm::Progpow,
            seal: [1; 32],
            height: 0,
            nonce: 0,
            hash: [2; 32],
        };
        let mut v = Verifier::default();
        for i in 0..WORK_MEMO_ENTRIES + 32 {
            v.remember_header_proof(i.to_be_bytes().to_vec(), proof.clone());
        }
        assert_eq!(v.header_proof_memo_stats().entries, WORK_MEMO_ENTRIES);
        assert_eq!(v.proofs.front().unwrap().key, 32usize.to_be_bytes());
        for _ in 0..10 {
            v.remember_header_proof(vec![0; WORK_MEMO_BYTES / 3], proof.clone());
        }
        let stats = v.header_proof_memo_stats();
        assert!(stats.entries <= 2 && stats.logical_bytes <= WORK_MEMO_BYTES);
        v.remember_header_proof(vec![0; WORK_MEMO_BYTES + 1], proof);
        assert_eq!(v.header_proof_memo_stats(), stats);
        assert_eq!(v.work_memo_stats().entries, 0);
    }
}
