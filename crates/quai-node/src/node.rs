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
    /// Target `[region, zone]` slice (default `[0, 0]`).
    pub slice: [u8; 2],
    /// Preserve every historical state version for archive RPC queries.
    pub archive: bool,
    /// Maintain workshare, donor, supply analytics, and locked Quai indexes.
    pub index_address_utxos: bool,
    /// Prune canonical blocks older than the newest `N` blocks.
    pub prune_blocks: Option<u64>,
    /// Number of latest checkpoints and compacted state files to retain.
    pub keep_checkpoints: usize,
    /// Serve Circuit Relay v2 for NAT-constrained peers.
    pub p2p_relay_service: bool,
    /// Enable GossipSub publishing (also auto-enabled when mining or Stratum is active).
    pub p2p_publish: bool,
    /// RPC transport allowlists, CORS, vhosts, and Unix IPC path.
    pub rpc_security: crate::cli::RpcSecurityOptions,
    /// Local mining worker configuration.
    pub miner: crate::cli::MinerOptions,
    /// Optional upstream stateful builder RPC (`--builder-rpc <URL>`) for
    /// zero-storage full-block mining in stateless mode.
    pub builder_rpc: Option<String>,
    /// Stratum v1 server and HTTP/WS dashboard configuration.
    pub stratum: crate::stratum::StratumConfig,
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

/// Load the persistent libp2p Ed25519 identity key from `<datadir>/node/key`,
/// generating and saving it (mode 0600 on Unix) on first run so the node keeps
/// a stable PeerId and peer reputation across restarts.
fn load_or_create_p2p_key(
    datadir: &std::path::Path,
    log: &Log,
) -> Result<libp2p::identity::Keypair, String> {
    let dir = datadir.join("node");
    let path = dir.join("key");
    if path.is_file() {
        let bytes =
            std::fs::read(&path).map_err(|e| format!("read p2p key {}: {e}", path.display()))?;
        return libp2p::identity::Keypair::from_protobuf_encoding(&bytes)
            .map_err(|e| format!("decode p2p key {}: {e}", path.display()));
    }
    let key = libp2p::identity::Keypair::generate_ed25519();
    let encoded = key
        .to_protobuf_encoding()
        .map_err(|e| format!("encode p2p key: {e}"))?;
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| {
        let tmp = dir.join("key.tmp");
        std::fs::write(&tmp, &encoded)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &path)
    }) {
        log.warn(&format!(
            "could not persist p2p key {}: {e}",
            path.display()
        ));
    }
    Ok(key)
}

/// Shared JSON formatter for `admin_peers` across both `run` and `light` modes.
pub(crate) fn format_peer_snapshot(
    p: quai_network::public::PeerSnapshot,
    head_height: Option<u64>,
) -> serde_json::Value {
    let direction = match (p.inbound, p.outbound) {
        (true, true) => "both",
        (true, false) => "inbound",
        (false, true) => "outbound",
        (false, false) => "unknown",
    };
    serde_json::json!({
        "id": p.id,
        "peerId": p.id,
        "name": p.agent,
        "agentVersion": p.agent,
        "caps": p.protocols,
        "multiaddrs": p.multiaddrs,
        "remoteIp": p.remote_ip,
        "direction": direction,
        "latencyMs": p.latency_ms,
        "connectedSecs": p.connected_secs,
        "relayed": p.relayed,
        "verified": p.verified,
        "quality": p.quality,
        "headHeight": head_height,
        "network": {
            "remoteAddress": p.remote_addr,
            "inbound": p.inbound,
            "outbound": p.outbound,
        },
        "protocols": {
            "quai": {
                "verified": p.verified,
            }
        }
    })
}

