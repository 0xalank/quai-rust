use super::*;
use crate::transport::{Event as PrivateEvent, PrivatePeer};
use quai_primitives::schema::block::ProtoWorkObjectBlockView;

/// Mainnet gossip captured from the colosseum boot peers (2026-11-18).
const CAPTURE: &str = include_str!("../../tests/data/mainnet-gossip.jsonl");
fn sample(suffix: &str) -> (String, Vec<u8>) {
    CAPTURE
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|v| v["topic"].as_str().unwrap().ends_with(suffix))
        .map(|v| {
            (
                v["topic"].as_str().unwrap().to_string(),
                decode_hex(v["data"].as_str().unwrap()).unwrap(),
            )
        })
        .unwrap()
}

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
    assert!(!DialPolicy::Public.allows_target(&"/dns4/x.example/udp/1/quic-v1".parse().unwrap()));
}

#[test]
fn replies_are_scored_as_go_request_from_peer() {
    let (_, data) = sample("/0,0/blocks");
    let view = ProtoWorkObjectBlockView::decode(data.as_slice()).unwrap();
    let object = WorkObject::decode_auxiliary(
        &view.work_object.clone().unwrap().encode_to_vec(),
        PINNED_SCHEDULE,
    )
    .unwrap();
    let header = object.auxiliary_work_header(PINNED_SCHEDULE).unwrap();
    let hash = header.block_hash().unwrap();
    let number = validate::number_of(header.wire().number.as_deref()).unwrap();
    let zone = [0u8, 0];
    let frame = |id: u32, location: &[u8], response: rs::Response| -> Result<Vec<u8>, String> {
        Ok(QuaiMessage {
            payload: Some(Payload::Response(QuaiResponseMessage {
                id,
                location: Some(ProtoLocation {
                    value: location.to_vec(),
                }),
                response: Some(response),
            })),
        }
        .encode_to_vec())
    };
    let block = rs::Response::WorkObjectBlockView(view.clone());
    let by_hash = Query::Hash(hash);
    let score = |f, q: &Query, w| classify(f, 5, &zone, q, w).0;
    // The block asked by its hash or by its zone number: +10.
    assert_eq!(
        score(frame(5, &zone, block.clone()), &by_hash, Want::Block),
        Outcome::Valid
    );
    let (outcome, kept) = classify(
        frame(5, &zone, block.clone()),
        5,
        &zone,
        &Query::Number(number),
        Want::Block,
    );
    assert_eq!(outcome, Outcome::Valid);
    assert_eq!(kept, Some(block.clone()));
    // Another hash or number: -40 (go-quai adds +10, then drops it).
    for q in [Query::Hash([1; 32]), Query::Number(number + 1)] {
        assert!(matches!(
            score(frame(5, &zone, block.clone()), &q, Want::Block),
            Outcome::Bad(_)
        ));
    }
    // Not found: -10 by hash, no change by number (Go's comment: a peer may
    // be behind near the tip).
    let empty = rs::Response::WorkObjectBlockView(Default::default());
    assert_eq!(
        score(frame(5, &zone, empty), &by_hash, Want::Block),
        Outcome::Empty
    );
    let no_hash = rs::Response::BlockHash(ProtoHash::default());
    assert_eq!(
        score(frame(5, &zone, no_hash), &Query::Number(0), Want::BlockHash),
        Outcome::Empty
    );
    // A block-hash request answered with another type: go-quai BanPeer.
    assert!(matches!(
        score(
            frame(5, &zone, block.clone()),
            &Query::Number(0),
            Want::BlockHash
        ),
        Outcome::Ban(_)
    ));
    // A header request answered with a block: -40 and no ban, as in Go.
    assert!(matches!(
        score(frame(5, &zone, block.clone()), &by_hash, Want::Header),
        Outcome::Bad(_)
    ));
    // Another location: -40 (go-quai does not compare it).
    assert!(matches!(
        score(frame(5, &[0], block.clone()), &by_hash, Want::Block),
        Outcome::Bad(_)
    ));
    // A reply to another request, garbage or silence: Go's request times out.
    for f in [
        frame(6, &zone, block.clone()),
        Ok(vec![1, 2, 3]),
        Err("deadline has elapsed".into()),
    ] {
        assert!(matches!(
            score(f, &by_hash, Want::Block),
            Outcome::Timeout(_)
        ));
    }
    // The score table of p2p/quality_scores.go, bounded to 0..=1000.
    let mut book = Book::default();
    let p = identity::Keypair::generate_ed25519().public().to_peer_id();
    for (adjustment, want) in [
        (Adjustment::Response, 10),
        (Adjustment::Broadcast, 11),
        (Adjustment::Nack, 1),
        (Adjustment::Response, 11),
        (Adjustment::Timeout, 0),
        (Adjustment::BadResponse, 0),
    ] {
        assert_eq!(book.adjust(p, adjustment), want);
    }
}

