//! Read-only public-network peer for go-quai interoperation probes.
//!
//! This is an explicit opt-in profile. The enrolled loopback profile in
//! [`crate::transport`] is unchanged. This peer:
//! - dials out only and never listens;
//! - registers no inbound `/quai/1.0.0` handler, so peer requests fail during
//!   protocol negotiation;
//! - runs Kademlia in client mode, so it answers no DHT queries;
//! - has no publish method and reports every gossip message as `Ignore`, so
//!   nothing enters the IWANT cache or goes to another peer;
//! - limits connections (established plus pending) to [`MAX_PEERS`] and checks
//!   every dial address against a [`DialPolicy`] after DNS resolution.
//!
//! Peer data is unvalidated input. Callers must decode and validate it before
//! any use; this module only frames and correlates it.
use crate::{
    decode_envelope, response,
    transport::{loopback_address, WireStream, IO_TIMEOUT},
    RequestBook, MAX_FRAME_BYTES, STREAM_PROTOCOL,
};
use futures::StreamExt;
use libp2p::{
    connection_limits,
    core::{
        transport::{DialOpts as TransportDialOpts, ListenerId, TransportError, TransportEvent},
        upgrade::Version,
    },
    dns, gossipsub, identify, identity, kad,
    multiaddr::Protocol,
    noise,
    swarm::{dial_opts::DialOpts, NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, StreamProtocol, Transport,
};
use prost::Message;
use quai_primitives::schema::quaiprotocol::{
    quai_message::Payload, QuaiMessage, QuaiRequestMessage, QuaiResponseMessage,
};
use std::{
    collections::{BTreeSet, HashMap},
    io,
    net::{Ipv4Addr, Ipv6Addr},
    pin::Pin,
    sync::{Arc, RwLock},
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, Mutex, Semaphore};

/// go-quai sets `kaddht.ProtocolPrefix("/quai")`; go-libp2p-kad-dht v0.25.2
/// appends `/kad/1.0.0`. The bare prefix [`crate::DHT_PROTOCOL`] does not match.
pub const KAD_PROTOCOL: &str = "/quai/kad/1.0.0";
/// go-quai identify protocol version (`p2p/protocol.ProtocolVersion`).
pub const IDENTIFY_PROTOCOL_VERSION: &str = "/quai/1.0.0";
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
/// Most simultaneous connections: established plus pending dials.
pub const MAX_PEERS: u32 = 8;
const MAX_PENDING_DIALS: u32 = 2;
const MAX_STREAMS: usize = 16;

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

/// Which addresses the peer may dial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialPolicy {
    /// IPv4 loopback TCP only (a local go-quai node). No DNS resolution.
    Loopback,
    /// Globally routable IPv4/IPv6 TCP. DNS names are resolved first, and the
    /// resolved address must pass the same check. No LAN or loopback dials.
    Public,
}
impl DialPolicy {
    /// The check for every resolved dial address.
    pub fn allows(self, a: &Multiaddr) -> bool {
        match self {
            Self::Loopback => loopback_address(a),
            Self::Public => global_tcp(a),
        }
    }
    /// The check for a dial target before DNS resolution.
    pub fn allows_target(self, a: &Multiaddr) -> bool {
        match self {
            Self::Loopback => loopback_address(a),
            Self::Public => global_tcp(a) || dns_tcp(a),
        }
    }
}
fn tcp_tail<'a>(mut p: impl Iterator<Item = Protocol<'a>>) -> bool {
    matches!(p.next(), Some(Protocol::Tcp(port)) if port != 0)
        && match p.next() {
            None => true,
            Some(Protocol::P2p(_)) => p.next().is_none(),
            _ => false,
        }
}
fn global_tcp(a: &Multiaddr) -> bool {
    let mut p = a.iter();
    let global = match p.next() {
        Some(Protocol::Ip4(ip)) => global_v4(ip),
        Some(Protocol::Ip6(ip)) => global_v6(ip),
        _ => false,
    };
    global && tcp_tail(p)
}
fn dns_tcp(a: &Multiaddr) -> bool {
    let mut p = a.iter();
    matches!(
        p.next(),
        Some(Protocol::Dns(_) | Protocol::Dns4(_) | Protocol::Dns6(_))
    ) && tcp_tail(p)
}
fn global_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (18..20).contains(&b))
        || a >= 240)
}
fn global_v6(ip: Ipv6Addr) -> bool {
    // Global unicast 2000::/3 without documentation 2001:db8::/32.
    let s = ip.segments();
    s[0] & 0xe000 == 0x2000 && !(s[0] == 0x2001 && s[1] == 0x0db8)
}

