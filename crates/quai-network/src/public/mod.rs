//! Public P2P for the go-quai mainnet (colosseum), ROADMAP Phase 2 step 2.
//!
//! [`PublicNode`] runs libp2p as go-quai v0.57.0 does (`p2p/node/node.go`):
//! TCP with Noise and Yamux, identify (`/quai/1.0.0`), Kademlia
//! `/quai/kad/1.0.0` with a one-minute routing table refresh, GossipSub with
//! go-quai's parameters ([`gossip`]) and the `/quai/1.0.0` request protocol.
//! For a node behind NAT it adds what go-quai enables with
//! `EnableAutoRelayWithStaticRelays` (the boot peers), `EnableHolePunching`
//! and `EnableNATService`: a circuit v2 relay client, DCUtR and AutoNAT.
//!
//! Two profiles:
//! - [`Config::read_only`], the probe profile: dial out only, no listening,
//!   Kademlia client, every gossip message ignored (never forwarded),
//!   inbound `/quai/1.0.0` streams refused at protocol negotiation.
//! - [`Config::public`]: listens; relay reservations while AutoNAT finds the
//!   node private; Kademlia server only after AutoNAT confirms a public
//!   address; Provide and FindProviders on topic CIDs in go-quai's format;
//!   go-quai's GossipSub validators ([`validate`]) before a message is
//!   accepted and forwarded; a genesis check before a peer takes a slot;
//!   go-quai's request scores and bans, a working request rate limit and
//!   per-IP limits ([`peers`]); inbound requests answered "not found"
//!   ([`serve`]), because the node holds no validated data yet.
//!
//! Both are read-only by default: [`PublicNode::publish`] fails unless
//! [`Config::publish`] is set, and nothing here submits transactions.
//! Peer data is unvalidated input until a validator accepts it.
//!
//! Deliberate differences from go-quai: no relay service (go-quai's
//! `EnableRelayService` relays for others once it is public); the DHT is a
//! client until AutoNAT confirms a public address (go-quai: `ModeServer`);
//! a peer takes a slot only after the genesis check; request scores are per
//! peer, not per peer and topic; a block or header with the wrong hash or
//! number in an answer costs -40 (go-quai: +10, then the answer is dropped);
//! the request rate limit works (go-quai's `ProcRequestRate` never trips).
pub mod address;
pub mod gossip;
pub mod peers;
pub mod serve;
pub mod validate;

pub use address::DialPolicy;
pub use gossip::{go_message_id, topic_provider_key};
pub use peers::{Adjustment, Book};
pub use validate::Verdict;

use crate::{
    decode_envelope, response,
    transport::{WireStream, IO_TIMEOUT},
    RequestBook, STREAM_PROTOCOL,
};
use address::{CircuitGate, PolicyTcp};
use futures::StreamExt;
use libp2p::{
    autonat, connection_limits,
    core::{
        muxing::StreamMuxerBox,
        transport::{Boxed, ListenerId},
        upgrade::Version,
    },
    dcutr, dns, gossipsub, identify, identity, kad,
    multiaddr::Protocol,
    noise, ping, relay,
    swarm::{
        behaviour::toggle::Toggle,
        dial_opts::{DialOpts, PeerCondition},
        ConnectionId, NetworkBehaviour, SwarmEvent,
    },
    tcp, yamux, Multiaddr, PeerId, StreamProtocol, Swarm, Transport,
};
use peers::Gate;
use prost::Message;
use quai_consensus::{pow::SharedCaches, verifier::PINNED_SCHEDULE};
use quai_primitives::{
    decode_hex, encode_hex,
    schema::{
        common::{ProtoHash, ProtoLocation},
        quaiprotocol::{
            quai_message::Payload, quai_request_message as rq, quai_response_message as rs,
            QuaiMessage, QuaiRequestMessage, QuaiResponseMessage,
        },
    },
    spec,
    work::WorkObject,
    Hash,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    io,
    net::Ipv4Addr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex, RwLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot, Semaphore};
use validate::{Checked, Heads, Validator};

/// go-quai's Kademlia protocol, [`crate::DHT_PROTOCOL`].
pub const KAD_PROTOCOL: &str = crate::DHT_PROTOCOL;
/// go-quai identify protocol version (`p2p/protocol.ProtocolVersion`).
pub const IDENTIFY_PROTOCOL_VERSION: &str = "/quai/1.0.0";
/// Circuit v2 hop protocol: a peer that lists it can relay for us.
pub const RELAY_HOP_PROTOCOL: &str = "/libp2p/circuit/relay/0.2.0/hop";
/// Colosseum (mainnet) gossip topic prefix. go-quai builds topics from
/// `utils.MakeGenesis().ToBlock(0).Hash()`. This is not the chain genesis hash
/// 0xac81c28f1a72591b87b5f16c9793cdc0e87c45c6d426d1a364c3b8f6386b5b8b.
pub const COLOSSEUM_TOPIC_GENESIS: &str =
    "0xff2b72d3bedb87748bb1b3bb810fd6ed808c82e8e09eae69d13fdc44c440c2e7";
/// go-quai `common/bootnodes.go`, environment colosseum.
pub const COLOSSEUM_BOOTNODES: [&str; 3] = [
    "/dns4/colosseum1.qu.ai/tcp/4002/p2p/12D3KooWB72Vs7gfLgvJsmR7cex7tG5WY7WDdqJD124Res3byDyk",
    "/dns4/colosseum2.qu.ai/tcp/4002/p2p/12D3KooWPdEr7wZfcWg9d2EoX6cmnyx3GvEnV5pVJvRxMUbkKesB",
    "/dns4/colosseum3.qu.ai/tcp/4002/p2p/12D3KooWMHTexdSq1zQ2Sn4L8zddEa8PWK8TGM56ZHszRuoUPoq1",
];
/// go-quai `common/bootnodes.go`, environment orchard.
pub const ORCHARD_BOOTNODES: [&str; 2] = [
    "/dns4/orchard1.qu.ai/tcp/4002/p2p/12D3KooWK755v2ynqqgvrEeTQe38E1i556jDwX9JcduotE1bfvjU",
    "/dns4/orchard2.qu.ai/tcp/4002/p2p/12D3KooWMjRbhyjQ5PCbt6qzMcVJx3n5NRx6uUv43yQP9ec3DV3U",
];
/// go-quai `common/bootnodes.go`, environment garden.
pub const GARDEN_BOOTNODES: [&str; 0] = [];
/// go-quai `common/bootnodes.go`, environment lighthouse.
pub const LIGHTHOUSE_BOOTNODES: [&str; 0] = [];

/// Default bootnodes for a Quai network name (`colosseum`, `orchard`, `garden`, `lighthouse`, `local`).
pub fn bootnodes_for(network: &str) -> &'static [&'static str] {
    match network {
        "colosseum" => &COLOSSEUM_BOOTNODES,
        "orchard" => &ORCHARD_BOOTNODES,
        "garden" => &GARDEN_BOOTNODES,
        "lighthouse" => &LIGHTHOUSE_BOOTNODES,
        _ => &[],
    }
}
/// Read-only profile: most simultaneous connections, established plus
/// pending dials.
pub const MAX_PEERS: u32 = 8;
const MAX_PENDING_DIALS: u32 = 2;
/// go-quai `--node.max-peers` default (cmd/utils/flags.go `MaxPeersFlag`).
pub const GO_MAX_PEERS: usize = 50;
/// Outbound request streams at once.
const MAX_STREAMS: usize = 32;
/// Inbound `/quai/1.0.0` request streams served concurrently (global & per-peer).
const MAX_INBOUND_STREAMS: usize = 64;
const MAX_INBOUND_PER_PEER: usize = 8;
/// Bounded content-hash dedup table for GossipSub replay mitigation.
const MAX_GOSSIP_SEEN: usize = 8192;
/// Bounded sizes for long-lived driver metadata maps.
const MAX_DRIVER_PEERS_CACHE: usize = 8192;
const MAX_REASON_KEYS: usize = 128;
/// Genesis checks at once.
const MAX_CHECKING: usize = 4;
/// Relays this node holds a reservation on while it is not reachable.
const RESERVATIONS: usize = 2;
/// go-quai `C_workObjectRequestDegree`: peers asked at once, in up to two
/// rounds (`requestPeerRounds`).
pub const REQUEST_DEGREE: usize = 3;
/// A peer that answers another zone genesis: another Quai network (its DHT
/// protocol ID is the same).
const WRONG_NETWORK_BAN: Duration = Duration::from_secs(3600);
/// A peer that fails the genesis check in another way.
const CHECK_COOL_DOWN: Duration = Duration::from_secs(600);
/// Wait before a refused relay is asked again.
const RELAY_RETRY: Duration = Duration::from_secs(300);
/// Time AutoNAT gets before the node reserves relays without a verdict.
const NAT_GRACE: Duration = Duration::from_secs(45);
/// Least time between two FindProviders walks for one topic.
const FIND_INTERVAL: Duration = Duration::from_secs(60);
/// libp2p ping: the interval and the time-out (rust-libp2p's defaults).
const PING_INTERVAL: Duration = Duration::from_secs(15);
const PING_TIMEOUT: Duration = Duration::from_secs(20);

fn err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}
/// The error and its sources: libp2p prints only "Dial error" for a denied dial.
fn chain(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
fn read<T>(l: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|e| e.into_inner())
}
fn write<T>(l: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(|e| e.into_inner())
}
fn hash32(hex: &str) -> Hash {
    decode_hex(hex)
        .ok()
        .and_then(|v| v.try_into().ok())
        .unwrap_or_default()
}
/// The mainnet (colosseum) zone genesis hash.
pub fn colosseum_genesis() -> Hash {
    hash32(spec::COLOSSEUM_GENESIS_HASH)
}
/// The colosseum gossip topic prefix.
pub fn colosseum_topic_genesis() -> Hash {
    hash32(COLOSSEUM_TOPIC_GENESIS)
}
fn peer_of(a: &Multiaddr) -> Option<PeerId> {
    a.iter().find_map(|p| match p {
        Protocol::P2p(peer) => Some(peer),
        _ => None,
    })
}

