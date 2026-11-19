//! Command line: global options, the `replay`, `rpc` and `run` commands, and
//! the configuration file (`config.rs`), which command line options override.
use crate::config::Config;
use crate::log::{Level, Log};
use crate::replay::{self, End};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const USAGE: &str = "\
Usage: quai-node [GLOBAL OPTIONS] <COMMAND> [OPTIONS]

Commands:
  replay    Check go-quai blocks exported in the QEX1 format with the Rust
            consensus code; with --state, also execute them
  rpc       Serve JSON-RPC from the block store and the latest checkpoint of
            the data directory, or from an export (--export): blocks,
            receipts, logs, state, calls
  run       Sync zone [0 0] from go-quai peers, validate and store every
            block, follow the peers' head, and serve RPC and metrics
  observe   Apply go-quai's head choice to the live gossip of slice [0 0]
            without validating (proof-of-work and entropy only); count reorgs
  help      Print this help

Global options:
  --config <FILE>       Configuration file (TOML; see below). Command line
                        options override it
  --datadir <DIR>       Data directory (default: $HOME/.quai-node)
  --network <NAME>      Chain environment: colosseum (mainnet, default) or local
  --log-level <LEVEL>   error, warn, info or debug (default: info)

Replay options:
  --export <DIR>        Export directory with manifest.json and segments/ (required)
  --to <HEIGHT>         Stop after this zone height
  --jobs <N>            Proof-of-work threads (default: available cores minus one)
  --progress <SECONDS>  Seconds between progress lines (default: 10)
  --state               Also execute zone blocks from genesis and compare the
                        state, ETX, UTXO and receipt roots. The state lives
                        in <datadir>/replay/<chain>/state/
  --resume              Continue from the latest checkpoint, after checking it
  --checkpoint-every <N>  Zone heights between checkpoints (default: 10000)
  --commit-every <N>    Zone heights between state commits (default: 1000)
  --store               Keep the accepted blocks in the block store
                        (<checkpoint dir>/blocks.redb) for `rpc`
  --no-report           Do not write <datadir>/replay/last.json

RPC options:
  --export <DIR>        Serve this export (the one the replay checked)
                        instead of the block store
  --listen <ADDR>       Listen address (default: 127.0.0.1:8547)

Transaction pool options (rpc, and run with state):
  --no-txpool           No transaction pool: quai_sendRawTransaction and
                        txpool_* refuse, the pending nonce is the head's
  --txpool-sharing-clients <URLS>
                        Comma-separated http:// endpoints that receive each
                        submitted transaction (go-quai's
                        --txpool.sharing-clients; default: none)
  --txpool-sync-tx-with-return <BOOL>
                        With sharing clients: forward to the first one,
                        return its answer and skip the local pool (go-quai's
                        default true); false: add locally, then forward

Run options:
  --stateless           Every check except execution (no state); follows the
                        head while a replay with state is still behind
  --rpc-listen <ADDR>   RPC address, or off (default: 127.0.0.1:8547)
  --metrics-listen <ADDR>  Prometheus metrics address, or off
                        (default: 127.0.0.1:9101)
  --peer <MULTIADDR>    A peer to dial, with /p2p/<id> (repeatable)
  --no-public           Do not use the colosseum boot peers and the DHT
  --margin <N>          Stay N zone blocks below the peers' head (default: 64)
  --max-peers <N>       Peers to fetch from (default: 8)
  --no-gossip           At the peers' head, poll by number instead of
                        following the head through gossip
  --no-store            Keep no block store (about 50 KB per zone block);
                        needs --rpc-listen off. For a long shadow run
  --shadow-bootstrap <HASH|auto>
                        TEST TOOL for the shadow run, not for production.
                        With --stateless and an empty data directory, start
                        at a trusted zone block instead of the genesis: that
                        block and its ancestry are NOT validated by this
                        node. The node fetches the ancestry the checks read
                        (header windows, rollups, prime controller inputs)
                        by hash links back from it, writes a checkpoint there
                        and checks every block after it. auto chooses the
                        block --shadow-bootstrap-depth below the best peer
                        head. The flag also lets run continue from the
                        checkpoints of such a run; without it run refuses
                        them and starts only from the genesis or a checkpoint
                        it validated itself
  --shadow-bootstrap-quorum <N>  Peers that must report the trusted block
                        as canonical (default: 3)
  --shadow-bootstrap-depth <N>  Zone blocks the trusted block stays below
                        the best peer head (default: 1000)
  --jobs, --to, --progress, --checkpoint-every, --commit-every as for replay