/// Checks every dial address before TCP sees it, and refuses every listen
/// request: this profile dials out only.
struct PolicyTcp<T> {
    inner: T,
    policy: DialPolicy,
}
impl<T: Transport + Unpin> Transport for PolicyTcp<T> {
    type Output = T::Output;
    type Error = T::Error;
    type ListenerUpgrade = T::ListenerUpgrade;
    type Dial = T::Dial;
    fn listen_on(
        &mut self,
        _: ListenerId,
        a: Multiaddr,
    ) -> Result<(), TransportError<Self::Error>> {
        Err(TransportError::MultiaddrNotSupported(a))
    }
    fn remove_listener(&mut self, id: ListenerId) -> bool {
        self.inner.remove_listener(id)
    }
    fn dial(
        &mut self,
        a: Multiaddr,
        opts: TransportDialOpts,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        if !self.policy.allows(&a) {
            return Err(TransportError::MultiaddrNotSupported(a));
        }
        self.inner.dial(a, opts)
    }
    fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<TransportEvent<Self::ListenerUpgrade, Self::Error>> {
        Pin::new(&mut self.get_mut().inner).poll(cx)
    }
}

/// go-libp2p-pubsub `DefaultMsgIdFn`: raw `from` bytes then raw `seqno` bytes.
/// Matching it lets IHAVE/IWANT identifiers agree with Go peers.
pub fn go_message_id(message: &gossipsub::Message) -> gossipsub::MessageId {
    let mut id = message.source.map(|p| p.to_bytes()).unwrap_or_default();
    if let Some(seqno) = message.sequence_number {
        id.extend_from_slice(&seqno.to_be_bytes());
    }
    gossipsub::MessageId::new(&id)
}

/// DHT provider key for a gossip topic, as go-quai `TopicToCid` builds it: a
/// multihash with code sha2-256 whose digest is the raw topic string
/// (go-multihash `Encode` does not hash). go-libp2p-kad-dht keys providers by
/// these multihash bytes.
pub fn topic_provider_key(topic: &str) -> Vec<u8> {
    let mut key = vec![0x12];
    let mut n = topic.len() as u64;
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            key.push(byte);
            break;
        }
        key.push(byte | 0x80);
    }
    key.extend_from_slice(topic.as_bytes());
    key
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    streams: libp2p_stream::Behaviour,
    gossip: gossipsub::Behaviour,
    identify: identify::Behaviour,
    kad: kad::Behaviour<kad::store::MemoryStore>,
    limits: connection_limits::Behaviour,
}

/// Kademlia query progress as this client observes it.
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
    Identified {
        peer: PeerId,
        info: Box<identify::Info>,
    },
    /// A remote announced a topic subscription.
    Subscribed {
        peer: PeerId,
        topic: String,
    },
    /// A gossip message. It is already reported as `Ignore`: never forwarded.
    Gossip {
        peer: PeerId,
        source: Option<PeerId>,
        topic: String,
        data: Vec<u8>,
    },
    Kad(KadProgress),
}

enum Command {
    Dial(Multiaddr, oneshot::Sender<io::Result<()>>),
    KadAdd(PeerId, Multiaddr, oneshot::Sender<io::Result<()>>),
    Subscribe(String, oneshot::Sender<io::Result<()>>),
    ClosestPeers(Vec<u8>, oneshot::Sender<io::Result<()>>),
    Providers(Vec<u8>, oneshot::Sender<io::Result<()>>),
}

