//! GossipSub as go-quai v0.57.0 runs it (`p2p/node/pubsubManager`).
//!
//! Parameters: `pubsub.DefaultGossipSubParams()` (go-libp2p-pubsub v0.10.0)
//! with go-quai's `D = 30`, `Dlo = 6`, `Dhi = 45`, `Dout = 20`,
//! `WithMaxMessageSize(3 MiB)`, the default `StrictSign` policy, the default
//! message ID (`from || seqno`) and no peer scoring or peer exchange. Where
//! rust-libp2p cannot take Go's value, [`GO_DIFFERENCES`] says why.
use crate::{topic, TopicKind, MAX_FRAME_BYTES};
use libp2p::gossipsub;
use quai_primitives::{decode_hex, Hash};
use std::time::Duration;

/// go-quai `NewGossipSubManager`: `cfg.D = 30`.
pub const MESH_N: usize = 30;
/// `cfg.Dlo = 6`.
pub const MESH_N_LOW: usize = 6;
/// `cfg.Dhi = 45`.
pub const MESH_N_HIGH: usize = 45;
/// go-quai sets `cfg.Dout = 20`, above `Dlo`. go-libp2p-pubsub documents
/// `Dout < Dlo` and `Dout <= D/2` but does not check it, and rust-libp2p 0.49
/// checks those rules only for per-topic parameters, so both run with 20: the
/// heartbeat grafts outbound peers while the mesh has fewer than 20.
pub const MESH_OUTBOUND_MIN: usize = 20;
/// go-libp2p-pubsub `TimeCacheDuration`: seen message IDs are kept 120 s.
pub const SEEN_TTL: Duration = Duration::from_secs(120);

/// The go-quai settings that rust-libp2p cannot take unchanged.
pub const GO_DIFFERENCES: &[(&str, &str, &str)] = &[
    (
        "Dscore",
        "4",
        "not used: rust-libp2p keeps no score-based mesh quota without peer scoring, and Go scores no peers",
    ),
    (
        "PrunePeers",
        "16 (PX off)",
        "0: no peer-exchange dials; Go never sends PX because go-quai does not enable it",
    ),
    (
        "Connectors/MaxPendingConnections/ConnectionTimeout",
        "8/128/30s",
        "not used: they serve PX connections, which neither side makes",
    ),
    (
        "IDONTWANT",
        "not in v0.10.0",
        "rust-libp2p speaks /meshsub/1.2.0 only to 1.2 peers; Go peers negotiate /meshsub/1.1.0",
    ),
];

