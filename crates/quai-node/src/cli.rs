//! Command line: global options, the `replay` and `rpc` commands and room
//! for `run`.
use crate::log::{Level, Log};
use crate::replay::{self, End};
use std::path::PathBuf;
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
  run       Run a node (not available yet)
  help      Print this help

Global options:
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

The RPC serves zone blocks to the latest state checkpoint in
<datadir>/replay/<chain>/state/ (the block store of `run` or `replay --store`,
or the export), with go-quai v0.57.0's JSON. The tag latest is the checkpoint
block. It keeps only that state, as a pruned go-quai node would. A block
store without state (headers/) is served to its head, without state.

Checkpoints go to <datadir>/replay/<chain>/{state,headers}/ every
--checkpoint-every heights and at the end. A run without --resume refuses a
directory that already holds checkpoints.

Exit status: 0 all checks passed, 1 a check failed, 2 usage or setup error.
";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Run,
    Replay(ReplayArgs),
    Rpc(RpcArgs),
}
#[derive(Debug, PartialEq, Eq)]
pub struct RpcArgs {
    pub export: Option<PathBuf>,
    pub listen: String,
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

pub fn parse(args: &[String]) -> Result<Args, String> {
    let mut datadir = None;
    let mut network = None;
    let mut store = false;
    let mut level = Level::Info;
    let mut command: Option<String> = None;
    let mut export = None;
    let mut to = None;
    let mut jobs = None;
    let mut progress = 10u64;
    let mut report = true;
    let mut state = false;
    let mut resume = false;
    let mut checkpoint_every = None;
    let mut commit_every = None;
    let mut listen = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let takes_value = matches!(
            flag.as_str(),
            "--datadir"
                | "--network"
                | "--log-level"
                | "--export"
                | "--to"
                | "--jobs"
                | "--progress"
                | "--checkpoint-every"
                | "--commit-every"
                | "--listen"
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
        match flag.as_str() {
            "--datadir" => datadir = Some(PathBuf::from(value.unwrap())),
            "--network" => network = value,
            "--log-level" => {
                let v = value.unwrap();
                level =
                    Level::parse(&v).ok_or_else(|| format!("--log-level: unknown level {v:?}"))?;
            }
            "--export" => export = Some(PathBuf::from(value.unwrap())),
            "--listen" => listen = value,
            "--to" => to = Some(number("--to", &value.unwrap())?),
            "--jobs" => {
                let n: usize = number("--jobs", &value.unwrap())?;
                if n == 0 || n > 256 {
                    return Err("--jobs must be between 1 and 256".into());
                }
                jobs = Some(n);
            }
            "--progress" => {
                progress = number("--progress", &value.unwrap())?;
                if progress == 0 {
                    return Err("--progress must be at least 1".into());
                }
            }
            "--no-report" => report = false,
            "--state" => state = true,
            "--store" => store = true,
            "--resume" => resume = true,
            "--checkpoint-every" | "--commit-every" => {
                let n: u64 = number(&flag, &value.unwrap())?;
                if n == 0 {
                    return Err(format!("{flag} must be at least 1"));
                }
                if flag == "--checkpoint-every" {
                    checkpoint_every = Some(n);
                } else {
                    commit_every = Some(n);
                }
            }
            "-h" | "--help" => command = Some("help".into()),
            f if f.starts_with('-') => return Err(format!("unknown option {f}")),
            c => {
                if command.is_some() {
                    return Err(format!("unexpected argument {c:?}"));
                }
                command = Some(c.to_string());
            }
        }
        i += 1;
    }
    let datadir = match datadir {
        Some(d) => d,
        None => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".quai-node"))
            .ok_or("--datadir is required when HOME is not set")?,
    };
    let replay_only = to.is_some()
        || jobs.is_some()
        || !report
        || store
        || state
        || resume
        || checkpoint_every.is_some()
        || commit_every.is_some();
    let command = match command.as_deref() {
        None | Some("help") => Command::Help,
        Some("run") => Command::Run,
        Some("replay") => Command::Replay(ReplayArgs {
            export: export.take().ok_or("replay needs --export <DIR>")?,
            to,
            jobs: jobs.unwrap_or_else(default_jobs),
            progress,
            report,
            state,
            resume,
            checkpoint_every: checkpoint_every.unwrap_or(10_000),
            commit_every: commit_every.unwrap_or(1_000),
            store,
        }),
        Some("rpc") => Command::Rpc(RpcArgs {
            export: export.take(),
            listen: listen.take().unwrap_or_else(|| "127.0.0.1:8547".into()),
        }),
        Some(c) => return Err(format!("unknown command {c:?}")),
    };
    if replay_only && !matches!(command, Command::Replay(_)) {
        return Err("--to, --jobs, --state, --store, --resume, --checkpoint-every, --commit-every and --no-report belong to replay".into());
    }
    if export.is_some() && !matches!(command, Command::Replay(_)) {
        return Err("--export belongs to replay and rpc".into());
    }
    if listen.is_some() {
        return Err("--listen belongs to rpc".into());
    }
    Ok(Args {
        datadir,
        network: network.unwrap_or_else(|| "colosseum".into()),
        level,
        command,
    })
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
        Command::Run => {
            log.error("run is not available yet: the node has no network or live head. Use replay, then rpc.");
            2
        }
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
            let config = quai_rpc::Config {
                mode: "native".into(),
                listen: r.listen,
                timeout_ms: 30_000,
                ..quai_rpc::Config::default()
            };
            match quai_rpc::App::native(config, Arc::new(view)).and_then(quai_rpc::serve_blocking) {
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
            let summary = match replay::run(&opts, &log) {
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
            if summary.end == End::Failed {
                1
            } else {
                0
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
        assert_eq!(parse(&args(&["run"])).unwrap().command, Command::Run);
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
            })
        );
        assert_eq!(
            parse(&args(&["rpc", "--export=/x", "--listen", "0.0.0.0:1"]))
                .unwrap()
                .command,
            Command::Rpc(RpcArgs {
                export: Some("/x".into()),
                listen: "0.0.0.0:1".into(),
            })
        );
        assert_eq!(
            parse(&args(&["rpc"])).unwrap().command,
            Command::Rpc(RpcArgs {
                export: None,
                listen: "127.0.0.1:8547".into(),
            })
        );
        assert!(parse(&args(&["rpc", "--store"])).is_err());
        assert!(parse(&args(&["rpc", "--export", "/x", "--state"])).is_err());
        assert!(parse(&args(&["replay", "--export", "/x", "--listen", "a"])).is_err());
        assert!(parse(&args(&["--log-level", "loud", "run"])).is_err());
        assert!(parse(&args(&["sync"])).is_err());
    }
}