Observe options:
  --seconds <N>         Stop after N seconds (default: 1800)

`run` starts from the latest checkpoint of the data directory (or the
genesis), keeps checkpoints and the block store in
<datadir>/replay/<chain>/{state,headers}/, and polls the peers at their head.
It refuses public peers when the default gateway is an iPhone hotspot
(172.20.10.x) unless ALLOW_METERED=1.

The RPC serves zone blocks to the latest state checkpoint in
<datadir>/replay/<chain>/state/ (the block store of `run` or `replay --store`,
or the export), with go-quai v0.57.0's JSON. The tag latest is the checkpoint
block. It keeps only that state, as a pruned go-quai node would. A block
store without state (headers/) is served to its head, without state.

Checkpoints go to <datadir>/replay/<chain>/{state,headers}/ every
--checkpoint-every heights and when the command stops. A replay without
--resume refuses a directory that already holds state checkpoints.

SIGINT or SIGTERM stops `replay` and `run` after the current height with a
checkpoint (a second signal stops at once).

Configuration file keys: datadir, network, log_level, rpc.listen,
metrics.listen, sync.stateless, sync.jobs, sync.margin, sync.max_peers,
sync.public, sync.peers, sync.checkpoint_every, sync.commit_every,
txpool.enabled, txpool.sharing_clients, txpool.sync_tx_with_return.

Exit status: 0 all checks passed, 1 a check failed, 2 usage or setup error,
130 or 143 stopped by SIGINT or SIGTERM.
";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    /// `observe` for this many seconds.
    Observe(u64),
    Run(crate::node::Options),
    Replay(ReplayArgs),
    Rpc(RpcArgs),
}
#[derive(Debug, PartialEq, Eq)]
pub struct RpcArgs {
    pub export: Option<PathBuf>,
    pub listen: String,
    pub txpool: TxpoolOptions,
}
/// The transaction pool's options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxpoolOptions {
    pub enabled: bool,
    pub sharing_clients: Vec<String>,
    pub sync_tx_with_return: bool,
}
impl Default for TxpoolOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            sharing_clients: vec![],
            sync_tx_with_return: true,
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct ReplayArgs {
    pub export: PathBuf,
    pub to: Option<u64>,
    pub jobs: usize,
    pub progress: u64,
    pub report: bool,
    pub state: bool,
    pub resume: bool,
    pub checkpoint_every: u64,
    pub commit_every: u64,
    pub store: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub datadir: PathBuf,
    pub network: String,
    pub level: Level,
    pub command: Command,
}
fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1))
        .unwrap_or(1)
        .max(1)
}
fn number<T: std::str::FromStr>(flag: &str, value: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("{flag}: not a number: {value:?}"))
}
fn off(v: String) -> Option<String> {
    (v != "off" && !v.is_empty()).then_some(v)
}

/// Flags as given, before the configuration file fills the gaps.
#[derive(Default)]
struct Flags {
    config: Option<PathBuf>,
    datadir: Option<PathBuf>,
    network: Option<String>,
    level: Option<Level>,
    command: Option<String>,
    export: Option<PathBuf>,
    to: Option<u64>,
    jobs: Option<usize>,
    progress: Option<u64>,
    report: bool,
    state: bool,
    store: bool,
    resume: bool,
    checkpoint_every: Option<u64>,
    commit_every: Option<u64>,
    listen: Option<String>,
    stateless: bool,
    rpc_listen: Option<String>,
    metrics_listen: Option<String>,
    peers: Vec<String>,
    no_public: bool,
    no_gossip: bool,
    no_store: bool,
    seconds: Option<u64>,
    margin: Option<u64>,
    max_peers: Option<usize>,
    shadow_bootstrap: Option<Option<quai_primitives::Hash>>,
    bootstrap_quorum: Option<usize>,
    bootstrap_depth: Option<u64>,
    no_txpool: bool,
    sharing_clients: Option<Vec<String>>,
    sync_tx_with_return: Option<bool>,
}
const REPLAY_ONLY: &[&str] = &["--state", "--store", "--resume", "--no-report"];
const RUN_ONLY: &[&str] = &[
    "--stateless",
    "--rpc-listen",
    "--metrics-listen",
    "--peer",
    "--no-public",
    "--no-gossip",
    "--no-store",
    "--margin",
    "--max-peers",
    "--shadow-bootstrap",
    "--shadow-bootstrap-quorum",
    "--shadow-bootstrap-depth",
];
const SYNC: &[&str] = &[
    "--to",
    "--jobs",
    "--progress",
    "--checkpoint-every",
    "--commit-every",
];

