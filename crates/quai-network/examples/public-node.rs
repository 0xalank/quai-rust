//! Run the public P2P profile on the go-quai mainnet (colosseum) and report
//! what it saw: peers, reachability through relays, gossip verdicts and bans.
//!
//! ```text
//! cargo run --release -p quai-network --features native-transport \
//!     --example public-node -- [--minutes 5] [--port 0] [--max-peers 50] \
//!     [--out results/public-p2p] [--dial ADDR]... [--no-boot] [--no-check]
//! ```
//!
//! `--dial` also dials an address, for example another node's relay circuit
//! `<relay>/p2p-circuit/p2p/<peer>`. `--no-boot` skips the boot peers, the
//! topics and the DHT records; `--no-check` turns off the genesis check. With
//! all three, the run only probes whether a node is reachable through its
//! relay.
//!
//! The node is read-only by default: it forwards only gossip that passes
//! go-quai's validators, answers every request "not found", never publishes
//! and submits nothing. It does not dial public peers when the default
//! gateway is an iPhone hotspot (172.20.10.x), the rule of
//! scripts/mainnet/node_guard.sh, unless ALLOW_METERED=1.
use libp2p::{identity, multiaddr::Protocol, Multiaddr, PeerId};
use quai_network::public::{
    self, gossip, gossip_label, Config, Event, PublicNode, Stats, Verdict, COLOSSEUM_BOOTNODES,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Refuse public peers on a metered iPhone hotspot gateway (172.20.10.x),
/// closed when the default gateway cannot be read.
fn check_gateway() -> Res<String> {
    let out = std::process::Command::new("route")
        .args(["-n", "get", "default"])
        .output();
    let gateway = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).lines().find_map(|l| {
            l.trim()
                .strip_prefix("gateway:")
                .map(|g| g.trim().to_string())
        }),
        _ => std::process::Command::new("ip")
            .args(["route", "show", "default"])
            .output()
            .ok()
            .and_then(|o| {
                let text = String::from_utf8_lossy(&o.stdout).to_string();
                let mut words = text.split_whitespace();
                words.find(|w| *w == "via")?;
                words.next().map(str::to_string)
            }),
    }
    .ok_or("cannot read the default gateway; refusing to dial public peers")?;
    if gateway.starts_with("172.20.10.") && std::env::var("ALLOW_METERED").as_deref() != Ok("1") {
        return Err(format!("default gateway {gateway} is an iPhone hotspot (metered)").into());
    }
    Ok(gateway)
}

struct Args {
    minutes: u64,
    port: u16,
    max_peers: usize,
    out: PathBuf,
    dial: Vec<Multiaddr>,
    boot: bool,
    check: bool,
}
fn args() -> Res<Args> {
    let mut a = Args {
        minutes: 5,
        port: 0,
        max_peers: public::GO_MAX_PEERS,
        out: "results/public-p2p".into(),
        dial: vec![],
        boot: true,
        check: true,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--no-boot" => {
                a.boot = false;
                continue;
            }
            "--no-check" => {
                a.check = false;
                continue;
            }
            _ => {}
        }
        let value = it.next().ok_or(format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--minutes" => a.minutes = value.parse()?,
            "--port" => a.port = value.parse()?,
            "--max-peers" => a.max_peers = value.parse()?,
            "--out" => a.out = value.into(),
            "--dial" => a.dial.push(value.parse()?),
            _ => return Err(format!("unknown flag {flag}").into()),
        }
    }
    Ok(a)
}

/// The last `/p2p/` peer ID of an address (the target of a circuit).
fn peer_of(a: &Multiaddr) -> Option<PeerId> {
    a.iter()
        .filter_map(|p| match p {
            Protocol::P2p(peer) => Some(peer),
            _ => None,
        })
        .last()
}

/// What the event stream showed over the run.
#[derive(Default)]
struct Seen {
    connected: BTreeSet<PeerId>,
    verified: BTreeSet<PeerId>,
    agents: BTreeMap<String, u64>,
    relayed_connections: u64,
    dial_failures: u64,
    inbound_circuits: BTreeSet<PeerId>,
    nat: Vec<String>,
    external: BTreeSet<String>,
    /// Seconds from the start to the first accepted message, per topic.
    first_accept: BTreeMap<String, u64>,
}

