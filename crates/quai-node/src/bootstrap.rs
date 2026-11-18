//! Shadow bootstrap, a TEST TOOL for the shadow run, not for production:
//! `quai-node run --stateless --shadow-bootstrap <zone block hash | auto>`.
//! The trusted block and its ancestry are not validated by this node; the
//! checkpoints of such a run carry the trusted block, and `run` without the
//! flag (and every replay) refuses them.
//!
//! A stateless node needs no execution state, but its checks read an
//! ancestry window: the recent records and append snapshots of each context
//! (manifests, interlinks, termini), the order of recent zone blocks, the
//! region rollups, the prime views, and the prime controller state (the
//! exchange rates of the last 4,002 prime blocks, the inbound ETXs of the
//! prime head, and the token choice sets of the prime head and its parent,
//! 4,000 rows each). A replay builds them from the genesis. The bootstrap
//! builds the same at a trusted zone block T from blocks the peers serve:
//!
//! 1. T: given, or chosen `--shadow-bootstrap-depth` (1,000) zone blocks below
//!    the best peer head where at least `--shadow-bootstrap-quorum` peers (3) report the
//!    same canonical hash. A given hash must also be canonical for that many
//!    peers and that far below the head.
//! 2. P, the prime head at T, and the prime blocks P-8,000 ..= P-4,002 by
//!    number: their exchange rates (each token choice row reads the rate
//!    4,000 prime blocks before its block). Their parent links end at
//!    P-4,002, a prime block, whose region view is fetched by hash.
//! 3. The zone heights from P-4,002 to T from the peer source of `run`, with
//!    the prime and region records at coincident heights; every record links
//!    to the one before it in its context, and the last one is T.
//! 4. `processor::bootstrap_checkpoint` derives the window from them (the
//!    manifests, interlinks and termini checked against the commitments the
//!    next headers carry, the rollups against the ETX rollup roots, the
//!    region manifests against the zone manifests), rebuilds the 4,000-row
//!    token choice sets from the inbound ETXs of the last 4,001 prime blocks,
//!    and writes a stateless checkpoint at T.
//!
//! `run` then resumes from that checkpoint: from T+1 on every block passes
//! every stateless check. The heights before T+1 are trusted through their
//! hash links to T, not checked. The bootstrap needs no state, so it refuses
//! a run with state: go-quai serves no state over P2P
//! (`quai/p2p_backend.go` `GetTrieNode` panics "todo"), so the execution
//! checks (state, ETX, UTXO and receipt roots, gas and state counters) have
//! no parent state at T.
use crate::log::Log;
use crate::peers::{self, Block, PeerSource};
use crate::processor::{self, BootstrapSeed};
use crate::spec::Chain;
use libp2p::{Multiaddr, PeerId};
use quai_consensus::{controllers::WINDOW, history::Record, share_rules};
use quai_network::public::PublicNode;
use quai_primitives::{encode_hex, spec::FORKS, Hash};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Zone blocks below the best peer head (default of `--bootstrap-depth`).
pub const DEPTH: u64 = 1_000;
/// Peers that must agree on the trusted block (default of `--bootstrap-quorum`).
pub const QUORUM: usize = 3;
/// Peers asked for heads and hashes.
const ASK: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The trusted zone block; None chooses one.
    pub trusted: Option<Hash>,
    pub quorum: usize,
    pub depth: u64,
    pub peers: Vec<Multiaddr>,
    pub public: bool,
    pub max_peers: usize,
}

fn hex(h: &[u8]) -> String {
    encode_hex(h)
}
fn short(p: &PeerId) -> String {
    let s = p.to_string();
    s[s.len().saturating_sub(8)..].to_string()
}