pub fn parse(args: &[String]) -> Result<Args, String> {
    let mut f = Flags {
        report: true,
        ..Default::default()
    };
    let mut seen: Vec<String> = vec![];
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let takes_value = matches!(
            flag.as_str(),
            "--config"
                | "--datadir"
                | "--network"
                | "--log-level"
                | "--export"
                | "--to"
                | "--jobs"
                | "--progress"
                | "--checkpoint-every"
                | "--commit-every"
                | "--listen"
                | "--rpc-listen"
                | "--metrics-listen"
                | "--peer"
                | "--margin"
                | "--max-peers"
                | "--shadow-bootstrap"
                | "--shadow-bootstrap-quorum"
                | "--shadow-bootstrap-depth"
                | "--seconds"
                | "--txpool-sharing-clients"
                | "--txpool-sync-tx-with-return"
        );
        let value = if takes_value {
            match inline {
                Some(v) => Some(v),
                None => {
                    i += 1;
                    Some(
                        args.get(i)
                            .cloned()
                            .ok_or_else(|| format!("{flag} needs a value"))?,
                    )
                }
            }
        } else {
            if inline.is_some() {
                return Err(format!("{flag} takes no value"));
            }
            None
        };
        if flag.starts_with("--") {
            seen.push(flag.clone());
        }
        let v = value.unwrap_or_default();
        match flag.as_str() {
            "--config" => f.config = Some(PathBuf::from(v)),
            "--datadir" => f.datadir = Some(PathBuf::from(v)),
            "--network" => f.network = Some(v),
            "--log-level" => {
                f.level = Some(
                    Level::parse(&v).ok_or_else(|| format!("--log-level: unknown level {v:?}"))?,
                )
            }
            "--export" => f.export = Some(PathBuf::from(v)),
            "--listen" => f.listen = Some(v),
            "--rpc-listen" => f.rpc_listen = Some(v),
            "--metrics-listen" => f.metrics_listen = Some(v),
            "--peer" => f.peers.push(v),
            "--to" => f.to = Some(number("--to", &v)?),
            "--margin" => f.margin = Some(number("--margin", &v)?),
            "--shadow-bootstrap" => {
                f.shadow_bootstrap =
                    Some(if v == "auto" {
                        None
                    } else {
                        let raw = quai_primitives::decode_hex(&v)
                            .map_err(|_| format!("--shadow-bootstrap: not a hash: {v:?}"))?;
                        Some(raw.try_into().map_err(|_| {
                            format!("--shadow-bootstrap: not a 32-byte hash: {v:?}")
                        })?)
                    })
            }
            "--shadow-bootstrap-quorum" => {
                let n: usize = number("--shadow-bootstrap-quorum", &v)?;
                if n == 0 || n > 8 {
                    return Err("--shadow-bootstrap-quorum must be between 1 and 8".into());
                }
                f.bootstrap_quorum = Some(n);
            }
            "--shadow-bootstrap-depth" => {
                f.bootstrap_depth = Some(number("--shadow-bootstrap-depth", &v)?)
            }
            "--max-peers" => {
                let n: usize = number("--max-peers", &v)?;
                if n == 0 || n > 8 {
                    return Err("--max-peers must be between 1 and 8".into());
                }
                f.max_peers = Some(n);
            }
            "--jobs" => {
                let n: usize = number("--jobs", &v)?;
                if n == 0 || n > 256 {
                    return Err("--jobs must be between 1 and 256".into());
                }
                f.jobs = Some(n);
            }
            "--progress" => {
                let n: u64 = number("--progress", &v)?;
                if n == 0 {
                    return Err("--progress must be at least 1".into());
                }
                f.progress = Some(n);
            }
            "--no-report" => f.report = false,
            "--state" => f.state = true,
            "--store" => f.store = true,
            "--resume" => f.resume = true,
            "--stateless" => f.stateless = true,
            "--no-public" => f.no_public = true,
            "--no-gossip" => f.no_gossip = true,
            "--no-store" => f.no_store = true,
            "--seconds" => f.seconds = Some(number("--seconds", &v)?),
            "--no-txpool" => f.no_txpool = true,
            "--txpool-sharing-clients" => {
                f.sharing_clients = Some(
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                )
            }
            "--txpool-sync-tx-with-return" => {
                f.sync_tx_with_return = Some(match v.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(format!("{flag}: true or false, not {v:?}")),
                })
            }
            "--checkpoint-every" | "--commit-every" => {
                let n: u64 = number(&flag, &v)?;
                if n == 0 {
                    return Err(format!("{flag} must be at least 1"));
                }
                if flag == "--checkpoint-every" {
                    f.checkpoint_every = Some(n);
                } else {
                    f.commit_every = Some(n);
                }
            }
            "-h" | "--help" => f.command = Some("help".into()),
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            c => {
                if f.command.is_some() {
                    return Err(format!("unexpected argument {c:?}"));
                }
                f.command = Some(c.to_string());
            }
        }
        i += 1;
    }
    let config = match &f.config {
        Some(path) => Config::load(path)?,
        None => Config::default(),
    };
    let datadir = match f.datadir.or(config.path("datadir")?) {
        Some(d) => d,
        None => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".quai-node"))
            .ok_or("--datadir is required when HOME is not set")?,
    };
    let network = f
        .network
        .or(config.text("network")?)
        .unwrap_or_else(|| "colosseum".into());
    let level = match f.level {
        Some(l) => l,
        None => match config.text("log_level")? {
            Some(v) => Level::parse(&v).ok_or_else(|| format!("log_level: unknown level {v:?}"))?,
            None => Level::Info,
        },
    };
    let command_name = f.command.clone().unwrap_or_else(|| "help".into());
    let belongs = |list: &[&str], to: &[&str]| -> Result<(), String> {
        for flag in &seen {
            if list.contains(&flag.as_str()) && !to.contains(&command_name.as_str()) {
                return Err(format!("{flag} belongs to {}", to.join(" and ")));
            }
        }
        Ok(())
    };
    belongs(&["--export"], &["replay", "rpc"])?;
    belongs(REPLAY_ONLY, &["replay"])?;
    belongs(RUN_ONLY, &["run"])?;
    belongs(SYNC, &["replay", "run"])?;
    belongs(&["--listen"], &["rpc"])?;
    belongs(&["--seconds"], &["observe"])?;
    belongs(
        &[
            "--no-txpool",
            "--txpool-sharing-clients",
            "--txpool-sync-tx-with-return",
        ],
        &["rpc", "run"],
    )?;
    let txpool = TxpoolOptions {
        enabled: !f.no_txpool && config.flag("txpool.enabled")?.unwrap_or(true),
        sharing_clients: match f.sharing_clients.take() {
            Some(c) => c,
            None => config.list("txpool.sharing_clients")?.unwrap_or_default(),
        },
        sync_tx_with_return: match f.sync_tx_with_return {
            Some(b) => b,
            None => config.flag("txpool.sync_tx_with_return")?.unwrap_or(true),
        },
    };
    for url in &txpool.sharing_clients {
        crate::txpool::sharing::Client::new(url)?;
    }
    let command = match command_name.as_str() {
        "help" => Command::Help,
        "observe" => Command::Observe(f.seconds.unwrap_or(1800)),
        "replay" => Command::Replay(ReplayArgs {
            export: f.export.take().ok_or("replay needs --export <DIR>")?,
            to: f.to,
            jobs: f.jobs.unwrap_or_else(default_jobs),
            progress: f.progress.unwrap_or(10),
            report: f.report,
            state: f.state,
            resume: f.resume,
            checkpoint_every: f.checkpoint_every.unwrap_or(10_000),
            commit_every: f.commit_every.unwrap_or(1_000),
            store: f.store,
        }),
        "rpc" => Command::Rpc(RpcArgs {
            export: f.export.take(),
            listen: f
                .listen
                .or(config.text("rpc.listen")?.and_then(off))
                .unwrap_or_else(|| "127.0.0.1:8547".into()),
            txpool: txpool.clone(),
        }),
        "run" => {
            let jobs = match f.jobs {
                Some(j) => j,
                None => config
                    .number("sync.jobs")?
                    .map_or_else(default_jobs, |n| n as usize),
            };
            let o = crate::node::Options {
                datadir: datadir.clone(),
                network: network.clone(),
                stateless: f.stateless || config.flag("sync.stateless")?.unwrap_or(false),
                jobs,
                progress: Duration::from_secs(f.progress.unwrap_or(10)),
                rpc_listen: match f.rpc_listen.or(config.text("rpc.listen")?) {
                    Some(v) => off(v),
                    None => Some("127.0.0.1:8547".into()),
                },
                metrics_listen: match f.metrics_listen.or(config.text("metrics.listen")?) {
                    Some(v) => off(v),
                    None => Some("127.0.0.1:9101".into()),
                },
                peers: if f.peers.is_empty() {
                    config.list("sync.peers")?.unwrap_or_default()
                } else {
                    f.peers
                },
                public: !f.no_public && config.flag("sync.public")?.unwrap_or(true),
                margin: match f.margin {
                    Some(m) => m,
                    None => config.number("sync.margin")?.unwrap_or(64),
                },
                max_peers: match f.max_peers {
                    Some(m) => m,
                    None => config.number("sync.max_peers")?.unwrap_or(8).clamp(1, 8) as usize,
                },
                to: f.to,
                checkpoint_every: match f.checkpoint_every {
                    Some(n) => n,
                    None => config.number("sync.checkpoint_every")?.unwrap_or(10_000),
                },
                commit_every: match f.commit_every {
                    Some(n) => n,
                    None => config.number("sync.commit_every")?.unwrap_or(1_000),
                },
                gossip: !f.no_gossip,
                shadow_bootstrap: f.shadow_bootstrap,
                bootstrap_quorum: f.bootstrap_quorum.unwrap_or(crate::bootstrap::QUORUM),
                bootstrap_depth: f.bootstrap_depth.unwrap_or(crate::bootstrap::DEPTH),
                store: !f.no_store,
                txpool: txpool.clone(),
            };
            if !o.store && o.rpc_listen.is_some() {
                return Err(
                    "--no-store needs --rpc-listen off: the RPC serves the block store".into(),
                );
            }
            if o.shadow_bootstrap.is_some() && !o.stateless {
                return Err("--shadow-bootstrap needs --stateless: peers serve no state, so execution has no parent state at the trusted block".into());
            }
            Command::Run(o)
        }
        c => return Err(format!("unknown command {c:?}")),
    };
    Ok(Args {
        datadir,
        network,
        level,
        command,
    })
}