async fn wait<T>(
    node: &PublicNode,
    secs: u64,
    mut pick: impl FnMut(Event) -> Option<T>,
) -> io::Result<T> {
    tokio::time::timeout(Duration::from_secs(secs), async {
        loop {
            if let Some(v) = pick(node.next().await?) {
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
    assert_eq!(
        probe.publish("t".into(), vec![1]).await.unwrap_err().kind(),
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
        wait(&probe, 10, |e| match e {
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
    assert!(matches!(reply.response, Some(rs::Response::BlockHash(h)) if h.value == vec![7; 32]));
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
    let (data, verdict) = wait(&probe, 10, |e| match e {
        Event::Gossip { data, verdict, .. } => Some((data, verdict)),
        _ => None,
    })
    .await
    .unwrap();
    assert_eq!(data, b"from-server");
    assert!(matches!(verdict, Verdict::Ignore(_)));
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

/// A loopback stand-in for a go-quai peer: it answers every request with
/// the block hash `genesis` and reports the gossip it receives.
async fn remote(
    genesis: Vec<u8>,
    node: PeerId,
) -> (Arc<PrivatePeer>, mpsc::UnboundedReceiver<Vec<u8>>) {
    let peer = Arc::new(PrivatePeer::bind().await.unwrap());
    peer.enroll(node).unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    let p = peer.clone();
    tokio::spawn(async move {
        while let Ok(event) = p.next().await {
            match event {
                PrivateEvent::Stream { mut stream, .. } => {
                    let genesis = genesis.clone();
                    tokio::spawn(async move {
                        let Ok(frame) = stream.read().await else {
                            return;
                        };
                        let Ok(Some(Payload::Request(r))) =
                            decode_envelope(&frame).map(|m| m.payload)
                        else {
                            return;
                        };
                        let reply = QuaiMessage {
                            payload: Some(Payload::Response(QuaiResponseMessage {
                                id: r.id,
                                location: r.location,
                                response: Some(rs::Response::BlockHash(ProtoHash {
                                    value: genesis,
                                })),
                            })),
                        };
                        let _ = stream.write(&reply.encode_to_vec()).await;
                    });
                }
                PrivateEvent::Gossip { data, .. } => {
                    let _ = tx.send(data);
                }
                _ => {}
            }
        }
    });
    (peer, rx)
}

struct Setup {
    node: PublicNode,
    good: Arc<PrivatePeer>,
    other: Arc<PrivatePeer>,
    bad: Arc<PrivatePeer>,
    other_gossip: mpsc::UnboundedReceiver<Vec<u8>>,
}
/// A public-profile node on loopback with two mainnet peers and one peer of
/// another network.
async fn setup() -> Setup {
    let mut config = Config::read_only(DialPolicy::Loopback);
    config.listen = vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()];
    config.serve = true;
    config.validate = true;
    config.genesis_check = true;
    config.validators = 1;
    config.max_peers = 4;
    config.max_connections = 8;
    let node = PublicNode::launch(config, identity::Keypair::generate_ed25519()).unwrap();
    let genesis = colosseum_genesis().to_vec();
    let (good, _) = remote(genesis.clone(), node.id).await;
    let (other, other_gossip) = remote(genesis, node.id).await;
    let (bad, _) = remote(vec![9; 32], node.id).await;
    for p in [&good, &other, &bad] {
        node.dial(p.address.clone().with_p2p(p.id).unwrap())
            .await
            .unwrap();
    }
    let (mut verified, mut banned) = (BTreeSet::new(), None);
    wait(&node, 30, |e| {
        match e {
            Event::Verified { peer } => {
                verified.insert(peer);
            }
            Event::Banned { peer, reason } => banned = Some((peer, reason)),
            _ => {}
        }
        (verified.len() == 2 && banned.is_some()).then_some(())
    })
    .await
    .unwrap();
    assert_eq!(verified, BTreeSet::from([good.id, other.id]));
    let (peer, reason) = banned.unwrap();
    assert_eq!(peer, bad.id);
    assert!(reason.contains("is not mainnet"), "{reason}");
    Setup {
        node,
        good,
        other,
        bad,
        other_gossip,
    }
}
async fn subscribe_all(s: &Setup, topic: &str) {
    s.node.subscribe(topic.into()).await.unwrap();
    s.good.subscribe(topic.into()).await.unwrap();
    s.other.subscribe(topic.into()).await.unwrap();
    // Subscriptions travel and the meshes form within a few heartbeats.
    tokio::time::sleep(Duration::from_secs(3)).await;
}
fn block_request(id: u32) -> Vec<u8> {
    QuaiMessage {
        payload: Some(Payload::Request(QuaiRequestMessage {
            id,
            location: Some(ProtoLocation { value: vec![0, 0] }),
            data: Some(rq::Data::Hash(ProtoHash { value: vec![7; 32] })),
            request: Some(rq::Request::WorkObjectBlock(Default::default())),
        })),
    }
    .encode_to_vec()
}

#[tokio::test]
async fn public_node_checks_genesis_answers_not_found_and_limits_requests() {
    let mut s = setup().await;
    // The peer of another network is gone, and the gate refuses it.
    tokio::time::timeout(Duration::from_secs(10), async {
        while s.node.is_connected(&s.bad.id) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let refused = s
        .node
        .dial(s.bad.address.clone().with_p2p(s.bad.id).unwrap())
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("banned peer"), "{refused}");
    // Inbound requests get go-quai's "not found" answer with the same ID and
    // location.
    let mut stream = s.good.open(s.node.id).await.unwrap();
    stream.write(&block_request(1)).await.unwrap();
    let reply = response(decode_envelope(&stream.read().await.unwrap()).unwrap()).unwrap();
    assert_eq!(reply.id, 1);
    assert_eq!(reply.location.unwrap().value, vec![0, 0]);
    assert!(
        matches!(reply.response, Some(rs::Response::WorkObjectBlockView(v)) if v.work_object.is_none())
    );
    // A chatty peer has its stream closed after the burst (go-quai's
    // ProcRequestRate never closes it).
    for id in 2..=400 {
        stream.write(&block_request(id)).await.unwrap();
    }
    let mut answered = 0;
    while stream.read().await.is_ok() {
        answered += 1;
    }
    assert!((90..399).contains(&answered), "{answered}");
    // Publishing stays off.
    let e = s.node.publish("t".into(), vec![1]).await.unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
    // A gossip message that fails go-quai's validator is rejected and not
    // forwarded.
    let (topic, _) = sample("/0,0/blocks");
    subscribe_all(&s, &topic).await;
    s.good
        .publish(topic.clone(), b"not a block".to_vec())
        .await
        .unwrap();
    let verdict = wait(&s.node, 10, |e| match e {
        Event::Gossip { verdict, .. } => Some(verdict),
        _ => None,
    })
    .await
    .unwrap();
    assert!(matches!(verdict, Verdict::Reject(_)), "{verdict:?}");
    let forwarded = tokio::time::timeout(Duration::from_secs(2), s.other_gossip.recv()).await;
    assert!(forwarded.is_err(), "rejected gossip was forwarded");
    let stats = s.node.stats().await.unwrap();
    assert_eq!(stats.verified, 2);
    assert_eq!(stats.genesis_checks, (2, 1));
    assert!(stats.bans.iter().any(|(p, _)| *p == s.bad.id.to_string()));
    assert!(stats.gate_refused["banned peer"] >= 1);
    assert_eq!(stats.served.0 as usize, answered + 2);
    assert_eq!((stats.served.1 as usize, stats.served.2), (answered + 1, 1));
    assert_eq!(stats.gossip["zone/blocks"].rejected, 1);
}

#[tokio::test]
#[cfg_attr(debug_assertions, ignore = "KAWPOW verification needs a release build")]
async fn public_node_relays_only_gossip_that_passes_go_validation() {
    let mut s = setup().await;
    let (topic, data) = sample("/0,0/blocks");
    subscribe_all(&s, &topic).await;
    // A mainnet block passes and goes on to the other peer. The first copy
    // can wait for the KAWPOW cache, which takes about 30 s on a loaded host,
    // so send up to three and give each copy 30 s.
    let mut forwarded = None;
    for _ in 0..3 {
        s.good.publish(topic.clone(), data.clone()).await.unwrap();
        if let Ok(Some(d)) =
            tokio::time::timeout(Duration::from_secs(30), s.other_gossip.recv()).await
        {
            forwarded = Some(d);
            break;
        }
    }
    assert_eq!(forwarded.as_deref(), Some(data.as_slice()));
    // The same block with a broken seal is rejected and stays here.
    let mut view = ProtoWorkObjectBlockView::decode(data.as_slice()).unwrap();
    let header = view
        .work_object
        .as_mut()
        .unwrap()
        .wo_header
        .as_mut()
        .unwrap();
    header.aux_pow.as_mut().unwrap().header.as_mut().unwrap()[80] ^= 1;
    let broken = view.encode_to_vec();
    s.good.publish(topic.clone(), broken.clone()).await.unwrap();
    let verdict = wait(&s.node, 30, |e| match e {
        Event::Gossip { data, verdict, .. } if data == broken => Some(verdict),
        _ => None,
    })
    .await
    .unwrap();
    assert!(matches!(verdict, Verdict::Reject(_)), "{verdict:?}");
    let leaked = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(d) = s.other_gossip.recv().await {
            if d == broken {
                return;
            }
        }
        std::future::pending::<()>().await
    })
    .await;
    assert!(leaked.is_err(), "rejected gossip was forwarded");
    let stats = s.node.stats().await.unwrap();
    let zone = stats.gossip["zone/blocks"];
    assert!(zone.accepted >= 1, "{zone:?}");
    assert_eq!(zone.rejected, 1);
    assert!(stats.heads[2].is_some());
}

/// A circuit relay v2 server on loopback, as go-quai's boot peers run one
/// (`EnableRelayService`).
async fn relay_server() -> (PeerId, Multiaddr) {
    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_behaviour(|key| {
            relay::Behaviour::new(key.public().to_peer_id(), relay::Config::default())
        })
        .unwrap()
        .build();
    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .unwrap();
    let address = loop {
        if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
            break address;
        }
    };
    swarm.add_external_address(address.clone());
    let id = *swarm.local_peer_id();
    tokio::spawn(async move {
        loop {
            swarm.select_next_some().await;
        }
    });
    (id, address.with_p2p(id).unwrap())
}

#[tokio::test]
async fn relay_reservation_makes_a_node_reachable_through_a_circuit() {
    let (relay, relay_address) = relay_server().await;
    // A node that cannot be dialed directly (it does not listen on TCP)
    // reserves a slot on the relay.
    let mut config = Config::read_only(DialPolicy::Loopback);
    config.serve = true;
    config.genesis_check = true;
    config.reachability = true;
    config.relays = vec![relay_address.clone()];
    config.relay_after = Duration::ZERO;
    let listener = PublicNode::launch(config, identity::Keypair::generate_ed25519()).unwrap();
    let reserved = wait(&listener, 20, |e| match e {
        Event::Reservation { relay: r, result } if r == relay => Some(result),
        _ => None,
    })
    .await
    .unwrap();
    assert_eq!(reserved, Ok(()));
    // Another node reaches it through `<relay>/p2p-circuit/p2p/<listener>`.
    let mut config = Config::read_only(DialPolicy::Loopback);
    config.reachability = true;
    let dialer = PublicNode::launch(config, identity::Keypair::generate_ed25519()).unwrap();
    let circuit = relay_address
        .with(Protocol::P2pCircuit)
        .with_p2p(listener.id)
        .unwrap();
    dialer.dial(circuit).await.unwrap();
    let address = wait(&dialer, 20, |e| match e {
        Event::Connected { peer, address } if peer == listener.id => Some(address),
        _ => None,
    })
    .await
    .unwrap();
    assert!(address::is_circuit(&address), "{address}");
    let peer = wait(&listener, 20, |e| match e {
        Event::InboundCircuit { peer } => Some(peer),
        _ => None,
    })
    .await
    .unwrap();
    assert_eq!(peer, dialer.id);
    // As in go-libp2p, a relayed connection carries no GossipSub, DHT or Quai
    // streams: the peer gets no genesis check and no slot until it has a
    // direct connection, and it stays connected for hole punching.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(listener.is_connected(&dialer.id));
    let stats = listener.stats().await.unwrap();
    // One failed check: the relay server speaks no /quai/1.0.0. It gets no
    // slot, but it keeps the reservation: no ban, no disconnect.
    assert_eq!((stats.genesis_checks, stats.verified), ((0, 1), 0));
    assert!(stats.bans.is_empty(), "{:?}", stats.bans);
    assert_eq!(stats.relayed_peers, 1);
    assert_eq!(stats.reservations, vec![relay.to_string()]);
    assert_eq!((stats.reservations_accepted, stats.circuits_in), (1, 1));
    assert!(stats
        .external_addrs
        .iter()
        .any(|a| a.contains("p2p-circuit")));
    assert_eq!(dialer.stats().await.unwrap().circuits_out, 1);
}

/// A peer that stops answering (a sleep or a network change: no reset
/// reaches this node, and the socket stays open) loses its connection after
/// one ping time-out, and the peer lists and counts follow.
#[test]
fn a_connection_that_stops_answering_pings_is_closed() {
    let quick = |mut config: Config| {
        config.ping_interval = Duration::from_millis(500);
        config.ping_timeout = Duration::from_secs(2);
        config
    };
    // The silent peer runs on a runtime of one worker, which the test then
    // blocks: its TCP socket stays open, but nothing answers.
    let silent_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let mut config = quick(Config::read_only(DialPolicy::Loopback));
    config.listen = vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()];
    let silent = {
        let _guard = silent_rt.enter();
        PublicNode::launch(config, identity::Keypair::generate_ed25519()).unwrap()
    };
    let listen = silent_rt
        .block_on(wait(&silent, 10, |e| match e {
            Event::ListenAddr(a) => Some(a),
            _ => None,
        }))
        .unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let node = {
        let _guard = rt.enter();
        PublicNode::launch(
            quick(Config::read_only(DialPolicy::Loopback)),
            identity::Keypair::generate_ed25519(),
        )
        .unwrap()
    };
    rt.block_on(async {
        node.dial(listen.with_p2p(silent.id).unwrap())
            .await
            .unwrap();
        wait(&node, 10, |e| match e {
            Event::Connected { peer, .. } if peer == silent.id => Some(()),
            _ => None,
        })
        .await
        .unwrap();
        // Pings pass while the peer answers.
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(node.connected_peers(), vec![silent.id]);
        assert_eq!(node.peer_counts(), (1, 0, 1));
    });
    silent_rt.spawn(async { std::thread::sleep(Duration::from_secs(30)) });
    rt.block_on(async {
        let error = wait(&node, 15, |e| match e {
            Event::DeadConnection { peer, error } if peer == silent.id => Some(error),
            _ => None,
        })
        .await
        .unwrap();
        assert!(!error.is_empty());
        wait(&node, 10, |e| match e {
            Event::Disconnected { peer, .. } if peer == silent.id => Some(()),
            _ => None,
        })
        .await
        .unwrap();
        assert!(!node.is_connected(&silent.id));
        assert!(node.connected_peers().is_empty());
        assert_eq!(node.peer_counts(), (0, 0, 0));
        assert_eq!(node.stats().await.unwrap().dead_connections, 1);
    });
    drop(node);
    rt.shutdown_background();
    silent_rt.shutdown_background();
}

/// A Kademlia server on loopback that knows `contacts` (go-quai's DHT
/// protocol, server mode).
async fn kad_server(contacts: Vec<(PeerId, Multiaddr)>) -> (PeerId, Multiaddr) {
    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_behaviour(|key| {
            let local = key.public().to_peer_id();
            let cfg = kad::Config::new(StreamProtocol::new(KAD_PROTOCOL));
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), cfg)
        })
        .unwrap()
        .build();
    swarm.behaviour_mut().set_mode(Some(kad::Mode::Server));
    for (peer, address) in contacts {
        swarm.behaviour_mut().add_address(&peer, address);
    }
    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .unwrap();
    let address = loop {
        if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
            break address;
        }
    };
    swarm.add_external_address(address.clone());
    let id = *swarm.local_peer_id();
    tokio::spawn(async move {
        loop {
            swarm.select_next_some().await;
        }
    });
    (id, address)
}

/// A peer behind NAT, which a Kademlia walk meets with only a circuit
/// address, is dialed through its relay while slots are free (go-libp2p
/// dials such peers). The walk's own dial is refused by the circuit gate.
#[tokio::test]
async fn a_kademlia_contact_with_only_a_circuit_address_is_dialed_through_its_relay() {
    let (relay, relay_address) = relay_server().await;
    // The NATed peer: no TCP listener, a reservation on the relay.
    let mut config = Config::read_only(DialPolicy::Loopback);
    config.serve = true;
    config.genesis_check = true;
    config.reachability = true;
    config.relays = vec![relay_address.clone()];
    config.relay_after = Duration::ZERO;
    let natted = PublicNode::launch(config, identity::Keypair::generate_ed25519()).unwrap();
    let reserved = wait(&natted, 20, |e| match e {
        Event::Reservation { relay: r, result } if r == relay => Some(result),
        _ => None,
    })
    .await
    .unwrap();
    assert_eq!(reserved, Ok(()));
    let circuit = relay_address
        .with(Protocol::P2pCircuit)
        .with_p2p(natted.id)
        .unwrap();
    // A DHT server that knows only the circuit address.
    let (server, server_address) = kad_server(vec![(natted.id, circuit.clone())]).await;
    let mut config = Config::read_only(DialPolicy::Loopback);
    config.reachability = true;
    let node = PublicNode::launch(config, identity::Keypair::generate_ed25519()).unwrap();
    node.kad_add(server, server_address).await.unwrap();
    node.closest_peers(natted.id.to_bytes()).await.unwrap();
    let address = wait(&node, 30, |e| match e {
        Event::Connected { peer, address } if peer == natted.id => Some(address),
        _ => None,
    })
    .await
    .unwrap();
    assert!(address::is_circuit(&address), "{address}");
    assert_eq!(node.stats().await.unwrap().circuit_dials, 1);
}

#[test]
fn relay_cap_slice_validator_and_peer_scoring_defaults() {
    let relay = PeerId::random();
    let circuit: Multiaddr = format!("/ip4/1.2.3.4/tcp/4001/p2p/{relay}/p2p-circuit")
        .parse()
        .unwrap();
    assert_eq!(address::relay_peer(&circuit), Some(relay));
    assert_eq!(
        address::relay_peer(&"/ip4/1.2.3.4/tcp/4001".parse().unwrap()),
        None
    );

    // Slice validator constructs cleanly for non-[0,0] slices.
    let v = validate::Validator::with_slice(
        Arc::new(quai_consensus::pow::SharedCaches::new(2).unwrap()),
        Arc::new(std::sync::RwLock::new(validate::Heads::default())),
        [0, 1],
    );
    let _ = v;

    // Conservative GossipSub peer/topic scoring params construct cleanly.
    let (params, thresholds) = gossip::peer_score_params();
    assert!(thresholds.graylist_threshold <= thresholds.publish_threshold);
    assert!(thresholds.gossip_threshold >= thresholds.graylist_threshold);
    assert!(params.decay_interval > Duration::ZERO);
    let _ = gossip::topic_score_params();
}
