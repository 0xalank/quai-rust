//! Peer management as go-quai does it, plus the limits it lacks.
//!
//! - Request quality: go-quai `p2p/quality_scores.go` and
//!   `peerManager.QualityBucket`. A score from 0 to 1000 per peer: +10 for a
//!   response, -10 for an empty answer to a request by hash, -20 for a
//!   timeout, -40 for a response that fails validation, +1 for a gossip
//!   message. Peers are tried best bucket first.
//! - Bans: go-quai `BanPeer` blocks a peer for the life of the process and
//!   closes its connections. go-quai bans a peer whose answer to a
//!   block-hash request has another type (`requestFromPeer`), and the source
//!   of an accepted broadcast of unknown type (`OnNewBroadcast`). A block
//!   with the wrong hash or number only fails the request. This node also
//!   bans, for a limited time, a peer that fails the genesis check.
//!   [`Gate`] refuses every connection from a banned peer.
//! - Request rate: go-quai's `ProcRequestRate` never triggers (it updates a
//!   copy of the map entry, so the average stays near its start value). This
//!   module has a working limit with Go's intended rate: an average of at
//!   least 20 ms between requests (50 per second), with a burst of 100.
//! - Per-IP limits: go-libp2p v0.32.1 has none; [`Gate`] caps connections per
//!   IP address.
use libp2p::{
    core::{transport::PortUse, Endpoint},
    swarm::{
        dummy, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler,
        THandlerInEvent, THandlerOutEvent, ToSwarm,
    },
    Multiaddr, PeerId,
};
use std::{
    collections::{BTreeMap, HashMap},
    net::IpAddr,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::{Duration, Instant},
};

/// p2p/quality_scores.go `MinScore` and `MaxScore`.
pub const MIN_SCORE: i32 = 0;
pub const MAX_SCORE: i32 = 1000;
/// go-quai `c_bestThreshold` and `c_worstThreshold`.
const BEST: f64 = 0.8;
const WORST: f64 = 0.2;
/// go-quai `requestRateLimitPeriod_ms`: 20 ms, 50 requests per second.
pub const RATE_PER_SECOND: f64 = 50.0;
/// Requests a peer may send at once before the rate applies.
pub const RATE_BURST: f64 = 100.0;

/// go-quai's quality adjustments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adjustment {
    /// `QualityAdjOnBroadcast`.
    Broadcast,
    /// `QualityAdjOnResponse`.
    Response,
    /// `QualityAdjOnNack`: an empty answer to a request by hash.
    Nack,
    /// `QualityAdjOnTimeout`.
    Timeout,
    /// `QualityAdjOnBadResponse`: the response failed validation.
    BadResponse,
}
impl Adjustment {
    pub fn delta(self) -> i32 {
        match self {
            Self::Broadcast => 1,
            Self::Response => 10,
            Self::Nack => -10,
            Self::Timeout => -20,
            Self::BadResponse => -40,
        }
    }
}
/// `boundedAdj`.
pub fn adjust(current: i32, adjustment: Adjustment) -> i32 {
    (current + adjustment.delta()).clamp(MIN_SCORE, MAX_SCORE)
}

/// go-quai `PeerQuality` buckets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bucket {
    Best,
    Responsive,
    LastResort,
}
/// go-quai `QualityBucket(quality, maxQuality)`.
pub fn bucket(quality: i32, max: i32) -> Bucket {
    if quality > (BEST * f64::from(max)) as i32 {
        Bucket::Best
    } else if quality >= (WORST * f64::from(max)) as i32 {
        Bucket::Responsive
    } else {
        Bucket::LastResort
    }
}

/// A token bucket per peer.
#[derive(Clone, Copy, Debug)]
pub struct Rate {
    tokens: f64,
    at: Instant,
}
impl Rate {
    pub fn new(now: Instant) -> Self {
        Self {
            tokens: RATE_BURST,
            at: now,
        }
    }
    /// Take one request; false when the peer is over the limit.
    pub fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
        self.tokens = (self.tokens + elapsed * RATE_PER_SECOND).min(RATE_BURST);
        self.at = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

/// Why a peer cannot connect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ban {
    pub reason: String,
    /// None: until the process stops, as go-quai's `BanPeer`.
    pub until: Option<Instant>,
}

/// Maximum tracked peers per table in [`Book`] to bound long-uptime memory.
const MAX_BOOK_ENTRIES: usize = 8192;

