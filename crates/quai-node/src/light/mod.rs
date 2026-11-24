//! `quai-node light`: Helios-style trustless light client and zero-storage
//! mining node.
//!
//! Combines:
//! 1. Bounded header-only P2P sync ([`sync`], [`store::LightHeaderStore`]):
//!    validates PoW and PoEM fork choice locally with minimal RAM/disk.
//! 2. Trustless Merkle-proof JSON-RPC ([`proof_rpc::LightView`]): verifies
//!    every balance, nonce, storage slot, contract code, `quai_call`, and
//!    transaction/receipt against locally validated header roots.
//! 3. Zero-storage mining & Stratum v1:
//!    - `--mine-workshares`: mines 100% consensus-valid workshares directly to
//!      P2P gossip with zero chain state.
//!    - `--builder-rpc <URL>`: pulls executed full-block templates from a
//!      stateful builder node, verifies consensus rules locally, and mines both
//!      valid full blocks and workshares via Stratum v1 with zero local state.
pub mod proof_rpc;
pub mod store;
pub mod sync;

use crate::cli::{MinerOptions, RpcSecurityOptions};
use crate::log::Log;
use crate::metrics::{self, Metrics};
use crate::miner::{BuilderRelay, Miner, MinerMode};
use crate::peers;
use crate::spec::Chain;
use libp2p::Multiaddr;
use quai_consensus::pow::SharedCaches;
use quai_network::public::{Config as NetConfig, DialPolicy, PublicNode};
use quai_rpc::view::PeerCounts;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