fn stats_json(s: &Stats) -> Value {
    let gossip: BTreeMap<_, _> = s
        .gossip
        .iter()
        .map(|(k, c)| {
            (
                k.clone(),
                json!({"received": c.received, "accepted": c.accepted, "ignored": c.ignored, "rejected": c.rejected}),
            )
        })
        .collect();
    json!({
        "connected": s.connected,
        "verified": s.verified,
        "relayed_peers": s.relayed_peers,
        "nat": s.nat,
        "kad_mode": s.kad_mode,
        "kad_routing": s.kad_routing,
        "listen_addrs": s.listen_addrs,
        "external_addrs": s.external_addrs,
        "reservations": s.reservations,
        "reservations_accepted": s.reservations_accepted,
        "reservations_failed": s.reservations_failed,
        "circuits_in": s.circuits_in,
        "circuits_out": s.circuits_out,
        "hole_punches_ok": s.hole_punches.0,
        "hole_punches_failed": s.hole_punches.1,
        "provided": s.provided,
        "provider_walks": s.provider_walks,
        "provider_dials": s.provider_dials,
        "gossip": gossip,
        "reasons": s.reasons,
        "genesis_checks_passed": s.genesis_checks.0,
        "genesis_checks_failed": s.genesis_checks.1,
        "bans": s.bans.iter().map(|(p, r)| json!({"peer": p, "reason": r})).collect::<Vec<_>>(),
        "gate_refused": s.gate_refused,
        "dial_failures": s.dial_failures,
        "served_requests": s.served.0,
        "served_answers": s.served.1,
        "served_rate_limited": s.served.2,
        "heads": s.heads,
    })
}

