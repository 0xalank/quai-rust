//! Private-network libp2p transport. Noise authenticates peers; application
//! validation is still mandatory before admitting or forwarding chain data.
use crate::{decode_envelope, DHT_PROTOCOL, MAX_FRAME_BYTES, STREAM_PROTOCOL};
use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use libp2p::Transport;
use libp2p::{
    connection_limits, gossipsub, identify, identity, kad,
    multiaddr::Protocol,
    noise,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, StreamProtocol,
};
use std::{
    collections::BTreeSet,
    io,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, Mutex, OwnedSemaphorePermit, Semaphore};
pub const IO_TIMEOUT: Duration = Duration::from_secs(10);
fn err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
pub fn loopback_address(a: &Multiaddr) -> bool {
    let mut p = a.iter();
    matches!(p.next(),Some(Protocol::Ip4(ip))if ip.is_loopback())
        && matches!(p.next(), Some(Protocol::Tcp(_)))
        && match p.next() {
            None => true,
            Some(Protocol::P2p(_)) => p.next().is_none(),
            _ => false,
        }
}
// Filter before invoking TCP: discovery cannot cause public dials, even when
// an enrolled peer advertises a public routing address.
struct LoopbackTcp<T>(T);
impl<T: Transport + Unpin> Transport for LoopbackTcp<T> {
    type Output = T::Output;
    type Error = T::Error;
    type ListenerUpgrade = T::ListenerUpgrade;
    type Dial = T::Dial;
    fn listen_on(
        &mut self,
        id: libp2p::core::transport::ListenerId,
        a: Multiaddr,
    ) -> Result<(), libp2p::core::transport::TransportError<Self::Error>> {
        if !loopback_address(&a) {
            return Err(libp2p::core::transport::TransportError::MultiaddrNotSupported(a));
        }
        self.0.listen_on(id, a)
    }
    fn remove_listener(&mut self, id: libp2p::core::transport::ListenerId) -> bool {
        self.0.remove_listener(id)
    }
    fn dial(
        &mut self,
        a: Multiaddr,
        opts: libp2p::core::transport::DialOpts,
    ) -> Result<Self::Dial, libp2p::core::transport::TransportError<Self::Error>> {
        if !loopback_address(&a) {
            return Err(libp2p::core::transport::TransportError::MultiaddrNotSupported(a));
        }
        self.0.dial(a, opts)
    }
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<libp2p::core::transport::TransportEvent<Self::ListenerUpgrade, Self::Error>>
    {
        std::pin::Pin::new(&mut self.get_mut().0).poll(cx)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_dns_and_non_tcp_addresses_are_rejected_before_transport() {
        for a in [
            "/ip4/203.0.113.1/tcp/1234",
            "/dns4/localhost/tcp/1234",
            "/ip4/127.0.0.1/udp/1234",
            "/ip6/::1/tcp/1234",
        ] {
            assert!(!loopback_address(&a.parse().unwrap()));
        }
        assert!(loopback_address(&"/ip4/127.0.0.1/tcp/0".parse().unwrap()));
    }
    async fn incoming(p: &mut PrivatePeer) -> io::Result<WireStream> {
        tokio::time::timeout(IO_TIMEOUT, async {
            loop {
                if let Event::Stream { stream, .. } = p.next().await? {
                    return Ok(stream);
                }
            }
        })
        .await
        .map_err(err)?
    }
    #[tokio::test]
    async fn oversized_incoming_frame_poisoned_and_unenrolled_peer_blocked() {
        let mut receiver = PrivatePeer::bind().await.unwrap();
        let sender = PrivatePeer::bind().await.unwrap();
        let unknown = identity::Keypair::generate_ed25519().public().to_peer_id();
        assert!(sender.open(unknown).await.is_err());
        assert!(sender
            .dial(receiver.id, receiver.address.clone())
            .await
            .is_err());
        receiver.enroll(sender.id).unwrap();
        sender.enroll(receiver.id).unwrap();
        sender
            .dial(receiver.id, receiver.address.clone())
            .await
            .unwrap();
        let mut outbound = sender.open(receiver.id).await.unwrap();
        outbound
            .inner
            .write_all(&((MAX_FRAME_BYTES + 1) as u32).to_be_bytes())
            .await
            .unwrap();
        outbound.inner.flush().await.unwrap();
        let mut stream = incoming(&mut receiver).await.unwrap();
        assert_eq!(
            stream.read().await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(stream.failed);
        assert!(stream
            .read()
            .await
            .unwrap_err()
            .to_string()
            .contains("reconnect"));
    }
    async fn gossip(p: &mut PrivatePeer) -> io::Result<(gossipsub::MessageId, PeerId, Vec<u8>)> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Event::Gossip { id, peer, data, .. } = p.next().await? {
                    return Ok((id, peer, data));
                }
            }
        })
        .await
        .map_err(err)?
    }
    #[tokio::test]
    async fn gossip_waits_for_application_acceptance_and_rejects_invalid_payload() {
        let a = PrivatePeer::bind().await.unwrap();
        let mut b = PrivatePeer::bind().await.unwrap();
        let mut c = PrivatePeer::bind().await.unwrap();
        a.enroll(b.id).unwrap();
        b.enroll(a.id).unwrap();
        b.enroll(c.id).unwrap();
        c.enroll(b.id).unwrap();
        let topic = "quai-private-validation-gate".to_string();
        a.subscribe(topic.clone()).await.unwrap();
        b.subscribe(topic.clone()).await.unwrap();
        c.subscribe(topic.clone()).await.unwrap();
        a.dial(b.id, b.address.clone()).await.unwrap();
        c.dial(b.id, b.address.clone()).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
        a.publish(topic.clone(), b"verified-by-application".to_vec())
            .await
            .unwrap();
        let (id, peer, data) = gossip(&mut b).await.unwrap();
        assert_eq!(data, b"verified-by-application");
        assert!(
            tokio::time::timeout(Duration::from_millis(700), gossip(&mut c))
                .await
                .is_err()
        );
        b.validate_gossip(id, peer, true).await.unwrap();
        let (_, _, forwarded) = gossip(&mut c).await.unwrap();
        assert_eq!(forwarded, data);
        a.publish(topic, b"invalid-execution-commitment".to_vec())
            .await
            .unwrap();
        let (id, peer, data) = gossip(&mut b).await.unwrap();
        assert_eq!(data, b"invalid-execution-commitment");
        b.validate_gossip(id, peer, false).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(1000), gossip(&mut c))
                .await
                .is_err()
        );
    }
}
#[derive(NetworkBehaviour)]
struct Behaviour {
    streams: libp2p_stream::Behaviour,
    gossip: gossipsub::Behaviour,
    identify: identify::Behaviour,
    kad: kad::Behaviour<kad::store::MemoryStore>,
    limits: connection_limits::Behaviour,
}
pub struct WireStream {
    inner: libp2p::swarm::Stream,
    _permit: OwnedSemaphorePermit,
    failed: bool,
}
impl WireStream {
    /// Shared framing for other transport profiles; the permit bounds streams.
    pub(crate) fn new(inner: libp2p::swarm::Stream, permit: OwnedSemaphorePermit) -> Self {
        Self {
            inner,
            _permit: permit,
            failed: false,
        }
    }
    pub async fn read(&mut self) -> io::Result<Vec<u8>> {
        if self.failed {
            return Err(io::Error::other("Quai stream failed; reconnect"));
        }
        self.failed = true;
        let result = tokio::time::timeout(IO_TIMEOUT, async {
            let mut prefix = [0; 4];
            self.inner.read_exact(&mut prefix).await?;
            let n = u32::from_be_bytes(prefix) as usize;
            if n > MAX_FRAME_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Quai frame exceeds 3 MiB",
                ));
            }
            // Grow with the bytes that arrive instead of reserving the declared size.
            let mut b = Vec::with_capacity(n.min(64 * 1024));
            (&mut self.inner).take(n as u64).read_to_end(&mut b).await?;
            if b.len() != n {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            decode_envelope(&b).map_err(err)?;
            Ok(b)
        })
        .await
        .map_err(err)?;
        if result.is_ok() {
            self.failed = false;
        }
        result
    }
    pub async fn write(&mut self, b: &[u8]) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other("Quai stream failed; reconnect"));
        }
        if b.len() > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Quai frame exceeds 3 MiB",
            ));
        }
        decode_envelope(b).map_err(err)?;
        self.failed = true;
        let result = tokio::time::timeout(IO_TIMEOUT, async {
            self.inner
                .write_all(&(b.len() as u32).to_be_bytes())
                .await?;
            self.inner.write_all(b).await?;
            self.inner.flush().await
        })
        .await
        .map_err(err)?;
        if result.is_ok() {
            self.failed = false;
        }
        result
    }
    pub async fn close(&mut self) -> io::Result<()> {
        self.inner.close().await
    }
}
pub enum Event {
    Stream {
        peer: PeerId,
        stream: WireStream,
    },
    Gossip {
        peer: PeerId,
        id: gossipsub::MessageId,
        topic: gossipsub::TopicHash,
        data: Vec<u8>,
    },
    Connected(PeerId),
    Disconnected(PeerId),
}
enum Command {
    Dial(PeerId, Multiaddr, oneshot::Sender<io::Result<()>>),
    Subscribe(String, oneshot::Sender<io::Result<()>>),
    Publish(String, Vec<u8>, oneshot::Sender<io::Result<()>>),
    Validate(gossipsub::MessageId, PeerId, bool),
}
pub struct PrivatePeer {
    pub id: PeerId,
    pub address: Multiaddr,
    allowed: Arc<RwLock<BTreeSet<PeerId>>>,
    commands: mpsc::Sender<Command>,
    control: Mutex<libp2p_stream::Control>,
    slots: Arc<Semaphore>,
    events: Mutex<mpsc::Receiver<Event>>,
    driver: tokio::task::JoinHandle<()>,
}
impl Drop for PrivatePeer {
    fn drop(&mut self) {
        self.driver.abort();
    }
}
impl PrivatePeer {
    pub async fn bind() -> io::Result<Self> {
        Self::bind_with_identity(identity::Keypair::generate_ed25519()).await
    }
    /// Bind with a caller-owned peer identity; enrollment and loopback policy
    /// remain mandatory. This does not import a wallet or consensus authority.
    pub async fn bind_with_identity(key: identity::Keypair) -> io::Result<Self> {
        let id = key.public().to_peer_id();
        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(key)
            .with_tokio()
            .with_other_transport(|key| {
                Ok(LoopbackTcp(tcp::tokio::Transport::new(
                    tcp::Config::default().nodelay(true),
                ))
                .upgrade(libp2p::core::upgrade::Version::V1Lazy)
                .authenticate(noise::Config::new(key)?)
                .multiplex(yamux::Config::default())
                .boxed())
            })
            .map_err(err)?
            .with_behaviour(|key| {
                let cfg = gossipsub::ConfigBuilder::default()
                    .validation_mode(gossipsub::ValidationMode::Strict)
                    .validate_messages()
                    .max_transmit_size(MAX_FRAME_BYTES)
                    .heartbeat_interval(Duration::from_millis(500))
                    .build()
                    .map_err(err)?;
                let gossip = gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(key.clone()),
                    cfg,
                )
                .map_err(err)?;
                let mut kad_cfg = kad::Config::new(StreamProtocol::new(DHT_PROTOCOL));
                kad_cfg.set_query_timeout(Duration::from_secs(10));
                Ok(Behaviour {
                    streams: libp2p_stream::Behaviour::new(),
                    gossip,
                    identify: identify::Behaviour::new(
                        identify::Config::new(STREAM_PROTOCOL.into(), key.public())
                            .with_agent_version("quai-rust/0.1".into()),
                    ),
                    kad: kad::Behaviour::with_config(
                        key.public().to_peer_id(),
                        kad::store::MemoryStore::new(key.public().to_peer_id()),
                        kad_cfg,
                    ),
                    limits: connection_limits::Behaviour::new(
                        connection_limits::ConnectionLimits::default()
                            .with_max_pending_incoming(Some(16))
                            .with_max_pending_outgoing(Some(16))
                            .with_max_established(Some(32))
                            .with_max_established_per_peer(Some(2)),
                    ),
                })
            })
            .map_err(err)?
            .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(120)))
            .build();
        swarm
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().map_err(err)?)
            .map_err(err)?;
        let mut control = swarm.behaviour().streams.new_control();
        let mut incoming = control
            .accept(StreamProtocol::new(STREAM_PROTOCOL))
            .map_err(err)?;
        let allowed = Arc::new(RwLock::new(BTreeSet::new()));
        let permitted = allowed.clone();
        let slots = Arc::new(Semaphore::new(32));
        let permits = slots.clone();
        let (tx, mut commands) = mpsc::channel(32);
        let (events, receiver) = mpsc::channel(32);
        let (ready, wait) = oneshot::channel();
        let driver = tokio::spawn(async move {
            let mut ready = Some(ready);
            loop {
                tokio::select! {
                 stream=incoming.next()=>{if let Some((peer,inner))=stream{if permitted.read().unwrap_or_else(|e| e.into_inner()).contains(&peer){if let Ok(permit)=permits.clone().try_acquire_owned(){let _=events.try_send(Event::Stream{peer,stream:WireStream{inner,_permit:permit,failed:false}});}}}else{break;}},
                 command=commands.recv()=>{let Some(command)=command else{break;};match command{
                  Command::Dial(peer,address,reply)=>{let result=if !loopback_address(&address)||!permitted.read().unwrap_or_else(|e| e.into_inner()).contains(&peer){Err(io::Error::new(io::ErrorKind::PermissionDenied,"private peer is not enrolled"))}else{swarm.behaviour_mut().kad.add_address(&peer,address.clone());swarm.dial(libp2p::swarm::dial_opts::DialOpts::peer_id(peer).addresses(vec![address]).build()).map_err(err)};let _=reply.send(result);},
                  Command::Subscribe(topic,reply)=>{let result=swarm.behaviour_mut().gossip.subscribe(&gossipsub::IdentTopic::new(topic)).map(|_|()).map_err(err);let _=reply.send(result);},
                  Command::Publish(topic,data,reply)=>{let result=swarm.behaviour_mut().gossip.publish(gossipsub::IdentTopic::new(topic),data).map(|_|()).map_err(err);let _=reply.send(result);},
                  Command::Validate(id,peer,accepted)=>{let decision=if accepted{gossipsub::MessageAcceptance::Accept}else{gossipsub::MessageAcceptance::Reject};let _=swarm.behaviour_mut().gossip.report_message_validation_result(&id,&peer,decision);},
                 }},
                 event=swarm.select_next_some()=>{match event{
                  SwarmEvent::NewListenAddr{address,..}=>{if let Some(reply)=ready.take(){let _=reply.send(address);}},
                  SwarmEvent::ConnectionEstablished{peer_id,endpoint,..}=>{if !permitted.read().unwrap_or_else(|e| e.into_inner()).contains(&peer_id)||!loopback_address(endpoint.get_remote_address()){let _=swarm.disconnect_peer_id(peer_id);}else{let _=events.try_send(Event::Connected(peer_id));}},
                  SwarmEvent::ConnectionClosed{peer_id,..}=>{let _=events.try_send(Event::Disconnected(peer_id));},
                  SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received{peer_id,info,..}))=>{if permitted.read().unwrap_or_else(|e| e.into_inner()).contains(&peer_id){for a in info.listen_addrs{if loopback_address(&a){swarm.behaviour_mut().kad.add_address(&peer_id,a);}}}},
                  SwarmEvent::Behaviour(BehaviourEvent::Gossip(gossipsub::Event::Message{propagation_source,message_id,message}))=>{
                   let id=message_id;let peer=propagation_source;
                   if !permitted.read().unwrap_or_else(|e| e.into_inner()).contains(&peer)||message.data.len()>MAX_FRAME_BYTES{let _=swarm.behaviour_mut().gossip.report_message_validation_result(&id,&peer,gossipsub::MessageAcceptance::Reject);}else if events.try_send(Event::Gossip{peer,id:id.clone(),topic:message.topic,data:message.data}).is_err(){let _=swarm.behaviour_mut().gossip.report_message_validation_result(&id,&peer,gossipsub::MessageAcceptance::Ignore);}
                  },_=>{}
                 }}
                }
            }
        });
        let address = tokio::time::timeout(IO_TIMEOUT, wait)
            .await
            .map_err(err)?
            .map_err(err)?;
        Ok(Self {
            id,
            address,
            allowed,
            commands: tx,
            control: Mutex::new(control),
            slots,
            events: Mutex::new(receiver),
            driver,
        })
    }
    pub fn enroll(&self, peer: PeerId) -> io::Result<()> {
        let mut peers = self.allowed.write().unwrap_or_else(|e| e.into_inner());
        if peers.len() >= 32 && !peers.contains(&peer) {
            return Err(io::Error::other("private peer limit"));
        }
        peers.insert(peer);
        Ok(())
    }
    pub async fn dial(&self, peer: PeerId, address: Multiaddr) -> io::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Dial(peer, address, tx))
            .await
            .map_err(err)?;
        rx.await.map_err(err)?
    }
    pub async fn open(&self, peer: PeerId) -> io::Result<WireStream> {
        if !self
            .allowed
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&peer)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private peer is not enrolled",
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
        Ok(WireStream {
            inner,
            _permit: permit,
            failed: false,
        })
    }
    pub async fn next(&self) -> io::Result<Event> {
        self.events
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::other("private peer stopped"))
    }
    pub async fn subscribe(&self, topic: String) -> io::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Subscribe(topic, tx))
            .await
            .map_err(err)?;
        rx.await.map_err(err)?
    }
    pub async fn publish(&self, topic: String, data: Vec<u8>) -> io::Result<()> {
        if data.len() > MAX_FRAME_BYTES {
            return Err(io::Error::other("gossip payload exceeds 3 MiB"));
        }
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Publish(topic, data, tx))
            .await
            .map_err(err)?;
        rx.await.map_err(err)?
    }
    pub async fn validate_gossip(
        &self,
        id: gossipsub::MessageId,
        peer: PeerId,
        accepted: bool,
    ) -> io::Result<()> {
        self.commands
            .send(Command::Validate(id, peer, accepted))
            .await
            .map_err(err)
    }
}