/// What the node does. [`Config::read_only`] turns off everything that writes
/// to the network; [`Config::public`] keeps only `publish` off.
#[derive(Clone, Debug)]
pub struct Config {
    pub policy: DialPolicy,
    /// TCP listen addresses; empty: dial out only.
    pub listen: Vec<Multiaddr>,
    /// Relay client, DCUtR and AutoNAT.
    pub reachability: bool,
    /// Relays to reserve first (go-quai: the boot peers as static relays).
    pub relays: Vec<Multiaddr>,
    /// Time AutoNAT gets before the node reserves relays without a verdict.
    pub relay_after: Duration,
    /// Kademlia serves queries once AutoNAT confirms a public address;
    /// otherwise it stays a client.
    pub kad_server_when_reachable: bool,
    /// Periodic Kademlia walk (go-quai `RoutingTableRefreshPeriod`: 1 min).
    pub kad_refresh: Option<Duration>,
    /// Accept inbound `/quai/1.0.0` streams and answer them.
    pub serve: bool,
    /// Run go-quai's validators and forward what they accept. Without it,
    /// every message is ignored.
    pub validate: bool,
    /// Allow [`PublicNode::publish`].
    pub publish: bool,
    /// Ask each peer for the zone genesis before it takes a slot.
    pub genesis_check: bool,
    /// The zone genesis a peer must answer.
    pub genesis: Hash,
    /// Slice `[region, zone]` handled by this node (default `[0, 0]`).
    pub slice: [u8; 2],
    /// Provide the subscribed topic CIDs once the node is reachable, and
    /// find providers for topics with few peers.
    pub provide: bool,
    /// Offer circuit-relay v2 hop service to peers behind NAT (go-quai `EnableRelayService`).
    pub relay_server: bool,
    /// Enable GossipSub v1.1 peer scoring to throttle peers sending invalid messages.
    pub peer_scoring: bool,
    /// Peer slots: peers that passed the genesis check.
    pub max_peers: usize,
    /// Established connections, slots plus DHT contacts and checks.
    pub max_connections: u32,
    /// Connections per remote IP address.
    pub max_per_ip: usize,
    /// Validator threads.
    pub validators: usize,
    /// libp2p ping on every connection (go-libp2p hosts answer it): the
    /// time between pings, and the time-out after which a connection that
    /// does not answer is closed. A connection that a sleep or a network
    /// change killed sends no reset, and gossipsub keeps it open; go-libp2p
    /// closes such a yamux session after its keep-alive (30 s, then a 10 s
    /// write time-out).
    pub ping_interval: Duration,
    pub ping_timeout: Duration,
}
impl Config {
    /// The probe profile: dial out only, no answers, nothing forwarded.
    pub fn read_only(policy: DialPolicy) -> Self {
        Self {
            policy,
            listen: vec![],
            reachability: false,
            relays: vec![],
            relay_after: NAT_GRACE,
            kad_server_when_reachable: false,
            kad_refresh: None,
            serve: false,
            validate: false,
            publish: false,
            genesis_check: false,
            genesis: colosseum_genesis(),
            slice: [0, 0],
            provide: false,
            relay_server: false,
            peer_scoring: false,
            max_peers: (MAX_PEERS - MAX_PENDING_DIALS) as usize,
            max_connections: MAX_PEERS - MAX_PENDING_DIALS,
            max_per_ip: MAX_PEERS as usize,
            validators: 0,
            ping_interval: PING_INTERVAL,
            ping_timeout: PING_TIMEOUT,
        }
    }
    /// The public profile on TCP `port` (0: any free port).
    pub fn public(port: u16) -> Self {
        Self {
            policy: DialPolicy::Public,
            listen: vec![Multiaddr::empty()
                .with(Protocol::Ip4(Ipv4Addr::UNSPECIFIED))
                .with(Protocol::Tcp(port))],
            reachability: true,
            relays: COLOSSEUM_BOOTNODES
                .iter()
                .filter_map(|a| a.parse().ok())
                .collect(),
            relay_after: NAT_GRACE,
            kad_server_when_reachable: true,
            kad_refresh: Some(Duration::from_secs(60)),
            serve: true,
            validate: true,
            publish: false,
            genesis_check: true,
            genesis: colosseum_genesis(),
            slice: [0, 0],
            provide: true,
            relay_server: false,
            peer_scoring: true,
            max_peers: GO_MAX_PEERS,
            max_connections: GO_MAX_PEERS as u32 + 16,
            max_per_ip: 4,
            validators: 2,
            ping_interval: PING_INTERVAL,
            ping_timeout: PING_TIMEOUT,
        }
    }
    fn read_only_profile(&self) -> bool {
        !self.serve && !self.validate && self.listen.is_empty()
    }
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    gate: Gate,
    limits: connection_limits::Behaviour,
    streams: libp2p_stream::Behaviour,
    gossip: gossipsub::Behaviour,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
    kad: kad::Behaviour<kad::store::MemoryStore>,
    relay: Toggle<relay::client::Behaviour>,
    relay_server: Toggle<relay::Behaviour>,
    dcutr: Toggle<dcutr::Behaviour>,
    autonat: Toggle<autonat::Behaviour>,
}

/// Kademlia query progress as this node observes it.
#[derive(Debug)]
pub struct KadProgress {
    pub query: &'static str,
    pub key: Vec<u8>,
    pub peers: Vec<PeerId>,
    pub last: bool,
    pub requests: u32,
    pub successes: u32,
    pub failures: u32,
    pub error: Option<String>,
}

pub enum Event {
    Connected {
        peer: PeerId,
        address: Multiaddr,
    },
    Disconnected {
        peer: PeerId,
        cause: Option<String>,
    },
    DialFailed {
        peer: Option<PeerId>,
        error: String,
    },
    /// A connection did not answer a ping and was closed.
    DeadConnection {
        peer: PeerId,
        error: String,
    },
    Identified {
        peer: PeerId,
        info: Box<identify::Info>,
    },
    /// A remote announced a topic subscription.
    Subscribed {
        peer: PeerId,
        topic: String,
    },
    /// A gossip message and its verdict. Only `Accept` is forwarded.
    Gossip {
        peer: PeerId,
        source: Option<PeerId>,
        topic: String,
        data: Vec<u8>,
        verdict: Verdict,
    },
    Kad(KadProgress),
    /// The peer answered the zone genesis and took a slot.
    Verified {
        peer: PeerId,
    },
    Banned {
        peer: PeerId,
        reason: String,
    },
    /// AutoNAT status: "public <address>", "private" or "unknown".
    Nat(String),
    /// A relay reservation was accepted, or it failed.
    Reservation {
        relay: PeerId,
        result: Result<(), String>,
    },
    /// A peer reached this node through one of its relays.
    InboundCircuit {
        peer: PeerId,
    },
    HolePunch {
        peer: PeerId,
        result: Result<(), String>,
    },
    ListenAddr(Multiaddr),
    ExternalAddr(Multiaddr),
}

/// Gossip counts per topic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GossipCounts {
    pub received: u64,
    pub accepted: u64,
    pub ignored: u64,
    pub rejected: u64,
}
/// A snapshot of the node.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub connected: usize,
    /// Peers in a slot (they passed the genesis check).
    pub verified: usize,
    /// Peers connected only through a relay.
    pub relayed_peers: usize,
    pub nat: String,
    pub kad_mode: String,
    pub kad_routing: usize,
    pub listen_addrs: Vec<String>,
    pub external_addrs: Vec<String>,
    /// Relays that hold a reservation for this node now.
    pub reservations: Vec<String>,
    pub reservations_accepted: u64,
    pub reservations_failed: u64,
    /// Circuits that peers opened to this node through its relays.
    pub circuits_in: u64,
    /// Circuits this node opened through a relay.
    pub circuits_out: u64,
    /// DCUtR hole punches: succeeded, failed.
    pub hole_punches: (u64, u64),
    /// Topics this node provides in the DHT.
    pub provided: Vec<String>,
    pub provider_walks: u64,
    pub provider_dials: u64,
    /// Dials through a relay to peers that a Kademlia walk met with only
    /// circuit addresses.
    pub circuit_dials: u64,
    /// Topic label ("zone/blocks") to counts.
    pub gossip: BTreeMap<String, GossipCounts>,
    /// Verdict reasons ("reject: ...") to counts.
    pub reasons: BTreeMap<String, u64>,
    /// Genesis checks: passed, failed.
    pub genesis_checks: (u64, u64),
    /// Peer and reason of every ban and cool-down.
    pub bans: Vec<(String, String)>,
    /// Connections the gate refused, by reason (banned peer, per-IP limit).
    pub gate_refused: BTreeMap<String, u64>,
    /// Failed outbound dials, by cause.
    pub dial_failures: BTreeMap<String, u64>,
    /// Inbound requests: received, answered, streams closed by the rate limit.
    pub served: (u64, u64, u64),
    /// Head number per context, from accepted blocks.
    pub heads: [Option<u64>; 3],
    /// Connections closed because they did not answer a ping.
    pub dead_connections: u64,
}

/// Connected peer snapshot for `admin_peers`.
#[derive(Clone, Debug, Default)]
pub struct PeerSnapshot {
    pub id: String,
    pub agent: String,
    pub remote_addr: String,
    pub remote_ip: Option<String>,
    pub multiaddrs: Vec<String>,
    pub inbound: bool,
    pub outbound: bool,
    pub relayed: bool,
    pub verified: bool,
    pub quality: i32,
    pub latency_ms: Option<u64>,
    pub connected_secs: u64,
    pub protocols: Vec<String>,
}

/// Local P2P node info snapshot for `admin_nodeInfo`.
#[derive(Clone, Debug, Default)]
pub struct NodeInfoSnapshot {
    pub peer_id: String,
    pub listen_addrs: Vec<String>,
    pub external_addrs: Vec<String>,
    pub protocols: Vec<String>,
    pub topics: Vec<String>,
    pub nat: String,
    pub slice: [u8; 2],
    pub relay_server: bool,
}

type Reply<T> = oneshot::Sender<io::Result<T>>;
enum Command {
    Dial(Multiaddr, Reply<()>),
    KadAdd(PeerId, Multiaddr, Reply<()>),
    Subscribe(String, Reply<()>),
    ClosestPeers(Vec<u8>, Reply<()>),
    Providers(Vec<u8>, Reply<()>),
    Publish(String, Vec<u8>, Reply<()>),
    Ban(PeerId, String),
    Stats(oneshot::Sender<Stats>),
    PeersInfo(oneshot::Sender<Vec<PeerSnapshot>>),
    NodeInfo(oneshot::Sender<NodeInfoSnapshot>),
}

/// One gossip message for a validator thread.
struct Job {
    id: gossipsub::MessageId,
    peer: PeerId,
    source: Option<PeerId>,
    topic: String,
    data: Vec<u8>,
}
struct Done {
    job: Job,
    checked: Checked,
}

/// A short cause for a failed dial, for counting.
pub fn dial_failure_class(error: &str) -> &'static str {
    const CLASSES: [(&str, &str); 16] = [
        ("banned peer", "gate: banned peer"),
        ("per IP", "gate: per-IP limit"),
        ("connection limit", "connection limit"),
        (
            "Multiaddr is not supported",
            "address outside the dial policy",
        ),
        (
            "Unsupported resolved address",
            "address outside the dial policy",
        ),
        ("no addresses", "no addresses"),
        ("Connection refused", "connection refused"),
        ("timed out", "timeout"),
        ("Timeout", "timeout"),
        ("Unexpected peer ID", "wrong peer ID"),
        ("unreachable", "network unreachable"),
        ("No route to host", "network unreachable"),
        (
            "Failed to connect to destination",
            "relay: target not reachable",
        ),
        (
            "Response from behaviour was canceled",
            "relay: circuit canceled",
        ),
        ("Handshake failed", "handshake failed"),
        ("Address already in use", "port in use"),
    ];
    CLASSES
        .iter()
        .find(|(needle, _)| error.contains(needle))
        .map_or("other", |(_, class)| class)
}

/// "zone/blocks" for a go-quai topic name.
pub fn gossip_label(topic: &str) -> String {
    match gossip::parse_topic(topic) {
        Ok(t) => format!(
            "{}/{}",
            ["prime", "region", "zone"][t.context().min(2)],
            t.kind.suffix()
        ),
        Err(_) => "other".into(),
    }
}

