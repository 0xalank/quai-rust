//! Following the head through gossip (`quai-node run` at the peers' head, and
//! `quai-node observe`).
//!
//! The node subscribes to go-quai's topics of slice [0 0] with
//! `quai_network::public::Config::public` (publish off): prime, region and
//! zone blocks, zone workshares. Each zone block that go-quai's gossip
//! validators accept is assembled into a height (its prime and region views
//! when its order says it is coincident, fetched by hash), its missing
//! ancestors are fetched by hash first, and the processor's tip mode
//! (`Processor::offer`) checks it and applies go-quai's head choice
//! (`forkchoice.rs`), with reorgs. The heads of the peers are polled too, so
//! a block that gossip missed is fetched by number.
//!
//! `quai-node observe` runs the same intake without a processor: it checks
//! each block's proof-of-work, computes its entropy as go-quai's backends do,
//! applies the head choice and counts reorgs and their depth. It needs no
//! checkpoint near the head, so it shows the live head choice while a
//! validating node is still far behind.
use crate::log::Log;
use crate::peers::{decode_block, Block, PeerSource};
use crate::processor::{BlockSource, Next, Offer, Processor};
use crate::qex::{Context, Height, RawRecord};
use crypto_bigint::U256;
use prost::Message;
use quai_consensus::{pow::SharedCaches, rules, share_rules, verifier::Verifier};
use quai_network::public::{
    gossip::{parse_topic, slice_topic_names},
    Event, PublicNode, Query, Verdict, Want,
};
use quai_network::TopicKind;
use quai_primitives::schema::{
    block::{ProtoWorkObjectBlockView, ProtoWorkObjectShareView},
    quaiprotocol::quai_response_message as rs,
};
use quai_primitives::{encode_hex, Hash};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

const LOCATIONS: [&[u8]; 3] = [&[], &[0], &[0, 0]];
/// Ancestors fetched for one gossip block at most.
const MAX_ANCESTORS: usize = 256;

fn hex(b: &[u8]) -> String {
    encode_hex(b)
}

/// Gossip counts for logs and metrics.
#[derive(Default)]
pub struct GossipStats {
    pub blocks: AtomicU64,
    pub workshares: AtomicU64,
    pub ignored: AtomicU64,
}
/// A zone work object that go-quai's gossip validators accepted.
pub enum Gossip {
    Block(Vec<u8>),
    Workshare(Vec<u8>),
}