pub struct ReadOnlyPeer {
    pub id: PeerId,
    pub policy: DialPolicy,
    connected: Arc<RwLock<BTreeSet<PeerId>>>,
    commands: mpsc::Sender<Command>,
    control: Mutex<libp2p_stream::Control>,
    slots: Arc<Semaphore>,
    events: Mutex<mpsc::Receiver<Event>>,
    driver: tokio::task::JoinHandle<()>,
}
impl Drop for ReadOnlyPeer {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

fn gossip_config() -> io::Result<gossipsub::Config> {
    gossipsub::ConfigBuilder::default()
        // go-libp2p-pubsub default StrictSign: signed messages with from/seqno.
        .validation_mode(gossipsub::ValidationMode::Strict)
        // Nothing is forwarded or cached for IWANT until accepted; we never accept.
        .validate_messages()
        // go-quai: pubsub.WithMaxMessageSize(params.MaxGossipsubPacketSize = 3 MiB).
        .max_transmit_size(MAX_FRAME_BYTES)
        .message_id_fn(go_message_id)
        // go-libp2p-pubsub defaults.
        .heartbeat_initial_delay(Duration::from_millis(100))
        .heartbeat_interval(Duration::from_secs(1))
        // No peer exchange dials.
        .prune_peers(0)
        .build()
        .map_err(err)
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

impl ReadOnlyPeer {
    pub fn start(policy: DialPolicy) -> io::Result<Self> {
        Self::start_with_identity(policy, identity::Keypair::generate_ed25519())
    }
    /// Use a fresh throwaway identity unless a caller needs a stable peer ID.
    pub fn start_with_identity(policy: DialPolicy, key: identity::Keypair) -> io::Result<Self> {
        let id = key.public().to_peer_id();
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(key)
            .with_tokio()
            .with_other_transport(
                |key| -> Result<_, Box<dyn std::error::Error + Send + Sync>> {
                    let tcp = PolicyTcp {
                        inner: tcp::tokio::Transport::new(tcp::Config::default().nodelay(true)),
                        policy,
                    };
                    let base = match policy {
                        DialPolicy::Loopback => tcp.boxed(),
                        DialPolicy::Public => dns::tokio::Transport::system(tcp)?.boxed(),
                    };
                    Ok(base
                        .upgrade(Version::V1Lazy)
                        .authenticate(noise::Config::new(key)?)
                        .multiplex(yamux::Config::default())
                        .boxed())
                },
            )
            .map_err(err)?
            .with_behaviour(|key| {
                let gossip = gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(key.clone()),
                    gossip_config()?,
                )
                .map_err(err)?;
                let mut cfg = kad::Config::new(StreamProtocol::new(KAD_PROTOCOL));
                cfg.set_query_timeout(Duration::from_secs(20));
                cfg.set_periodic_bootstrap_interval(None);
                // Only explicit `kad_add` calls fill the routing table.
                cfg.set_kbucket_inserts(kad::BucketInserts::Manual);
                let local = key.public().to_peer_id();
                let mut kad =
                    kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), cfg);
                kad.set_mode(Some(kad::Mode::Client));
                Ok(Behaviour {
                    streams: libp2p_stream::Behaviour::new(),
                    gossip,
                    identify: identify::Behaviour::new(
                        identify::Config::new(IDENTIFY_PROTOCOL_VERSION.into(), key.public())
                            .with_agent_version(format!(
                                "quai-rust/{} read-only",
                                env!("CARGO_PKG_VERSION")
                            )),
                    ),
                    kad,
                    limits: connection_limits::Behaviour::new(
                        connection_limits::ConnectionLimits::default()
                            .with_max_pending_incoming(Some(0))
                            .with_max_established_incoming(Some(0))
                            .with_max_pending_outgoing(Some(MAX_PENDING_DIALS))
                            .with_max_established(Some(MAX_PEERS - MAX_PENDING_DIALS))
                            .with_max_established_per_peer(Some(1)),
                    ),
                })
            })
            .map_err(err)?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();
        // No `accept`: inbound `/quai/1.0.0` streams fail protocol negotiation.
        let control = swarm.behaviour().streams.new_control();
        let connected = Arc::new(RwLock::new(BTreeSet::new()));
        let seen = connected.clone();
        let (tx, mut commands) = mpsc::channel(32);
        let (events, receiver) = mpsc::channel(256);
        let driver = tokio::spawn(async move {
            let mut queries = HashMap::new();
            loop {
                tokio::select! {
                    command = commands.recv() => {
                        let Some(command) = command else { break };
                        match command {
                            Command::Dial(address, reply) => {
                                let result = if !policy.allows_target(&address) {
                                    Err(io::Error::new(
                                        io::ErrorKind::PermissionDenied,
                                        "dial address is outside the dial policy",
                                    ))
                                } else if let Some(peer) = address.iter().find_map(|p| match p {
                                    Protocol::P2p(peer) => Some(peer),
                                    _ => None,
                                }) {
                                    let bare: Multiaddr = address
                                        .iter()
                                        .filter(|p| !matches!(p, Protocol::P2p(_)))
                                        .collect();
                                    swarm
                                        .dial(DialOpts::peer_id(peer).addresses(vec![bare]).build())
                                        .map_err(err)
                                } else {
                                    swarm.dial(address).map_err(err)
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
                                    match swarm.behaviour_mut().kad.add_address(&peer, address) {
                                        kad::RoutingUpdate::Failed => {
                                            Err(io::Error::other("Kademlia routing table refused the peer"))
                                        }
                                        _ => Ok(()),
                                    }
                                };
                                let _ = reply.send(result);
                            }
                            Command::Subscribe(topic, reply) => {
                                let result = swarm
                                    .behaviour_mut()
                                    .gossip
                                    .subscribe(&gossipsub::IdentTopic::new(topic))
                                    .map(|_| ())
                                    .map_err(err);
                                let _ = reply.send(result);
                            }
                            Command::ClosestPeers(key, reply) => {
                                let query = swarm.behaviour_mut().kad.get_closest_peers(key.clone());
                                queries.insert(query, ("closest_peers", key));
                                let _ = reply.send(Ok(()));
                            }
                            Command::Providers(key, reply) => {
                                let query = swarm
                                    .behaviour_mut()
                                    .kad
                                    .get_providers(kad::RecordKey::new(&key));
                                queries.insert(query, ("providers", key));
                                let _ = reply.send(Ok(()));
                            }
                        }
                    }
                    event = swarm.select_next_some() => match event {
                        SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                            seen.write().unwrap().insert(peer_id);
                            let _ = events.try_send(Event::Connected {
                                peer: peer_id,
                                address: endpoint.get_remote_address().clone(),
                            });
                        }
                        SwarmEvent::ConnectionClosed { peer_id, cause, num_established, .. } => {
                            if num_established == 0 {
                                seen.write().unwrap().remove(&peer_id);
                                let _ = events.try_send(Event::Disconnected {
                                    peer: peer_id,
                                    cause: cause.map(|c| chain(&c)),
                                });
                            }
                        }
                        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                            let _ = events.try_send(Event::DialFailed {
                                peer: peer_id,
                                error: chain(&error),
                            });
                        }
                        SwarmEvent::Behaviour(BehaviourEvent::Identify(
                            identify::Event::Received { peer_id, info, .. },
                        )) => {
                            let _ = events.try_send(Event::Identified {
                                peer: peer_id,
                                info: Box::new(info),
                            });
                        }
                        SwarmEvent::Behaviour(BehaviourEvent::Gossip(
                            gossipsub::Event::Subscribed { peer_id, topic },
                        )) => {
                            let _ = events.try_send(Event::Subscribed {
                                peer: peer_id,
                                topic: topic.into_string(),
                            });
                        }
                        SwarmEvent::Behaviour(BehaviourEvent::Gossip(
                            gossipsub::Event::Message {
                                propagation_source,
                                message_id,
                                message,
                            },
                        )) => {
                            // Read-only: never accept, so never forward or serve IWANT.
                            let _ = swarm.behaviour_mut().gossip.report_message_validation_result(
                                &message_id,
                                &propagation_source,
                                gossipsub::MessageAcceptance::Ignore,
                            );
                            let _ = events.try_send(Event::Gossip {
                                peer: propagation_source,
                                source: message.source,
                                topic: message.topic.into_string(),
                                data: message.data,
                            });
                        }
                        SwarmEvent::Behaviour(BehaviourEvent::Kad(
                            kad::Event::OutboundQueryProgressed { id, result, stats, step },
                        )) => {
                            let Some((query, key)) = queries.get(&id).cloned() else {
                                continue;
                            };
                            if step.last {
                                queries.remove(&id);
                            }
                            if let Some(progress) = kad_progress(query, key, result, &stats, step.last) {
                                let _ = events.try_send(Event::Kad(progress));
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
        Ok(Self {
            id,
            policy,
            connected,
            commands: tx,
            control: Mutex::new(control),
            slots: Arc::new(Semaphore::new(MAX_STREAMS)),
            events: Mutex::new(receiver),
            driver,
        })
    }
    async fn command<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<io::Result<T>>) -> Command,
    ) -> io::Result<T> {
        let (tx, rx) = oneshot::channel();
        self.commands.send(make(tx)).await.map_err(err)?;
        rx.await.map_err(err)?
    }
    /// Dial a target that the dial policy allows. A `/p2p/` suffix pins the
    /// expected peer ID.
    pub async fn dial(&self, address: Multiaddr) -> io::Result<()> {
        self.command(|tx| Command::Dial(address, tx)).await
    }
    /// Add a Kademlia server to the routing table. Nothing else adds peers,
    /// so queries start only from peers that the caller chose. Kademlia may
    /// then bootstrap from them; its dials obey the same policy and limits.
    pub async fn kad_add(&self, peer: PeerId, address: Multiaddr) -> io::Result<()> {
        self.command(|tx| Command::KadAdd(peer, address, tx)).await
    }
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
    pub fn is_connected(&self, peer: &PeerId) -> bool {
        self.connected.read().unwrap().contains(peer)
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
            .ok_or_else(|| io::Error::other("read-only peer stopped"))
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
mod tests {
    use super::*;
    use crate::transport::{Event as PrivateEvent, PrivatePeer};
    use quai_primitives::schema::{
        common::{ProtoHash, ProtoLocation},
        quaiprotocol::{quai_request_message as rq, quai_response_message as rs},
    };

    #[test]
    fn dial_policy_separates_loopback_and_public_addresses() {
        let loopback: Multiaddr = "/ip4/127.0.0.1/tcp/4002".parse().unwrap();
        assert!(DialPolicy::Loopback.allows(&loopback));
        assert!(!DialPolicy::Public.allows(&loopback));
        for a in [
            "/ip4/8.8.8.8/tcp/4002",
            "/ip4/34.120.1.2/tcp/4002/p2p/12D3KooWB72Vs7gfLgvJsmR7cex7tG5WY7WDdqJD124Res3byDyk",
            "/ip6/2606:4700::1111/tcp/4002",
        ] {
            let a: Multiaddr = a.parse().unwrap();
            assert!(DialPolicy::Public.allows(&a), "{a}");
            assert!(!DialPolicy::Loopback.allows(&a), "{a}");
        }
        for a in [
            "/ip4/192.168.1.10/tcp/4002",
            "/ip4/10.0.0.1/tcp/4002",
            "/ip4/172.20.10.2/tcp/4002",
            "/ip4/100.64.0.1/tcp/4002",
            "/ip4/169.254.1.1/tcp/4002",
            "/ip4/0.1.2.3/tcp/4002",
            "/ip4/240.0.0.1/tcp/4002",
            "/ip4/8.8.8.8/tcp/0",
            "/ip4/8.8.8.8/udp/4002/quic-v1",
            "/ip6/::1/tcp/4002",
            "/ip6/fe80::1/tcp/4002",
            "/ip6/fd00::1/tcp/4002",
            "/ip6/2001:db8::1/tcp/4002",
            "/dns4/colosseum1.qu.ai/tcp/4002",
        ] {
            assert!(!DialPolicy::Public.allows(&a.parse().unwrap()), "{a}");
        }
        for a in COLOSSEUM_BOOTNODES {
            let a: Multiaddr = a.parse().unwrap();
            assert!(DialPolicy::Public.allows_target(&a));
            assert!(!DialPolicy::Loopback.allows_target(&a));
        }
        assert!(
            !DialPolicy::Public.allows_target(&"/dns4/x.example/udp/1/quic-v1".parse().unwrap())
        );
    }

    fn base32_decode(text: &str) -> Vec<u8> {
        let (mut bits, mut value, mut out) = (0u32, 0u32, vec![]);
        for c in text.bytes() {
            let digit = match c {
                b'a'..=b'z' => c - b'a',
                b'2'..=b'7' => c - b'2' + 26,
                _ => panic!("not base32"),
            };
            value = (value << 5) | u32::from(digit);
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                out.push((value >> bits) as u8);
                value &= (1 << bits) - 1;
            }
        }
        out
    }

    #[test]
    fn provider_key_matches_go_topic_cid() {
        // CIDs from a go-quai v0.57.0 colosseum node log ("Creating topic").
        let genesis = COLOSSEUM_TOPIC_GENESIS;
        for (suffix, cid) in [
            ("//headers", "bafkreszqpbtgmmtcg4zgim3cmvsgeobxg42dqytcgfrdgytchaytaztegzswiobqhbrtqmtfhbstaolfmfstmolegezwmzddgq2ggnbugbrtezjxf4xwqzlbmrsxe4y"),
            ("//blocks", "bafkresrqpbtgmmtcg4zgim3cmvsgeobxg42dqytcgfrdgytchaytaztegzswiobqhbrtqmtfhbstaolfmfstmolegezwmzddgq2ggnbugbrtezjxf4xwe3dpmnvxg"),
        ] {
            let topic = format!("{genesis}{suffix}");
            let cid = base32_decode(cid.strip_prefix('b').unwrap());
            // CIDv1, raw codec, then the multihash that the DHT uses as key.
            assert_eq!(&cid[..2], &[0x01, 0x55]);
            assert_eq!(cid[2..], topic_provider_key(&topic));
        }
    }

    #[test]
    fn message_id_is_go_from_then_seqno() {
        let source = identity::Keypair::generate_ed25519().public().to_peer_id();
        let message = gossipsub::Message {
            source: Some(source),
            data: vec![1, 2, 3],
            sequence_number: Some(0x0102_0304_0506_0708),
            topic: gossipsub::TopicHash::from_raw("t"),
        };
        let mut expected = source.to_bytes();
        expected.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            go_message_id(&message),
            gossipsub::MessageId::new(&expected)
        );
    }

    async fn wait<T>(
        peer: &ReadOnlyPeer,
        mut pick: impl FnMut(Event) -> Option<T>,
    ) -> io::Result<T> {
        tokio::time::timeout(IO_TIMEOUT, async {
            loop {
                if let Some(v) = pick(peer.next().await?) {
                    return Ok(v);
                }
            }
        })
        .await
        .map_err(err)?
    }

    #[tokio::test]
    async fn read_only_peer_requests_refuses_streams_and_never_forwards_gossip() {
        let probe = ReadOnlyPeer::start(DialPolicy::Loopback).unwrap();
        assert_eq!(
            probe
                .dial("/ip4/8.8.8.8/tcp/4002".parse().unwrap())
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let server = PrivatePeer::bind().await.unwrap();
        let other = PrivatePeer::bind().await.unwrap();
        server.enroll(probe.id).unwrap();
        other.enroll(probe.id).unwrap();
        for p in [&server, &other] {
            probe
                .dial(p.address.clone().with_p2p(p.id).unwrap())
                .await
                .unwrap();
            wait(&probe, |e| match e {
                Event::Connected { peer, .. } if peer == p.id => Some(()),
                _ => None,
            })
            .await
            .unwrap();
        }
        // Outbound request/response with Go framing and correlation.
        let server_id = server.id;
        let task = tokio::spawn(async move {
            loop {
                if let PrivateEvent::Stream { mut stream, .. } = server.next().await? {
                    let message = decode_envelope(&stream.read().await?).map_err(bad)?;
                    let Some(Payload::Request(request)) = message.payload else {
                        return Err(bad("expected request"));
                    };
                    let Some(rq::Data::Hash(hash)) = request.data else {
                        return Err(bad("expected hash"));
                    };
                    let reply = QuaiMessage {
                        payload: Some(Payload::Response(QuaiResponseMessage {
                            id: request.id,
                            location: request.location,
                            response: Some(rs::Response::BlockHash(hash)),
                        })),
                    };
                    stream.write(&reply.encode_to_vec()).await?;
                    return Ok::<_, io::Error>(server);
                }
            }
        });
        let mut stream = probe.open(server_id).await.unwrap();
        let mut book = RequestBook::new(4).unwrap();
        let reply = exchange(
            &mut stream,
            &mut book,
            QuaiRequestMessage {
                id: 9,
                location: Some(ProtoLocation { value: vec![0, 0] }),
                data: Some(rq::Data::Hash(ProtoHash { value: vec![7; 32] })),
                request: Some(rq::Request::BlockHash(ProtoHash::default())),
            },
        )
        .await
        .unwrap();
        assert!(
            matches!(reply.response, Some(rs::Response::BlockHash(h)) if h.value == vec![7; 32])
        );
        assert!(book.is_empty());
        let server = tokio::time::timeout(IO_TIMEOUT, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        // Inbound Quai streams are refused at protocol negotiation.
        assert!(server.open(probe.id).await.is_err());
        // Gossip reaches the probe but never leaves it.
        let topic = "quai-read-only-gate".to_string();
        probe.subscribe(topic.clone()).await.unwrap();
        server.subscribe(topic.clone()).await.unwrap();
        other.subscribe(topic.clone()).await.unwrap();
        tokio::time::sleep(Duration::from_secs(3)).await;
        server
            .publish(topic, b"from-server".to_vec())
            .await
            .unwrap();
        let data = wait(&probe, |e| match e {
            Event::Gossip { data, .. } => Some(data),
            _ => None,
        })
        .await
        .unwrap();
        assert_eq!(data, b"from-server");
        let forwarded = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let PrivateEvent::Gossip { .. } = other.next().await? {
                    return Ok::<_, io::Error>(());
                }
            }
        })
        .await;
        assert!(forwarded.is_err(), "read-only peer forwarded gossip");
    }
}