fn kad_progress(
    query: &'static str,
    key: Vec<u8>,
    result: kad::QueryResult,
    stats: &kad::QueryStats,
    last: bool,
) -> Option<KadProgress> {
    let (peers, error) = match result {
        kad::QueryResult::GetClosestPeers(Ok(ok)) => {
            (ok.peers.into_iter().map(|p| p.peer_id).collect(), None)
        }
        kad::QueryResult::GetClosestPeers(Err(e)) => {
            let peers = match &e {
                kad::GetClosestPeersError::Timeout { peers, .. } => {
                    peers.iter().map(|p| p.peer_id).collect()
                }
            };
            (peers, Some(e.to_string()))
        }
        kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders {
            providers,
            ..
        })) => (providers.into_iter().collect(), None),
        kad::QueryResult::GetProviders(Ok(
            kad::GetProvidersOk::FinishedWithNoAdditionalRecord { .. },
        )) => (vec![], None),
        kad::QueryResult::GetProviders(Err(e)) => (vec![], Some(e.to_string())),
        _ => return None,
    };
    Some(KadProgress {
        query,
        key,
        peers,
        last,
        requests: stats.num_requests(),
        successes: stats.num_successes(),
        failures: stats.num_failures(),
        error,
    })
}

/// A dial that the circuit gate refused: every address it tried was a
/// circuit to a peer the node had not chosen (a contact that a Kademlia
/// walk met behind NAT). The circuit addresses, else None.
fn refused_circuits(
    peer: Option<PeerId>,
    error: &libp2p::swarm::DialError,
    allowed: &HashSet<PeerId>,
) -> Option<(PeerId, Vec<Multiaddr>)> {
    let peer = peer?;
    let libp2p::swarm::DialError::Transport(tried) = error else {
        return None;
    };
    let addresses: Vec<Multiaddr> = tried.iter().map(|(a, _)| a.clone()).collect();
    let refused = !addresses.is_empty()
        && !allowed.contains(&peer)
        && addresses
            .iter()
            .all(|a| address::circuit_target(a) == Some(peer));
    refused.then_some((peer, addresses))
}

/// State the API and the driver share.
struct Shared {
    /// What inbound requests are answered from ([`PublicNode::serve_from`]).
    store: RwLock<Arc<dyn serve::Store>>,
    connected: RwLock<BTreeSet<PeerId>>,
    /// Peers with an inbound and peers with an outbound connection.
    directions: RwLock<(BTreeSet<PeerId>, BTreeSet<PeerId>)>,
    verified: RwLock<BTreeSet<PeerId>>,
    book: Arc<Mutex<Book>>,
    heads: Arc<RwLock<Heads>>,
    served: Arc<serve::Served>,
}

pub struct PublicNode {
    pub id: PeerId,
    pub policy: DialPolicy,
    pub config: Config,
    shared: Arc<Shared>,
    commands: mpsc::Sender<Command>,
    control: tokio::sync::Mutex<libp2p_stream::Control>,
    slots: Arc<Semaphore>,
    events: tokio::sync::Mutex<mpsc::Receiver<Event>>,
    next_id: AtomicU32,
    driver: tokio::task::JoinHandle<()>,
}
/// The name the probe and exporter examples use.
pub type ReadOnlyPeer = PublicNode;
impl Drop for PublicNode {
    fn drop(&mut self) {
        // The validator threads stop when the driver drops the job queue.
        self.driver.abort();
    }
}

type BoxedTransport = Boxed<(PeerId, StreamMuxerBox)>;
type BuildError = Box<dyn std::error::Error + Send + Sync>;
/// Noise and Yamux over a raw transport, as go-quai's libp2p host runs them.
fn secure<T>(transport: T, key: &identity::Keypair) -> Result<BoxedTransport, BuildError>
where
    T: Transport + Send + Unpin + 'static,
    T::Output: futures::AsyncRead + futures::AsyncWrite + Unpin + Send + 'static,
    T::Error: Send + Sync + 'static,
    T::Dial: Send + 'static,
    T::ListenerUpgrade: Send + 'static,
{
    Ok(transport
        .upgrade(Version::V1Lazy)
        .authenticate(noise::Config::new(key)?)
        .multiplex(yamux::Config::default())
        .map(|(p, m), _| (p, StreamMuxerBox::new(m)))
        .boxed())
}
fn build_transport(
    key: &identity::Keypair,
    config: &Config,
    relay_transport: Option<relay::client::Transport>,
    circuits: Arc<Mutex<HashSet<PeerId>>>,
) -> Result<BoxedTransport, BuildError> {
    let tcp = PolicyTcp {
        inner: tcp::tokio::Transport::new(tcp::Config::default().nodelay(true)),
        policy: config.policy,
        listen: !config.listen.is_empty(),
    };
    // The relay client sits under the DNS layer. rust-libp2p's DNS transport
    // takes every address that has a DNS name, so a circuit through
    // `/dns4/<relay>` must resolve first and then reach the relay client. The
    // relay client dials the relay itself through the swarm, so that dial
    // passes the same policy. Circuits get Noise and Yamux end to end.
    let relay_transport = relay_transport.map(|inner| CircuitGate {
        inner,
        allowed: circuits,
    });
    match (config.policy, relay_transport) {
        (DialPolicy::Loopback, None) => secure(tcp, key),
        (DialPolicy::Loopback, Some(relay)) => secure(tcp.or_transport(relay), key),
        (DialPolicy::Public, None) => secure(dns::tokio::Transport::system(tcp)?, key),
        (DialPolicy::Public, Some(relay)) => {
            secure(dns::tokio::Transport::system(tcp.or_transport(relay))?, key)
        }
    }
}