/// Shared JSON formatter for on-disk storage breakdown in `admin_nodeInfo`.
pub(crate) fn format_disk_usage(
    d: &crate::store::DiskUsage,
    prune_blocks: Option<u64>,
) -> serde_json::Value {
    serde_json::json!({
        "stateTrieBytes": d.state_trie_bytes,
        "pastBytes": d.past_bytes,
        "blocksBytes": d.blocks_bytes,
        "checkpointsBytes": d.checkpoints_bytes,
        "lightHeadersBytes": d.light_headers_bytes,
        "otherBytes": d.other_bytes,
        "totalBytes": d.total_bytes,
        "pruneBlocks": prune_blocks,
    })
}

/// Parse and dial a libp2p multiaddr for `admin_addPeer`.
pub(crate) fn dial_peer_multiaddr(
    node: &Arc<PublicNode>,
    rt: &tokio::runtime::Handle,
    multiaddr: &str,
) -> Result<bool, String> {
    let addr: Multiaddr = multiaddr
        .parse()
        .map_err(|e| format!("invalid peer multiaddr {multiaddr:?}: {e}"))?;
    let n = Arc::clone(node);
    rt.spawn(async move {
        let _ = n.dial(addr).await;
    });
    Ok(true)
}

/// Anchor the GossipSub validator's [`Heads`] to the processor's validated
/// canonical heads so untrusted gossip cannot poison or eclipse the node.
pub(crate) fn sync_gossip_heads(processor: &mut Processor, node: &PublicNode) {
    for (ctx, head) in processor.canonical_gossip_heads().into_iter().enumerate() {
        if let Some(head) = head {
            node.set_canonical_head(ctx, head);
        }
    }
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
    let chain = Chain::for_environment_and_location(&o.network, o.slice).map_err(|e| {
        format!(
            "chain spec {} [{},{}]: {}",
            o.network,
            o.slice[0],
            o.slice[1],
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
        // and relay; publishing turns on when requested or when mining/Stratum runs.
        let mut config = if o.gossip && o.public {
            NetConfig::public(0)
        } else {
            NetConfig::read_only(policy)
        };
        config.genesis = chain.genesis_hash();
        config.slice = o.slice;
        config.relay_server = o.p2p_relay_service;
        config.publish = o.p2p_publish || o.miner.enabled || o.stratum.enabled;
        let key = load_or_create_p2p_key(&o.datadir, log)?;
        Arc::new(PublicNode::launch(config, key).map_err(|e| format!("p2p: {e}"))?)
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
        archive: o.archive,
        index_address_utxos: o.index_address_utxos,
        prune_blocks: o.prune_blocks,
        keep_checkpoints: o.keep_checkpoints.max(1),
    };
    let mut processor = Processor::open(chain.clone(), settings, None, started)?;
    processor.set_stop_flag(stop.clone());
    sync_gossip_heads(&mut processor, &node);
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
    let miner = if let Some((view, _)) = &mut live_view {
        let mut miner_cfg = o.miner.to_miner_config(Some(&o.stratum.pool_tag));
        if o.stratum.enabled || o.miner.workshares_only || o.builder_rpc.is_some() {
            miner_cfg.enabled = true;
        }
        if o.builder_rpc.is_some() {
            miner_cfg.mode = crate::miner::MinerMode::BuilderRelay;
        } else if o.miner.workshares_only || o.stateless {
            miner_cfg.mode = crate::miner::MinerMode::WorksharesOnly;
        } else {
            miner_cfg.mode = crate::miner::MinerMode::Full;
        }
        let m = crate::miner::Miner::new(
            chain.clone(),
            Arc::new(view.share()),
            view.caches().clone(),
            miner_cfg,
        );
        if !o.stateless && m.mode() == crate::miner::MinerMode::Full {
            m.set_state_builder(view.make_state_block_builder());
        }
        if let Some(builder_url) = &o.builder_rpc {
            let client = crate::light::HttpRpcClient::new(builder_url)?;
            let submit_log = *log;
            let submit_client = client.clone();
            m.set_builder_submit_hook(Arc::new(move |raw_wo| {
                let hex = quai_primitives::encode_hex(raw_wo);
                if let Err(e) =
                    submit_client.call("quai_receiveMinedHeader", serde_json::json!([hex]))
                {
                    submit_log.warn(&format!("builder relay submit failed: {e}"));
                }
            }));
            // Spawn background polling for executed pending headers from `--builder-rpc`.
            let poll_miner = m.clone();
            let poll_view = view.share();
            let poll_stop = stop.clone();
            let poll_log = *log;
            runtime.spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(2));
                loop {
                    if poll_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let c = client.clone();
                    let pm = poll_miner.clone();
                    let pv = poll_view.share();
                    let pl = poll_log;
                    let _ = tokio::task::spawn_blocking(move || {
                        use quai_rpc::view::ChainView;
                        if let Ok(serde_json::Value::String(hex)) =
                            c.call("quai_getPendingHeader", serde_json::json!([]))
                        {
                            if let Ok(raw) = quai_primitives::decode_hex(&hex) {
                                if let Ok(wo) = quai_primitives::work::WorkObject::decode_auxiliary(
                                    &raw,
                                    pm.aux_schedule(),
                                ) {
                                    let head_num = pv.head();
                                    if let Ok(Some(head_blk)) = pv.block_by_number(head_num) {
                                        let ph_ok = wo
                                            .auxiliary_work_header(pm.aux_schedule())
                                            .ok()
                                            .and_then(|h| {
                                                h.wire().parent_hash.as_ref().map(|p| {
                                                    p.value.as_slice() == head_blk.hash.as_slice()
                                                })
                                            })
                                            .unwrap_or(false);
                                        if ph_ok {
                                            pm.set_builder_template(
                                                crate::miner::VerifiedBuilderTemplate {
                                                    parent_hash: head_blk.hash,
                                                    number: head_num + 1,
                                                    work_object: wo,
                                                    raw,
                                                },
                                            );
                                        }
                                    }
                                }
                            }
                        } else {
                            pl.debug("builder relay: waiting for upstream pending header");
                        }
                    })
                    .await;
                    tick.tick().await;
                }
            });
        }
        if o.p2p_publish || o.miner.enabled || o.miner.workshares_only || o.stratum.enabled {
            let p2p = node.clone();
            let rt = runtime.handle().clone();
            m.set_publish_hook(Arc::new(move |topic, payload| {
                let p2p = p2p.clone();
                rt.spawn(async move {
                    let _ = p2p.publish(topic, payload).await;
                });
            }));
        }
        view.set_miner(m.clone());
        Some(m)
    } else {
        None
    };
    let live_view = live_view.map(|(view, live)| (Arc::new(view), live));
    // Peers' requests are answered from the block store from now on.
    if let Some((view, _)) = &live_view {
        node.serve_from(Arc::new(crate::serve::StoreServer::new(
            view.share(),
            chain.genesis_hash(),
            chain.location(),
        )));
    }
    // net_peerCount, admin_nodeInfo, admin_peers and eth_syncing read live node state.
    if let Some((_, live)) = &live_view {
        {
            let node = node.clone();
            live.node().set_peer_counter(Box::new(move || {
                let (peers, incoming, outgoing) = node.peer_counts();
                quai_rpc::view::PeerCounts {
                    peers: peers as u64,
                    incoming: incoming as u64,
                    outgoing: outgoing as u64,
                }
            }));
        }
        {
            let node = node.clone();
            let rt = runtime.handle().clone();
            let env = chain.spec().environment.clone();
            let chain_id = chain.spec().chain_id;
            let location = chain.location();
            let genesis = chain.genesis_hash();
            let genesis_hex = quai_primitives::encode_hex(&genesis);
            let datadir = o.datadir.clone();
            let metrics_c = metrics.clone();
            let mode_str = if o.stateless {
                if o.builder_rpc.is_some() {
                    "builder-relay"
                } else if o.miner.workshares_only {
                    "workshares"
                } else {
                    "stateless"
                }
            } else if o.archive {
                "archive"
            } else if o.prune_blocks.is_some() && o.keep_checkpoints <= 1 {
                if o.miner.enabled {
                    "compact-miner"
                } else {
                    "compact"
                }
            } else {
                "full"
            }
            .to_string();
            let mining_enabled = o.miner.enabled || o.miner.workshares_only || o.stratum.enabled;
            let mining_mode_str = if o.builder_rpc.is_some() {
                "builder-relay"
            } else if o.miner.workshares_only {
                "workshares"
            } else if mining_enabled {
                "full"
            } else {
                "off"
            }
            .to_string();
            let coinbase_hex = o.miner.coinbase.map(|c| quai_primitives::encode_hex(&c));
            let stratum_cfg = o.stratum.clone();
            let prune_blocks = o.prune_blocks;
            let p2p_publish = o.p2p_publish || mining_enabled;
            let p2p_relay_service = o.p2p_relay_service;
            live.node().set_p2p_node_info(Box::new(move || {
                let info = rt.block_on(node.node_info()).ok();
                let (peer_id, listen_addrs, external_addrs, nat, relay_server) = match info {
                    Some(i) => (
                        i.peer_id,
                        i.listen_addrs,
                        i.external_addrs,
                        i.nat,
                        i.relay_server,
                    ),
                    None => (
                        node.id.to_string(),
                        vec![],
                        vec![],
                        "unknown".into(),
                        p2p_relay_service,
                    ),
                };
                let local_head = metrics_c.head.load(Ordering::Relaxed);
                let peer_head = metrics_c.peer_head();
                let disk = crate::store::datadir_disk_usage(&datadir, &env, &genesis);
                serde_json::json!({
                    "id": peer_id,
                    "name": format!("quai-rust/{}", env!("CARGO_PKG_VERSION")),
                    "mode": mode_str,
                    "network": env,
                    "slice": location,
                    "chainId": chain_id,
                    "genesis": genesis_hex,
                    "datadir": datadir.display().to_string(),
                    "listenAddr": listen_addrs.first().cloned().unwrap_or_default(),
                    "listenAddrs": listen_addrs,
                    "externalAddrs": external_addrs,
                    "nat": nat,
                    "relayServer": relay_server,
                    "p2pPublish": p2p_publish,
                    "sync": {
                        "localHead": local_head,
                        "peerHead": peer_head,
                        "blocksPerSec": metrics_c.rate(),
                        "syncing": peer_head > local_head,
                        "reorgs": metrics_c.reorgs.load(Ordering::Relaxed),
                        "worksharesValid": metrics_c.workshares_valid.load(Ordering::Relaxed),
                    },
                    "mining": {
                        "enabled": mining_enabled,
                        "mode": mining_mode_str,
                        "coinbase": coinbase_hex,
                        "stratumEnabled": stratum_cfg.enabled,
                        "stratum": {
                            "shaAddr": stratum_cfg.sha_addr,
                            "scryptAddr": stratum_cfg.scrypt_addr,
                            "kawpowAddr": stratum_cfg.kawpow_addr,
                            "apiAddr": stratum_cfg.api_addr,
                        },
                    },
                    "storage": format_disk_usage(&disk, prune_blocks),
                    "protocols": {
                        "quai": {
                            "mode": mode_str,
                            "network": env,
                            "chainId": chain_id,
                            "location": location,
                            "genesis": genesis_hex,
                            "head": local_head,
                        }
                    }
                })
            }));
        }
        {
            let node = node.clone();
            let rt = runtime.handle().clone();
            let metrics_c = metrics.clone();
            live.node().set_p2p_peers_info(Box::new(move || {
                let peers = rt.block_on(node.peers_info()).unwrap_or_default();
                let peer_head = metrics_c.peer_head();
                let head_opt = (peer_head > 0).then_some(peer_head);
                let list: Vec<serde_json::Value> = peers
                    .into_iter()
                    .map(|p| format_peer_snapshot(p, head_opt))
                    .collect();
                serde_json::Value::Array(list)
            }));
        }
        {
            let node = node.clone();
            let rt = runtime.handle().clone();
            live.node()
                .set_p2p_add_peer(Box::new(move |addr| dial_peer_multiaddr(&node, &rt, addr)));
        }
        {
            let metrics_c = metrics.clone();
            let starting = start;
            live.node().set_syncing(Box::new(move || {
                let current = metrics_c.head.load(Ordering::Relaxed);
                let highest = metrics_c.peer_head();
                if highest > current {
                    serde_json::json!({
                        "startingBlock": format!("0x{starting:x}"),
                        "currentBlock": format!("0x{current:x}"),
                        "highestBlock": format!("0x{highest:x}"),
                    })
                } else {
                    serde_json::Value::Bool(false)
                }
            }));
        }
    }
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
    if let Some((view, _)) = &live_view {
        let needs_rpc = o.rpc_listen.is_some() || o.rpc_security.ipc_path.is_some();
        if needs_rpc {
            let listen_cfg = o
                .rpc_listen
                .clone()
                .unwrap_or_else(|| "127.0.0.1:8547".into());
            let config = crate::cli::rpc_config_with(listen_cfg, &o.rpc_security);
            let app = quai_rpc::App::native(config, view.clone())?;
            if let Some(listen) = &o.rpc_listen {
                let listener = runtime
                    .block_on(tokio::net::TcpListener::bind(listen))
                    .map_err(|e| format!("rpc listen {listen}: {e}"))?;
                log.info(&format!(
                    "rpc: native JSON-RPC on {}",
                    listener.local_addr().map_err(|e| e.to_string())?
                ));
                let rpc_log = *log;
                let done = shutdown();
                let http_app = app.clone();
                runtime.spawn(async move {
                    if let Err(e) = quai_rpc::serve_until(http_app, listener, done).await {
                        rpc_log.error(&format!("rpc: {e}"));
                    }
                });
            }
            #[cfg(unix)]
            if let Some(ipc_path) = o.rpc_security.ipc_path.clone() {
                log.info(&format!("rpc: Unix IPC on {}", ipc_path.display()));
                let ipc_log = *log;
                let done = shutdown();
                runtime.spawn(async move {
                    if let Err(e) = quai_rpc::ipc::serve_ipc(app, ipc_path, done).await {
                        ipc_log.error(&format!("ipc: {e}"));
                    }
                });
            }
        }
    }
    let _stratum_handle = if o.stratum.enabled {
        let Some(m) = miner.clone() else {
            return Err("--stratum requires a block store (do not combine with --no-store)".into());
        };
        let srv = crate::stratum::StratumServer::new(o.stratum.clone(), m, stop.clone(), *log);
        Some(runtime.block_on(srv.start())?)
    } else {
        None
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
            // Only a run that follows the head through gossip hands off;
            // a run with --to fetches by number up to it.
            handoff: o.gossip && o.public && o.to.is_none(),
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
    sync_gossip_heads(&mut processor, &node);
    // At the peers' head: follow it through gossip.
    if result.is_ok()
        && o.gossip
        && o.public
        && processor.summary().end == End::Complete
        && o.to.is_none()
        && !stop.load(Ordering::Relaxed)
    {
        let stats = Arc::new(crate::tip::GossipStats::default());
        let topic_genesis = chain
            .topic_genesis()
            .map_err(|e| format!("topic genesis: {}", crate::processor::describe(&e)))?;
        match crate::tip::subscribe_slice_with_miner(
            runtime.handle(),
            node.clone(),
            topic_genesis,
            o.slice,
            stop.clone(),
            stats.clone(),
            pool.clone(),
            miner.clone(),
            *log,
        ) {
            Ok((local_tx, gossip)) => {
                if let Some(m) = &miner {
                    m.set_block_sink(Arc::new(move |raw| {
                        let _ = local_tx.try_send(crate::tip::Gossip::Block(raw));
                    }));
                }
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
                    &crate::tip::CatchUp {
                        genesis: chain.genesis_hash(),
                        margin: o.margin,
                        max_peers: o.max_peers,
                    },
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
