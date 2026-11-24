//! Header-only P2P synchronization for `quai-node light`.
//!
//! Requests `WorkObjectHeaderView` (never full block bodies when catching up),
//! verifies proof-of-work (ProgPoW / KAWPOW / SHA-256d / Scrypt AuxPoW) and
//! header consensus rules locally, maintains PoEM canonical head selection in
//! [`LightHeaderStore`], and subscribes to live block, workshare, and
//! AuxTemplate gossip topics.
use super::store::LightHeaderStore;
use crate::log::Log;
use crate::metrics::Metrics;
use crate::miner::Miner;
use crypto_bigint::Encoding as _;
use prost::Message;
use quai_consensus::{
    auxiliary::Auxiliary,
    pow::SharedCaches,
    share_rules::{self, Classification},
    validation,
    verifier::PINNED_SCHEDULE,
};
use quai_network::{
    public::{Event, PublicNode, Query, Want},
    TopicKind,
};
use quai_primitives::{
    schema::{
        block::{ProtoWorkObjectBlockView, ProtoWorkObjectHeaderView, ProtoWorkObjectShareView},
        quaiprotocol::quai_response_message::Response as P2pResponse,
    },
    Hash,
};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

/// Verify a candidate header/block's PoW and parent linkage, then insert into
/// `store`. Returns `Ok(true)` if it became the new canonical head.
pub fn validate_and_insert_header(
    store: &LightHeaderStore,
    caches: &SharedCaches,
    raw: &[u8],
) -> Result<bool, String> {
    let (block, record) = store.decode_block_data(raw, 2)?;
    let header = &block.header;
    let pow_hash = crate::uncles::pow_hash(caches, header)
        .map_err(|e| format!("light header PoW verify: {e}"))?;

    // Check AuxPoW binding when post-KAWPOW.
    let prime = record.prime();
    if prime >= quai_primitives::spec::FORKS.kawpow {
        if let Some(aux_bytes) = header.wire().aux_pow.as_deref() {
            let aux = Auxiliary::decode(aux_bytes).map_err(|e| format!("aux decode: {e}"))?;
            aux.verify_binding(header.seal_hash())
                .map_err(|e| format!("aux binding: {e}"))?;
        }
    }

    // Check block target classification.
    let class = share_rules::classify(header, pow_hash, store.chain().spec().header.p2p_threshold)
        .map_err(|e| format!("header PoW classify: {e}"))?;
    if class != Classification::Block {
        return Err(format!(
            "header #{} fails block PoW target (got {class:?})",
            block.number
        ));
    }

    // Check parent linkage & consensus rules when parent is in our bounded window.
    let parent_hash = record.parent(2).map_err(|e| format!("parent hash: {e}"))?;
    let (order, total_entropy) = if let Some(parent) = store.stored_by_hash(&parent_hash) {
        let history = store.history_window(&parent_hash, 4);
        let profile = store.chain().zone_profile();
        if let Ok(expected) = validation::derive_next_header_rules(
            &history,
            &parent.record,
            &profile,
            parent.block.order,
            header.wire().time.unwrap_or(0),
        ) {
            if block.number != expected.number {
                return Err(format!(
                    "header height mismatch: got {}, expected {}",
                    block.number, expected.number
                ));
            }
            if let Ok(actual_diff) =
                share_rules::uint(header.wire().difficulty.as_deref().unwrap_or_default())
            {
                if actual_diff != expected.difficulty {
                    return Err(format!(
                        "header #{} difficulty mismatch: got {}, expected {}",
                        block.number, actual_diff, expected.difficulty
                    ));
                }
            }
        }
        store.compute_next_entropy(&parent, header, pow_hash)
    } else {
        // Standalone bootstrap anchor: PoW verified above; start entropy from
        // parent_entropy header commitment so subsequent children order properly.
        let pe = block
            .object
            .header()
            .wire()
            .parent_entropy
            .get(2)
            .and_then(|b| share_rules::uint(b).ok())
            .unwrap_or(crypto_bigint::U256::ONE);
        (2, pe.to_be_bytes())
    };

    store.insert_verified_raw(raw, order, total_entropy, true)
}

/// Request a single `WorkObjectHeaderView` by number from connected peers.
async fn fetch_header_by_number(
    node: &PublicNode,
    location: [u8; 2],
    number: u64,
) -> Option<Vec<u8>> {
    let (_, resp) = node
        .request(&location, Query::Number(number), Want::BlockHash)
        .await
        .ok()??;
    let hash: Hash = match resp {
        P2pResponse::BlockHash(h) => h.value.try_into().ok()?,
        _ => return None,
    };
    fetch_header_by_hash(node, location, hash).await
}

/// Request a single `WorkObjectHeaderView` by hash from connected peers.
async fn fetch_header_by_hash(node: &PublicNode, location: [u8; 2], hash: Hash) -> Option<Vec<u8>> {
    let (_, resp) = node
        .request(&location, Query::Hash(hash), Want::Header)
        .await
        .ok()??;
    match resp {
        P2pResponse::WorkObjectHeaderView(v) => v.work_object.map(|wo| wo.encode_to_vec()),
        P2pResponse::WorkObjectBlockView(v) => v.work_object.map(|wo| wo.encode_to_vec()),
        _ => None,
    }
}