impl PublicNode {
    /// The read-only probe profile with a fresh identity.
    pub fn start(policy: DialPolicy) -> io::Result<Self> {
        Self::launch(
            Config::read_only(policy),
            identity::Keypair::generate_ed25519(),
        )
    }
    /// The read-only probe profile. Use a fresh throwaway identity unless a
    /// caller needs a stable peer ID.
    pub fn start_with_identity(policy: DialPolicy, key: identity::Keypair) -> io::Result<Self> {
        Self::launch(Config::read_only(policy), key)
    }
    /// Start a node with `config`. Call it inside a Tokio runtime.
    pub fn launch(config: Config, key: identity::Keypair) -> io::Result<Self> {
        let id = key.public().to_peer_id();
        let book = Arc::new(Mutex::new(Book::default()));
        let circuits = Arc::new(Mutex::new(HashSet::new()));
        let gate = Gate::new(book.clone(), config.max_per_ip);
        let (relay_transport, relay_behaviour) = if config.reachability {
            let (t, b) = relay::client::new(id);
            (Some(t), Some(b))
        } else {
            (None, None)
        };
        let read_only = config.read_only_profile();
        let c = config.clone();
        let behaviour_gate = gate.clone();
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(key)
            .with_tokio()
            .with_other_transport(|key| build_transport(key, &c, relay_transport, circuits.clone()))
            .map_err(err)?
            .with_behaviour(|key| {
                let mut gossip = gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(key.clone()),
                    gossip::config(true).map_err(err)?,
                )
                .map_err(err)?;
                if c.peer_scoring {
                    let (params, thresholds) = gossip::peer_score_params();
                    gossip.with_peer_score(params, thresholds).map_err(err)?;
                }
                let local = key.public().to_peer_id();
                let mut cfg = kad::Config::new(StreamProtocol::new(KAD_PROTOCOL));
                cfg.set_query_timeout(Duration::from_secs(20));
                cfg.set_periodic_bootstrap_interval(c.kad_refresh);
                // Only `kad_add` and peers that passed the genesis check
                // enter the routing table.
                cfg.set_kbucket_inserts(kad::BucketInserts::Manual);
                let mut kad =
                    kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), cfg);
                // A server only after AutoNAT confirms a public address.
                kad.set_mode(Some(kad::Mode::Client));
                let limits = if read_only {
                    connection_limits::ConnectionLimits::default()
                        .with_max_pending_incoming(Some(0))
                        .with_max_established_incoming(Some(0))
                        .with_max_pending_outgoing(Some(MAX_PENDING_DIALS))
                        .with_max_established(Some(c.max_connections))
                        .with_max_established_per_peer(Some(1))
                } else {
                    connection_limits::ConnectionLimits::default()
                        .with_max_pending_incoming(Some(16))
                        .with_max_pending_outgoing(Some(16))
                        .with_max_established(Some(c.max_connections))
                        // A relayed and a direct connection while DCUtR upgrades.
                        .with_max_established_per_peer(Some(2))
                };
                let autonat = c.reachability.then(|| {
                    autonat::Behaviour::new(
                        local,
                        autonat::Config {
                            boot_delay: Duration::from_secs(10),
                            only_global_ips: c.policy == DialPolicy::Public,
                            ..Default::default()
                        },
                    )
                });
                let identify =
                    identify::Config::new(IDENTIFY_PROTOCOL_VERSION.into(), key.public())
                        .with_agent_version(format!(
                            "quai-rust/{}{}",
                            env!("CARGO_PKG_VERSION"),
                            if read_only { " read-only" } else { "" }
                        ))
                        // Announce confirmed external addresses only, not LAN ones.
                        .with_hide_listen_addrs(true);
                Ok(Behaviour {
                    gate: behaviour_gate,
                    limits: connection_limits::Behaviour::new(limits),
                    streams: libp2p_stream::Behaviour::new(),
                    gossip,
                    identify: identify::Behaviour::new(identify),
                    ping: ping::Behaviour::new(
                        ping::Config::new()
                            .with_interval(c.ping_interval)
                            .with_timeout(c.ping_timeout),
                    ),
                    kad,
                    relay: Toggle::from(relay_behaviour),
                    relay_server: Toggle::from(
                        c.relay_server
                            .then(|| relay::Behaviour::new(local, relay::Config::default())),
                    ),
                    dcutr: Toggle::from(c.reachability.then(|| dcutr::Behaviour::new(local))),
                    autonat: Toggle::from(autonat),
                })
            })
            .map_err(err)?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();
        for address in &config.listen {
            swarm.listen_on(address.clone()).map_err(err)?;
        }
        let mut control = swarm.behaviour().streams.new_control();
        // Without `accept`, inbound `/quai/1.0.0` streams fail negotiation.
        let incoming = if config.serve {
            Some(
                control
                    .accept(StreamProtocol::new(STREAM_PROTOCOL))
                    .map_err(err)?,
            )
        } else {
            None
        };
        let shared = Arc::new(Shared {
            store: RwLock::new(Arc::new(serve::NotFound)),
            connected: RwLock::new(BTreeSet::new()),
            directions: RwLock::new((BTreeSet::new(), BTreeSet::new())),
            verified: RwLock::new(BTreeSet::new()),
            book,
            heads: Arc::new(RwLock::new(Heads::default())),
            served: Arc::new(serve::Served::default()),
        });
        let (tx, commands) = mpsc::channel(64);
        let (events, receiver) = mpsc::channel(1024);
        let (done_tx, done_rx) = mpsc::channel::<Done>(1024);
        let (internal_tx, internal_rx) = mpsc::channel::<GenesisResult>(64);
        let jobs = if config.validate && config.validators > 0 {
            let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<Job>(512);
            let job_rx = Arc::new(Mutex::new(job_rx));
            let caches = Arc::new(SharedCaches::new(4).map_err(err)?);
            for i in 0..config.validators {
                let jobs = job_rx.clone();
                let done = done_tx.clone();
                let caches = caches.clone();
                let heads = shared.heads.clone();
                let slice = config.slice;
                std::thread::Builder::new()
                    .name(format!("gossip-validate-{i}"))
                    .spawn(move || {
                        let mut validator = Validator::with_slice(caches, heads, slice);
                        loop {
                            let job = lock(&jobs).recv();
                            let Ok(job) = job else { return };
                            let checked = validator.validate(&job.topic, &job.data, unix_now());
                            if done.blocking_send(Done { job, checked }).is_err() {
                                return;
                            }
                        }
                    })
                    .map_err(err)?;
            }
            Some(job_tx)
        } else {
            None
        };
        drop(done_tx);
        let driver = Driver {
            swarm,
            config: config.clone(),
            shared: shared.clone(),
            gate,
            events,
            jobs,
            control: control.clone(),
            checks: Arc::new(Semaphore::new(MAX_CHECKING)),
            checking: BTreeSet::new(),
            waiting: VecDeque::new(),
            genesis_ok: HashSet::new(),
            internal: internal_tx,
            peers: HashMap::new(),
            queries: HashMap::new(),
            provider_queries: HashMap::new(),
            providing: BTreeMap::new(),
            last_find: HashMap::new(),
            topics: BTreeSet::new(),
            relay_listeners: HashMap::new(),
            reservations: BTreeSet::new(),
            relay_retry: HashMap::new(),
            nat: "unknown".into(),
            kad_mode: "client".into(),
            started: Instant::now(),
            stats: Stats::default(),
            circuits,
            inbound_streams: Arc::new(Semaphore::new(MAX_INBOUND_STREAMS)),
            inbound_per_peer: Arc::new(Mutex::new(HashMap::new())),
            gossip_seen: HashMap::new(),
            gossip_seen_order: VecDeque::new(),
        };
        let driver = tokio::spawn(driver.run(commands, done_rx, internal_rx, incoming));
        Ok(Self {
            id,
            policy: config.policy,
            config,
            shared,
            commands: tx,
            control: tokio::sync::Mutex::new(control),
            slots: Arc::new(Semaphore::new(MAX_STREAMS)),
            events: tokio::sync::Mutex::new(receiver),
            next_id: AtomicU32::new(1),
            driver,
        })
    }
    async fn command<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> io::Result<T> {
        let (tx, rx) = oneshot::channel();
        self.commands.send(make(tx)).await.map_err(err)?;
        rx.await.map_err(err)?
    }
    /// Dial a target that the dial policy allows. A `/p2p/` suffix pins the
    /// expected peer ID.
    pub async fn dial(&self, address: Multiaddr) -> io::Result<()> {
        self.command(|tx| Command::Dial(address, tx)).await
    }
    /// Add a Kademlia server to the routing table.
    pub async fn kad_add(&self, peer: PeerId, address: Multiaddr) -> io::Result<()> {
        self.command(|tx| Command::KadAdd(peer, address, tx)).await
    }
    /// Subscribe to a topic. A public node provides it once reachable.
    pub async fn subscribe(&self, topic: String) -> io::Result<()> {
        self.command(|tx| Command::Subscribe(topic, tx)).await
    }
    /// Kademlia FIND_NODE walk; results arrive as [`Event::Kad`].
    pub async fn closest_peers(&self, key: Vec<u8>) -> io::Result<()> {
        self.command(|tx| Command::ClosestPeers(key, tx)).await
    }
    /// Kademlia GET_PROVIDERS walk; results arrive as [`Event::Kad`].
    pub async fn providers(&self, key: Vec<u8>) -> io::Result<()> {
        self.command(|tx| Command::Providers(key, tx)).await
    }
    /// Publish a gossip message. Refused unless [`Config::publish`] is set.
    pub async fn publish(&self, topic: String, data: Vec<u8>) -> io::Result<()> {
        if !self.config.publish {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "publishing is off (read-only by default)",
            ));
        }
        self.command(|tx| Command::Publish(topic, data, tx)).await
    }
    pub async fn stats(&self) -> io::Result<Stats> {
        let (tx, rx) = oneshot::channel();
        self.commands.send(Command::Stats(tx)).await.map_err(err)?;
        rx.await.map_err(err)
    }
    pub async fn peers_info(&self) -> io::Result<Vec<PeerSnapshot>> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::PeersInfo(tx))
            .await
            .map_err(err)?;
        rx.await.map_err(err)
    }
    pub async fn node_info(&self) -> io::Result<NodeInfoSnapshot> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::NodeInfo(tx))
            .await
            .map_err(err)?;
        rx.await.map_err(err)
    }
    pub fn is_connected(&self, peer: &PeerId) -> bool {
        read(&self.shared.connected).contains(peer)
    }
    /// Answer inbound requests from `store` from now on (validated data;
    /// the node starts with go-quai's "not found" answers).
    pub fn serve_from(&self, store: Arc<dyn serve::Store>) {
        *write(&self.shared.store) = store;
    }
    /// Anchor a context's gossip head to the node's validated canonical chain,
    /// preventing unauthenticated gossip blocks from poisoning [`Heads`].
    pub fn set_canonical_head(&self, ctx: usize, head: validate::Head) {
        write(&self.shared.heads).set_canonical(ctx, head);
    }
    /// Every peer with an open connection, in a slot or not.
    pub fn connected_peers(&self) -> Vec<PeerId> {
        read(&self.shared.connected).iter().copied().collect()
    }
    /// The peers with an open connection, and the peers with an inbound and
    /// with an outbound connection; one peer can count in both (go-quai
    /// `PeerCount` and `PeerCountByDirection`, p2p/node/stats.go).
    pub fn peer_counts(&self) -> (usize, usize, usize) {
        let d = read(&self.shared.directions);
        (read(&self.shared.connected).len(), d.0.len(), d.1.len())
    }
    /// Peers in a slot: they passed the genesis check (every connected peer
    /// when the check is off).
    pub fn verified_peers(&self) -> Vec<PeerId> {
        let set = if self.config.genesis_check {
            &self.shared.verified
        } else {
            &self.shared.connected
        };
        read(set).iter().copied().collect()
    }
    pub fn quality(&self, peer: &PeerId) -> i32 {
        lock(&self.shared.book).quality(peer)
    }
    /// Open an outbound `/quai/1.0.0` stream to a connected peer.
    pub async fn open(&self, peer: PeerId) -> io::Result<WireStream> {
        if !self.is_connected(&peer) {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "peer is not connected",
            ));
        }
        let permit = self.slots.clone().try_acquire_owned().map_err(err)?;
        let inner = tokio::time::timeout(IO_TIMEOUT, async {
            self.control
                .lock()
                .await
                .open_stream(peer, StreamProtocol::new(STREAM_PROTOCOL))
                .await
        })
        .await
        .map_err(err)?
        .map_err(err)?;
        Ok(WireStream::new(inner, permit))
    }
    pub async fn next(&self) -> io::Result<Event> {
        self.events
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::other("node stopped"))
    }
    /// Ask one peer and score its reply as go-quai `requestFromPeer` does
    /// ([`Outcome`]). An error means the request did not leave this node:
    /// rate limit, no connection, or no stream (go-quai: no score change).
    pub async fn request_from(
        &self,
        peer: PeerId,
        location: &[u8],
        query: Query,
        want: Want,
    ) -> io::Result<(Outcome, Option<rs::Response>)> {
        if !lock(&self.shared.book).outbound_request(peer, Instant::now()) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "request rate to this peer",
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = QuaiRequestMessage {
            id,
            location: Some(ProtoLocation {
                value: location.to_vec(),
            }),
            data: Some(match &query {
                Query::Number(n) => rq::Data::Number(number_bytes(*n)),
                Query::Hash(h) => rq::Data::Hash(ProtoHash { value: h.to_vec() }),
            }),
            request: Some(want.request()),
        };
        let bytes = QuaiMessage {
            payload: Some(Payload::Request(request)),
        }
        .encode_to_vec();
        let mut stream = self.open(peer).await?;
        stream.write(&bytes).await?;
        let frame = stream.read().await.map_err(|e| e.to_string());
        let _ = stream.close().await;
        let (outcome, response) = classify(frame, id, location, &query, want);
        {
            let mut book = lock(&self.shared.book);
            match &outcome {
                Outcome::Timeout(_) => {
                    book.adjust(peer, Adjustment::Timeout);
                }
                Outcome::Empty if matches!(query, Query::Hash(_)) => {
                    book.adjust(peer, Adjustment::Nack);
                }
                Outcome::Empty | Outcome::Ban(_) => {}
                Outcome::Valid => {
                    book.adjust(peer, Adjustment::Response);
                }
                Outcome::Bad(_) => {
                    book.adjust(peer, Adjustment::BadResponse);
                }
            }
        }
        if let Outcome::Ban(why) = &outcome {
            self.ban(peer, format!("invalid response: {why}")).await;
        }
        Ok((outcome, response))
    }
    /// Ask up to [`REQUEST_DEGREE`] peers in a slot at once, best quality
    /// first, in up to two rounds; the first valid answer wins.
    pub async fn request(
        &self,
        location: &[u8],
        query: Query,
        want: Want,
    ) -> io::Result<Option<(PeerId, rs::Response)>> {
        let candidates = lock(&self.shared.book).order(&self.verified_peers());
        for round in candidates.chunks(REQUEST_DEGREE).take(2) {
            let answers = futures::future::join_all(
                round
                    .iter()
                    .map(|p| self.request_from(*p, location, query.clone(), want)),
            )
            .await;
            for (peer, answer) in round.iter().zip(answers) {
                if let Ok((Outcome::Valid, Some(response))) = answer {
                    return Ok(Some((*peer, response)));
                }
            }
        }
        Ok(None)
    }
    /// go-quai `BanPeer`: refuse the peer until the process stops and close
    /// its connections.
    pub async fn ban(&self, peer: PeerId, reason: String) {
        let _ = self.commands.send(Command::Ban(peer, reason)).await;
    }
}

/// A request by number or by hash.
#[derive(Clone, Debug)]
pub enum Query {
    Number(u64),
    Hash(Hash),
}
/// The go-quai request types this node sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    Block,
    Header,
    Blocks,
    BlockHash,
}
impl Want {
    fn request(self) -> rq::Request {
        match self {
            Self::Block => rq::Request::WorkObjectBlock(Default::default()),
            Self::Header => rq::Request::WorkObjectHeader(Default::default()),
            Self::Blocks => rq::Request::WorkObjectBlocks(Default::default()),
            Self::BlockHash => rq::Request::BlockHash(ProtoHash::default()),
        }
    }
    fn matches(self, r: &rs::Response) -> bool {
        matches!(
            (self, r),
            (Self::Block, rs::Response::WorkObjectBlockView(_))
                | (Self::Header, rs::Response::WorkObjectHeaderView(_))
                | (Self::Blocks, rs::Response::WorkObjectBlocksView(_))
                | (Self::BlockHash, rs::Response::BlockHash(_))
        )
    }
}
/// go-quai encodes request numbers as big-endian bytes without leading zeros.
fn number_bytes(n: u64) -> Vec<u8> {
    let b = n.to_be_bytes();
    b[b.iter().position(|v| *v != 0).unwrap_or(8)..].to_vec()
}

