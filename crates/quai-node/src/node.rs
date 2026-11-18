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
    let settings = Settings {
        jobs: o.jobs,
        progress: o.progress,
        state: !o.stateless,
        datadir: Some(o.datadir.clone()),
        resume: true,
        checkpoint_every: o.checkpoint_every,
        commit_every: o.commit_every,
        store: true,
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
    // The RPC view follows the processor's published head.
    let (mut view, live) = processor.live_view()?;
    // The transaction pool resets on every published head (with state).
    let pool = if o.stateless {
        None
    } else {
        crate::cli::start_pool(&mut view, &o.txpool, runtime.handle(), stop.clone(), *log)?
    };
    let view = Arc::new(view);
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
    if let Some(listen) = &o.rpc_listen {
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
    // Peers.
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
                let followed = crate::tip::follow(
                    &mut processor,
                    runtime.handle(),
                    node.clone(),
                    gossip,
                    &stop,
                    log,
                );
                match followed {
                    Ok(t) => log.info(&format!(
                        "tip: {} offered, {} accepted, {} rejected, {} fetched, {} reorgs ({} blocks, deepest {}), {} refused by the horizon; gossip {} blocks, {} workshares",
                        t.offered, t.accepted, t.rejected, t.fetched, t.reorgs, t.reorged_blocks, t.deepest, t.refused,
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
    drop(live);
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
