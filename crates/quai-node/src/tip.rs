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
use crate::peers::{decode_block, Block};
use crate::processor::{Offer, Processor};
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
    block::ProtoWorkObjectBlockView, quaiprotocol::quai_response_message as rs,
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

/// Subscribe to slice [0 0] and forward the zone blocks go-quai's validators
/// accept; count the workshares.
pub fn subscribe(
    runtime: &tokio::runtime::Handle,
    node: Arc<PublicNode>,
    stop: Arc<AtomicBool>,
    stats: Arc<GossipStats>,
    log: Log,
) -> Result<Receiver<Vec<u8>>, String> {
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
                }
                (TopicKind::Blocks, 2) => {
                    let Ok(view) = ProtoWorkObjectBlockView::decode(data.as_slice()) else {
                        continue;
                    };
                    let Some(wo) = view.work_object else { continue };
                    stats.blocks.fetch_add(1, Ordering::Relaxed);
                    if tx.try_send(wo.encode_to_vec()).is_err() {
                        stats.ignored.fetch_add(1, Ordering::Relaxed);
                    }
                }
                _ => {}
            }
        }
    });
    Ok(rx)
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
}

/// Follow the head with the processor in tip mode until `stop`.
pub fn follow(
    processor: &mut Processor,
    runtime: &tokio::runtime::Handle,
    node: Arc<PublicNode>,
    gossip: Receiver<Vec<u8>>,
    stop: &AtomicBool,
    log: &Log,
) -> Result<TipStats, String> {
    processor.enable_tip()?;
    let caches = Arc::new(SharedCaches::new(2).map_err(|e| e.to_string())?);
    let mut verifier = Verifier::with_shared_caches(caches);
    let mut stats = TipStats::default();
    let mut last_poll = Instant::now();
    log.info(&format!(
        "tip: following the head through gossip from {}",
        processor.tip().map_or(0, |t| t.head().1)
    ));
    while !stop.load(Ordering::Relaxed) {
        let mut queue: VecDeque<Block> = VecDeque::new();
        match gossip.recv_timeout(Duration::from_secs(1)) {
            Ok(raw) => {
                if let Ok(b) = decode_block(raw, 2) {
                    queue.push_back(b);
                }
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
        while let Some(block) = queue.pop_front() {
            let mut chain = vec![block];
            while let Some(top) = chain.last().cloned() {
                if stop.load(Ordering::Relaxed) {
                    break;
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
                match processor.offer(height, log)? {
                    Offer::NeedParent(p) => {
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
                        if refused {
                            stats.refused += 1;
                        }
                        if !removed.is_empty() {
                            stats.reorgs += 1;
                            stats.reorged_blocks += removed.len() as u64;
                            stats.deepest = stats.deepest.max(removed.len() as u64);
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
                    Offer::Rejected(_) => stats.rejected += 1,
                    Offer::Known | Offer::TooFar | Offer::BelowBase => {}
                }
                chain.pop();
            }
        }
    }
    Ok(stats)
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
            Ok(raw) => raw,
            Err(RecvTimeoutError::Timeout) => continue,
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
