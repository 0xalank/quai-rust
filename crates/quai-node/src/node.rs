//! `quai-node run`: sync zone [0 0] from public go-quai peers through the
//! chain processor, keep the block store and checkpoints, and serve the
//! native RPC and Prometheus metrics while it runs.
//!
//! The node starts from the latest checkpoint in the data directory (or the
//! genesis), fetches zone heights by number from peers (`peers.rs`),
//! validates and stores them, and at the peers' head (less `--margin`) polls
//! for new heights. `--stateless` runs every check except execution, so the
//! node can follow the head while a replay with state is still far behind.
//! SIGINT and SIGTERM stop it after the current height with a checkpoint, so
//! the next run resumes there; a second signal ends it at once.
use crate::log::Log;
use crate::metrics::{self, Metrics};
use crate::peers::{self, PeerSource};
use crate::processor::{End, Processor, Settings};
use crate::spec::Chain;
use libp2p::Multiaddr;
use quai_network::public::{Config as NetConfig, DialPolicy, PublicNode};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub datadir: PathBuf,
    pub network: String,
    pub stateless: bool,
    pub jobs: usize,
    pub progress: Duration,
    /// None: no RPC server.
    pub rpc_listen: Option<String>,
    /// None: no metrics endpoint.
    pub metrics_listen: Option<String>,
    /// Peers to dial (multiaddresses with /p2p/), besides the public ones.
    pub peers: Vec<String>,
    /// Use the colosseum boot peers and the DHT.
    pub public: bool,
    pub margin: u64,
    pub max_peers: usize,
    /// Stop after this zone height.
    pub to: Option<u64>,
    pub checkpoint_every: u64,
    pub commit_every: u64,
    /// At the peers' head less the margin, follow the head through gossip
    /// with go-quai's head choice (tip mode); otherwise poll by number.
    pub gossip: bool,
    /// Test tool for the shadow run: start a stateless node at a trusted
    /// zone block (`bootstrap.rs`) when the data directory holds no
    /// checkpoint (Some(None) chooses the block), and accept the checkpoints
    /// of such a run. That block and its ancestry are not validated by this
    /// node. None: start only from the genesis or a validated checkpoint.
    pub shadow_bootstrap: Option<Option<quai_primitives::Hash>>,
    /// Peers that must agree on the trusted block.
    pub bootstrap_quorum: usize,
    /// Zone blocks the trusted block stays below the best peer head.
    pub bootstrap_depth: u64,
    /// Keep the block store (needed by the RPC). Off for a long shadow run
    /// with bounded disk: the store grows about 50 KB per zone block.
    pub store: bool,
    /// The transaction pool (with state only).
    pub txpool: crate::cli::TxpoolOptions,
}

/// SIGINT and SIGTERM set `stop`; the code is 130 or 143. A second signal
/// ends the process at once.
pub fn install_signals(
    runtime: &tokio::runtime::Runtime,
    stop: Arc<AtomicBool>,
    log: Log,
) -> Arc<AtomicI32> {
    let code = Arc::new(AtomicI32::new(0));
    let c = code.clone();
    runtime.spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let (Ok(mut term), Ok(mut int)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) else {
            log.warn("signals: no handler; SIGINT and SIGTERM end the process at once");
            return;
        };
        let first = tokio::select! {
            _ = term.recv() => 143,
            _ = int.recv() => 130,
        };
        c.store(first, Ordering::Relaxed);
        stop.store(true, Ordering::Relaxed);
        log.warn("signal: stopping after the current height (a second signal stops at once)");
        let second = tokio::select! {
            _ = term.recv() => 143,
            _ = int.recv() => 130,
        };
        log.warn("signal: stopping at once");
        std::process::exit(second);
    });
    code
}