type Signals = (tokio::runtime::Runtime, Arc<AtomicBool>, Arc<AtomicI32>);
/// SIGINT or SIGTERM set the stop flag of a replay; the runtime that
/// listens must outlive the run.
fn replay_signals(log: Log) -> Option<Signals> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("quai-node-signal")
        .enable_all()
        .build()
        .ok()?;
    let stop = Arc::new(AtomicBool::new(false));
    let code = crate::node::install_signals(&runtime, stop.clone(), log);
    Some((runtime, stop, code))
}

/// The pool's configuration from the options; the journal of local
/// transactions lives in `<datadir>/txpool/`.
pub fn pool_config(o: &TxpoolOptions, datadir: &std::path::Path) -> crate::txpool::Config {
    crate::txpool::Config {
        sharing_clients: o.sharing_clients.clone(),
        sync_tx_with_return: o.sync_tx_with_return,
        journal: Some(datadir.join("txpool").join("transactions.journal")),
        ..crate::txpool::Config::default()
    }
}
/// The native RPC config of `rpc` and `run`: go-quai's 5 MiB request
/// limit (`maxRequestContentLength`), so that every transaction go-quai
/// takes fits.
pub fn rpc_config(listen: String) -> quai_rpc::Config {
    quai_rpc::Config {
        mode: "native".into(),
        listen,
        timeout_ms: 30_000,
        max_body_bytes: 5 * 1024 * 1024,
        ..quai_rpc::Config::default()
    }
}
/// Start a transaction pool over `view` (which must keep state) and answer
/// the pool methods from it.
pub fn start_pool(
    view: &mut crate::rpc_view::ExportView,
    o: &TxpoolOptions,
    datadir: &std::path::Path,
    runtime: &tokio::runtime::Handle,
    stop: Arc<AtomicBool>,
    log: Log,
) -> Result<Option<Arc<crate::txpool::service::Service>>, String> {
    use quai_rpc::view::ChainView;
    if !o.enabled {
        log.info("txpool: off");
        return Ok(None);
    }
    if view.head_utxo_set_size().is_none() {
        log.info("txpool: off (the view has no state)");
        return Ok(None);
    }
    let chain = Arc::new(crate::rpc_view::ViewChain::new(view.share())?);
    let config = pool_config(o, datadir);
    let frequency = Duration::from_nanos(config.reorg_frequency);
    let svc = Arc::new(crate::txpool::service::Service::new(
        config,
        chain,
        crate::txpool::service::monotonic(),
        Some(runtime.clone()),
        Some(log),
    )?);
    svc.start(runtime, view.head_events(), stop, frequency);
    view.set_pool(svc.clone());
    log.info(&format!(
        "txpool: on at zone {}, {} sharing clients{}",
        view.head(),
        o.sharing_clients.len(),
        if o.sharing_clients.is_empty() {
            String::new()
        } else {
            format!(", sync with return {}", o.sync_tx_with_return)
        }
    ));
    Ok(Some(svc))
}
/// `quai-node rpc`: serve `view` (and its transaction pool) until SIGINT.
fn serve_rpc(
    mut view: crate::rpc_view::ExportView,
    r: RpcArgs,
    datadir: &std::path::Path,
    log: Log,
) -> Result<(), String> {
    let config = rpc_config(r.listen);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.workers)
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    start_pool(
        &mut view,
        &r.txpool,
        datadir,
        runtime.handle(),
        stop.clone(),
        log,
    )?;
    let mode = config.mode.clone();
    let listen = config.listen.clone();
    let app = quai_rpc::App::native(config, Arc::new(view))?;
    let result = runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(&listen)
            .await
            .map_err(|e| format!("listen {listen}: {e}"))?;
        let address = listener.local_addr().map_err(|e| e.to_string())?;
        println!(
            "{}",
            serde_json::json!({"ready":true,"address":address.to_string(),"mode":mode,"version":quai_rpc::VERSION})
        );
        quai_rpc::serve_until(app, listener, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
    });
    stop.store(true, Ordering::Relaxed);
    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}