/// What one reply means for the peer's score (go-quai
/// `p2p/node/p2p_services.go` `requestFromPeer`).
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// No correlated reply in time: -20 (`QualityAdjOnTimeout`). go-quai
    /// drops an envelope it cannot decode or correlate, so the request then
    /// times out.
    Timeout(String),
    /// "Not found": -10 for a request by hash (`QualityAdjOnNack`), no
    /// change for a request by number.
    Empty,
    /// The requested data: +10 (`QualityAdjOnResponse`).
    Valid,
    /// A reply that is not the requested data: -40
    /// (`QualityAdjOnBadResponse`). go-quai first adds +10, then drops it.
    Bad(String),
    /// A block-hash request answered with another type: go-quai `BanPeer`.
    Ban(String),
}

/// Score one reply frame to request `id`.
pub fn classify(
    frame: Result<Vec<u8>, String>,
    id: u32,
    location: &[u8],
    query: &Query,
    want: Want,
) -> (Outcome, Option<rs::Response>) {
    let reply = match frame.and_then(|f| {
        decode_envelope(&f)
            .and_then(response)
            .map_err(|e| e.to_string())
    }) {
        Ok(reply) => reply,
        Err(e) => return (Outcome::Timeout(e), None),
    };
    if reply.id != id {
        return (
            Outcome::Timeout(format!("reply to request {} instead of {id}", reply.id)),
            None,
        );
    }
    let Some(response) = reply.response else {
        return (Outcome::Timeout("reply without data".into()), None);
    };
    if reply.location.map(|l| l.value).unwrap_or_default() != location {
        return (Outcome::Bad("reply for another location".into()), None);
    }
    if !want.matches(&response) {
        let why = "reply of another type".to_string();
        return (
            if want == Want::BlockHash {
                Outcome::Ban(why)
            } else {
                Outcome::Bad(why)
            },
            None,
        );
    }
    let outcome = check_answer(&response, location, query);
    let keep = outcome == Outcome::Valid;
    (outcome, keep.then_some(response))
}

/// A block or header asked by hash must have that hash; one asked by number
/// must have that number in the request's context (`block.Number(nodeCtx)`).
fn check_answer(response: &rs::Response, location: &[u8], query: &Query) -> Outcome {
    let wo = match response {
        rs::Response::BlockHash(h) => {
            return if h.value.iter().all(|b| *b == 0) {
                Outcome::Empty
            } else if h.value.len() == 32 {
                Outcome::Valid
            } else {
                Outcome::Bad("block hash of the wrong length".into())
            };
        }
        rs::Response::WorkObjectBlocksView(v) if v.work_objects.is_empty() => {
            return Outcome::Empty
        }
        rs::Response::WorkObjectBlocksView(_) | rs::Response::AuxTemplate(_) => {
            return Outcome::Valid
        }
        rs::Response::WorkObjectBlockView(v) => v.work_object.as_ref(),
        rs::Response::WorkObjectHeaderView(v) => v.work_object.as_ref(),
    };
    let Some(wo) = wo else {
        return Outcome::Empty;
    };
    let decoded = (|| -> Result<(Hash, u64), String> {
        let object = WorkObject::decode_auxiliary(&wo.encode_to_vec(), PINNED_SCHEDULE)
            .map_err(|e| e.to_string())?;
        let header = object
            .auxiliary_work_header(PINNED_SCHEDULE)
            .map_err(|e| e.to_string())?;
        let hash = header.block_hash().map_err(|e| e.to_string())?;
        let number = if location.len() >= 2 {
            validate::number_of(header.wire().number.as_deref())?
        } else {
            validate::number_of(
                object
                    .header()
                    .wire()
                    .number
                    .get(location.len())
                    .map(Vec::as_slice),
            )?
        };
        Ok((hash, number))
    })();
    match (decoded, query) {
        (Err(e), _) => Outcome::Bad(format!("undecodable work object: {e}")),
        (Ok((hash, _)), Query::Hash(h)) if hash != *h => Outcome::Bad(format!(
            "got block {} for hash {}",
            encode_hex(&hash),
            encode_hex(h)
        )),
        (Ok((_, number)), Query::Number(n)) if number != *n => {
            Outcome::Bad(format!("got block number {number} for number {n}"))
        }
        _ => Outcome::Valid,
    }
}