/// Subscribe to slice [0 0] and forward the zone blocks and workshares
/// go-quai's validators accept. With a transaction pool, the transactions of
/// each accepted workshare also go to its remote queue, as go-quai's
/// `OnNewBroadcast` sends them to `SendRemoteTxs`.
pub fn subscribe(
    runtime: &tokio::runtime::Handle,
    node: Arc<PublicNode>,
    stop: Arc<AtomicBool>,
    stats: Arc<GossipStats>,
    pool: Option<Arc<crate::txpool::service::Service>>,
    log: Log,
) -> Result<Receiver<Gossip>, String> {
    let genesis = quai_network::public::colosseum_topic_genesis();
    let topics = slice_topic_names(&genesis, [0, 0])?;
    for t in &topics {
        runtime
            .block_on(node.subscribe(t.clone()))
            .map_err(|e| format!("subscribe {t}: {e}"))?;
    }
    log.info(&format!(
        "gossip: subscribed to {} topics of slice [0 0]",
        topics.len()
    ));
    let (tx, rx) = std::sync::mpsc::sync_channel(1024);
    runtime.spawn(async move {
        while !stop.load(Ordering::Relaxed) {
            let Ok(Ok(event)) = tokio::time::timeout(Duration::from_secs(1), node.next()).await
            else {
                continue;
            };
            let Event::Gossip {
                topic,
                data,
                verdict,
                ..
            } = event
            else {
                continue;
            };
            if verdict != Verdict::Accept {
                stats.ignored.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let Ok(t) = parse_topic(&topic) else { continue };
            match (t.kind, t.context()) {
                (TopicKind::Workshares, 2) => {
                    stats.workshares.fetch_add(1, Ordering::Relaxed);
                    if let Some(pool) = &pool {
                        let txs = workshare_transactions(&data, pool.now());
                        if !txs.is_empty() {
                            pool.add_remotes(txs);
                        }
                    }
                    let Ok(view) = ProtoWorkObjectShareView::decode(data.as_slice()) else {
                        continue;
                    };
                    let Some(wo) = view.work_object else { continue };
                    if tx.try_send(Gossip::Workshare(wo.encode_to_vec())).is_err() {
                        stats.ignored.fetch_add(1, Ordering::Relaxed);
                    }
                }
                (TopicKind::Blocks, 2) => {
                    let Ok(view) = ProtoWorkObjectBlockView::decode(data.as_slice()) else {
                        continue;
                    };
                    let Some(wo) = view.work_object else { continue };
                    stats.blocks.fetch_add(1, Ordering::Relaxed);
                    if tx.try_send(Gossip::Block(wo.encode_to_vec())).is_err() {
                        stats.ignored.fetch_add(1, Ordering::Relaxed);
                    }
                }
                _ => {}
            }
        }
    });
    Ok(rx)
}

/// The transactions of a gossiped workshare view, decoded as go-quai's
/// `ProtoDecode` decodes them (the validators accepted the view, so each
/// decodes).
pub fn workshare_transactions(data: &[u8], now: u64) -> Vec<crate::txpool::list::Tx> {
    use quai_primitives::schema::block::ProtoWorkObjectShareView;
    let Ok(view) = ProtoWorkObjectShareView::decode(data) else {
        return vec![];
    };
    view.work_object
        .and_then(|wo| wo.wo_body)
        .and_then(|b| b.transactions)
        .map(|t| {
            t.transactions
                .iter()
                .filter_map(|tx| {
                    crate::txpool::tx::PoolTx::decode(&tx.encode_to_vec(), now)
                        .ok()
                        .map(Arc::new)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A block by hash in a context from the peers (`WorkObjectBlock`).
pub fn fetch(
    runtime: &tokio::runtime::Handle,
    node: &PublicNode,
    ctx: usize,
    hash: Hash,
) -> Option<Block> {
    for _ in 0..3 {
        let answer = runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(20),
                node.request(LOCATIONS[ctx], Query::Hash(hash), Want::Block),
            )
            .await
        });
        if let Ok(Ok(Some((_, rs::Response::WorkObjectBlockView(view))))) = answer {
            if let Some(wo) = view.work_object {
                if let Ok(b) = decode_block(wo.encode_to_vec(), ctx) {
                    if b.hash == hash {
                        return Some(b);
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    None
}
/// The canonical zone hash at `number` of the peers, if one answers.
pub fn hash_at(runtime: &tokio::runtime::Handle, node: &PublicNode, number: u64) -> Option<Hash> {
    let answer = runtime.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(20),
            node.request(LOCATIONS[2], Query::Number(number), Want::BlockHash),
        )
        .await
    });
    match answer {
        Ok(Ok(Some((_, rs::Response::BlockHash(h))))) => h.value.as_slice().try_into().ok(),
        _ => None,
    }
}

/// The order and the entropy go-quai's coordinator gets for a zone block:
/// `TotalLogEntropy` in the context of its order, with the workshare
/// entropy for zone blocks (after the KawPoW fork: the log of the share
/// count). Before the KawPoW fork the zone entropy needs the ancestry; the
/// observer refuses such blocks.
pub fn entropy_of(verifier: &mut Verifier, block: &Block) -> Result<(u8, U256), String> {
    let record = quai_consensus::history::Record::decode(&block.raw).map_err(|e| e.to_string())?;
    if record.prime() < quai_primitives::spec::FORKS.kawpow {
        return Err("the observer needs blocks after the KawPoW fork".into());
    }
    let proof = verifier
        .verify_header_proof(record.header())
        .map_err(|e| format!("proof-of-work: {e}"))?;
    let b = record.object().header().wire();
    let arr = |v: &[Vec<u8>]| -> Result<[U256; 3], String> {
        v.iter()
            .map(|b| share_rules::uint(b).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "entropy array".to_string())
    };
    let input = |ctx: u8| -> Result<rules::EntropyInput, String> {
        Ok(rules::EntropyInput {
            hash: proof.hash(),
            difficulty: record.difficulty().map_err(|e| e.to_string())?,
            expansion: b.expansion_number.unwrap_or(0) as u8,
            parent_entropy: arr(&b.parent_entropy)?,
            parent_delta: arr(&b.parent_delta_entropy)?,
            parent_uncled_delta: arr(&b.parent_uncled_delta_entropy)?,
            uncled_entropy: share_rules::uint(b.uncled_entropy.as_deref().unwrap_or_default())
                .map_err(|e| e.to_string())?,
            postfork_share_count: Some(record.uncles().map_err(|e| e.to_string())?.len() as u32),
            node_context: ctx,
        })
    };
    let zone = rules::entropy(&input(2)?).map_err(|e| e.to_string())?;
    if zone.order == 2 {
        return Ok((2, zone.total));
    }
    let e = rules::entropy(&input(zone.order)?).map_err(|e| e.to_string())?;
    Ok((zone.order, e.total))
}

/// The height of a zone block: its prime and region views when its order
/// says it is coincident, fetched by hash.
pub fn height_of(
    runtime: &tokio::runtime::Handle,
    node: &PublicNode,
    verifier: &mut Verifier,
    zone: Block,
) -> Result<Height, String> {
    let (order, _) = entropy_of(verifier, &zone)?;
    let mut h = Height {
        zone_height: zone.number,
        records: [None, None, None],
    };
    for ctx in order as usize..2 {
        let view = fetch(runtime, node, ctx, zone.hash).ok_or_else(|| {
            format!(
                "no {} view of {}",
                ["prime", "region"][ctx],
                hex(&zone.hash)
            )
        })?;
        h.records[ctx] = Some(record(&view));
    }
    h.records[2] = Some(record(&zone));
    Ok(h)
}
fn record(b: &Block) -> RawRecord {
    RawRecord {
        context: Context::from_u8(b.ctx as u8).unwrap_or(Context::Zone),
        number: b.number,
        hash: b.hash,
        block: b.raw.clone(),
        receipts: vec![],
    }
}

/// Counts of the tip follower.
#[derive(Default, Debug, Clone)]
pub struct TipStats {
    pub offered: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub fetched: u64,
    pub reorgs: u64,
    pub reorged_blocks: u64,
    pub deepest: u64,
    pub refused: u64,
    pub below_base: u64,
    pub shares_valid: u64,
    pub shares_parent_unknown: u64,
    pub shares_rejected: u64,
    pub shares_old: u64,
}
/// go-quai's AddWorkShare ignores shares this far below its head
/// (2 * params.NewWorkSharesInclusionDepth).
const SHARE_DISTANCE: u64 = 8;
/// Hashes the follower stops offering: rejected blocks, their descendants
/// and blocks that fork below the tip base.
const DEAD: usize = 4096;
/// Time between comparisons with the peers' canonical chain.
const CHECK_EVERY: Duration = Duration::from_secs(30);
/// Peers asked per comparison (a different set each time).
const CHECK_PEERS: usize = 4;
/// A gossip block this far above the head, past the hand-off margin,
/// starts a catch-up by number. The sync hands off to gossip `margin`
/// below the best head, and the ancestor walk closes such a gap.
const CATCH_UP_GAP: u64 = 64;
/// A catch-up starts at the canonical block this far below the head,
/// deeper than the reorgs seen at the head (at most 3).
const CATCH_UP_BACK: u64 = 8;
/// Least time between two catch-ups.
const CATCH_UP_EVERY: Duration = Duration::from_secs(60);
/// A catch-up that gets no height for this long ends.
const CATCH_UP_IDLE: Duration = Duration::from_secs(120);
/// Least time between two dials of the boot peers while no peer answers.
const REDIAL_EVERY: Duration = Duration::from_secs(120);

/// What a catch-up by number needs: the sync's peer source settings.
#[derive(Clone, Debug)]
pub struct CatchUp {
    pub genesis: Hash,
    pub margin: u64,
    pub max_peers: usize,
}

/// A bounded set of hashes, oldest out first.
#[derive(Default)]
struct Recent {
    set: std::collections::HashSet<Hash>,
    order: VecDeque<Hash>,
    cap: usize,
}
impl Recent {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            ..Self::default()
        }
    }
    fn contains(&self, h: &Hash) -> bool {
        self.set.contains(h)
    }
    /// False when it was there already.
    fn insert(&mut self, h: Hash) -> bool {
        if !self.set.insert(h) {
            return false;
        }
        self.order.push_back(h);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}
/// The answers of a comparison with the peers.
enum Check {
    Head {
        number: u64,
        ours: Hash,
        answers: Vec<(libp2p::PeerId, Option<Hash>)>,
    },
    Lag {
        head: u64,
        answers: Vec<(libp2p::PeerId, Option<Hash>)>,
    },
    Block {
        rejected: crate::divergence::Rejected,
        answers: Vec<(libp2p::PeerId, Option<Hash>)>,
    },
}
/// The canonical zone hash at `number` of each of `peers` that answers (a
/// hash, or None: no block there). A request that fails is no answer.
async fn ask(
    node: Arc<PublicNode>,
    peers: Vec<libp2p::PeerId>,
    number: u64,
) -> Vec<(libp2p::PeerId, Option<Hash>)> {
    let tasks: Vec<_> = peers
        .into_iter()
        .map(|p| {
            let node = node.clone();
            tokio::spawn(async move { (p, crate::peers::peer_hash_at(&node, p, 2, number).await) })
        })
        .collect();
    let mut out = vec![];
    for t in tasks {
        if let Ok((p, Ok(h))) = t.await {
            out.push((p, h));
        }
    }
    out
}
/// The block hashes of a block's uncles (workshares included).
fn uncle_hashes(block: &Block) -> Vec<Hash> {
    uncle_hashes_of(&block.raw)
}
fn uncle_hashes_of(raw: &[u8]) -> Vec<Hash> {
    quai_consensus::history::Record::decode(raw)
        .and_then(|r| r.uncles())
        .map(|u| u.iter().filter_map(|h| h.block_hash().ok()).collect())
        .unwrap_or_default()
}

/// Follow the head with the processor in tip mode until `stop`: every
/// gossip block through every stateless check and go-quai's head choice,
/// every gossip workshare through go-quai's intake and the uncle rules, and
/// the canonical chain compared with the peers' (`divergence.rs`).
#[allow(clippy::too_many_arguments)]
pub fn follow(
    processor: &mut Processor,
    runtime: &tokio::runtime::Handle,
    node: Arc<PublicNode>,
    gossip: Receiver<Gossip>,
    stop: &AtomicBool,
    log: &Log,
    detector: &mut crate::divergence::Detector,
    metrics: Option<Arc<crate::metrics::Metrics>>,
    gossip_stats: Arc<GossipStats>,
    catch: &CatchUp,
) -> Result<TipStats, String> {
    processor.enable_tip()?;
    let caches = Arc::new(SharedCaches::new(2).map_err(|e| e.to_string())?);
    let mut verifier = Verifier::with_shared_caches(caches);
    let mut stats = TipStats::default();
    let mut last_poll = Instant::now();
    let mut last_check = Instant::now();
    let mut last_report = Instant::now();
    let mut dead = Recent::new(DEAD);
    let mut shares_seen = Recent::new(16_384);
    // Peer health and the catch-up by number.
    let mut check_round = 0usize;
    let mut lag_head: Option<u64> = None;
    let mut silent_checks = 0u32;
    let mut last_redial: Option<Instant> = None;
    let mut last_catch_up: Option<Instant> = None;
    let mut catch_up_due = false;
    let (check_tx, check_rx) = std::sync::mpsc::channel::<Check>();
    let bump = |f: fn(&crate::metrics::Metrics) -> &AtomicU64| {
        if let Some(m) = &metrics {
            f(m).fetch_add(1, Ordering::Relaxed);
        }
    };
    log.info(&format!(
        "tip: following the head through gossip from {}",
        processor.tip().map_or(0, |t| t.head().1)
    ));
    while !stop.load(Ordering::Relaxed) {
        // Answers of the comparisons with the peers.
        while let Ok(c) = check_rx.try_recv() {
            match c {
                Check::Head {
                    number,
                    ours,
                    answers,
                } => detector.head_verdict(number, ours, &answers),
                Check::Lag { head, answers } => {
                    // Peers ahead and a head that did not move since the
                    // last check: gossip does not bring the node back.
                    if answers.iter().any(|(_, h)| h.is_some()) && lag_head == Some(head) {
                        catch_up_due = true;
                    }
                    lag_head = Some(head);
                    silent_checks = if answers.is_empty() {
                        silent_checks + 1
                    } else {
                        0
                    };
                    detector.lag_verdict(head, &answers);
                }
                Check::Block { rejected, answers } => detector.block_verdict(rejected, &answers),
            }
        }
        if let Some(m) = &metrics {
            m.gossip_blocks.store(
                gossip_stats.blocks.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
            m.gossip_workshares.store(
                gossip_stats.workshares.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
        }
        let mut queue: VecDeque<Block> = VecDeque::new();
        match gossip.recv_timeout(Duration::from_secs(1)) {
            Ok(Gossip::Block(raw)) => match decode_block(raw, 2) {
                Ok(b) => {
                    if let Some(m) = &metrics {
                        m.note_peer_head(b.number);
                    }
                    let head = processor.tip().map_or(0, |t| t.head().1);
                    if b.number > head + catch.margin + CATCH_UP_GAP {
                        catch_up_due = true;
                    }
                    queue.push_back(b)
                }
                Err(e) => log.warn(&format!("tip: gossip block the Rust codec refuses: {e}")),
            },
            Ok(Gossip::Workshare(raw)) => {
                workshare(processor, &raw, detector, &mut shares_seen, &mut stats, log);
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The gossip task ends on a stop signal before this loop sees it.
            Err(RecvTimeoutError::Disconnected) if stop.load(Ordering::Relaxed) => break,
            Err(RecvTimeoutError::Disconnected) => return Err("gossip stopped".into()),
        }
        // Blocks that gossip missed: the peers' next height.
        if last_poll.elapsed() >= Duration::from_secs(20) {
            last_poll = Instant::now();
            let head = processor.tip().map_or(0, |t| t.head().1);
            if let Some(h) = hash_at(runtime, &node, head + 1) {
                if let Some(b) = fetch(runtime, &node, 2, h) {
                    stats.fetched += 1;
                    queue.push_back(b);
                }
            }
        }
        // Compare with the peers' canonical chain, off this thread.
        if last_check.elapsed() >= CHECK_EVERY {
            last_check = Instant::now();
            if let Some(tip) = processor.tip() {
                let (_, head) = tip.head();
                let probe = head
                    .checked_sub(crate::divergence::DEPTH)
                    .filter(|n| *n >= tip.base().1)
                    .and_then(|n| tip.canonical_at(n).map(|h| (n, h)));
                let due = detector.due(head);
                let mut all = node.verified_peers();
                all.sort();
                let peers: Vec<_> = if all.len() <= CHECK_PEERS {
                    all
                } else {
                    (0..CHECK_PEERS)
                        .map(|i| all[(check_round * CHECK_PEERS + i) % all.len()])
                        .collect()
                };
                check_round = check_round.wrapping_add(1);
                let (node, tx) = (node.clone(), check_tx.clone());
                runtime.spawn(async move {
                    if let Some((number, ours)) = probe {
                        let answers = ask(node.clone(), peers.clone(), number).await;
                        let _ = tx.send(Check::Head {
                            number,
                            ours,
                            answers,
                        });
                    }
                    let answers = ask(
                        node.clone(),
                        peers.clone(),
                        head + crate::divergence::STALL_BLOCKS,
                    )
                    .await;
                    let _ = tx.send(Check::Lag { head, answers });
                    for rejected in due {
                        let answers = ask(node.clone(), peers.clone(), rejected.number).await;
                        let _ = tx.send(Check::Block { rejected, answers });
                    }
                });
            }
        }
        // No peer in a slot, or none answered two checks in a row: after a
        // sleep or a network change the old connections may be dead (ping
        // closes them); dial the boot peers and walk the DHT again.
        if (node.verified_peers().is_empty() || silent_checks >= 2)
            && last_redial.is_none_or(|t| t.elapsed() >= REDIAL_EVERY)
        {
            last_redial = Some(Instant::now());
            silent_checks = 0;
            log.warn("tip: no peer answers; dialing the boot peers again");
            runtime.spawn(crate::peers::dial_targets(node.clone(), vec![], true, *log));
        }
        if catch_up_due && last_catch_up.is_none_or(|t| t.elapsed() >= CATCH_UP_EVERY) {
            catch_up_due = false;
            last_catch_up = Some(Instant::now());
            lag_head = None;
            catch_up(
                processor, runtime, &node, catch, stop, log, detector, &mut stats, &metrics,
                &mut dead,
            )?;
        }
        while let Some(block) = queue.pop_front() {
            let mut chain = vec![block];
            while let Some(top) = chain.last().cloned() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if dead.contains(&top.hash) {
                    chain.pop();
                    continue;
                }
                let height = match height_of(runtime, &node, &mut verifier, top.clone()) {
                    Ok(h) => h,
                    Err(e) => {
                        log.warn(&format!(
                            "tip: block {} at {}: {e}",
                            hex(&top.hash),
                            top.number
                        ));
                        break;
                    }
                };
                stats.offered += 1;
                bump(|m| &m.tip_offered);
                match processor.offer(height, log)? {
                    Offer::NeedParent(p) => {
                        if dead.contains(&p) {
                            dead.insert(top.hash);
                            log.warn(&format!(
                                "tip: block {} at {} descends from rejected block {}",
                                hex(&top.hash),
                                top.number,
                                hex(&p)
                            ));
                            chain.pop();
                            continue;
                        }
                        if chain.len() >= MAX_ANCESTORS {
                            log.warn("tip: too many missing ancestors; waiting for the next block");
                            break;
                        }
                        match fetch(runtime, &node, 2, p) {
                            Some(b) => {
                                stats.fetched += 1;
                                chain.push(b);
                                continue;
                            }
                            None => {
                                log.warn(&format!("tip: ancestor {} not found", hex(&p)));
                                break;
                            }
                        }
                    }
                    Offer::Accepted {
                        head,
                        removed,
                        refused,
                    } => {
                        stats.accepted += 1;
                        detector.block_included(top.hash, top.number, &uncle_hashes(&top));
                        if refused {
                            stats.refused += 1;
                        }
                        if !removed.is_empty() {
                            stats.reorgs += 1;
                            stats.reorged_blocks += removed.len() as u64;
                            stats.deepest = stats.deepest.max(removed.len() as u64);
                            if let Some(m) = &metrics {
                                m.deepest_reorg
                                    .fetch_max(removed.len() as u64, Ordering::Relaxed);
                            }
                            log.info(&format!(
                                "tip: reorg to {} at {}: {} blocks removed ({})",
                                hex(&head.0),
                                head.1,
                                removed.len(),
                                removed
                                    .iter()
                                    .map(|(h, n)| format!("{n}:{}", &hex(h)[..10]))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        }
                    }
                    Offer::Rejected(f) => {
                        stats.rejected += 1;
                        dead.insert(top.hash);
                        detector.block_rejected(top.hash, top.number, f);
                    }
                    Offer::BelowBase => {
                        stats.below_base += 1;
                        dead.insert(top.hash);
                        log.info(&format!(
                            "tip: block {} at {} forks below the tip base; not offered again",
                            hex(&top.hash),
                            top.number
                        ));
                    }
                    Offer::Known | Offer::TooFar => {}
                }
                chain.pop();
            }
        }
        if last_report.elapsed() >= Duration::from_secs(300) {
            last_report = Instant::now();
            let head = processor.tip().map_or((Hash::default(), 0), |t| t.head());
            log.info(&format!(
                "tip: head {} at {}; blocks {} offered, {} accepted, {} rejected; workshares {} valid, {} valid without the parent rules, {} rejected, {} older than go-quai takes; {} reorgs (deepest {})",
                hex(&head.0),
                head.1,
                stats.offered,
                stats.accepted,
                stats.rejected,
                stats.shares_valid,
                stats.shares_parent_unknown,
                stats.shares_rejected,
                stats.shares_old,
                stats.reorgs,
                stats.deepest
            ));
        }
    }
    Ok(stats)
}
/// Where a catch-up by number starts: the canonical block `CATCH_UP_BACK`
/// below the head (the tip base at the lowest), and the prime, region and
/// zone heads after it.
pub fn catch_up_start(processor: &Processor) -> Option<((Hash, u64), [(Hash, u64); 3])> {
    let tip = processor.tip()?;
    let (_, head) = tip.head();
    let start = head.saturating_sub(CATCH_UP_BACK).max(tip.base().1);
    let hash = tip.canonical_at(start)?;
    let [Some(prime), Some(region), Some(zone)] = tip.heads_after(&hash)? else {
        return None;
    };
    Some(((hash, start), [prime, region, zone]))
}

/// What a catch-up did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CaughtUp {
    pub offered: u64,
    pub accepted: u64,
    pub reorgs: u64,
    /// The height this node rejected, which ended the catch-up.
    pub rejected: Option<Hash>,
    /// Why it ended.
    pub end: String,
}

/// Offer the heights of `source` in tip mode, with the checks and the head
/// choice of a gossip block. The source's first height must be this node's
/// canonical block `start`; a different block there ends the catch-up at
/// once (the chains part below it: gossip and its ancestor walk take over).
/// It also ends at a height that is not accepted, at the end of the source,
/// after `CATCH_UP_IDLE` without a height, and on `stop`.
#[allow(clippy::too_many_arguments)]
pub fn catch_up_from(
    processor: &mut Processor,
    source: &mut dyn BlockSource,
    start: (Hash, u64),
    stop: &AtomicBool,
    log: &Log,
    detector: &mut crate::divergence::Detector,
    stats: &mut TipStats,
    metrics: Option<&crate::metrics::Metrics>,
) -> Result<CaughtUp, String> {
    let mut done = CaughtUp::default();
    let mut last_height = Instant::now();
    done.end = loop {
        if stop.load(Ordering::Relaxed) {
            break "a stop signal".to_string();
        }
        match source.next() {
            Ok(Next::Height(h)) => {
                last_height = Instant::now();
                let (hash, number) = (h.zone().hash, h.zone().number);
                if number <= start.1 {
                    if number == start.1 && hash != start.0 {
                        break format!("the peers' block at {number} is not this node's");
                    }
                    continue;
                }
                let uncles = uncle_hashes_of(&h.zone().block);
                done.offered += 1;
                stats.offered += 1;
                if let Some(m) = metrics {
                    m.tip_offered.fetch_add(1, Ordering::Relaxed);
                }
                match processor.offer(*h, log)? {
                    Offer::Accepted {
                        head,
                        removed,
                        refused,
                    } => {
                        done.accepted += 1;
                        stats.accepted += 1;
                        detector.block_included(hash, number, &uncles);
                        if refused {
                            stats.refused += 1;
                        }
                        if !removed.is_empty() {
                            done.reorgs += 1;
                            stats.reorgs += 1;
                            stats.reorged_blocks += removed.len() as u64;
                            stats.deepest = stats.deepest.max(removed.len() as u64);
                            if let Some(m) = metrics {
                                m.deepest_reorg
                                    .fetch_max(removed.len() as u64, Ordering::Relaxed);
                            }
                            log.info(&format!(
                                "tip: reorg to {} at {}: {} blocks removed",
                                hex(&head.0),
                                head.1,
                                removed.len()
                            ));
                        }
                    }
                    Offer::Known => {}
                    Offer::Rejected(f) => {
                        stats.rejected += 1;
                        done.rejected = Some(hash);
                        detector.block_rejected(hash, number, f);
                        break format!("block {} at {number} rejected", hex(&hash));
                    }
                    other => break format!("block {} at {number}: {other:?}", hex(&hash)),
                }
            }
            Ok(Next::Wait) => {
                if last_height.elapsed() >= CATCH_UP_IDLE {
                    break format!("no height for {}s", CATCH_UP_IDLE.as_secs());
                }
                source.wait(stop);
            }
            Ok(Next::End) => break "the source ended".to_string(),
            Err(e) => break format!("{}: {}", e.check, e.actual),
        }
    };
    Ok(done)
}

/// Catch up by number when far behind the peers (after a sleep, a network
/// change or a gap in gossip): the sync's peer source fetches the canonical
/// heights from `catch_up_start`, in parallel batches with their prime and
/// region records as at the start of a run, and hands off where the sync
/// does (the best peer head less the margin).
#[allow(clippy::too_many_arguments)]
fn catch_up(
    processor: &mut Processor,
    runtime: &tokio::runtime::Handle,
    node: &Arc<PublicNode>,
    c: &CatchUp,
    stop: &AtomicBool,
    log: &Log,
    detector: &mut crate::divergence::Detector,
    stats: &mut TipStats,
    metrics: &Option<Arc<crate::metrics::Metrics>>,
    dead: &mut Recent,
) -> Result<(), String> {
    let Some((start, heads)) = catch_up_start(processor) else {
        return Ok(());
    };
    let head = processor.tip().map_or(0, |t| t.head().1);
    log.info(&format!(
        "tip: behind the peers at {head}; catching up by number from {}",
        start.1
    ));
    let mut source = PeerSource::start(
        runtime,
        node.clone(),
        crate::peers::Options {
            start: start.1,
            heads,
            genesis: c.genesis,
            to: None,
            margin: c.margin,
            max_peers: c.max_peers,
            inflight: 3,
            peers: vec![],
            public: true,
            handoff: true,
        },
        *log,
    )?;
    let done = catch_up_from(
        processor,
        &mut source,
        start,
        stop,
        log,
        detector,
        stats,
        metrics.as_deref(),
    )?;
    drop(source);
    if let Some(h) = done.rejected {
        dead.insert(h);
    }
    let head = processor.tip().map_or(0, |t| t.head().1);
    log.info(&format!(
        "tip: catch-up ended ({}): {} heights offered, {} accepted, {} reorgs; head {head}",
        done.end, done.offered, done.accepted, done.reorgs
    ));
    Ok(())
}
/// One gossip workshare through go-quai's intake (`worker.AddWorkShare`)
/// and the uncle rules that do not depend on an including block.
fn workshare(
    processor: &mut Processor,
    raw: &[u8],
    detector: &mut crate::divergence::Detector,
    seen: &mut Recent,
    stats: &mut TipStats,
    log: &Log,
) {
    // A share view carries the work object header (and transactions), not a
    // body header: decode the header as go-quai's gossip validator does.
    let schedule = quai_consensus::verifier::PINNED_SCHEDULE;
    let header = match quai_primitives::schema::block::ProtoWorkObject::decode(raw)
        .map_err(|e| e.to_string())
        .and_then(|wo| {
            wo.wo_header
                .ok_or_else(|| "no work object header".to_string())
        })
        .and_then(|h| quai_primitives::Header::decode_auxiliary(&h.encode_to_vec(), schedule))
    {
        Ok(h) => h,
        Err(e) => {
            stats.shares_rejected += 1;
            let failure = crate::processor::Failure {
                zone_height: None,
                context: "zone".into(),
                number: None,
                hash: None,
                check: "workshare: decode".into(),
                expected: "a work object the Rust codec accepts".into(),
                actual: e,
            };
            detector.share_rejected([0; 32], 0, &failure);
            return;
        }
    };
    let Ok(hash) = header.block_hash() else {
        return;
    };
    if !seen.insert(hash) {
        return;
    }
    let number =
        share_rules::number(header.wire().number.as_deref().unwrap_or_default()).unwrap_or(0);
    let head = processor.tip().map_or(0, |t| t.head().1);
    if number + SHARE_DISTANCE < head {
        stats.shares_old += 1;
        detector.count_old();
        return;
    }
    match processor.check_workshare(&header) {
        Ok(r) if r.parent_checked => {
            stats.shares_valid += 1;
            detector.count_share(true);
        }
        Ok(_) => {
            stats.shares_parent_unknown += 1;
            detector.count_share(false);
            log.debug(&format!(
                "workshare {} at {number}: parent unknown, parent rules not run",
                hex(&hash)
            ));
        }
        Err(f) => {
            stats.shares_rejected += 1;
            detector.share_rejected(hash, number, &f);
        }
    }
}

/// One block the observer knows.
struct Seen {
    number: u64,
    parent: Hash,
}
/// `quai-node observe`: go-quai's head choice on live gossip, without a
/// processor. Runs until `stop` or `seconds`; prints one JSON line per reorg
/// and a summary.
pub fn observe(seconds: u64, log: &Log) -> Result<i32, String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let _signal = crate::node::install_signals(&runtime, stop.clone(), *log);
    crate::peers::public_allowed().map_err(|e| format!("public peers: {e}"))?;
    let node = {
        let _guard = runtime.enter();
        Arc::new(
            PublicNode::launch(
                quai_network::public::Config::public(0),
                libp2p::identity::Keypair::generate_ed25519(),
            )
            .map_err(|e| format!("p2p: {e}"))?,
        )
    };
    for a in quai_network::public::COLOSSEUM_BOOTNODES {
        if let Ok(addr) = a.parse() {
            let _ = runtime.block_on(node.dial(addr));
        }
    }
    let stats = Arc::new(GossipStats::default());
    let gossip = subscribe(
        runtime.handle(),
        node.clone(),
        stop.clone(),
        stats.clone(),
        None,
        *log,
    )?;
    let caches = Arc::new(SharedCaches::new(2).map_err(|e| e.to_string())?);
    let mut verifier = Verifier::with_shared_caches(caches);
    let started = Instant::now();
    let mut seen: HashMap<Hash, Seen> = HashMap::new();
    let mut choice: Option<crate::forkchoice::Choice> = None;
    let mut head: Option<Hash> = None;
    let mut t = TipStats::default();
    let mut agree = (0u64, 0u64);
    let mut last_check = Instant::now();
    while !stop.load(Ordering::Relaxed) && started.elapsed() < Duration::from_secs(seconds) {
        let raw = match gossip.recv_timeout(Duration::from_secs(1)) {
            Ok(Gossip::Block(raw)) => raw,
            Ok(Gossip::Workshare(_)) | Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let Ok(first) = decode_block(raw, 2) else {
            continue;
        };
        // The block and its missing ancestors (once a head is known).
        let mut chain = vec![first];
        while let (Some(top), true) = (chain.last(), choice.is_some()) {
            if seen.contains_key(&top.parent) || chain.len() > 64 {
                break;
            }
            match fetch(runtime.handle(), &node, 2, top.parent) {
                Some(b) => {
                    t.fetched += 1;
                    chain.push(b);
                }
                None => break,
            }
        }
        for b in chain.into_iter().rev() {
            if seen.contains_key(&b.hash) {
                continue;
            }
            t.offered += 1;
            let (order, entropy) = match entropy_of(&mut verifier, &b) {
                Ok(x) => x,
                Err(e) => {
                    t.rejected += 1;
                    log.warn(&format!("observe: {} at {}: {e}", hex(&b.hash), b.number));
                    continue;
                }
            };
            let parents = [b.dom_parents[0], b.dom_parents[1], b.parent];
            // go-quai's first block after a restart: a node set at its
            // parents, keyed at its own entropy.
            let c = choice.get_or_insert_with(|| {
                let mut seed = crate::forkchoice::NodeSet::at(b.parent, entropy);
                seed.nodes[0].hash = parents[0];
                seed.nodes[1].hash = parents[1];
                crate::forkchoice::Choice::keyed(seed, entropy)
            });
            seen.insert(
                b.hash,
                Seen {
                    number: b.number,
                    parent: b.parent,
                },
            );
            let Some(best) = c.build(parents, b.hash, order, entropy) else {
                continue;
            };
            t.accepted += 1;
            let new = best.zone();
            if let Some(old) = head.filter(|h| *h != new) {
                // Removed: the old head's ancestors that are not the new head's.
                let mut ours = std::collections::HashSet::new();
                let mut at = Some(new);
                while let Some(h) = at {
                    ours.insert(h);
                    at = seen.get(&h).map(|s| s.parent);
                }
                let mut removed = vec![];
                let mut at = Some(old);
                while let Some(h) = at {
                    if ours.contains(&h) {
                        break;
                    }
                    removed.push(h);
                    at = seen.get(&h).map(|s| s.parent);
                }
                if !removed.is_empty() && seen.contains_key(&new) {
                    t.reorgs += 1;
                    t.reorged_blocks += removed.len() as u64;
                    t.deepest = t.deepest.max(removed.len() as u64);
                    let n = seen.get(&new).map_or(0, |s| s.number);
                    println!(
                        "{}",
                        serde_json::json!({"event":"reorg","head":hex(&new),"number":n,"removed":removed.len(),
                            "removed_hashes":removed.iter().map(|h| hex(h)).collect::<Vec<_>>(),
                            "seconds":started.elapsed().as_secs()})
                    );
                }
            }
            head = Some(new);
        }
        // Compare the head with the peers' canonical hash a few blocks down.
        if last_check.elapsed() >= Duration::from_secs(60) {
            last_check = Instant::now();
            log.info(&format!(
                "observe: {}s, {} gossip blocks, {} workshares, head {} at {}, {} reorgs",
                started.elapsed().as_secs(),
                stats.blocks.load(Ordering::Relaxed),
                stats.workshares.load(Ordering::Relaxed),
                head.map_or("-".into(), |h| hex(&h)),
                head.and_then(|h| seen.get(&h).map(|s| s.number))
                    .unwrap_or(0),
                t.reorgs
            ));
            let mut at = head;
            for _ in 0..3 {
                at = at.and_then(|h| seen.get(&h).map(|s| s.parent));
            }
            if let Some(h) = at {
                if let Some(n) = seen.get(&h).map(|s| s.number) {
                    if let Some(peer) = hash_at(runtime.handle(), &node, n) {
                        agree.1 += 1;
                        if peer == h {
                            agree.0 += 1;
                        }
                    }
                }
            }
        }
    }
    let head_number = head.and_then(|h| seen.get(&h).map(|s| s.number));
    println!(
        "{}",
        serde_json::json!({"event":"summary","seconds":started.elapsed().as_secs(),
            "gossip_blocks":stats.blocks.load(Ordering::Relaxed),
            "gossip_workshares":stats.workshares.load(Ordering::Relaxed),
            "gossip_not_accepted":stats.ignored.load(Ordering::Relaxed),
            "blocks_offered":t.offered,"blocks_in_choice":t.accepted,"bad_blocks":t.rejected,
            "ancestors_fetched":t.fetched,"reorgs":t.reorgs,"reorged_blocks":t.reorged_blocks,
            "deepest_reorg":t.deepest,"head":head.map(|h| hex(&h)),"head_number":head_number,
            "peer_agreement_3_deep":format!("{}/{}", agree.0, agree.1)})
    );
    stop.store(true, Ordering::Relaxed);
    runtime.shutdown_timeout(Duration::from_secs(5));
    Ok(0)
}