/// Run the background header-only sync + gossip intake loop for `quai-node light`.
#[allow(clippy::too_many_arguments)]
pub async fn run_header_sync(
    node: Arc<PublicNode>,
    store: Arc<LightHeaderStore>,
    caches: Arc<SharedCaches>,
    miner: Option<Arc<Miner>>,
    peer_head: Arc<AtomicU64>,
    metrics: Arc<Metrics>,
    checkpoint_height: Option<u64>,
    stop: Arc<AtomicBool>,
    log: Log,
) {
    let location = store.chain().location();
    let topic_gen = store
        .chain()
        .topic_genesis()
        .unwrap_or_else(|_| store.chain().genesis_hash());

    let blocks_topic = quai_network::topic(&topic_gen, &location, TopicKind::Blocks).ok();
    let ws_topic = quai_network::topic(&topic_gen, &location, TopicKind::Workshares).ok();
    let aux_topic = quai_network::topic(&topic_gen, &location, TopicKind::AuxTemplate).ok();

    for topic in [&blocks_topic, &ws_topic, &aux_topic].into_iter().flatten() {
        if let Err(e) = node.subscribe(topic.clone()).await {
            log.warn(&format!("light subscribe {topic}: {e}"));
        }
    }

    // Optional instant tip bootstrap by height (fetches only recent headers!).
    if let Some(target_h) = checkpoint_height {
        if store.head().0 < target_h {
            let mut wait_tries = 0;
            while !stop.load(Ordering::Relaxed) && node.peer_counts().0 == 0 && wait_tries < 60 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                wait_tries += 1;
            }
            if let Some(raw) = fetch_header_by_number(&node, location, target_h).await {
                match validate_and_insert_header(&store, &caches, &raw) {
                    Ok(_) => log.info(&format!(
                        "light: anchored header chain at PoW-verified height {target_h}"
                    )),
                    Err(e) => log.warn(&format!("light: anchor height {target_h} failed: {e}")),
                }
            }
        }
    }

    let mut poll = tokio::time::interval(Duration::from_secs(3));
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        tokio::select! {
            _ = poll.tick() => {
                if node.peer_counts().0 == 0 {
                    continue;
                }
                let next_num = store.head().0.saturating_add(1);
                if let Some(raw) = fetch_header_by_number(&node, location, next_num).await {
                    match validate_and_insert_header(&store, &caches, &raw) {
                        Ok(is_head) => {
                            peer_head.fetch_max(next_num, Ordering::Relaxed);
                            metrics.blocks.fetch_add(1, Ordering::Relaxed);
                            metrics.head.store(store.head().0, Ordering::Relaxed);
                            metrics.sample_now();
                            if is_head {
                                if let Some(m) = &miner {
                                    m.notify_new_head();
                                }
                            }
                        }
                        Err(e) => log.debug(&format!("light sync #{next_num}: {e}")),
                    }
                }
            }
            ev = node.next() => {
                let Ok(ev) = ev else {
                    break;
                };
                if let Event::Gossip { topic, data, .. } = ev {
                    if blocks_topic.as_deref() == Some(topic.as_str()) {
                        metrics.gossip_blocks.fetch_add(1, Ordering::Relaxed);
                        let raw = ProtoWorkObjectBlockView::decode(data.as_slice())
                            .ok()
                            .and_then(|v| v.work_object)
                            .map(|wo| wo.encode_to_vec())
                            .or_else(|| {
                                ProtoWorkObjectHeaderView::decode(data.as_slice())
                                    .ok()
                                    .and_then(|v| v.work_object)
                                    .map(|wo| wo.encode_to_vec())
                            });
                        if let Some(wo_bytes) = raw {
                            // If parent is missing, walk back a few headers by hash so PoEM links cleanly.
                            if let Ok((b, rec)) = store.decode_block_data(&wo_bytes, 2) {
                                peer_head.fetch_max(b.number, Ordering::Relaxed);
                                if let Ok(parent_h) = rec.parent(2) {
                                    if store.block_by_hash(&parent_h).is_none() && b.number > 0 {
                                        if let Some(p_raw) = fetch_header_by_hash(&node, location, parent_h).await {
                                            let _ = validate_and_insert_header(&store, &caches, &p_raw);
                                        }
                                    }
                                }
                            }
                            match validate_and_insert_header(&store, &caches, &wo_bytes) {
                                Ok(is_head) => {
                                    metrics.blocks.fetch_add(1, Ordering::Relaxed);
                                    metrics.head.store(store.head().0, Ordering::Relaxed);
                                    metrics.sample_now();
                                    if is_head {
                                        if let Some(m) = &miner {
                                            m.notify_new_head();
                                        }
                                    }
                                }
                                Err(e) => log.debug(&format!("light gossip block ignored: {e}")),
                            }
                        }
                    } else if ws_topic.as_deref() == Some(topic.as_str()) {
                        metrics.gossip_workshares.fetch_add(1, Ordering::Relaxed);
                        metrics.workshares_valid.fetch_add(1, Ordering::Relaxed);
                        if let Some(m) = &miner {
                            if let Ok(view) = ProtoWorkObjectShareView::decode(data.as_slice()) {
                                if let Some(wo) = view.work_object {
                                    if let Some(hdr) = wo.wo_header {
                                        if let Ok(h) = quai_primitives::Header::decode_auxiliary(
                                            &hdr.encode_to_vec(),
                                            PINNED_SCHEDULE,
                                        ) {
                                            let _ = m.receive_workshare_header(h);
                                        }
                                    }
                                }
                            }
                        }
                    } else if aux_topic.as_deref() == Some(topic.as_str()) {
                        if let Some(m) = &miner {
                            let _ = m.submit_aux_template(&data);
                        }
                    }
                }
            }
        }
    }
}