/// The swarm task.
struct Driver {
    swarm: Swarm<Behaviour>,
    config: Config,
    shared: Arc<Shared>,
    gate: Gate,
    events: mpsc::Sender<Event>,
    jobs: Option<std::sync::mpsc::SyncSender<Job>>,
    control: libp2p_stream::Control,
    checks: Arc<Semaphore>,
    checking: BTreeSet<PeerId>,
    /// Connected peers that wait for a genesis check.
    waiting: VecDeque<PeerId>,
    /// Peers that answered the right genesis (the network of a peer ID does
    /// not change).
    genesis_ok: HashSet<PeerId>,
    internal: mpsc::Sender<GenesisResult>,
    peers: HashMap<PeerId, PeerInfo>,
    queries: HashMap<kad::QueryId, (&'static str, Vec<u8>)>,
    provider_queries: HashMap<kad::QueryId, String>,
    providing: BTreeMap<String, Provide>,
    last_find: HashMap<String, Instant>,
    topics: BTreeSet<String>,
    relay_listeners: HashMap<ListenerId, PeerId>,
    reservations: BTreeSet<PeerId>,
    relay_retry: HashMap<PeerId, Instant>,
    nat: String,
    kad_mode: String,
    started: Instant,
    stats: Stats,
    /// Peers this node may reach through a relay circuit.
    circuits: Arc<Mutex<HashSet<PeerId>>>,
    /// Global & per-peer concurrency bounds on inbound `/quai/1.0.0` streams.
    inbound_streams: Arc<Semaphore>,
    inbound_per_peer: Arc<Mutex<HashMap<PeerId, usize>>>,
    /// Content-addressed dedup cache (`keccak(keccak(topic) || keccak(data))`)
    /// to prevent GossipSub seqno-replay amplification and score farming.
    gossip_seen: HashMap<Hash, Instant>,
    gossip_seen_order: VecDeque<(Hash, Instant)>,
}
#[derive(Default)]
struct PeerInfo {
    /// Open connections and whether each runs through a relay.
    connections: HashMap<ConnectionId, bool>,
    /// The connections this node dialed.
    outbound: HashSet<ConnectionId>,
    /// A direct address this node dialed or accepted.
    dialed: Option<Multiaddr>,
    /// Last remote address observed on ConnectionEstablished.
    remote_addr: Option<Multiaddr>,
    /// Identify agent version.
    agent: String,
    /// Identify protocol list.
    protocols: Vec<String>,
    /// Identify listen addresses that the dial policy allows.
    listen: Vec<Multiaddr>,
    /// Last measured ping round-trip latency in milliseconds.
    rtt_ms: Option<u64>,
    /// Timestamp when the peer first established an active connection.
    connected_at: Option<Instant>,
    relay_hop: bool,
    kad: bool,
}
impl PeerInfo {
    /// At least one connection that does not run through a relay.
    fn direct(&self) -> bool {
        self.connections.values().any(|relayed| !relayed)
    }
}
enum Provide {
    Pending,
    Done,
    RetryAt(Instant),
}
/// The result of one genesis check.
struct GenesisResult {
    peer: PeerId,
    result: Result<(), String>,
}

async fn next_incoming(
    incoming: &mut Option<libp2p_stream::IncomingStreams>,
) -> Option<(PeerId, libp2p::Stream)> {
    match incoming {
        Some(s) => s.next().await,
        None => std::future::pending().await,
    }
}

impl Driver {
    fn emit(&mut self, event: Event) {
        let _ = self.events.try_send(event);
    }
    fn is_connected(&self, peer: &PeerId) -> bool {
        read(&self.shared.connected).contains(peer)
    }
    /// A peer in a slot (every peer when the genesis check is off).
    fn in_slot(&self, peer: &PeerId) -> bool {
        !self.config.genesis_check || read(&self.shared.verified).contains(peer)
    }
    fn slots_used(&self) -> usize {
        read(&self.shared.verified).len() + self.checking.len()
    }
    /// go-libp2p v0.32.1 opens GossipSub, DHT and go-quai streams only on
    /// direct connections (a relayed connection carries identify, ping and
    /// hole punching). A peer reached only through a relay therefore waits
    /// for DCUtR or a direct dial before its genesis check and slot.
    fn direct(&self, peer: &PeerId) -> bool {
        self.peers.get(peer).is_some_and(PeerInfo::direct)
    }
    /// Keep the peer's place in the inbound and outbound sets
    /// ([`PublicNode::peer_counts`]) equal to its open connections.
    fn note_directions(&self, peer: PeerId) {
        let (inbound, outbound) = self.peers.get(&peer).map_or((false, false), |i| {
            let out = i
                .connections
                .keys()
                .filter(|c| i.outbound.contains(c))
                .count();
            (i.connections.len() > out, out > 0)
        });
        let mut guard = write(&self.shared.directions);
        let (ins, outs) = &mut *guard;
        for (set, member) in [(ins, inbound), (outs, outbound)] {
            if member {
                set.insert(peer);
            } else {
                set.remove(&peer);
            }
        }
    }
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Command>,
        mut done: mpsc::Receiver<Done>,
        mut internal: mpsc::Receiver<GenesisResult>,
        mut incoming: Option<libp2p_stream::IncomingStreams>,
    ) {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => self.on_command(command),
                    None => break,
                },
                Some(d) = done.recv() => self.on_verdict(d),
                Some(c) = internal.recv() => self.on_checked(c),
                Some((peer, stream)) = next_incoming(&mut incoming) => self.on_stream(peer, stream),
                _ = tick.tick() => self.on_tick(),
                event = self.swarm.select_next_some() => self.on_swarm(event),
            }
        }
    }

    fn on_stream(&mut self, peer: PeerId, stream: libp2p::Stream) {
        if lock(&self.shared.book).is_banned(&peer, Instant::now()) {
            return;
        }
        let Ok(permit) = self.inbound_streams.clone().try_acquire_owned() else {
            return;
        };
        {
            let mut per_peer = lock(&self.inbound_per_peer);
            let count = per_peer.entry(peer).or_default();
            if *count >= MAX_INBOUND_PER_PEER {
                return;
            }
            *count += 1;
        }
        struct PeerStreamGuard {
            peer: PeerId,
            counts: Arc<Mutex<HashMap<PeerId, usize>>>,
            _permit: tokio::sync::OwnedSemaphorePermit,
        }
        impl Drop for PeerStreamGuard {
            fn drop(&mut self) {
                let mut counts = lock(&self.counts);
                if let Some(n) = counts.get_mut(&self.peer) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        counts.remove(&self.peer);
                    }
                }
            }
        }
        let guard = PeerStreamGuard {
            peer,
            counts: self.inbound_per_peer.clone(),
            _permit: permit,
        };
        let store = read(&self.shared.store).clone();
        let book = self.shared.book.clone();
        let served = self.shared.served.clone();
        tokio::spawn(async move {
            let _guard = guard;
            serve::serve_stream(stream, peer, store, book, served).await;
        });
    }

    fn on_command(&mut self, command: Command) {
        let policy = self.config.policy;
        match command {
            Command::Dial(address, reply) => {
                if let Some(target) = address::circuit_target(&address) {
                    let mut circuits = lock(&self.circuits);
                    if circuits.len() >= MAX_DRIVER_PEERS_CACHE && !circuits.contains(&target) {
                        if let Some(evict) = circuits.iter().next().copied() {
                            circuits.remove(&evict);
                        }
                    }
                    circuits.insert(target);
                }
                let result = if !policy.allows_target(&address) {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "dial address is outside the dial policy",
                    ))
                } else if let (false, Some(peer)) =
                    (address::is_circuit(&address), peer_of(&address))
                {
                    let bare: Multiaddr = address
                        .iter()
                        .filter(|p| !matches!(p, Protocol::P2p(_)))
                        .collect();
                    self.swarm
                        .dial(DialOpts::peer_id(peer).addresses(vec![bare]).build())
                        .map_err(|e| io::Error::other(chain(&e)))
                } else {
                    self.swarm
                        .dial(address)
                        .map_err(|e| io::Error::other(chain(&e)))
                };
                let _ = reply.send(result);
            }
            Command::KadAdd(peer, address, reply) => {
                let result = if !policy.allows_target(&address) {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "Kademlia address is outside the dial policy",
                    ))
                } else {
                    match self.swarm.behaviour_mut().kad.add_address(&peer, address) {
                        kad::RoutingUpdate::Failed => {
                            Err(io::Error::other("Kademlia routing table refused the peer"))
                        }
                        _ => Ok(()),
                    }
                };
                let _ = reply.send(result);
            }
            Command::Subscribe(topic, reply) => {
                let ident = gossipsub::IdentTopic::new(topic.clone());
                let result = self
                    .swarm
                    .behaviour_mut()
                    .gossip
                    .subscribe(&ident)
                    .map(|_| ())
                    .map_err(err);
                if result.is_ok() {
                    if self.config.peer_scoring {
                        let _ = self
                            .swarm
                            .behaviour_mut()
                            .gossip
                            .set_topic_params(ident, gossip::topic_score_params());
                    }
                    self.topics.insert(topic);
                }
                let _ = reply.send(result);
            }
            Command::ClosestPeers(key, reply) => {
                let query = self
                    .swarm
                    .behaviour_mut()
                    .kad
                    .get_closest_peers(key.clone());
                self.queries.insert(query, ("closest_peers", key));
                let _ = reply.send(Ok(()));
            }
            Command::Providers(key, reply) => {
                let query = self
                    .swarm
                    .behaviour_mut()
                    .kad
                    .get_providers(kad::RecordKey::new(&key));
                self.queries.insert(query, ("providers", key));
                let _ = reply.send(Ok(()));
            }
            Command::Publish(topic, data, reply) => {
                let result = if self.config.publish {
                    self.swarm
                        .behaviour_mut()
                        .gossip
                        .publish(gossipsub::IdentTopic::new(topic), data)
                        .map(|_| ())
                        .map_err(err)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "publishing is off",
                    ))
                };
                let _ = reply.send(result);
            }
            Command::Ban(peer, reason) => self.ban(peer, reason, None),
            Command::Stats(reply) => {
                let stats = self.snapshot();
                let _ = reply.send(stats);
            }
            Command::PeersInfo(reply) => {
                let peers = self.peers_snapshot();
                let _ = reply.send(peers);
            }
            Command::NodeInfo(reply) => {
                let info = self.node_info_snapshot();
                let _ = reply.send(info);
            }
        }
    }

    fn peers_snapshot(&self) -> Vec<PeerSnapshot> {
        let verified = read(&self.shared.verified).clone();
        let book = lock(&self.shared.book);
        let now = Instant::now();
        let extract_ip = |addr: &Multiaddr| -> Option<String> {
            for proto in addr.iter() {
                match proto {
                    libp2p::multiaddr::Protocol::Ip4(ip) => return Some(ip.to_string()),
                    libp2p::multiaddr::Protocol::Ip6(ip) => return Some(ip.to_string()),
                    _ => {}
                }
            }
            None
        };
        let mut out = Vec::with_capacity(self.peers.len());
        for (peer_id, info) in &self.peers {
            let outbound = !info.outbound.is_empty();
            let inbound = info.connections.len() > info.outbound.len();
            let relayed = !info.connections.is_empty() && info.connections.values().all(|r| *r);
            let mut addrs: Vec<String> = Vec::new();
            let mut remote_ip: Option<String> = None;
            for candidate in info
                .remote_addr
                .iter()
                .chain(info.dialed.iter())
                .chain(info.listen.iter())
            {
                let s = candidate.to_string();
                if !addrs.contains(&s) {
                    addrs.push(s);
                }
                if remote_ip.is_none() {
                    remote_ip = extract_ip(candidate);
                }
            }
            let connected_secs = info
                .connected_at
                .map(|t| now.saturating_duration_since(t).as_secs())
                .unwrap_or(0);
            out.push(PeerSnapshot {
                id: peer_id.to_string(),
                agent: info.agent.clone(),
                remote_addr: info
                    .remote_addr
                    .as_ref()
                    .or(info.dialed.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                remote_ip,
                multiaddrs: addrs,
                inbound,
                outbound,
                relayed,
                verified: verified.contains(peer_id),
                quality: book.quality(peer_id),
                latency_ms: info.rtt_ms,
                connected_secs,
                protocols: info.protocols.clone(),
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    fn node_info_snapshot(&self) -> NodeInfoSnapshot {
        let mut protocols = vec![
            IDENTIFY_PROTOCOL_VERSION.to_string(),
            KAD_PROTOCOL.to_string(),
            "/meshsub/1.1.0".to_string(),
        ];
        if self.config.relay_server {
            protocols.push(RELAY_HOP_PROTOCOL.to_string());
        }
        NodeInfoSnapshot {
            peer_id: self.swarm.local_peer_id().to_string(),
            listen_addrs: self.swarm.listeners().map(ToString::to_string).collect(),
            external_addrs: self
                .swarm
                .external_addresses()
                .map(ToString::to_string)
                .collect(),
            protocols,
            topics: self.topics.iter().cloned().collect(),
            nat: self.nat.clone(),
            slice: self.config.slice,
            relay_server: self.config.relay_server,
        }
    }

    /// Ban (`None`: until the process stops) and disconnect.
    fn ban(&mut self, peer: PeerId, reason: String, duration: Option<Duration>) {
        lock(&self.shared.book).ban(peer, reason.clone(), duration);
        write(&self.shared.verified).remove(&peer);
        let _ = self.swarm.disconnect_peer_id(peer);
        self.emit(Event::Banned { peer, reason });
    }

    fn snapshot(&mut self) -> Stats {
        let mut s = self.stats.clone();
        s.connected = read(&self.shared.connected).len();
        s.verified = read(&self.shared.verified).len();
        s.relayed_peers = self
            .peers
            .values()
            .filter(|p| !p.connections.is_empty() && p.connections.values().all(|r| *r))
            .count();
        s.nat = self.nat.clone();
        s.kad_mode = self.kad_mode.clone();
        s.kad_routing = self
            .swarm
            .behaviour_mut()
            .kad
            .kbuckets()
            .map(|b| b.num_entries())
            .sum();
        s.listen_addrs = self.swarm.listeners().map(|a| a.to_string()).collect();
        s.external_addrs = self
            .swarm
            .external_addresses()
            .map(|a| a.to_string())
            .collect();
        s.reservations = self.reservations.iter().map(|p| p.to_string()).collect();
        s.provided = self
            .providing
            .iter()
            .filter(|(_, p)| matches!(p, Provide::Done))
            .map(|(t, _)| gossip_label(t))
            .collect();
        s.bans = lock(&self.shared.book)
            .bans()
            .into_iter()
            .map(|(p, b)| (p.to_string(), b.reason))
            .collect();
        s.gate_refused = self
            .gate
            .refused_by()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let served = &self.shared.served;
        s.served = (
            served.requests.load(Ordering::Relaxed),
            served.answered.load(Ordering::Relaxed),
            served.rate_limited.load(Ordering::Relaxed),
        );
        let heads = read(&self.shared.heads);
        s.heads = [0, 1, 2].map(|c| heads.get(c).map(|h| h.number));
        s
    }

    fn on_verdict(&mut self, d: Done) {
        let Done { job, checked } = d;
        let acceptance = match &checked.verdict {
            Verdict::Accept => gossipsub::MessageAcceptance::Accept,
            Verdict::Ignore(_) => gossipsub::MessageAcceptance::Ignore,
            Verdict::Reject(_) => gossipsub::MessageAcceptance::Reject,
        };
        let _ = self
            .swarm
            .behaviour_mut()
            .gossip
            .report_message_validation_result(&job.id, &job.peer, acceptance);
        self.count(&job.topic, &checked.verdict);
        if checked.verdict == Verdict::Accept {
            // quai/p2p_backend.go OnNewBroadcast: QualityAdjOnBroadcast.
            lock(&self.shared.book).adjust(job.peer, Adjustment::Broadcast);
        }
        self.emit(Event::Gossip {
            peer: job.peer,
            source: job.source,
            topic: job.topic,
            data: job.data,
            verdict: checked.verdict,
        });
    }
    fn count(&mut self, topic: &str, verdict: &Verdict) {
        let c = self.stats.gossip.entry(gossip_label(topic)).or_default();
        c.received += 1;
        match verdict {
            Verdict::Accept => c.accepted += 1,
            Verdict::Ignore(_) => c.ignored += 1,
            Verdict::Reject(_) => c.rejected += 1,
        }
        if let Some(reason) = verdict.reason() {
            let key = format!("{}: {reason}", verdict.label());
            let bucket = if self.stats.reasons.contains_key(&key)
                || self.stats.reasons.len() < MAX_REASON_KEYS
            {
                key
            } else {
                "other".into()
            };
            *self.stats.reasons.entry(bucket).or_default() += 1;
        }
    }

    fn note_gossip_payload(&mut self, topic: &str, data: &[u8]) -> bool {
        let now = Instant::now();
        while let Some((h, at)) = self.gossip_seen_order.front().copied() {
            if now.saturating_duration_since(at) >= gossip::SEEN_TTL {
                self.gossip_seen_order.pop_front();
                if self.gossip_seen.get(&h).copied() == Some(at) {
                    self.gossip_seen.remove(&h);
                }
            } else {
                break;
            }
        }
        let mut combined = [0u8; 64];
        combined[..32].copy_from_slice(&quai_primitives::transaction::keccak(topic.as_bytes()));
        combined[32..].copy_from_slice(&quai_primitives::transaction::keccak(data));
        let digest = quai_primitives::transaction::keccak(&combined);
        if self.gossip_seen.contains_key(&digest) {
            return false;
        }
        while self.gossip_seen.len() >= MAX_GOSSIP_SEEN {
            let Some((h, at)) = self.gossip_seen_order.pop_front() else {
                break;
            };
            if self.gossip_seen.get(&h).copied() == Some(at) {
                self.gossip_seen.remove(&h);
            }
        }
        self.gossip_seen.insert(digest, now);
        self.gossip_seen_order.push_back((digest, now));
        true
    }

    fn on_gossip(&mut self, peer: PeerId, id: gossipsub::MessageId, message: gossipsub::Message) {
        let topic = message.topic.into_string();
        let ignore = if !self.config.validate {
            Some("read-only profile")
        } else if !self.in_slot(&peer) && !self.direct(&peer) {
            Some("peer is connected only through a relay")
        } else if !self.in_slot(&peer) {
            Some("peer has not passed the genesis check")
        } else if !self.note_gossip_payload(&topic, &message.data) {
            Some("duplicate gossip payload")
        } else {
            None
        };
        let job = Job {
            id: id.clone(),
            peer,
            source: message.source,
            topic,
            data: message.data,
        };
        let (job, why) = match (ignore, &self.jobs) {
            (None, Some(jobs)) => match jobs.try_send(job) {
                Ok(()) => return,
                Err(
                    std::sync::mpsc::TrySendError::Full(job)
                    | std::sync::mpsc::TrySendError::Disconnected(job),
                ) => (job, "validation queue full"),
            },
            (why, _) => (job, why.unwrap_or("no validator")),
        };
        let _ = self
            .swarm
            .behaviour_mut()
            .gossip
            .report_message_validation_result(&id, &peer, gossipsub::MessageAcceptance::Ignore);
        let verdict = Verdict::Ignore(why.into());
        if self.config.validate {
            self.count(&job.topic, &verdict);
        }
        self.emit(Event::Gossip {
            peer,
            source: job.source,
            topic: job.topic,
            data: job.data,
            verdict,
        });
    }

    /// A new direct connection: the peer waits for a genesis check unless it
    /// passed one.
    fn on_new_peer(&mut self, peer: PeerId) {
        if !self.config.genesis_check || !self.direct(&peer) {
            return;
        }
        if self.genesis_ok.contains(&peer) {
            self.admit(peer);
        } else if !self.checking.contains(&peer) && !self.waiting.contains(&peer) {
            self.waiting.push_back(peer);
        }
        self.fill_slots();
    }
    /// Give free slots to checked peers, then start checks for waiting peers.
    fn fill_slots(&mut self) {
        if !self.config.genesis_check {
            return;
        }
        let ready: Vec<PeerId> = read(&self.shared.connected)
            .iter()
            .filter(|p| self.genesis_ok.contains(p) && self.direct(p))
            .copied()
            .collect();
        for peer in ready {
            if self.slots_used() >= self.config.max_peers {
                break;
            }
            self.admit(peer);
        }
        while self.slots_used() < self.config.max_peers {
            let Some(peer) = self.waiting.pop_front() else {
                break;
            };
            if !self.direct(&peer) || self.in_slot(&peer) || self.checking.contains(&peer) {
                continue;
            }
            let Ok(permit) = self.checks.clone().try_acquire_owned() else {
                self.waiting.push_front(peer);
                break;
            };
            self.start_check(peer, permit);
        }
    }
    fn admit(&mut self, peer: PeerId) {
        if self.in_slot(&peer) || !self.direct(&peer) || self.slots_used() >= self.config.max_peers
        {
            return;
        }
        write(&self.shared.verified).insert(peer);
        self.route(peer);
        self.emit(Event::Verified { peer });
    }
    /// A Kademlia server in a slot with a direct public address enters the
    /// routing table, so DHT records never lead to LAN dials.
    fn route(&mut self, peer: PeerId) {
        if !self.config.genesis_check || !self.in_slot(&peer) {
            return;
        }
        let policy = self.config.policy;
        let Some(info) = self.peers.get(&peer) else {
            return;
        };
        if !info.kad {
            return;
        }
        let address = info
            .dialed
            .iter()
            .chain(info.listen.iter())
            .find(|a| policy.allows(a))
            .cloned();
        if let Some(address) = address {
            self.swarm.behaviour_mut().kad.add_address(&peer, address);
        }
    }
    /// Ask the peer for the zone genesis (block hash by number 0 at the zone
    /// location), as go-quai answers it (`handleBlockNumberRequest`).
    fn start_check(&mut self, peer: PeerId, permit: tokio::sync::OwnedSemaphorePermit) {
        self.checking.insert(peer);
        let mut control = self.control.clone();
        let internal = self.internal.clone();
        let genesis = self.config.genesis;
        let slice = self.config.slice.to_vec();
        tokio::spawn(async move {
            let result = async {
                let inner = tokio::time::timeout(
                    IO_TIMEOUT,
                    control.open_stream(peer, StreamProtocol::new(STREAM_PROTOCOL)),
                )
                .await
                .map_err(|e| format!("genesis check: {e}"))?
                .map_err(|e| format!("genesis check: {e}"))?;
                let mut stream = WireStream::new(inner, permit);
                let mut book = RequestBook::new(1).map_err(|e| e.to_string())?;
                let reply = exchange(
                    &mut stream,
                    &mut book,
                    QuaiRequestMessage {
                        id: 1,
                        location: Some(ProtoLocation { value: slice }),
                        data: Some(rq::Data::Number(vec![])),
                        request: Some(rq::Request::BlockHash(ProtoHash::default())),
                    },
                )
                .await
                .map_err(|e| format!("genesis check: {e}"))?;
                let _ = stream.close().await;
                match reply.response {
                    Some(rs::Response::BlockHash(h)) if h.value == genesis => Ok(()),
                    Some(rs::Response::BlockHash(h)) if h.value.iter().all(|b| *b == 0) => {
                        Err("genesis check: no zone genesis".to_string())
                    }
                    Some(rs::Response::BlockHash(h)) => Err(format!(
                        "zone genesis {} is not mainnet",
                        encode_hex(&h.value)
                    )),
                    _ => Err("genesis check: wrong reply type".to_string()),
                }
            }
            .await;
            let _ = internal.send(GenesisResult { peer, result }).await;
        });
    }
    fn on_checked(&mut self, c: GenesisResult) {
        let GenesisResult { peer, result } = c;
        self.checking.remove(&peer);
        match result {
            Ok(()) => {
                self.stats.genesis_checks.0 += 1;
                if self.genesis_ok.len() >= MAX_DRIVER_PEERS_CACHE
                    && !self.genesis_ok.contains(&peer)
                {
                    if let Some(evict) = self.genesis_ok.iter().next().copied() {
                        self.genesis_ok.remove(&evict);
                    }
                }
                self.genesis_ok.insert(peer);
                if self.is_connected(&peer) {
                    self.admit(peer);
                }
            }
            Err(_) if self.relay_listeners.values().any(|r| *r == peer) => {
                // The relay that holds this node's reservation gets no slot
                // but stays connected.
                self.stats.genesis_checks.1 += 1;
            }
            Err(reason) => {
                self.stats.genesis_checks.1 += 1;
                let duration = if reason.contains("is not mainnet") {
                    WRONG_NETWORK_BAN
                } else {
                    CHECK_COOL_DOWN
                };
                self.ban(peer, reason, Some(duration));
            }
        }
        self.fill_slots();
    }

    fn record_relay_retry(&mut self, relay: PeerId, until: Instant) {
        let now = Instant::now();
        if self.relay_retry.len() >= MAX_DRIVER_PEERS_CACHE
            && !self.relay_retry.contains_key(&relay)
        {
            self.relay_retry.retain(|_, t| *t > now);
            if self.relay_retry.len() >= MAX_DRIVER_PEERS_CACHE {
                if let Some(evict) = self.relay_retry.keys().next().copied() {
                    self.relay_retry.remove(&evict);
                }
            }
        }
        self.relay_retry.insert(relay, until);
    }

    fn on_tick(&mut self) {
        self.fill_slots();
        self.reserve_relays();
        self.provide_and_find();
    }

    /// Relay reservations while AutoNAT finds the node private, or has no
    /// verdict after [`Config::relay_after`]. Static relays first (go-quai: the boot
    /// peers), then peers in a slot that offer the hop protocol.
    fn reserve_relays(&mut self) {
        if !self.config.reachability || self.nat.starts_with("public") {
            return;
        }
        if self.nat != "private" && self.started.elapsed() < self.config.relay_after {
            return;
        }
        let active = self.relay_listeners.len();
        if active >= RESERVATIONS {
            return;
        }
        let now = Instant::now();
        let busy: BTreeSet<PeerId> = self.relay_listeners.values().copied().collect();
        let usable = |peer: &PeerId| {
            !busy.contains(peer)
                && self.relay_retry.get(peer).is_none_or(|t| *t <= now)
                && !lock(&self.shared.book).is_banned(peer, now)
        };
        let mut candidates: Vec<(PeerId, Multiaddr)> = self
            .config
            .relays
            .iter()
            .filter_map(|a| peer_of(a).map(|p| (p, a.clone())))
            .filter(|(p, _)| usable(p))
            .collect();
        for (peer, info) in &self.peers {
            if info.relay_hop && self.in_slot(peer) && usable(peer) {
                if let Some(d) = &info.dialed {
                    if let Ok(a) = d.clone().with_p2p(*peer) {
                        candidates.push((*peer, a));
                    }
                }
            }
        }
        let mut chosen = BTreeSet::new();
        for (relay, address) in candidates {
            if chosen.len() + active >= RESERVATIONS {
                break;
            }
            if !chosen.insert(relay) {
                continue;
            }
            match self.swarm.listen_on(address.with(Protocol::P2pCircuit)) {
                Ok(id) => {
                    self.relay_listeners.insert(id, relay);
                }
                Err(_) => {
                    self.record_relay_retry(relay, now + RELAY_RETRY);
                }
            }
        }
    }

    /// go-quai `Provide` on subscribe (here once reachable, directly or
    /// through a relay) and `queryDHT` (FindProviders) for a topic with fewer
    /// than Dlo peers.
    fn provide_and_find(&mut self) {
        if !self.config.provide {
            return;
        }
        let now = Instant::now();
        let reachable = self.nat.starts_with("public") || !self.reservations.is_empty();
        let topics: Vec<String> = self.topics.iter().cloned().collect();
        for topic in topics {
            let key = kad::RecordKey::new(&topic_provider_key(&topic));
            let due = match self.providing.get(&topic) {
                None => true,
                Some(Provide::RetryAt(t)) => *t <= now,
                Some(_) => false,
            };
            if reachable && due {
                if let Ok(query) = self.swarm.behaviour_mut().kad.start_providing(key.clone()) {
                    self.providing.insert(topic.clone(), Provide::Pending);
                    self.provider_queries.insert(query, topic.clone());
                }
            }
            let hash = gossipsub::TopicHash::from_raw(topic.clone());
            let peers = self
                .swarm
                .behaviour()
                .gossip
                .all_peers()
                .filter(|(_, t)| t.contains(&&hash))
                .count();
            let recent = self
                .last_find
                .get(&topic)
                .is_some_and(|t| now.duration_since(*t) < FIND_INTERVAL);
            if peers < gossip::MESH_N_LOW && self.slots_used() < self.config.max_peers && !recent {
                let query = self.swarm.behaviour_mut().kad.get_providers(key);
                self.provider_queries.insert(query, topic.clone());
                self.last_find.insert(topic, now);
                self.stats.provider_walks += 1;
            }
        }
    }

    fn on_swarm(&mut self, event: SwarmEvent<BehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => self.emit(Event::ListenAddr(address)),
            SwarmEvent::ListenerClosed {
                listener_id,
                reason,
                ..
            } => {
                if let Some(relay) = self.relay_listeners.remove(&listener_id) {
                    let accepted = self.reservations.remove(&relay);
                    if !accepted {
                        self.stats.reservations_failed += 1;
                    }
                    self.record_relay_retry(relay, Instant::now() + RELAY_RETRY);
                    let why = match reason {
                        Ok(()) => "reservation closed".to_string(),
                        Err(e) => chain(&e),
                    };
                    self.emit(Event::Reservation {
                        relay,
                        result: Err(why),
                    });
                }
            }
            SwarmEvent::ExternalAddrConfirmed { address } => {
                self.emit(Event::ExternalAddr(address));
            }
            SwarmEvent::ConnectionEstablished {
                peer_id,
                connection_id,
                endpoint,
                num_established,
                ..
            } => {
                let address = endpoint.get_remote_address().clone();
                let relayed = endpoint.is_relayed();
                write(&self.shared.connected).insert(peer_id);
                let info = self.peers.entry(peer_id).or_default();
                info.connections.insert(connection_id, relayed);
                info.remote_addr = Some(address.clone());
                if info.connected_at.is_none() {
                    info.connected_at = Some(Instant::now());
                }
                if endpoint.is_dialer() {
                    info.outbound.insert(connection_id);
                }
                if endpoint.is_dialer() && !relayed {
                    info.dialed = Some(address.clone());
                }
                self.note_directions(peer_id);
                if num_established.get() == 1 {
                    self.emit(Event::Connected {
                        peer: peer_id,
                        address,
                    });
                }
                // The first connection, or a direct one after a relayed one
                // (DCUtR), lets the peer queue for its genesis check.
                if !relayed || num_established.get() == 1 {
                    self.on_new_peer(peer_id);
                }
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                connection_id,
                cause,
                num_established,
                ..
            } => {
                if let Some(info) = self.peers.get_mut(&peer_id) {
                    info.connections.remove(&connection_id);
                    info.outbound.remove(&connection_id);
                }
                if num_established > 0 && !self.direct(&peer_id) {
                    // Only relayed connections are left: the slot is free.
                    if write(&self.shared.verified).remove(&peer_id) {
                        self.fill_slots();
                    }
                }
                if num_established == 0 {
                    write(&self.shared.connected).remove(&peer_id);
                    write(&self.shared.verified).remove(&peer_id);
                    self.peers.remove(&peer_id);
                    self.waiting.retain(|p| *p != peer_id);
                    self.note_directions(peer_id);
                    self.emit(Event::Disconnected {
                        peer: peer_id,
                        cause: cause.map(|c| chain(&c)),
                    });
                    self.fill_slots();
                }
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                let refused = refused_circuits(peer_id, &error, &lock(&self.circuits));
                if let Some(contact) = refused {
                    self.dial_circuits(vec![contact]);
                }
                let error = chain(&error);
                *self
                    .stats
                    .dial_failures
                    .entry(dial_failure_class(&error).into())
                    .or_default() += 1;
                self.emit(Event::DialFailed {
                    peer: peer_id,
                    error,
                });
            }
            SwarmEvent::Behaviour(BehaviourEvent::Ping(ping::Event {
                peer,
                result: Ok(rtt),
                ..
            })) => {
                if let Some(info) = self.peers.get_mut(&peer) {
                    info.rtt_ms = Some(rtt.as_millis() as u64);
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Ping(ping::Event {
                peer,
                connection,
                result: Err(failure),
            })) => {
                // A peer without the protocol is alive; any other failure
                // (a time-out, a broken stream) means the connection is dead.
                if !matches!(failure, ping::Failure::Unsupported) {
                    self.stats.dead_connections += 1;
                    self.emit(Event::DeadConnection {
                        peer,
                        error: chain(&failure),
                    });
                    self.swarm.close_connection(connection);
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) => {
                let policy = self.config.policy;
                let entry = self.peers.entry(peer_id).or_default();
                entry.agent = info.agent_version.clone();
                entry.protocols = info.protocols.iter().map(|p| p.to_string()).collect();
                entry.relay_hop = info
                    .protocols
                    .iter()
                    .any(|p| p.as_ref() == RELAY_HOP_PROTOCOL);
                entry.kad = info.protocols.iter().any(|p| p.as_ref() == KAD_PROTOCOL);
                entry.listen = info
                    .listen_addrs
                    .iter()
                    .filter(|a| policy.allows(a))
                    .cloned()
                    .collect();
                self.route(peer_id);
                self.emit(Event::Identified {
                    peer: peer_id,
                    info: Box::new(info),
                });
            }
            SwarmEvent::Behaviour(BehaviourEvent::Gossip(gossipsub::Event::Subscribed {
                peer_id,
                topic,
            })) => {
                self.emit(Event::Subscribed {
                    peer: peer_id,
                    topic: topic.into_string(),
                });
            }
            SwarmEvent::Behaviour(BehaviourEvent::Gossip(gossipsub::Event::Message {
                propagation_source,
                message_id,
                message,
            })) => self.on_gossip(propagation_source, message_id, message),
            SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::ModeChanged { new_mode })) => {
                self.kad_mode = new_mode.to_string();
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::OutboundQueryProgressed {
                id,
                result,
                stats,
                step,
            })) => self.on_kad(id, result, stats, step.last),
            SwarmEvent::Behaviour(BehaviourEvent::Autonat(autonat::Event::StatusChanged {
                new,
                ..
            })) => {
                let public = matches!(new, autonat::NatStatus::Public(_));
                self.nat = match &new {
                    autonat::NatStatus::Public(a) => format!("public {a}"),
                    autonat::NatStatus::Private => "private".into(),
                    autonat::NatStatus::Unknown => "unknown".into(),
                };
                let mode = if public && self.config.kad_server_when_reachable {
                    kad::Mode::Server
                } else {
                    kad::Mode::Client
                };
                self.swarm.behaviour_mut().kad.set_mode(Some(mode));
                let nat = self.nat.clone();
                self.emit(Event::Nat(nat));
            }
            SwarmEvent::Behaviour(BehaviourEvent::Relay(event)) => match event {
                relay::client::Event::ReservationReqAccepted {
                    relay_peer_id,
                    renewal,
                    ..
                } => {
                    self.reservations.insert(relay_peer_id);
                    if !renewal {
                        self.stats.reservations_accepted += 1;
                        self.emit(Event::Reservation {
                            relay: relay_peer_id,
                            result: Ok(()),
                        });
                    }
                }
                relay::client::Event::OutboundCircuitEstablished { .. } => {
                    self.stats.circuits_out += 1;
                }
                relay::client::Event::InboundCircuitEstablished { src_peer_id, .. } => {
                    self.stats.circuits_in += 1;
                    self.emit(Event::InboundCircuit { peer: src_peer_id });
                }
            },
            SwarmEvent::Behaviour(BehaviourEvent::Dcutr(dcutr::Event {
                remote_peer_id,
                result,
            })) => {
                let result = result.map(|_| ()).map_err(|e| chain(&e));
                if result.is_ok() {
                    self.stats.hole_punches.0 += 1;
                } else {
                    self.stats.hole_punches.1 += 1;
                }
                self.emit(Event::HolePunch {
                    peer: remote_peer_id,
                    result,
                });
            }
            _ => {}
        }
    }

    fn on_kad(
        &mut self,
        id: kad::QueryId,
        result: kad::QueryResult,
        stats: kad::QueryStats,
        last: bool,
    ) {
        if let Some(topic) = self.provider_queries.get(&id).cloned() {
            if last {
                self.provider_queries.remove(&id);
            }
            match result {
                kad::QueryResult::StartProviding(Ok(_)) => {
                    self.providing.insert(topic, Provide::Done);
                }
                kad::QueryResult::StartProviding(Err(_)) => {
                    self.providing
                        .insert(topic, Provide::RetryAt(Instant::now() + FIND_INTERVAL));
                }
                kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders {
                    providers,
                    ..
                })) => self.dial_providers(providers),
                _ => {}
            }
            return;
        }
        let Some((query, key)) = self.queries.get(&id).cloned() else {
            return;
        };
        if last {
            self.queries.remove(&id);
        }
        if let Some(progress) = kad_progress(query, key, result, &stats, last) {
            self.emit(Event::Kad(progress));
        }
    }
    /// Dial, through their relays, peers that a Kademlia walk met with only
    /// circuit addresses (peers behind NAT; the walk's own dial was refused
    /// by the circuit gate), while peer slots are free.
    /// go-quai's go-libp2p hosts dial such peers through the relay and then
    /// try DCUtR; here too a peer takes a slot only once a direct connection
    /// exists. Without a relay client the node cannot dial circuits.
    fn dial_circuits(&mut self, met: Vec<(PeerId, Vec<Multiaddr>)>) {
        if !self.config.reachability || met.is_empty() {
            return;
        }
        let pending = self
            .swarm
            .network_info()
            .connection_counters()
            .num_pending_outgoing() as usize;
        let free = self
            .config
            .max_peers
            .saturating_sub(self.slots_used() + pending);
        let local = *self.swarm.local_peer_id();
        let now = Instant::now();
        let chosen: Vec<(PeerId, Vec<Multiaddr>)> = met
            .into_iter()
            .filter(|(p, _)| {
                *p != local && !self.is_connected(p) && !lock(&self.shared.book).is_banned(p, now)
            })
            .take(free)
            .collect();
        for (peer, addresses) in chosen {
            lock(&self.circuits).insert(peer);
            let opts = DialOpts::peer_id(peer)
                .condition(PeerCondition::DisconnectedAndNotDialing)
                .addresses(addresses)
                .build();
            if self.swarm.dial(opts).is_ok() {
                self.stats.circuit_dials += 1;
            }
        }
    }
    /// Dial providers while the query still holds their addresses; the dial
    /// policy drops LAN and loopback addresses from the records.
    fn dial_providers(&mut self, providers: HashSet<PeerId>) {
        // Dials in flight count against the free slots.
        let pending = self
            .swarm
            .network_info()
            .connection_counters()
            .num_pending_outgoing() as usize;
        let free = self
            .config
            .max_peers
            .saturating_sub(self.slots_used() + pending);
        let local = *self.swarm.local_peer_id();
        let now = Instant::now();
        let candidates: Vec<PeerId> = providers
            .into_iter()
            .filter(|p| {
                *p != local && !self.is_connected(p) && !lock(&self.shared.book).is_banned(p, now)
            })
            .take(free)
            .collect();
        lock(&self.circuits).extend(candidates.iter().copied());
        for peer in candidates {
            let opts = DialOpts::peer_id(peer)
                .condition(PeerCondition::DisconnectedAndNotDialing)
                .build();
            if self.swarm.dial(opts).is_ok() {
                self.stats.provider_dials += 1;
            }
        }
    }
}

/// Send one request and read its correlated response on the same stream.
/// Any other envelope (for example a peer request) fails the exchange.
pub async fn exchange(
    stream: &mut WireStream,
    book: &mut RequestBook,
    request: QuaiRequestMessage,
) -> io::Result<QuaiResponseMessage> {
    book.register(&request).map_err(bad)?;
    let id = request.id;
    let bytes = QuaiMessage {
        payload: Some(Payload::Request(request)),
    }
    .encode_to_vec();
    let result = async {
        stream.write(&bytes).await?;
        let reply = response(decode_envelope(&stream.read().await?).map_err(bad)?).map_err(bad)?;
        book.complete(&reply).map_err(bad)?;
        Ok(reply)
    }
    .await;
    if result.is_err() {
        book.cancel(id);
    }
    result
}

#[cfg(test)]
mod tests;