/// Scores, bans and rates of the peers this node has seen.
#[derive(Debug, Default)]
pub struct Book {
    quality: HashMap<PeerId, i32>,
    bans: HashMap<PeerId, Ban>,
    inbound: HashMap<PeerId, Rate>,
    outbound: HashMap<PeerId, Rate>,
}
impl Book {
    pub fn quality(&self, peer: &PeerId) -> i32 {
        self.quality.get(peer).copied().unwrap_or(MIN_SCORE)
    }
    pub fn adjust(&mut self, peer: PeerId, adjustment: Adjustment) -> i32 {
        if !self.quality.contains_key(&peer) && self.quality.len() >= MAX_BOOK_ENTRIES {
            if let Some(evict) = self.quality.keys().next().copied() {
                self.quality.remove(&evict);
            }
        }
        let q = adjust(self.quality(&peer), adjustment);
        self.quality.insert(peer, q);
        q
    }
    /// The candidates in go-quai's request order: Best, Responsive, then
    /// LastResort, by score within a bucket.
    pub fn order(&self, peers: &[PeerId]) -> Vec<PeerId> {
        let max = peers.iter().map(|p| self.quality(p)).max().unwrap_or(0);
        let mut v: Vec<(Bucket, i32, PeerId)> = peers
            .iter()
            .filter(|p| !self.is_banned(p, Instant::now()))
            .map(|p| (bucket(self.quality(p), max), -self.quality(p), *p))
            .collect();
        v.sort();
        v.into_iter().map(|(_, _, p)| p).collect()
    }
    pub fn ban(&mut self, peer: PeerId, reason: impl Into<String>, duration: Option<Duration>) {
        let now = Instant::now();
        if !self.bans.contains_key(&peer) && self.bans.len() >= MAX_BOOK_ENTRIES {
            self.bans.retain(|_, b| b.until.is_none_or(|t| t > now));
            if self.bans.len() >= MAX_BOOK_ENTRIES {
                if let Some(evict) = self
                    .bans
                    .iter()
                    .find(|(_, b)| b.until.is_some())
                    .or_else(|| self.bans.iter().next())
                    .map(|(p, _)| *p)
                {
                    self.bans.remove(&evict);
                }
            }
        }
        let until = duration.map(|d| now + d);
        self.bans.insert(
            peer,
            Ban {
                reason: reason.into(),
                until,
            },
        );
    }
    pub fn is_banned(&self, peer: &PeerId, now: Instant) -> bool {
        self.bans
            .get(peer)
            .is_some_and(|b| b.until.is_none_or(|t| t > now))
    }
    pub fn bans(&self) -> Vec<(PeerId, Ban)> {
        self.bans.iter().map(|(p, b)| (*p, b.clone())).collect()
    }
    /// Count one inbound request; false when the peer is over the limit.
    pub fn inbound_request(&mut self, peer: PeerId, now: Instant) -> bool {
        if !self.inbound.contains_key(&peer) && self.inbound.len() >= MAX_BOOK_ENTRIES {
            let idle = Duration::from_secs(60);
            self.inbound
                .retain(|_, r| now.saturating_duration_since(r.at) < idle);
            if self.inbound.len() >= MAX_BOOK_ENTRIES {
                if let Some(evict) = self.inbound.keys().next().copied() {
                    self.inbound.remove(&evict);
                }
            }
        }
        self.inbound
            .entry(peer)
            .or_insert_with(|| Rate::new(now))
            .take(now)
    }
    /// Our own requests to a peer stay under the same rate.
    pub fn outbound_request(&mut self, peer: PeerId, now: Instant) -> bool {
        if !self.outbound.contains_key(&peer) && self.outbound.len() >= MAX_BOOK_ENTRIES {
            let idle = Duration::from_secs(60);
            self.outbound
                .retain(|_, r| now.saturating_duration_since(r.at) < idle);
            if self.outbound.len() >= MAX_BOOK_ENTRIES {
                if let Some(evict) = self.outbound.keys().next().copied() {
                    self.outbound.remove(&evict);
                }
            }
        }
        self.outbound
            .entry(peer)
            .or_insert_with(|| Rate::new(now))
            .take(now)
    }
}

