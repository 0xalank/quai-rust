//! Which addresses the public profile dials and listens on.
//!
//! Every dial goes through [`DialPolicy`] twice: [`DialPolicy::allows_target`]
//! before DNS resolution and [`DialPolicy::allows`] on the resolved address,
//! in the TCP transport itself. A relay circuit address is allowed when its
//! relay part is; the relay client then dials the relay through the same
//! filtered transport. DHT and identify records often carry LAN, loopback and
//! CGNAT addresses: the public policy never dials them.
use crate::transport::loopback_address;
use libp2p::{
    core::transport::{DialOpts, ListenerId, TransportError, TransportEvent},
    multiaddr::Protocol,
    Multiaddr, PeerId, Transport,
};
use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

/// Which addresses the node may dial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialPolicy {
    /// IPv4 loopback TCP (a local go-quai node), and one-hop circuits
    /// through a loopback relay. No DNS.
    Loopback,
    /// Globally routable IPv4/IPv6 TCP, DNS names that resolve to such
    /// addresses, and relay circuits through such relays. No LAN or loopback.
    Public,
}
impl DialPolicy {
    /// The check for every resolved TCP dial address.
    pub fn allows(self, a: &Multiaddr) -> bool {
        match self {
            Self::Loopback => loopback_address(a),
            Self::Public => global_tcp(a),
        }
    }
    /// The check for a dial target before DNS resolution.
    pub fn allows_target(self, a: &Multiaddr) -> bool {
        // A circuit needs the relay's peer ID; one hop only (`circuit_tail`).
        let circuit = |relay: &Multiaddr| {
            relay.iter().any(|p| matches!(p, Protocol::P2p(_))) && circuit_tail(a)
        };
        match (self, relay_part(a)) {
            (Self::Loopback, Some(relay)) => loopback_address(&relay) && circuit(&relay),
            (Self::Loopback, None) => loopback_address(a),
            (Self::Public, Some(relay)) => {
                (global_tcp(&relay) || dns_tcp(&relay)) && circuit(&relay)
            }
            (Self::Public, None) => global_tcp(a) || dns_tcp(a),
        }
    }
    /// The check for a listen address: loopback for the loopback policy,
    /// any TCP address (and relay circuits) for the public policy.
    pub fn allows_listen(self, a: &Multiaddr) -> bool {
        let mut p = a.iter();
        let ip = match p.next() {
            Some(Protocol::Ip4(ip)) => IpAddr::V4(ip),
            Some(Protocol::Ip6(ip)) => IpAddr::V6(ip),
            _ => return false,
        };
        let tcp = matches!(p.next(), Some(Protocol::Tcp(_))) && p.next().is_none();
        tcp && match self {
            Self::Loopback => ip.is_loopback(),
            Self::Public => true,
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
        Some(Protocol::Ip4(ip)) => global_ip(IpAddr::V4(ip)),
        Some(Protocol::Ip6(ip)) => global_ip(IpAddr::V6(ip)),
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
/// After the relay part: `/p2p-circuit`, then at most one `/p2p/<target>`.
fn circuit_tail(a: &Multiaddr) -> bool {
    let mut p = a.iter().skip_while(|p| !matches!(p, Protocol::P2pCircuit));
    matches!(p.next(), Some(Protocol::P2pCircuit))
        && match p.next() {
            None => true,
            Some(Protocol::P2p(_)) => p.next().is_none(),
            _ => false,
        }
}
/// True for `.../p2p-circuit...` addresses.
pub fn is_circuit(a: &Multiaddr) -> bool {
    a.iter().any(|p| matches!(p, Protocol::P2pCircuit))
}
/// The relay's own address (up to and including its `/p2p/` part) of a
/// circuit address.
pub fn relay_part(a: &Multiaddr) -> Option<Multiaddr> {
    if !is_circuit(a) {
        return None;
    }
    Some(
        a.iter()
            .take_while(|p| !matches!(p, Protocol::P2pCircuit))
            .collect(),
    )
}
/// The IP address of a direct TCP address (none for DNS or circuit addresses).
pub fn ip_of(a: &Multiaddr) -> Option<IpAddr> {
    if is_circuit(a) {
        return None;
    }
    match a.iter().next()? {
        Protocol::Ip4(ip) => Some(IpAddr::V4(ip)),
        Protocol::Ip6(ip) => Some(IpAddr::V6(ip)),
        _ => None,
    }
}
/// Globally routable unicast: no private, loopback, link-local, CGNAT,
/// documentation, benchmarking, multicast or reserved ranges.
pub fn global_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => global_v4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(v4) => global_v4(v4),
            None => global_v6(ip),
        },
    }
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

/// The target of a circuit address: the `/p2p/` part after `/p2p-circuit`.
pub fn circuit_target(a: &Multiaddr) -> Option<PeerId> {
    a.iter()
        .skip_while(|p| !matches!(p, Protocol::P2pCircuit))
        .find_map(|p| match p {
            Protocol::P2p(peer) => Some(peer),
            _ => None,
        })
}

/// The relay client, dialing circuits only to peers the node chose (explicit
/// dials and DHT providers), not to every peer a Kademlia walk meets. Each
/// circuit costs the relay, and go-libp2p's DHT opens no streams on relayed
/// connections. Reservations (listening) pass unchanged.
pub(crate) struct CircuitGate<T> {
    pub(crate) inner: T,
    pub(crate) allowed: Arc<Mutex<HashSet<PeerId>>>,
}
impl<T: Transport + Unpin> Transport for CircuitGate<T> {
    type Output = T::Output;
    type Error = T::Error;
    type ListenerUpgrade = T::ListenerUpgrade;
    type Dial = T::Dial;
    fn listen_on(
        &mut self,
        id: ListenerId,
        a: Multiaddr,
    ) -> Result<(), TransportError<Self::Error>> {
        self.inner.listen_on(id, a)
    }
    fn remove_listener(&mut self, id: ListenerId) -> bool {
        self.inner.remove_listener(id)
    }
    fn dial(
        &mut self,
        a: Multiaddr,
        opts: DialOpts,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        let chosen = circuit_target(&a).is_some_and(|target| {
            self.allowed
                .lock()
                .map(|s| s.contains(&target))
                .unwrap_or(false)
        });
        if !chosen {
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

/// TCP under the dial policy. Listening is refused unless the node was
/// configured to listen, and then only on addresses the policy allows.
pub(crate) struct PolicyTcp<T> {
    pub(crate) inner: T,
    pub(crate) policy: DialPolicy,
    pub(crate) listen: bool,
}
impl<T: Transport + Unpin> Transport for PolicyTcp<T> {
    type Output = T::Output;
    type Error = T::Error;
    type ListenerUpgrade = T::ListenerUpgrade;
    type Dial = T::Dial;
    fn listen_on(
        &mut self,
        id: ListenerId,
        a: Multiaddr,
    ) -> Result<(), TransportError<Self::Error>> {
        if !self.listen || !self.policy.allows_listen(&a) {
            return Err(TransportError::MultiaddrNotSupported(a));
        }
        self.inner.listen_on(id, a)
    }
    fn remove_listener(&mut self, id: ListenerId) -> bool {
        self.inner.remove_listener(id)
    }
    fn dial(
        &mut self,
        a: Multiaddr,
        opts: DialOpts,
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

#[cfg(test)]
mod tests {
    use super::*;
    const RELAY: &str =
        "/ip4/52.14.236.240/tcp/4002/p2p/12D3KooWMHTexdSq1zQ2Sn4L8zddEa8PWK8TGM56ZHszRuoUPoq1";
    const TARGET: &str = "12D3KooWHhfG6d4SyE9zoaSirNKowXCRwkVJrWQU2Bi16SXoBw6E";

    #[test]
    fn public_policy_dials_global_tcp_dns_and_relay_circuits_only() {
        let loopback: Multiaddr = "/ip4/127.0.0.1/tcp/4002".parse().unwrap();
        assert!(DialPolicy::Loopback.allows(&loopback));
        assert!(!DialPolicy::Public.allows(&loopback));
        for a in [
            "/ip4/8.8.8.8/tcp/4002",
            "/ip6/2606:4700::1111/tcp/4002",
            "/dns4/colosseum1.qu.ai/tcp/4002/p2p/12D3KooWB72Vs7gfLgvJsmR7cex7tG5WY7WDdqJD124Res3byDyk",
        ] {
            assert!(DialPolicy::Public.allows_target(&a.parse().unwrap()), "{a}");
        }
        let circuit: Multiaddr = format!("{RELAY}/p2p-circuit/p2p/{TARGET}").parse().unwrap();
        assert!(DialPolicy::Public.allows_target(&circuit));
        assert!(!DialPolicy::Loopback.allows_target(&circuit));
        // The loopback policy takes circuits through a loopback relay only.
        let local: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4002/p2p/{TARGET}/p2p-circuit/p2p/{TARGET}")
                .parse()
                .unwrap();
        assert!(DialPolicy::Loopback.allows_target(&local));
        assert!(!DialPolicy::Public.allows_target(&local));
        let two_hops: Multiaddr = format!("{local}/p2p-circuit").parse().unwrap();
        assert!(!DialPolicy::Loopback.allows_target(&two_hops));
        assert_eq!(relay_part(&circuit).unwrap().to_string(), RELAY);
        assert_eq!(circuit_target(&circuit).unwrap().to_string(), TARGET);
        assert!(circuit_target(&RELAY.parse().unwrap()).is_none());
        assert!(ip_of(&circuit).is_none());
        for a in [
            "/ip4/192.168.1.10/tcp/4002",
            "/ip4/10.0.0.1/tcp/4002",
            "/ip4/100.64.0.1/tcp/4002",
            "/ip4/169.254.1.1/tcp/4002",
            "/ip4/8.8.8.8/tcp/0",
            "/ip4/8.8.8.8/udp/4002/quic-v1",
            "/ip6/::1/tcp/4002",
            "/ip6/fd00::1/tcp/4002",
            "/ip6/::ffff:192.168.1.1/tcp/4002",
        ] {
            assert!(
                !DialPolicy::Public.allows_target(&a.parse().unwrap()),
                "{a}"
            );
        }
        // A circuit through a LAN relay, a relay of a relay, or extra parts.
        for a in [
            format!("/ip4/192.168.1.2/tcp/4002/p2p/{TARGET}/p2p-circuit"),
            format!("{RELAY}/p2p-circuit/p2p/{TARGET}/p2p-circuit/p2p/{TARGET}"),
            format!("{RELAY}/p2p-circuit/p2p/{TARGET}/tcp/1"),
        ] {
            assert!(
                !DialPolicy::Public.allows_target(&a.parse().unwrap()),
                "{a}"
            );
        }
        assert!(DialPolicy::Public.allows_listen(&"/ip4/0.0.0.0/tcp/0".parse().unwrap()));
        assert!(!DialPolicy::Loopback.allows_listen(&"/ip4/0.0.0.0/tcp/0".parse().unwrap()));
        assert!(DialPolicy::Loopback.allows_listen(&"/ip4/127.0.0.1/tcp/0".parse().unwrap()));
    }
}