/// The GossipSub configuration closest to go-quai's. `validate`: every
/// message waits for the application verdict before it is forwarded.
pub fn config(validate: bool) -> Result<gossipsub::Config, String> {
    let mut b = gossipsub::ConfigBuilder::default();
    b.validation_mode(gossipsub::ValidationMode::Strict)
        .max_transmit_size(MAX_FRAME_BYTES)
        .message_id_fn(go_message_id)
        .mesh_n(MESH_N)
        .mesh_n_low(MESH_N_LOW)
        .mesh_n_high(MESH_N_HIGH)
        .mesh_outbound_min(MESH_OUTBOUND_MIN)
        .gossip_lazy(6)
        .gossip_factor(0.25)
        .history_length(5)
        .history_gossip(3)
        .heartbeat_initial_delay(Duration::from_millis(100))
        .heartbeat_interval(Duration::from_secs(1))
        .fanout_ttl(Duration::from_secs(60))
        .prune_peers(0)
        .prune_backoff(Duration::from_secs(60))
        .unsubscribe_backoff(10)
        .opportunistic_graft_ticks(60)
        .opportunistic_graft_peers(2)
        .graft_flood_threshold(Duration::from_secs(10))
        .max_ihave_length(5000)
        .max_ihave_messages(10)
        .iwant_followup_time(Duration::from_secs(3))
        .gossip_retransimission(3)
        .check_explicit_peers_ticks(300)
        .duplicate_cache_time(SEEN_TTL)
        .flood_publish(true);
    if validate {
        b.validate_messages();
    }
    b.build().map_err(|e| e.to_string())
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

/// The topics a go-quai node of slice `[region, zone]` subscribes to
/// (`cmd/utils/hierarchical_coordinator.go`, `startNode`, for a node that
/// processes state): prime and region headers and blocks; zone headers,
/// workshares, blocks and AuxPoW templates.
pub fn slice_topics(slice: [u8; 2]) -> Vec<(Vec<u8>, TopicKind)> {
    let mut v = vec![];
    for location in [vec![], vec![slice[0]]] {
        v.push((location.clone(), TopicKind::Headers));
        v.push((location, TopicKind::Blocks));
    }
    for kind in [
        TopicKind::Headers,
        TopicKind::Workshares,
        TopicKind::Blocks,
        TopicKind::AuxTemplate,
    ] {
        v.push((slice.to_vec(), kind));
    }
    v
}
/// Topic names for [`slice_topics`].
pub fn slice_topic_names(genesis: &Hash, slice: [u8; 2]) -> Result<Vec<String>, String> {
    slice_topics(slice)
        .into_iter()
        .map(|(location, kind)| topic(genesis, &location, kind).map_err(|e| e.to_string()))
        .collect()
}

/// A topic name taken apart as go-quai `TopicFromString` does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedTopic {
    pub genesis: Hash,
    pub location: Vec<u8>,
    pub kind: TopicKind,
}
impl ParsedTopic {
    /// 0 prime, 1 region, 2 zone.
    pub fn context(&self) -> usize {
        self.location.len()
    }
}
/// go-quai `TopicFromString`: `<genesis>/<region,zone>/<type>` with a 66-character
/// non-zero genesis hash, at most two location parts below 16 each, and a
/// supported type. The transactions type is not supported.
pub fn parse_topic(name: &str) -> Result<ParsedTopic, String> {
    let parts: Vec<&str> = name.split('/').collect();
    if parts.len() < 3 {
        return Err("topic string should have 3 parts".into());
    }
    if parts[0].len() != 66 {
        return Err("invalid genesis hash".into());
    }
    let genesis: Hash = decode_hex(parts[0])
        .map_err(|e| format!("genesis hash: {e}"))?
        .try_into()
        .map_err(|_| "invalid genesis hash")?;
    if genesis == [0; 32] {
        return Err("invalid genesis hash".into());
    }
    let mut location = vec![];
    if !parts[1].is_empty() {
        let numbers: Vec<&str> = parts[1].split(',').collect();
        if numbers.len() > 2 {
            return Err("invalid location encoding".into());
        }
        for n in numbers {
            let v: u8 = n.parse().map_err(|_| "invalid location number")?;
            if v > 15 {
                return Err("location out of range".into());
            }
            location.push(v);
        }
    }
    let kind = match TopicKind::from_suffix(parts[2]) {
        Some(TopicKind::Transactions) | None => return Err("data type not supported".into()),
        Some(k) => k,
    };
    Ok(ParsedTopic {
        genesis,
        location,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::public::COLOSSEUM_TOPIC_GENESIS;
    use libp2p::identity;

    fn genesis() -> Hash {
        decode_hex(COLOSSEUM_TOPIC_GENESIS)
            .unwrap()
            .try_into()
            .unwrap()
    }

    #[test]
    fn config_is_valid_and_close_to_go() {
        let c = config(true).unwrap();
        assert_eq!((c.mesh_n(), c.mesh_n_low(), c.mesh_n_high()), (30, 6, 45));
        assert_eq!(c.mesh_outbound_min(), 20);
        assert_eq!(c.max_transmit_size(), 3 << 20);
        assert_eq!(c.heartbeat_interval(), Duration::from_secs(1));
        assert_eq!(c.history_length(), 5);
        assert_eq!(c.history_gossip(), 3);
        assert_eq!(c.duplicate_cache_time(), Duration::from_secs(120));
        assert!(c.validate_messages());
        assert!(!config(false).unwrap().validate_messages());
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
        // Unsigned messages (no from/seqno) get the empty ID, as in Go.
        let anonymous = gossipsub::Message {
            source: None,
            sequence_number: None,
            ..message
        };
        assert_eq!(go_message_id(&anonymous), gossipsub::MessageId::new(&[]));
    }

    #[test]
    fn slice_topics_are_the_go_names() {
        // The eight topics a go-quai v0.57.0 colosseum node of slice [0 0]
        // creates (its log: "Creating topic", 2026-11-17).
        let g = COLOSSEUM_TOPIC_GENESIS;
        let want = [
            format!("{g}//headers"),
            format!("{g}//blocks"),
            format!("{g}/0/headers"),
            format!("{g}/0/blocks"),
            format!("{g}/0,0/headers"),
            format!("{g}/0,0/worksharev2"),
            format!("{g}/0,0/blocks"),
            format!("{g}/0,0/auxtemplate"),
        ];
        assert_eq!(slice_topic_names(&genesis(), [0, 0]).unwrap(), want);
        assert_eq!(
            topic(&genesis(), &[1, 2], TopicKind::Transactions).unwrap(),
            format!("{g}/1,2/transactions")
        );
        for name in &want {
            let t = parse_topic(name).unwrap();
            assert_eq!(t.genesis, genesis());
            assert_eq!(topic(&t.genesis, &t.location, t.kind).unwrap(), *name);
        }
        assert_eq!(parse_topic(&want[6]).unwrap().context(), 2);
        assert_eq!(parse_topic(&want[1]).unwrap().context(), 0);
        // TopicFromString refusals.
        for bad in [
            format!("{g}/0,0/transactions"),
            format!("{g}/0,0/unknown"),
            format!("{g}/0,0,0/blocks"),
            format!("{g}/16/blocks"),
            "0x00/0,0/blocks".to_string(),
            format!("0x{}/0,0/blocks", "00".repeat(32)),
            "only/two".to_string(),
        ] {
            assert!(parse_topic(&bad).is_err(), "{bad}");
        }
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
}