/// Connection gate: refuses banned peers, caps connections per IP address, and
/// caps relayed circuits per relay peer so relayed connections cannot bypass
/// per-IP eclipse limits.
#[derive(Clone)]
pub struct Gate {
    state: Arc<Mutex<GateState>>,
}
#[derive(Default)]
struct GateState {
    book: Arc<Mutex<Book>>,
    max_per_ip: usize,
    max_per_relay: usize,
    per_ip: HashMap<IpAddr, usize>,
    by_connection: HashMap<ConnectionId, IpAddr>,
    per_relay: HashMap<PeerId, usize>,
    by_relay_connection: HashMap<ConnectionId, PeerId>,
    refused: BTreeMap<&'static str, u64>,
}
impl Gate {
    pub fn new(book: Arc<Mutex<Book>>, max_per_ip: usize) -> Self {
        let max_per_ip = max_per_ip.max(1);
        Self {
            state: Arc::new(Mutex::new(GateState {
                book,
                max_per_ip,
                max_per_relay: max_per_ip.saturating_mul(2).max(4),
                ..Default::default()
            })),
        }
    }
    /// Connections refused so far.
    pub fn refused(&self) -> u64 {
        self.refused_by().values().sum()
    }
    /// Connections refused so far, by reason.
    pub fn refused_by(&self) -> BTreeMap<&'static str, u64> {
        self.state
            .lock()
            .map(|s| s.refused.clone())
            .unwrap_or_default()
    }
    fn deny(&self, why: &'static str) -> ConnectionDenied {
        if let Ok(mut s) = self.state.lock() {
            *s.refused.entry(why).or_default() += 1;
        }
        ConnectionDenied::new(GateError(why))
    }
    fn check_peer(&self, peer: &PeerId) -> Result<(), ConnectionDenied> {
        let banned = self
            .state
            .lock()
            .ok()
            .and_then(|s| {
                s.book
                    .lock()
                    .ok()
                    .map(|b| b.is_banned(peer, Instant::now()))
            })
            .unwrap_or(false);
        if banned {
            return Err(self.deny("banned peer"));
        }
        Ok(())
    }
    fn reserve_ip(&self, id: ConnectionId, addr: &Multiaddr) -> Result<(), ConnectionDenied> {
        if let Some(ip) = super::address::ip_of(addr) {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if s.by_connection.contains_key(&id) {
                return Ok(());
            }
            if s.per_ip.get(&ip).copied().unwrap_or(0) >= s.max_per_ip {
                *s.refused
                    .entry("connection limit per IP address")
                    .or_default() += 1;
                return Err(ConnectionDenied::new(GateError(
                    "connection limit per IP address",
                )));
            }
            *s.per_ip.entry(ip).or_default() += 1;
            s.by_connection.insert(id, ip);
            return Ok(());
        }
        if let Some(relay) = super::address::relay_peer(addr) {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if s.by_relay_connection.contains_key(&id) {
                return Ok(());
            }
            if s.per_relay.get(&relay).copied().unwrap_or(0) >= s.max_per_relay {
                *s.refused.entry("connection limit per relay").or_default() += 1;
                return Err(ConnectionDenied::new(GateError(
                    "connection limit per relay",
                )));
            }
            *s.per_relay.entry(relay).or_default() += 1;
            s.by_relay_connection.insert(id, relay);
        }
        Ok(())
    }
    fn release(&self, id: ConnectionId) {
        if let Ok(mut s) = self.state.lock() {
            if let Some(ip) = s.by_connection.remove(&id) {
                if let Some(n) = s.per_ip.get_mut(&ip) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        s.per_ip.remove(&ip);
                    }
                }
            }
            if let Some(relay) = s.by_relay_connection.remove(&id) {
                if let Some(n) = s.per_relay.get_mut(&relay) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        s.per_relay.remove(&relay);
                    }
                }
            }
        }
    }
}
#[derive(Debug)]
struct GateError(&'static str);
impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for GateError {}

impl NetworkBehaviour for Gate {
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = std::convert::Infallible;

    fn handle_pending_inbound_connection(
        &mut self,
        id: ConnectionId,
        _: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<(), ConnectionDenied> {
        // Reserve the IP slot immediately before Noise handshake so concurrent
        // dials from one IP cannot bypass max_per_ip.
        self.reserve_ip(id, remote)
    }
    fn handle_established_inbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        remote: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        if let Err(denied) = self.check_peer(&peer) {
            self.release(id);
            return Err(denied);
        }
        if let Err(denied) = self.reserve_ip(id, remote) {
            self.release(id);
            return Err(denied);
        }
        Ok(dummy::ConnectionHandler)
    }
    fn handle_pending_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer: Option<PeerId>,
        _: &[Multiaddr],
        _: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        if let Some(peer) = peer {
            self.check_peer(&peer)?;
        }
        Ok(vec![])
    }
    fn handle_established_outbound_connection(
        &mut self,
        id: ConnectionId,
        peer: PeerId,
        addr: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.check_peer(&peer)?;
        self.reserve_ip(id, addr)?;
        Ok(dummy::ConnectionHandler)
    }
    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionClosed(c) => self.release(c.connection_id),
            FromSwarm::ListenFailure(f) => self.release(f.connection_id),
            _ => {}
        }
    }
    fn on_connection_handler_event(
        &mut self,
        _: PeerId,
        _: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {}
    }
    fn poll(&mut self, _: &mut Context<'_>) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::identity;

    fn peer() -> PeerId {
        identity::Keypair::generate_ed25519().public().to_peer_id()
    }

    #[test]
    fn quality_scores_follow_go() {
        assert_eq!(adjust(0, Adjustment::Response), 10);
        assert_eq!(adjust(0, Adjustment::Nack), 0);
        assert_eq!(adjust(995, Adjustment::Response), 1000);
        assert_eq!(adjust(30, Adjustment::BadResponse), 0);
        assert_eq!(adjust(30, Adjustment::Timeout), 10);
        assert_eq!(adjust(7, Adjustment::Broadcast), 8);
        // QualityBucket with max 100: Best above 80, Responsive from 20.
        assert_eq!(bucket(81, 100), Bucket::Best);
        assert_eq!(bucket(80, 100), Bucket::Responsive);
        assert_eq!(bucket(20, 100), Bucket::Responsive);
        assert_eq!(bucket(19, 100), Bucket::LastResort);
        // With every score 0 (new peers), all are Responsive, as in Go.
        assert_eq!(bucket(0, 0), Bucket::Responsive);
        let mut book = Book::default();
        let (a, b, c) = (peer(), peer(), peer());
        for _ in 0..10 {
            book.adjust(a, Adjustment::Response);
        }
        book.adjust(b, Adjustment::Response);
        book.adjust(c, Adjustment::Timeout);
        assert_eq!(book.quality(&a), 100);
        assert_eq!(book.order(&[c, b, a]), vec![a, b, c]);
        // A banned peer is never a candidate.
        book.ban(a, "wrong hash", None);
        assert!(book.is_banned(&a, Instant::now()));
        assert_eq!(book.order(&[c, b, a]), vec![b, c]);
        // A timed ban ends.
        book.ban(b, "wrong network", Some(Duration::from_millis(1)));
        assert!(!book.is_banned(&b, Instant::now() + Duration::from_millis(5)));
    }

    #[test]
    fn rate_limit_works_unlike_go_proc_request_rate() {
        let start = Instant::now();
        let mut book = Book::default();
        let p = peer();
        // A burst of 100 passes, the 101st request at once does not.
        for _ in 0..100 {
            assert!(book.inbound_request(p, start));
        }
        assert!(!book.inbound_request(p, start));
        // 20 ms later one more request fits: 50 per second.
        assert!(book.inbound_request(p, start + Duration::from_millis(20)));
        assert!(!book.inbound_request(p, start + Duration::from_millis(20)));
        // A steady 50 per second passes indefinitely; 100 per second does not.
        let mut ok = 0;
        for i in 0..1000u64 {
            if book.inbound_request(p, start + Duration::from_millis(1000 + 10 * i)) {
                ok += 1;
            }
        }
        assert!((500..=620).contains(&ok), "{ok}");
        // Other peers are not affected.
        assert!(book.inbound_request(peer(), start));
    }

    #[test]
    fn gate_caps_connections_per_ip_and_refuses_banned_peers() {
        let book = Arc::new(Mutex::new(Book::default()));
        let mut gate = Gate::new(book.clone(), 2);
        let addr: Multiaddr = "/ip4/203.0.113.7/tcp/4002".parse().unwrap();
        let local: Multiaddr = "/ip4/0.0.0.0/tcp/4003".parse().unwrap();
        let ids: Vec<ConnectionId> = (0..3)
            .map(|_| ConnectionId::new_unchecked(rand_id()))
            .collect();
        assert!(gate
            .handle_established_inbound_connection(ids[0], peer(), &local, &addr)
            .is_ok());
        assert!(gate
            .handle_established_outbound_connection(
                ids[1],
                peer(),
                &addr,
                Endpoint::Dialer,
                PortUse::Reuse
            )
            .is_ok());
        assert!(gate
            .handle_pending_inbound_connection(ids[2], &local, &addr)
            .is_err());
        assert!(gate
            .handle_established_inbound_connection(ids[2], peer(), &local, &addr)
            .is_err());
        // Another address is free; relayed connections are not counted per IP.
        let other: Multiaddr = "/ip4/203.0.113.8/tcp/4002".parse().unwrap();
        assert!(gate
            .handle_pending_inbound_connection(ids[2], &local, &other)
            .is_ok());
        // A banned peer is refused before and after the handshake.
        let bad = peer();
        book.lock().unwrap().ban(bad, "wrong hash", None);
        assert!(gate
            .handle_pending_outbound_connection(ids[2], Some(bad), &[], Endpoint::Dialer)
            .is_err());
        assert!(gate
            .handle_established_inbound_connection(ids[2], bad, &local, &other)
            .is_err());
        assert!(gate.refused() >= 4);
    }
    fn rand_id() -> usize {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }
}
