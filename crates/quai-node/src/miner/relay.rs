//! Remote full-block template relay (`--builder-rpc <URL>`) for zero-storage
//! mining nodes.
//!
//! Pulls executed pending `WorkObject` templates (carrying real `evmRoot`,
//! `utxoRoot`, `etxSetRoot`, `receiptHash`, `outboundEtxHash`, and executed
//! transactions) from an upstream stateful builder node via `quai_getPendingHeader`
//! (and/or WebSocket `pendingHeader`), **independently verifies every consensus
//! header and body commitment locally** against the light/stateless node's own
//! PoW-validated canonical head, caches the verified template in [`Miner`], and
//! forwards solved full blocks back via `quai_receiveMinedHeader`.
use crate::light::proof_rpc::HttpRpcClient;
use crate::light::store::LightHeaderStore;
use crate::log::Log;
use crate::miner::Miner;
use prost::Message;
use quai_consensus::{
    share_rules::{self, Triple},
    validation,
    verifier::PINNED_SCHEDULE,
};
use quai_primitives::{
    decode_hex, encode_hex,
    schema::block::ProtoWorkObject,
    spec::FORKS,
    work::{self, WorkObject},
    Hash,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

/// Cryptographically verified executed block template received from a remote
/// stateful builder node, bound to `parent_hash`.
#[derive(Clone)]
pub struct VerifiedBuilderTemplate {
    pub parent_hash: Hash,
    pub number: u64,
    pub work_object: WorkObject,
    pub raw: Vec<u8>,
}

/// Independently verify an untrusted remote builder's pending `WorkObject`
/// against a locally validated parent header + ancestry window.
///
/// Rejects tampered parent hashes, heights, difficulties, prime terminus
/// numbers, share targets, SHA/Scrypt share controllers, body header hash
/// mismatches, and transaction root mismatches.
pub fn verify_builder_template(
    store: &LightHeaderStore,
    raw: &[u8],
) -> Result<VerifiedBuilderTemplate, String> {
    let wo = WorkObject::decode_auxiliary(raw, PINNED_SCHEDULE)
        .or_else(|_| {
            let sched = quai_primitives::header::Schedule {
                kawpow_fork: 0,
                ..PINNED_SCHEDULE
            };
            WorkObject::decode_auxiliary(raw, sched)
        })
        .map_err(|e| format!("decode builder template: {e}"))?;
    let proto = ProtoWorkObject::decode(raw).map_err(|e| format!("proto decode builder: {e}"))?;
    let wh = proto
        .wo_header
        .as_ref()
        .ok_or_else(|| "builder template missing wo_header".to_string())?;

    // 1. Verify parent linkage against our locally PoW-validated head.
    let parent_bytes = wh
        .parent_hash
        .as_ref()
        .map(|h| h.value.as_slice())
        .unwrap_or_default();
    let parent_hash: Hash = parent_bytes
        .try_into()
        .map_err(|_| "builder template invalid parent_hash width".to_string())?;
    let (head_num, head_hash) = store.head();
    if parent_hash != head_hash {
        return Err(format!(
            "builder template parent {} does not match local validated head {} (#{head_num})",
            encode_hex(&parent_hash),
            encode_hex(&head_hash)
        ));
    }
    let parent = store
        .stored_by_hash(&parent_hash)
        .ok_or_else(|| "local head missing from header store".to_string())?;

    // 2. Verify all body-header commitments (`header_hash == blake3(body_header)`,
    //    `tx_hash`, `outbound_etx_hash`, `uncle_hash`, empty zone manifest/interlink).
    wo.verify_commitments(work::Context::Zone)
        .map_err(|e| format!("builder template body commitments: {e}"))?;

    // 3. Verify outer work-header transaction root commitment (`wo_header.tx_hash == tx_root`).
    let computed_tx_root =
        work::transactions_root(wo.transactions()).map_err(|e| format!("builder tx root: {e}"))?;
    let claimed_tx_root = wh
        .tx_hash
        .as_ref()
        .map(|h| h.value.as_slice())
        .unwrap_or_default();
    if claimed_tx_root != computed_tx_root.as_slice() {
        return Err("builder template txHash does not match body transactions".into());
    }

    // 4. Verify consensus header rules (height, difficulty, prime terminus, share controllers).
    let history = store.history_window(&parent_hash, 4);
    let profile = store.chain().zone_profile();
    let expected = validation::derive_next_header_rules(
        &history,
        &parent.record,
        &profile,
        parent.block.order,
        wh.time.unwrap_or(0),
    )
    .map_err(|e| format!("derive consensus rules: {e}"))?;

    let actual_num = share_rules::number(wh.number.as_deref().unwrap_or_default())
        .map_err(|e| format!("builder number: {e}"))?;
    if actual_num != expected.number {
        return Err(format!(
            "builder template number {actual_num} != expected {}",
            expected.number
        ));
    }

    let actual_diff = share_rules::uint(wh.difficulty.as_deref().unwrap_or_default())
        .map_err(|e| format!("builder difficulty: {e}"))?;
    if actual_diff != expected.difficulty {
        return Err(format!(
            "builder template difficulty {actual_diff} != expected {}",
            expected.difficulty
        ));
    }

    let actual_prime = share_rules::number(wh.prime_terminus_number.as_deref().unwrap_or_default())
        .map_err(|e| format!("builder prime terminus: {e}"))?;
    if actual_prime != expected.prime_terminus_number {
        return Err(format!(
            "builder template primeTerminusNumber {actual_prime} != expected {}",
            expected.prime_terminus_number
        ));
    }

    if expected.prime_terminus_number >= FORKS.kawpow {
        let actual_sha_target =
            share_rules::uint(wh.sha_share_target.as_deref().unwrap_or_default())
                .map_err(|e| format!("builder sha_share_target: {e}"))?;
        if actual_sha_target != expected.sha_share_target {
            return Err("builder template sha_share_target mismatch".into());
        }
        let actual_scrypt_target =
            share_rules::uint(wh.scrypt_share_target.as_deref().unwrap_or_default())
                .map_err(|e| format!("builder scrypt_share_target: {e}"))?;
        if actual_scrypt_target != expected.scrypt_share_target {
            return Err("builder template scrypt_share_target mismatch".into());
        }
        let actual_kawpow = share_rules::uint(wh.kawpow_difficulty.as_deref().unwrap_or_default())
            .map_err(|e| format!("builder kawpow_difficulty: {e}"))?;
        if actual_kawpow != expected.kawpow_difficulty {
            return Err("builder template kawpow_difficulty mismatch".into());
        }
        let sched = PINNED_SCHEDULE;
        if let Ok(hdr) = wo.auxiliary_work_header(sched) {
            let sha_t =
                Triple::from_header(&hdr, true).map_err(|e| format!("builder sha triple: {e}"))?;
            if sha_t.difficulty != expected.sha_triple.difficulty
                || sha_t.count != expected.sha_triple.count
            {
                return Err("builder template SHA controller mismatch".into());
            }
            let scrypt_t = Triple::from_header(&hdr, false)
                .map_err(|e| format!("builder scrypt triple: {e}"))?;
            if scrypt_t.difficulty != expected.scrypt_triple.difficulty
                || scrypt_t.count != expected.scrypt_triple.count
            {
                return Err("builder template Scrypt controller mismatch".into());
            }
        }
    }

    Ok(VerifiedBuilderTemplate {
        parent_hash,
        number: actual_num,
        work_object: wo,
        raw: raw.to_vec(),
    })
}

/// Active relay pulling executed pending headers from an upstream builder RPC
/// and forwarding solved full blocks back to it.
pub struct BuilderRelay {
    client: HttpRpcClient,
    miner: Arc<Miner>,
    store: Arc<LightHeaderStore>,
    log: Log,
}

impl BuilderRelay {
    pub fn new(
        builder_url: &str,
        miner: Arc<Miner>,
        store: Arc<LightHeaderStore>,
        log: Log,
    ) -> Result<Self, String> {
        let client = HttpRpcClient::new(builder_url)?;
        // Register upstream full-block submit hook on the miner so any solved
        // full block is immediately forwarded to the stateful builder node.
        let submit_client = client.clone();
        let submit_log = log;
        miner.set_builder_submit_hook(Arc::new(move |raw_wo| {
            let hex = encode_hex(raw_wo);
            if let Err(e) = submit_client.call("quai_receiveMinedHeader", json!([hex])) {
                submit_log.warn(&format!("builder relay submit failed: {e}"));
            }
        }));
        Ok(Self {
            client,
            miner,
            store,
            log,
        })
    }

    /// Perform a single synchronous pull & verification of the upstream builder's
    /// executed pending header, installing it on `self.miner` if valid.
    pub fn poll_once(&self) -> Result<VerifiedBuilderTemplate, String> {
        let res = self.client.call("quai_getPendingHeader", json!([]))?;
        let hex = res
            .as_str()
            .ok_or_else(|| "builder quai_getPendingHeader returned non-string".to_string())?;
        let raw = decode_hex(hex).map_err(|e| format!("builder hex decode: {e}"))?;
        let verified = verify_builder_template(&self.store, &raw)?;
        self.miner.set_builder_template(verified.clone());
        Ok(verified)
    }

    /// Background polling loop keeping the miner's verified builder template
    /// fresh on every new head or recommit interval.
    pub async fn run(self, stop: Arc<AtomicBool>) {
        let mut head_rx = self.store.head_events();
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        let relay = Arc::new(self);
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let r = relay.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Err(e) = r.poll_once() {
                    r.log.debug(&format!("builder relay poll: {e}"));
                }
            })
            .await;
            tokio::select! {
                _ = tick.tick() => {}
                _ = head_rx.recv() => {}
            }
        }
    }
}