#[tokio::main]
async fn main() -> Res<()> {
    let a = args()?;
    let gateway = check_gateway()?;
    std::fs::create_dir_all(&a.out)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let log_path = a.out.join(format!("public-node-{stamp}.events.jsonl"));
    let log = Arc::new(Mutex::new(std::fs::File::create(&log_path)?));
    let mut config = Config::public(a.port);
    config.max_peers = a.max_peers;
    config.max_connections = a.max_peers as u32 + 16;
    config.genesis_check = a.check;
    if !a.boot {
        config.relays.clear();
        config.provide = false;
    }
    let targets: BTreeSet<PeerId> = a.dial.iter().filter_map(peer_of).collect();
    let node = Arc::new(PublicNode::launch(
        config.clone(),
        identity::Keypair::generate_ed25519(),
    )?);
    let start = Instant::now();
    println!(
        "public node {} (gateway {gateway}, {} min, max {} peers)",
        node.id, a.minutes, a.max_peers
    );
    let seen = Arc::new(Mutex::new(Seen::default()));
    let events = {
        let (node, seen, log, targets) = (node.clone(), seen.clone(), log.clone(), targets.clone());
        tokio::spawn(async move {
            while let Ok(event) = node.next().await {
                let t = start.elapsed().as_secs();
                let mut s = seen.lock().unwrap_or_else(|e| e.into_inner());
                let record = match event {
                    Event::Connected { peer, address } => {
                        s.connected.insert(peer);
                        let relayed = address.iter().any(|p| matches!(p, Protocol::P2pCircuit));
                        if relayed {
                            s.relayed_connections += 1;
                        }
                        targets.contains(&peer).then(|| json!({
                            "t": t, "event": "target_connected", "peer": peer.to_string(),
                            "address": address.to_string(), "relayed": relayed,
                        }))
                    }
                    Event::Disconnected { peer, cause } => targets.contains(&peer).then(|| {
                        json!({"t": t, "event": "target_disconnected", "peer": peer.to_string(), "cause": cause})
                    }),
                    Event::DialFailed { peer, error } => {
                        s.dial_failures += 1;
                        // Every failed dial to a target, and the first 40 others.
                        let target = peer.is_some_and(|p| targets.contains(&p));
                        (target || s.dial_failures <= 40).then(|| json!({
                            "t": t,
                            "event": if target { "target_dial_failed" } else { "dial_failed" },
                            "peer": peer.map(|p| p.to_string()),
                            "error": error,
                        }))
                    }
                    Event::Identified { info, .. } => {
                        *s.agents.entry(info.agent_version.clone()).or_default() += 1;
                        None
                    }
                    Event::Verified { peer } => {
                        s.verified.insert(peer);
                        None
                    }
                    Event::Gossip { topic, verdict, .. } => {
                        if verdict == Verdict::Accept {
                            s.first_accept.entry(gossip_label(&topic)).or_insert(t);
                        }
                        None
                    }
                    Event::Banned { peer, reason } => Some(
                        json!({"t": t, "event": "banned", "peer": peer.to_string(), "reason": reason}),
                    ),
                    Event::Nat(status) => {
                        s.nat.push(format!("{t}s {status}"));
                        Some(json!({"t": t, "event": "nat", "status": status}))
                    }
                    Event::Reservation { relay, result } => Some(json!({
                        "t": t, "event": "reservation", "relay": relay.to_string(),
                        "ok": result.is_ok(), "error": result.err(),
                    })),
                    Event::InboundCircuit { peer } => {
                        s.inbound_circuits.insert(peer);
                        Some(json!({"t": t, "event": "inbound_circuit", "peer": peer.to_string()}))
                    }
                    Event::HolePunch { peer, result } => Some(json!({
                        "t": t, "event": "hole_punch", "peer": peer.to_string(),
                        "ok": result.is_ok(), "error": result.err(),
                    })),
                    Event::ExternalAddr(address) => {
                        s.external.insert(address.to_string());
                        Some(
                            json!({"t": t, "event": "external_addr", "address": address.to_string()}),
                        )
                    }
                    Event::ListenAddr(address) => Some(
                        json!({"t": t, "event": "listen_addr", "address": address.to_string()}),
                    ),
                    _ => None,
                };
                drop(s);
                if let Some(record) = record {
                    let mut f = log.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = writeln!(f, "{record}");
                }
            }
        })
    };
    if a.boot {
        for topic in gossip::slice_topic_names(&public::colosseum_topic_genesis(), [0, 0])? {
            node.subscribe(topic).await?;
        }
        for boot in COLOSSEUM_BOOTNODES {
            let address: Multiaddr = boot.parse()?;
            let peer = peer_of(&address).ok_or("boot node without peer ID")?;
            let bare: Multiaddr = address
                .iter()
                .filter(|p| !matches!(p, Protocol::P2p(_)))
                .collect();
            node.kad_add(peer, bare).await?;
            if let Err(e) = node.dial(address).await {
                println!("dial {boot}: {e}");
            }
        }
    }
    for address in &a.dial {
        if let Err(e) = node.dial(address.clone()).await {
            println!("dial {address}: {e}");
        }
    }
    let end = start + Duration::from_secs(a.minutes * 60);
    let mut samples = vec![];
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        tokio::time::sleep(left.min(Duration::from_secs(30))).await;
        let s = node.stats().await?;
        let (mut received, mut accepted, mut rejected) = (0, 0, 0);
        for c in s.gossip.values() {
            received += c.received;
            accepted += c.accepted;
            rejected += c.rejected;
        }
        println!(
            "{:>4}s peers {} (slots {}, relayed {}) nat {} kad {} ({} routes) reservations {} circuits in {} gossip {received} accepted {accepted} rejected {rejected} bans {} served {}/{}/{} heads {:?}",
            start.elapsed().as_secs(),
            s.connected,
            s.verified,
            s.relayed_peers,
            s.nat,
            s.kad_mode,
            s.kad_routing,
            s.reservations.len(),
            s.circuits_in,
            s.bans.len(),
            s.served.0,
            s.served.1,
            s.served.2,
            s.heads,
        );
        samples.push(json!({
            "t": start.elapsed().as_secs(),
            "connected": s.connected,
            "verified": s.verified,
            "reservations": s.reservations.len(),
            "gossip_received": received,
            "gossip_accepted": accepted,
            "gossip_rejected": rejected,
        }));
    }
    let s = node.stats().await?;
    events.abort();
    let seen = seen.lock().unwrap_or_else(|e| e.into_inner());
    let summary = json!({
        "peer_id": node.id.to_string(),
        "gateway": gateway,
        "minutes": a.minutes,
        "max_peers": a.max_peers,
        "publish": config.publish,
        "seconds": start.elapsed().as_secs(),
        "final": stats_json(&s),
        "peers_ever_connected": seen.connected.len(),
        "peers_ever_verified": seen.verified.len(),
        "relayed_connections": seen.relayed_connections,
        "dial_failures": seen.dial_failures,
        "inbound_circuit_peers": seen.inbound_circuits.len(),
        "agents": seen.agents,
        "nat_changes": seen.nat,
        "external_addrs_confirmed": seen.external,
        "first_accept_seconds": seen.first_accept,
        "samples": samples,
        "events_log": log_path.display().to_string(),
    });
    let path = a.out.join(format!("public-node-{stamp}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&summary)?)?;
    println!("{}", serde_json::to_string_pretty(&summary["final"])?);
    println!("summary: {}", path.display());
    Ok(())
}