/// Build the checkpoint at the trusted block in `datadir` (headers/). Returns
/// the evidence that `bootstrap.json` keeps: how the block was chosen, what
/// was fetched and derived, and which checks run from where.
pub fn run(
    runtime: &tokio::runtime::Handle,
    node: Arc<PublicNode>,
    chain: Arc<Chain>,
    datadir: &Path,
    o: &Options,
    stop: &AtomicBool,
    log: &Log,
) -> Result<serde_json::Value, String> {
    let started = Instant::now();
    let genesis = chain.genesis_hash();
    runtime.spawn(peers::dial_targets(
        node.clone(),
        o.peers.clone(),
        o.public,
        *log,
    ));
    // 1. Peers and their heads.
    let wait_until = Instant::now() + Duration::from_secs(300);
    let mut heads: Vec<(PeerId, u64)> = vec![];
    let mut probed = std::collections::HashSet::new();
    while heads.len() < o.quorum.max(ASK.min(o.max_peers)) {
        if stop.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        let fresh: Vec<PeerId> = node
            .verified_peers()
            .into_iter()
            .filter(|p| !probed.contains(p))
            .take(ASK)
            .collect();
        if !fresh.is_empty() {
            probed.extend(fresh.iter().copied());
            let answers = join_all(
                runtime,
                fresh.iter().map(|p| {
                    let node = node.clone();
                    let p = *p;
                    async move { (p, peers::peer_head(&node, p, genesis).await) }
                }),
            );
            for (p, r) in answers {
                match r {
                    Ok(h) => heads.push((p, h)),
                    Err(e) => log.debug(&format!("bootstrap: peer {p}: no head: {e}")),
                }
            }
        }
        if heads.len() >= o.quorum && Instant::now() >= wait_until.min(started + Duration::from_secs(90)) {
            break;
        }
        if Instant::now() >= wait_until {
            return Err(format!(
                "bootstrap: {} peers answered their head in {}s, {} needed",
                heads.len(),
                started.elapsed().as_secs(),
                o.quorum
            ));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    heads.sort_by_key(|(_, h)| std::cmp::Reverse(*h));
    let best = heads[0].1;
    log.info(&format!(
        "bootstrap: zone heads of {} peers: {}",
        heads.len(),
        heads
            .iter()
            .map(|(p, h)| format!("{}={h}", short(p)))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    let asked: Vec<PeerId> = heads.iter().map(|(p, _)| *p).collect();
    let hash_votes = |ctx: usize, number: u64| -> Vec<(PeerId, Option<Hash>)> {
        join_all(
            runtime,
            asked.iter().map(|p| {
                let node = node.clone();
                let p = *p;
                async move { (p, peers::peer_hash_at(&node, p, ctx, number).await.ok().flatten()) }
            }),
        )
    };
    // 2. The trusted block.
    let (trusted, how) = match o.trusted {
        Some(hash) => {
            let block = fetch_block(runtime, &node, &asked, 2, hash)?;
            if block.number + o.depth > best {
                return Err(format!(
                    "the trusted block {} is at zone {}, only {} below the best peer head {best}; choose one at least {} below",
                    hex(&hash),
                    block.number,
                    best.saturating_sub(block.number),
                    o.depth
                ));
            }
            (block, "given")
        }
        None => {
            let h = best
                .checked_sub(o.depth)
                .ok_or("the peers' head is below the bootstrap depth")?;
            let votes = hash_votes(2, h);
            let mut tally: BTreeMap<Hash, usize> = BTreeMap::new();
            for (_, v) in &votes {
                if let Some(v) = v {
                    *tally.entry(*v).or_default() += 1;
                }
            }
            let (hash, _) = tally
                .iter()
                .max_by_key(|(_, n)| **n)
                .ok_or_else(|| format!("no peer answered the hash at zone {h}"))?;
            (fetch_block(runtime, &node, &asked, 2, *hash)?, "chosen")
        }
    };
    let votes = hash_votes(2, trusted.number);
    let agree: Vec<PeerId> = votes
        .iter()
        .filter(|(_, v)| *v == Some(trusted.hash))
        .map(|(p, _)| *p)
        .collect();
    let disagree: Vec<String> = votes
        .iter()
        .filter(|(_, v)| *v != Some(trusted.hash))
        .map(|(p, v)| format!("{}={}", short(p), v.map_or("none".into(), |h| hex(&h))))
        .collect();
    if agree.len() < o.quorum {
        return Err(format!(
            "zone block {} at {} is canonical for {} of {} peers, {} needed (others: {})",
            hex(&trusted.hash),
            trusted.number,
            agree.len(),
            votes.len(),
            o.quorum,
            disagree.join(" ")
        ));
    }
    log.info(&format!(
        "bootstrap: trusted zone block {} at {} ({how}): {} below the best peer head {best}; canonical for {} of {} peers ({}){}",
        hex(&trusted.hash),
        trusted.number,
        best - trusted.number,
        agree.len(),
        votes.len(),
        agree.iter().map(short).collect::<Vec<_>>().join(" "),
        if disagree.is_empty() {
            String::new()
        } else {
            format!("; other answers: {}", disagree.join(" "))
        }
    ));
    // 3. The prime head at the trusted block.
    let numbers = trusted.numbers;
    let prime_votes = hash_votes(0, numbers[0]);
    let coincident = prime_votes
        .iter()
        .filter(|(_, v)| *v == Some(trusted.hash))
        .count()
        >= o.quorum;
    let prime_head = if coincident {
        (trusted.hash, numbers[0])
    } else {
        (trusted.dom_parents[0], numbers[0] - 1)
    };
    let n = prime_head.1;
    if n < FORKS.controller_kick_in + 2 * WINDOW + 2 {
        return Err(format!(
            "the prime head {n} is within {} prime blocks of ControllerKickInBlock; replay from a checkpoint instead",
            2 * WINDOW + 2
        ));
    }
    // 4. The exchange rates of P-8,000 ..= P-4,002, ending at the seed prime block.
    let (from, to) = (n - 2 * WINDOW, n - WINDOW - 2);
    log.info(&format!(
        "bootstrap: prime head {} at {n}; fetching prime blocks {from}..={to} for the exchange rates",
        hex(&prime_head.0)
    ));
    let primes = runtime.block_on(peers::fetch_range(&node, &asked, 0, from, to, *log))?;
    let mut rates = BTreeMap::new();
    for b in &primes {
        let r = Record::decode(&b.raw).map_err(|e| format!("prime {}: {e:?}", b.number))?;
        let rate = share_rules::uint(
            r.object()
                .header()
                .wire()
                .exchange_rate
                .as_deref()
                .unwrap_or_default(),
        )
        .map_err(|e| format!("prime {} exchange rate: {e:?}", b.number))?;
        rates.insert(b.number, rate);
    }
    let seed_prime = primes.last().ok_or("no seed prime block")?.clone();
    drop(primes);
    let seed_region = fetch_block(runtime, &node, &asked, 1, seed_prime.hash)?;
    let start = seed_prime.numbers[2];
    log.info(&format!(
        "bootstrap: seed prime block {} at prime {}, region {}, zone {start}; adopting zone heights {start}..={}",
        hex(&seed_prime.hash),
        seed_prime.numbers[0],
        seed_prime.numbers[1],
        trusted.number
    ));
    // 5. Zone heights from the seed prime block to the trusted block.
    let mut source = PeerSource::start(
        runtime,
        node.clone(),
        peers::Options {
            start,
            heads: [
                (seed_prime.hash, seed_prime.numbers[0]),
                (seed_prime.hash, seed_prime.numbers[1]),
                (seed_prime.hash, start),
            ],
            genesis,
            to: Some(trusted.number),
            margin: 64,
            max_peers: o.max_peers,
            inflight: 3,
            peers: o.peers.clone(),
            public: o.public,
            handoff: false,
        },
        *log,
    )?;
    let seed = BootstrapSeed {
        trusted: (trusted.hash, trusted.number),
        prime_head,
        prime_view: seed_prime.raw.clone(),
        region_view: seed_region.raw.clone(),
        rates,
    };
    let report = processor::bootstrap_checkpoint(chain, datadir, seed, &mut source, stop, log)?;
    drop(source);
    let evidence = serde_json::json!({
        "trusted_hash": hex(&trusted.hash),
        "trusted_zone_height": trusted.number,
        "how": how,
        "depth_below_best_peer_head": best - trusted.number,
        "best_peer_head": best,
        "peer_heads": heads.iter().map(|(p, h)| (p.to_string(), *h)).collect::<BTreeMap<_, _>>(),
        "agreeing_peers": agree.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        "peers_asked": votes.len(),
        "quorum": o.quorum,
        "prime_head": {"hash": hex(&prime_head.0), "number": n, "trusted_block_is_prime": coincident},
        "exchange_rate_prime_blocks": [from, to],
        "seed_prime_block": {"hash": hex(&seed_prime.hash), "numbers": seed_prime.numbers},
        "adopted_zone_heights": [report.start, report.trusted],
        "heights": report.heights,
        "records": {"prime": report.records[0], "region": report.records[1]},
        "region_rollups": report.rollups,
        "token_choice_rows": report.choices,
        "checkpoint_windows": {"prime": report.windows[0], "region": report.windows[1], "zone": report.windows[2]},
        "checkpoint": report.checkpoint.display().to_string(),
        "seconds": started.elapsed().as_secs_f64(),
        "checks": {
            "trusted_without_checks": format!("zone heights {}..={} (hash-linked to the trusted block)", report.start, report.trusted),
            "every_stateless_check_from": report.trusted + 1,
            "not_run": "execution (state, ETX, UTXO and receipt roots, gas and state counters): stateless mode",
        },
    });
    let path = report
        .checkpoint
        .parent()
        .map(|d| d.join("bootstrap.json"))
        .ok_or("checkpoint directory")?;
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&evidence).unwrap_or_default(),
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    log.info(&format!(
        "bootstrap: checkpoint at zone {} written in {:.0}s ({} heights adopted, {} prime and {} region records, {} rollups, {} token choice rows; windows {} prime, {} region, {} zone); evidence in {}",
        report.trusted,
        started.elapsed().as_secs_f64(),
        report.heights,
        report.records[0],
        report.records[1],
        report.rollups,
        report.choices,
        report.windows[0],
        report.windows[1],
        report.windows[2],
        path.display()
    ));
    log.info(&format!(
        "bootstrap: zone heights {}..={} are trusted through their hash links to {}; every stateless check runs from zone {} on; execution checks do not run (stateless)",
        report.start,
        report.trusted,
        hex(&trusted.hash),
        report.trusted + 1
    ));
    Ok(evidence)
}

/// A block by hash from the first of `peers` that has it.
fn fetch_block(
    runtime: &tokio::runtime::Handle,
    node: &Arc<PublicNode>,
    peers: &[PeerId],
    ctx: usize,
    hash: Hash,
) -> Result<Block, String> {
    let mut last = String::new();
    for p in peers {
        match runtime.block_on(peers::peer_block(node, *p, ctx, hash)) {
            Ok(b) => return Ok(b),
            Err(e) => last = format!("{p}: {e}"),
        }
    }
    Err(format!(
        "no peer served {} block {} (last: {last})",
        ["prime", "region", "zone"][ctx],
        hex(&hash)
    ))
}

/// Run futures on the runtime at once and collect their outputs in order.
fn join_all<T: Send + 'static, F: std::future::Future<Output = T> + Send + 'static>(
    runtime: &tokio::runtime::Handle,
    futures: impl Iterator<Item = F>,
) -> Vec<T> {
    let handles: Vec<_> = futures.map(|f| runtime.spawn(f)).collect();
    runtime.block_on(async {
        let mut out = Vec::with_capacity(handles.len());
        for h in handles {
            if let Ok(v) = h.await {
                out.push(v);
            }
        }
        out
    })
}