pub use proof_rpc::{HttpRpcClient, LightView, ProofVerifier};
pub use store::{
    LightHeaderStore, VerifiedAccount, VerifiedBlockPayload, VerifiedSlot, DEFAULT_MAX_HEADERS,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightOptions {
    pub datadir: PathBuf,
    pub network: String,
    pub slice: [u8; 2],
    pub rpc_listen: Option<String>,
    pub metrics_listen: Option<String>,
    pub upstream_rpc: Option<String>,
    pub builder_rpc: Option<String>,
    pub peers: Vec<String>,
    pub public: bool,
    pub p2p_publish: bool,
    pub max_headers: usize,
    pub checkpoint_height: Option<u64>,
    pub rpc_security: RpcSecurityOptions,
    pub miner: MinerOptions,
    pub stratum: crate::stratum::StratumConfig,
}

pub fn run_light(o: &LightOptions, log: &Log) -> Result<i32, String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("quai-light-io")
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let signal = crate::node::install_signals(&runtime, stop.clone(), *log);

    let chain = Chain::for_environment_and_location(&o.network, o.slice)
        .map_err(|e| format!("chain spec: {}", crate::processor::describe(&e)))?;
    let db_path = o
        .datadir
        .join("light")
        .join(&chain.spec().environment)
        .join("headers.redb");
    let store = LightHeaderStore::open(chain.clone(), &db_path, o.max_headers)?;
    let caches = Arc::new(SharedCaches::new(4).map_err(|e| format!("pow caches: {e}"))?);

    let upstream = match &o.upstream_rpc {
        Some(url) => Some(HttpRpcClient::new(url)?),
        None => None,
    };
    let view = LightView::new(store.clone(), caches.clone(), upstream);

    // Launch P2P host (optional if running purely against local peers).
    let peer_addrs: Vec<Multiaddr> = o
        .peers
        .iter()
        .map(|p| p.parse::<Multiaddr>().map_err(|e| format!("peer {p}: {e}")))
        .collect::<Result<Vec<_>, _>>()?;
    if o.public {
        peers::public_allowed().map_err(|e| format!("public peers: {e}"))?;
    }
    let node = {
        let _guard = runtime.enter();
        let policy = if o.public {
            DialPolicy::Public
        } else {
            DialPolicy::Loopback
        };
        let mut cfg = if o.public {
            NetConfig::public(0)
        } else {
            NetConfig::read_only(policy)
        };
        cfg.genesis = chain.genesis_hash();
        cfg.slice = o.slice;
        cfg.publish =
            o.p2p_publish || o.miner.enabled || o.miner.workshares_only || o.stratum.enabled;
        let key = libp2p::identity::Keypair::generate_ed25519();
        Arc::new(PublicNode::launch(cfg, key).map_err(|e| format!("light p2p: {e}"))?)
    };
    for addr in &peer_addrs {
        let n = node.clone();
        let a = addr.clone();
        runtime.spawn(async move {
            let _ = n.dial(a).await;
        });
    }

    // Configure mining worker (WorksharesOnly or BuilderRelay) on the light node.
    let mining_active =
        o.miner.enabled || o.miner.workshares_only || o.builder_rpc.is_some() || o.stratum.enabled;
    let miner = if mining_active {
        let mut mcfg = o.miner.to_miner_config(Some(&o.stratum.pool_tag));
        mcfg.enabled = true;
        mcfg.mode = if o.builder_rpc.is_some() {
            MinerMode::BuilderRelay
        } else {
            MinerMode::WorksharesOnly
        };
        let m = Miner::new(chain.clone(), view.clone(), caches.clone(), mcfg);
        if o.p2p_publish || mining_active {
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

    // Start remote builder template relay when `--builder-rpc` is given.
    if let (Some(builder_url), Some(m)) = (&o.builder_rpc, &miner) {
        let relay = BuilderRelay::new(builder_url, m.clone(), store.clone(), *log)?;
        let relay_stop = stop.clone();
        runtime.spawn(async move {
            relay.run(relay_stop).await;
        });
        log.info(&format!(
            "light: zero-storage full-block builder relay connected to {builder_url}"
        ));
    }

    let peer_head = Arc::new(AtomicU64::new(store.head().0));
    let metrics = Arc::new(Metrics::new(store.head().0));
    {
        let n = node.clone();
        view.set_peer_counter(Box::new(move || {
            let (peers, incoming, outgoing) = n.peer_counts();
            PeerCounts {
                peers: peers as u64,
                incoming: incoming as u64,
                outgoing: outgoing as u64,
            }
        }));
    }
    {
        let n = node.clone();
        let rt = runtime.handle().clone();
        let st = store.clone();
        let ph = peer_head.clone();
        let metrics_c = metrics.clone();
        let env = chain.spec().environment.clone();
        let chain_id = chain.spec().chain_id;
        let location = chain.location();
        let genesis = chain.genesis_hash();
        let genesis_hex = quai_primitives::encode_hex(&genesis);
        let datadir = o.datadir.clone();
        let mode_str = if o.builder_rpc.is_some() {
            "light+builder-relay"
        } else if mining_active {
            "light+workshares"
        } else {
            "light"
        }
        .to_string();
        let mining_mode_str = if o.builder_rpc.is_some() {
            "builder-relay"
        } else if mining_active {
            "workshares"
        } else {
            "off"
        }
        .to_string();
        let coinbase_hex = o.miner.coinbase.map(|c| quai_primitives::encode_hex(&c));
        let stratum_cfg = o.stratum.clone();
        let max_headers = o.max_headers as u64;
        let p2p_publish = o.p2p_publish || mining_active;
        view.set_p2p_node_info(Box::new(move || {
            let info = rt.block_on(n.node_info()).ok();
            let (peer_id, listen_addrs, external_addrs, nat, relay_server) = match info {
                Some(i) => (
                    i.peer_id,
                    i.listen_addrs,
                    i.external_addrs,
                    i.nat,
                    i.relay_server,
                ),
                None => (n.id.to_string(), vec![], vec![], "unknown".into(), false),
            };
            let local_head = st.head().0;
            let best_peer = ph.load(Ordering::Relaxed);
            let disk = crate::store::datadir_disk_usage(&datadir, &env, &genesis);
            serde_json::json!({
                "id": peer_id,
                "name": format!("quai-rust-light/{}", env!("CARGO_PKG_VERSION")),
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
                    "peerHead": best_peer,
                    "blocksPerSec": metrics_c.rate(),
                    "syncing": best_peer > local_head,
                    "reorgs": metrics_c.reorgs.load(Ordering::Relaxed),
                    "worksharesValid": metrics_c.workshares_valid.load(Ordering::Relaxed),
                },
                "mining": {
                    "enabled": mining_active,
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
                "storage": crate::node::format_disk_usage(&disk, Some(max_headers)),
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
        let n = node.clone();
        let rt = runtime.handle().clone();
        let ph = peer_head.clone();
        view.set_p2p_peers_info(Box::new(move || {
            let peers = rt.block_on(n.peers_info()).unwrap_or_default();
            let head = ph.load(Ordering::Relaxed);
            let head_opt = (head > 0).then_some(head);
            let list: Vec<serde_json::Value> = peers
                .into_iter()
                .map(|p| crate::node::format_peer_snapshot(p, head_opt))
                .collect();
            serde_json::Value::Array(list)
        }));
    }
    {
        let n = node.clone();
        let rt = runtime.handle().clone();
        view.set_p2p_add_peer(Box::new(move |addr| {
            crate::node::dial_peer_multiaddr(&n, &rt, addr)
        }));
    }
    {
        let st = store.clone();
        let ph = peer_head.clone();
        view.set_syncing(Box::new(move || {
            let cur = st.head().0;
            let high = ph.load(Ordering::Relaxed);
            if high > cur {
                serde_json::json!({
                    "startingBlock": "0x0",
                    "currentBlock": format!("0x{cur:x}"),
                    "highestBlock": format!("0x{high:x}"),
                })
            } else {
                serde_json::Value::Bool(false)
            }
        }));
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

    // Start local trustless JSON-RPC + Unix IPC.
    if o.rpc_listen.is_some() || o.rpc_security.ipc_path.is_some() {
        let listen_cfg = o
            .rpc_listen
            .clone()
            .unwrap_or_else(|| "127.0.0.1:8547".into());
        let cfg = crate::cli::rpc_config_with(listen_cfg, &o.rpc_security);
        let app = quai_rpc::App::native(cfg, view.clone())?;
        if let Some(listen) = &o.rpc_listen {
            let listener = runtime
                .block_on(tokio::net::TcpListener::bind(listen))
                .map_err(|e| format!("light rpc listen {listen}: {e}"))?;
            log.info(&format!(
                "light rpc: trustless JSON-RPC on {}",
                listener.local_addr().map_err(|e| e.to_string())?
            ));
            let rpc_log = *log;
            let done = shutdown();
            let http_app = app.clone();
            runtime.spawn(async move {
                if let Err(e) = quai_rpc::serve_until(http_app, listener, done).await {
                    rpc_log.error(&format!("light rpc: {e}"));
                }
            });
        }
        #[cfg(unix)]
        if let Some(ipc_path) = o.rpc_security.ipc_path.clone() {
            log.info(&format!("light rpc: Unix IPC on {}", ipc_path.display()));
            let ipc_log = *log;
            let done = shutdown();
            runtime.spawn(async move {
                if let Err(e) = quai_rpc::ipc::serve_ipc(app, ipc_path, done).await {
                    ipc_log.error(&format!("light ipc: {e}"));
                }
            });
        }
    }

    // Start Stratum v1 server if enabled.
    let _stratum_handle = if o.stratum.enabled {
        let Some(m) = miner.clone() else {
            return Err("--stratum requires miner initialization".into());
        };
        let srv = crate::stratum::StratumServer::new(o.stratum.clone(), m, stop.clone(), *log);
        Some(runtime.block_on(srv.start())?)
    } else {
        None
    };

    // Metrics endpoint.
    if let Some(listen) = &o.metrics_listen {
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind(listen))
            .map_err(|e| format!("light metrics listen {listen}: {e}"))?;
        log.info(&format!(
            "light metrics: Prometheus text on http://{}/metrics",
            listener.local_addr().map_err(|e| e.to_string())?
        ));
        runtime.spawn(metrics::serve(listener, metrics.clone(), shutdown()));
    }

    log.info(&format!(
        "light: environment {} (chain {}), slice [{}-{}], head #{}, max_headers={}, mode={}",
        chain.spec().environment,
        chain.spec().chain_id,
        o.slice[0],
        o.slice[1],
        store.head().0,
        o.max_headers,
        if o.builder_rpc.is_some() {
            "light+builder-relay"
        } else if mining_active {
            "light+workshare-miner"
        } else {
            "light-client"
        }
    ));

    // Run header-only sync loop until SIGINT/SIGTERM.
    runtime.block_on(sync::run_header_sync(
        node,
        store,
        caches,
        miner,
        peer_head,
        metrics,
        o.checkpoint_height,
        stop.clone(),
        *log,
    ));

    stop.store(true, Ordering::Relaxed);
    runtime.shutdown_timeout(Duration::from_secs(5));
    Ok(match signal.load(Ordering::Relaxed) {
        0 => 0,
        code => code,
    })
}