/// Runs the command line and returns the process exit status.
pub fn main_with(args: &[String]) -> i32 {
    let args = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("quai-node: {e}\n\n{USAGE}");
            return 2;
        }
    };
    let log = Log::new(args.level);
    match args.command {
        Command::Help => {
            print!("{USAGE}");
            0
        }
        Command::Observe(seconds) => match crate::tip::observe(seconds, &log) {
            Ok(code) => code,
            Err(e) => {
                log.error(&format!("observe: {e}"));
                2
            }
        },
        Command::Run(o) => match crate::node::run(&o, &log) {
            Ok(code) => code,
            Err(e) => {
                log.error(&format!("run: {e}"));
                println!("run ERROR: {e}");
                2
            }
        },
        Command::Rpc(r) => {
            let opened = match &r.export {
                Some(export) => crate::rpc_view::ExportView::open(export, &args.datadir, &log),
                None => crate::rpc_view::ExportView::open_store(&args.datadir, &args.network, &log),
            };
            let view = match opened {
                Ok(v) => v,
                Err(e) => {
                    log.error(&format!("rpc setup: {e}"));
                    return 2;
                }
            };
            match serve_rpc(view, r, &args.datadir, log) {
                Ok(()) => 0,
                Err(e) => {
                    log.error(&format!("rpc: {e}"));
                    1
                }
            }
        }
        Command::Replay(r) => {
            let opts = replay::Options {
                export: r.export,
                to: r.to,
                jobs: r.jobs,
                progress: Duration::from_secs(r.progress),
                state: r.state,
                datadir: Some(args.datadir.clone()),
                resume: r.resume,
                checkpoint_every: r.checkpoint_every,
                commit_every: r.commit_every,
                store: r.store,
            };
            let signals = replay_signals(log);
            let stop = signals.as_ref().map(|s| s.1.clone());
            let summary = match replay::run_until(&opts, &log, None, stop) {
                Ok(s) => s,
                Err(e) => {
                    log.error(&format!("replay setup: {e}"));
                    println!("replay ERROR: {e}");
                    return 2;
                }
            };
            log.info(&format!(
                "time: read+decode {:.1}s, waiting for proof-of-work {:.1}s, ordered checks {:.1}s",
                summary.timing.read_decode.as_secs_f64(),
                summary.timing.wait_pow.as_secs_f64(),
                summary.timing.ordered_checks.as_secs_f64()
            ));
            if r.report {
                let dir = args.datadir.join("replay");
                let path = dir.join("last.json");
                let written = std::fs::create_dir_all(&dir).and_then(|_| {
                    std::fs::write(
                        &path,
                        serde_json::to_vec_pretty(&summary.to_json()).unwrap_or_default(),
                    )
                });
                match written {
                    Ok(()) => log.info(&format!("report written to {}", path.display())),
                    Err(e) => log.warn(&format!("could not write {}: {e}", path.display())),
                }
            }
            println!("{}", summary.line());
            match summary.end {
                End::Failed => 1,
                End::Stopped => signals
                    .map(|s| s.2.load(Ordering::Relaxed))
                    .filter(|c| *c != 0)
                    .unwrap_or(130),
                _ => 0,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn parses_global_and_replay_options_in_any_order() {
        let a = parse(&args(&[
            "--log-level=debug",
            "replay",
            "--export",
            "/x",
            "--to=5",
            "--jobs",
            "3",
            "--datadir",
            "/d",
        ]))
        .unwrap();
        assert_eq!(a.level, Level::Debug);
        assert_eq!(a.datadir, PathBuf::from("/d"));
        assert_eq!(a.network, "colosseum");
        assert_eq!(
            a.command,
            Command::Replay(ReplayArgs {
                export: "/x".into(),
                to: Some(5),
                jobs: 3,
                progress: 10,
                report: true,
                state: false,
                resume: false,
                checkpoint_every: 10_000,
                commit_every: 1_000,
                store: false,
            })
        );
        assert_eq!(parse(&args(&[])).unwrap().command, Command::Help);
        assert!(parse(&args(&["replay"])).is_err());
        assert!(parse(&args(&["replay", "--export", "/x", "--jobs", "0"])).is_err());
        assert!(parse(&args(&["replay", "--export", "/x", "--bogus"])).is_err());
        assert!(parse(&args(&["run", "--export", "/x"])).is_err());
        assert_eq!(
            parse(&args(&["--datadir", "/d", "rpc", "--export", "/x"]))
                .unwrap()
                .command,
            Command::Rpc(RpcArgs {
                export: Some("/x".into()),
                listen: "127.0.0.1:8547".into(),
                txpool: TxpoolOptions::default(),
            })
        );
        assert_eq!(
            parse(&args(&["rpc", "--export=/x", "--listen", "0.0.0.0:1"]))
                .unwrap()
                .command,
            Command::Rpc(RpcArgs {
                export: Some("/x".into()),
                listen: "0.0.0.0:1".into(),
                txpool: TxpoolOptions::default(),
            })
        );
        assert_eq!(
            parse(&args(&["rpc"])).unwrap().command,
            Command::Rpc(RpcArgs {
                export: None,
                listen: "127.0.0.1:8547".into(),
                txpool: TxpoolOptions::default(),
            })
        );
        // Pool options: flags, then the file; only http:// sharing clients.
        let Command::Rpc(r) = parse(&args(&[
            "rpc",
            "--txpool-sharing-clients",
            "http://127.0.0.1:9,http://10.0.0.1:8",
            "--txpool-sync-tx-with-return=false",
        ]))
        .unwrap()
        .command
        else {
            panic!("not rpc")
        };
        assert_eq!(
            r.txpool,
            TxpoolOptions {
                enabled: true,
                sharing_clients: vec!["http://127.0.0.1:9".into(), "http://10.0.0.1:8".into()],
                sync_tx_with_return: false,
            }
        );
        assert!(parse(&args(&["rpc", "--txpool-sharing-clients", "ws://a:1"])).is_err());
        assert!(parse(&args(&["replay", "--export", "/x", "--no-txpool"])).is_err());
        let Command::Rpc(r) = parse(&args(&["rpc", "--no-txpool"])).unwrap().command else {
            panic!("not rpc")
        };
        assert!(!r.txpool.enabled);
        assert!(parse(&args(&["rpc", "--store"])).is_err());
        assert!(parse(&args(&["rpc", "--export", "/x", "--state"])).is_err());
        assert!(parse(&args(&["replay", "--export", "/x", "--listen", "a"])).is_err());
        assert!(parse(&args(&["replay", "--export", "/x", "--stateless"])).is_err());
        assert!(parse(&args(&["--log-level", "loud", "run"])).is_err());
        assert!(parse(&args(&["sync"])).is_err());
    }
    #[test]
    fn run_options_come_from_flags_then_the_config_file() {
        let dir = std::env::temp_dir().join(format!("quai-node-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("node.toml");
        std::fs::write(
            &file,
            "datadir = \"/cfg\"\nlog_level = \"warn\"\n[rpc]\nlisten = \"off\"\n[sync]\nstateless = true\njobs = 2\nmargin = 100\npeers = [\"/ip4/127.0.0.1/tcp/4002\"]\npublic = false\n",
        )
        .unwrap();
        let config = format!("--config={}", file.display());
        let a = parse(&args(&[&config, "run"])).unwrap();
        assert_eq!(a.datadir, PathBuf::from("/cfg"));
        assert_eq!(a.level, Level::Warn);
        let Command::Run(o) = a.command else {
            panic!("not run")
        };
        assert!(o.stateless);
        assert_eq!((o.jobs, o.margin, o.public), (2, 100, false));
        assert_eq!(o.rpc_listen, None);
        assert_eq!(o.metrics_listen.as_deref(), Some("127.0.0.1:9101"));
        assert_eq!(o.peers, vec!["/ip4/127.0.0.1/tcp/4002".to_string()]);
        // Flags win over the file.
        let a = parse(&args(&[
            &config,
            "--datadir",
            "/flag",
            "run",
            "--jobs",
            "4",
            "--rpc-listen",
            "127.0.0.1:1",
            "--peer",
            "/ip4/10.0.0.1/tcp/1",
            "--metrics-listen",
            "off",
            "--to",
            "9",
        ]))
        .unwrap();
        assert_eq!(a.datadir, PathBuf::from("/flag"));
        let Command::Run(o) = a.command else {
            panic!("not run")
        };
        assert_eq!(o.jobs, 4);
        assert_eq!(o.rpc_listen.as_deref(), Some("127.0.0.1:1"));
        assert_eq!(o.metrics_listen, None);
        assert_eq!(o.peers, vec!["/ip4/10.0.0.1/tcp/1".to_string()]);
        assert_eq!(o.to, Some(9));
        // Defaults without a file.
        let Command::Run(o) = parse(&args(&["run"])).unwrap().command else {
            panic!("not run")
        };
        assert!(!o.stateless && o.public);
        assert_eq!((o.margin, o.max_peers), (64, 8));
        assert_eq!(o.rpc_listen.as_deref(), Some("127.0.0.1:8547"));
        // A bad file is an error.
        std::fs::write(&file, "bogus = 1\n").unwrap();
        assert!(parse(&args(&[&config, "run"])).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