pub fn run(o: &Options, log: &Log) -> Result<i32, String> {
    let started = Instant::now();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("quai-node-io")
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let signal = install_signals(&runtime, stop.clone(), *log);
    let chain = Chain::for_environment(&o.network).map_err(|e| {
        format!(
            "chain spec {}: {}",
            o.network,
            crate::processor::describe(&e)
        )
    })?;
    // Production runs trust no block: refuse a shadow run's checkpoint
    // before anything else (the processor refuses it too).
    if o.shadow_bootstrap.is_none() {
        let dir = crate::processor::checkpoint_dir(&o.datadir, &chain, !o.stateless);
        let latest =
            crate::checkpoint::list(&dir).map_err(|e| format!("checkpoint directory: {e}"))?;
        if let Some((h, path)) = latest.first() {
            if let Some((t, at)) = crate::checkpoint::read(path).ok().and_then(|c| c.trusted) {
                return Err(format!(
                    "checkpoint {h} rejected: it continues a shadow run (a test tool) whose bootstrap trusted zone block {} at {at}; that block and its ancestry were not validated by this node. Without --shadow-bootstrap the node starts only from the genesis or from a checkpoint it validated itself: use another data directory",
                    quai_primitives::encode_hex(&t)
                ));
            }
        }
    }
    // The network first: a bootstrap fetches the trusted ancestry from it.
    let peer_addrs = o
        .peers
        .iter()
        .map(|p| p.parse::<Multiaddr>().map_err(|e| format!("peer {p}: {e}")))
        .collect::<Result<Vec<_>, _>>()?;
    if o.public {
        peers::public_allowed().map_err(|e| format!("public peers: {e}"))?;
    }
    if !o.public && peer_addrs.is_empty() {
        return Err("no peers: give --peer or allow public peers".into());
    }
    let node = {
        let _guard = runtime.enter();
        let policy = if o.public {
            DialPolicy::Public
        } else {
            DialPolicy::Loopback
        };
        // Gossip needs the public profile: listening, go-quai's validators
        // and relay; publishing stays off.
        let config = if o.gossip && o.public {
            NetConfig::public(0)
        } else {
            NetConfig::read_only(policy)
        };
        Arc::new(
            PublicNode::launch(config, libp2p::identity::Keypair::generate_ed25519())
                .map_err(|e| format!("p2p: {e}"))?,
        )
    };
    if let Some(trusted) = o.shadow_bootstrap {
        if !o.stateless {
            return Err("--shadow-bootstrap needs --stateless: peers serve no state (go-quai GetTrieNode is not implemented), so execution has no parent state at the trusted block".into());
        }
        log.warn("SHADOW BOOTSTRAP (test tool, not for production): this run starts at a trusted zone block; that block and its whole ancestry are NOT validated by this node. Every block after it passes every stateless check");
        let dir = crate::processor::checkpoint_dir(&o.datadir, &chain, false);
        let existing =
            crate::checkpoint::list(&dir).map_err(|e| format!("checkpoint directory: {e}"))?;
        match existing.first() {
            Some((h, path)) => {
                let cp =
                    crate::checkpoint::read(path).map_err(|e| format!("checkpoint {h}: {e}"))?;
                match (cp.trusted, trusted) {
                    (Some((t, at)), Some(want)) if t != want => {
                        return Err(format!(
                            "{} continues a shadow run from zone block {} at {at}, not {}; use another data directory",
                            dir.display(),
                            quai_primitives::encode_hex(&t),
                            quai_primitives::encode_hex(&want)
                        ))
                    }
                    (Some((t, at)), _) => log.warn(&format!(
                        "SHADOW BOOTSTRAP: resuming from checkpoint {h}; the trusted block is {} at {at} (evidence: {})",
                        quai_primitives::encode_hex(&t),
                        dir.join("bootstrap.json").display()
                    )),
                    (None, _) => log.info(&format!(
                        "shadow bootstrap: {} holds the validated checkpoint {h}; resuming from it without a trusted block",
                        dir.display()
                    )),
                }
            }
            None => {
                let bootstrap = crate::bootstrap::run(
                    runtime.handle(),
                    node.clone(),
                    chain.clone(),
                    &o.datadir,
                    &crate::bootstrap::Options {
                        trusted,
                        quorum: o.bootstrap_quorum,
                        depth: o.bootstrap_depth,
                        peers: peer_addrs.clone(),
                        public: o.public,
                        max_peers: o.max_peers,
                    },
                    &stop,
                    log,
                );
                if let Err(e) = bootstrap {
                    // A signal during the bootstrap: nothing was written.
                    if stop.load(Ordering::Relaxed) {
                        log.warn("bootstrap stopped by a signal before its checkpoint");
                        return Ok(match signal.load(Ordering::Relaxed) {
                            0 => 130,
                            code => code,
                        });
                    }
                    return Err(e);
                }
            }
        }
    }
    let settings = Settings {
        jobs: o.jobs,
        progress: o.progress,
        state: !o.stateless,
        datadir: Some(o.datadir.clone()),
        resume: true,
        checkpoint_every: o.checkpoint_every,
        commit_every: o.commit_every,
        store: o.store,
        shadow: o.shadow_bootstrap.is_some(),
    };
    let mut processor = Processor::open(chain.clone(), settings, None, started)?;
    processor.set_stop_flag(stop.clone());
    let start = processor.start();
    let heads = processor.heads().map(|h| h.unwrap_or_default());
    log.info(&format!(
        "run: environment {} (chain {}), {}, starting at zone {start} ({}), {} proof-of-work threads",
        chain.spec().environment,
        chain.spec().chain_id,
        if o.stateless { "stateless" } else { "with state" },
        match processor.resumed_from() {
            Some(h) => format!("checkpoint {h}"),
            None => "genesis".into(),
        },
        o.jobs
    ));
    let metrics = Arc::new(Metrics::new(start));
    processor.set_metrics(metrics.clone());
    // The RPC view follows the processor's published head (with a block
    // store), and the transaction pool resets on every published head (with
    // state and a store).
    let mut live_view = if o.store {
        Some(processor.live_view()?)
    } else {
        None
    };
    let pool = match (&mut live_view, o.stateless) {
        (Some((view, _)), false) => crate::cli::start_pool(
            view,
            &o.txpool,
            &o.datadir,
            runtime.handle(),
            stop.clone(),
            *log,
        )?,
        (None, false) if o.txpool.enabled => {
            log.info("txpool: off (no block store)");
            None
        }
        _ => None,
    };
    let live_view = live_view.map(|(view, live)| (Arc::new(view), live));
    let shutdown = {
        let stop = stop.clone();
        move || {
            let stop = stop.clone();
            async move {
                while !stop.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    };
    if let (Some(listen), Some((view, _))) = (&o.rpc_listen, &live_view) {
        let config = crate::cli::rpc_config(listen.clone());
        let app = quai_rpc::App::native(config, view.clone())?;
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind(listen))
            .map_err(|e| format!("rpc listen {listen}: {e}"))?;
        log.info(&format!(
            "rpc: native JSON-RPC on {}",
            listener.local_addr().map_err(|e| e.to_string())?
        ));
        let rpc_log = *log;
        let done = shutdown();
        runtime.spawn(async move {
            if let Err(e) = quai_rpc::serve_until(app, listener, done).await {
                rpc_log.error(&format!("rpc: {e}"));
            }
        });
    }
    let mut source = PeerSource::start(
        runtime.handle(),
        node.clone(),
        peers::Options {
            start,
            heads,
            genesis: chain.genesis_hash(),
            to: o.to,
            margin: o.margin,
            max_peers: o.max_peers,
            inflight: 3,
            peers: peer_addrs,
            public: o.public,
            handoff: o.gossip && o.public,
        },
        *log,
    )?;
    metrics.set_peers(source.status());
    if let Some(listen) = &o.metrics_listen {
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind(listen))
            .map_err(|e| format!("metrics listen {listen}: {e}"))?;
        log.info(&format!(
            "metrics: Prometheus text on http://{}/metrics",
            listener.local_addr().map_err(|e| e.to_string())?
        ));
        runtime.spawn(metrics::serve(listener, metrics.clone(), shutdown()));
    }
    runtime.spawn(metrics::sample(metrics.clone(), stop.clone()));
    // Validate on this thread until the source ends, a check fails or a
    // signal arrives.
    let mut result = processor.run(&mut source, log);
    drop(source);
    // At the peers' head: follow it through gossip.
    if result.is_ok()
        && o.gossip
        && o.public
        && processor.summary().end == End::Complete
        && o.to.is_none()
        && !stop.load(Ordering::Relaxed)
    {
        let stats = Arc::new(crate::tip::GossipStats::default());
        match crate::tip::subscribe(
            runtime.handle(),
            node.clone(),
            stop.clone(),
            stats.clone(),
            pool.clone(),
            *log,
        ) {
            Ok(gossip) => {
                let mut detector = crate::divergence::Detector::new(
                    Some(o.datadir.join("node").join("divergences.jsonl")),
                    Some(metrics.clone()),
                    *log,
                );
                let followed = crate::tip::follow(
                    &mut processor,
                    runtime.handle(),
                    node.clone(),
                    gossip,
                    &stop,
                    log,
                    &mut detector,
                    Some(metrics.clone()),
                    stats.clone(),
                );
                match followed {
                    Ok(t) => log.info(&format!(
                        "tip: blocks {} offered, {} accepted, {} rejected, {} below the tip base, {} fetched; {} reorgs ({} blocks, deepest {}), {} refused by the horizon; workshares {} valid, {} valid without the parent rules, {} rejected, {} too old; gossip {} blocks, {} workshares",
                        t.offered, t.accepted, t.rejected, t.below_base, t.fetched, t.reorgs, t.reorged_blocks, t.deepest, t.refused,
                        t.shares_valid, t.shares_parent_unknown, t.shares_rejected, t.shares_old,
                        stats.blocks.load(Ordering::Relaxed), stats.workshares.load(Ordering::Relaxed)
                    )),
                    Err(e) => result = Err(e),
                }
            }
            Err(e) => result = Err(e),
        }
    }
    stop.store(true, Ordering::Relaxed);
    let summary = match result {
        Ok(()) => processor.finish(log)?,
        Err(e) => {
            log.error(&format!("run: {e}"));
            return Err(e);
        }
    };
    drop(live_view);
    runtime.shutdown_timeout(Duration::from_secs(5));
    let line = node_line(&summary, metrics.peer_head());
    println!("{line}");
    let report = o.datadir.join("node").join("last.json");
    let written = std::fs::create_dir_all(o.datadir.join("node")).and_then(|_| {
        std::fs::write(
            &report,
            serde_json::to_vec_pretty(&summary.to_json()).unwrap_or_default(),
        )
    });
    if let Err(e) = written {
        log.warn(&format!("could not write {}: {e}", report.display()));
    }
    Ok(match summary.end {
        End::Failed => 1,
        End::Stopped => match signal.load(Ordering::Relaxed) {
            0 => 0,
            code => code,
        },
        _ => 0,
    })
}
/// The result line of a run.
fn node_line(s: &crate::processor::Summary, peer_head: u64) -> String {
    let passed = s.highest_passed.map_or("none".into(), |h| h.to_string());
    let how = match s.end {
        crate::processor::End::Failed => "FAIL",
        crate::processor::End::Stopped => "STOPPED",
        _ => "END",
    };
    let failure = s.failure.as_ref().map_or(String::new(), |f| {
        format!(
            "; first failure: zone_height={} context={} check=\"{}\" expected={} actual={}",
            f.zone_height.map_or("-".into(), |h| h.to_string()),
            f.context,
            f.check,
            f.expected,
            f.actual
        )
    });
    format!(
        "run {how}: highest zone height with all checks passed: {passed}; peers' head {peer_head}; {} heights in this run, {:.1} blocks/s, {:.1}s{}{failure}",
        s.run_heights,
        s.rate(),
        s.elapsed.as_secs_f64(),
        s.checkpoint.map_or(String::new(), |c| format!(", checkpoint {c}")),
    )
}
