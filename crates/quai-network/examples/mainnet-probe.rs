//! Read-only interoperation probe against go-quai v0.57.0 colosseum (mainnet)
//! peers, for ROADMAP Phase 2 step 2 (public P2P).
//!
//!   cargo run -p quai-network --features native-transport --example mainnet-probe -- local
//!   cargo run -p quai-network --features native-transport --example mainnet-probe -- public
//!
//! `local` dials only the local go-quai node on loopback
//! (scripts/mainnet/go_node.sh). `public` dials the colosseum boot peers, keeps
//! at most 8 connections, stops after a few minutes and refuses to start when
//! the default gateway is an iPhone hotspot (172.20.10.x), the same rule as
//! scripts/mainnet/node_guard.sh (ALLOW_METERED=1 overrides it).
//!
//! Both modes compare every returned block with the canonical hashes from the
//! local node's JSON-RPC. The probe never publishes gossip, never sends
//! transactions and refuses every inbound request (see `quai_network::public`).
//!
//! Options: --peer MULTIADDR (local target), --rpc-host HOST,
//! --gossip-secs N, --gossip-messages N, --deadline-secs N.
use prost::Message;
use quai_consensus::verifier::PINNED_SCHEDULE as SCHEDULE;
use quai_network::{
    public::{
        exchange, topic_provider_key, DialPolicy, Event, ReadOnlyPeer, COLOSSEUM_BOOTNODES,
        COLOSSEUM_TOPIC_GENESIS, KAD_PROTOCOL, MAX_PEERS,
    },
    topic,
    transport::WireStream,
    RequestBook, TopicKind, STREAM_PROTOCOL,
};
use quai_primitives::{
    auxiliary::canonical_pow,
    decode_hex, encode_hex,
    schema::{
        self,
        block::{ProtoWorkObject, ProtoWorkObjectBlockView, ProtoWorkObjectShareView},
        common::{ProtoHash, ProtoLocation},
        quaiprotocol::{
            quai_request_message as rq, quai_response_message as rs, QuaiRequestMessage,
        },
    },
    work::{Context, WorkObject},
    Hash, Header,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Res<T> = Result<T, Box<dyn Error + Send + Sync>>;

const LOCAL_PEER: &str =
    "/ip4/127.0.0.1/tcp/4002/p2p/12D3KooWShkvuVk9997YBBtqNdEHdsgzeiZ9DpFsktoxjKVMnRdJ";
/// go-quai cmd/utils/flags.go GetHttpPort for slice [0 0]: 9001, 9001+1+0,
/// 9001+199+20*0+0.
const CONTEXTS: [(&str, &[u8], u16, Context); 3] = [
    ("prime", &[], 9001, Context::Prime),
    ("region", &[0], 9002, Context::Region),
    ("zone", &[0, 0], 9200, Context::Zone),
];
/// go-quai p2p/protocol C_NumPrimeBlocksToDownload.
const RANGE_BLOCKS: u64 = 10;

fn emit(v: Value) {
    println!("{v}");
}

struct Args {
    public: bool,
    peer: String,
    rpc_host: String,
    gossip_secs: u64,
    gossip_messages: usize,
    deadline_secs: u64,
}
fn parse_args() -> Res<Args> {
    let mut it = std::env::args().skip(1);
    let mode = it.next().unwrap_or_else(|| "local".into());
    let public = match mode.as_str() {
        "local" => false,
        "public" => true,
        _ => return Err("usage: mainnet-probe local|public [--peer A] [--rpc-host H] [--gossip-secs N] [--gossip-messages N] [--deadline-secs N]".into()),
    };
    let mut args = Args {
        public,
        peer: LOCAL_PEER.into(),
        rpc_host: "127.0.0.1".into(),
        gossip_secs: if public { 90 } else { 60 },
        gossip_messages: 4,
        deadline_secs: if public { 240 } else { 180 },
    };
    while let Some(flag) = it.next() {
        let value = it.next().ok_or("missing option value")?;
        match flag.as_str() {
            "--peer" => args.peer = value,
            "--rpc-host" => args.rpc_host = value,
            "--gossip-secs" => args.gossip_secs = value.parse()?,
            "--gossip-messages" => args.gossip_messages = value.parse()?,
            "--deadline-secs" => args.deadline_secs = value.parse()?,
            _ => return Err(format!("unknown option {flag}").into()),
        }
    }
    Ok(args)
}

/// The same metered-link rule as scripts/mainnet/node_guard.sh. It fails
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
        return Err(format!(
            "default gateway {gateway} is an iPhone hotspot (metered); use the local mode"
        )
        .into());
    }
    Ok(gateway)
}

/// Minimal JSON-RPC client for the local go-quai node (HTTP/1.0, no chunking).
async fn rpc(host: &str, port: u16, method: &str, params: Value) -> Res<Value> {
    let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string();
    let mut s = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect((host, port)),
    )
    .await??;
    let request = format!(
        "POST / HTTP/1.0\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(request.as_bytes()).await?;
    let mut raw = vec![];
    tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut raw)).await??;
    let text = String::from_utf8(raw)?;
    let (head, body) = text.split_once("\r\n\r\n").ok_or("malformed HTTP reply")?;
    if !head.starts_with("HTTP/1.") || !head.lines().next().unwrap_or("").contains(" 200 ") {
        return Err(format!("RPC {method}: {}", head.lines().next().unwrap_or("")).into());
    }
    let mut v: Value = serde_json::from_str(body)?;
    if let Some(e) = v.get("error") {
        return Err(format!("RPC {method}: {e}").into());
    }
    Ok(v["result"].take())
}
fn hex_u64(v: &Value) -> Res<u64> {
    let s = v.as_str().ok_or("expected hex quantity")?;
    Ok(u64::from_str_radix(
        s.strip_prefix("0x").ok_or("hex prefix")?,
        16,
    )?)
}
async fn rpc_head(host: &str, port: u16) -> Res<u64> {
    hex_u64(&rpc(host, port, "quai_blockNumber", json!([])).await?)
}
async fn rpc_hash(host: &str, port: u16, n: u64) -> Res<Hash> {
    let block = rpc(
        host,
        port,
        "quai_getBlockByNumber",
        json!([format!("0x{n:x}"), false]),
    )
    .await?;
    let hash = block["hash"].as_str().ok_or("block without hash")?;
    Ok(decode_hex(hash)?.try_into().map_err(|_| "hash length")?)
}

fn be_u64(bytes: &[u8]) -> Result<u64, String> {
    if bytes.len() > 8 {
        return Err("number exceeds u64".into());
    }
    Ok(bytes.iter().fold(0, |n, b| (n << 8) | u64::from(*b)))
}
fn number_bytes(n: u64) -> Vec<u8> {
    // Go big.Int.Bytes(): minimal big-endian, empty for zero.
    let b = n.to_be_bytes();
    b[b.iter().position(|v| *v != 0).unwrap_or(8)..].to_vec()
}

struct Decoded {
    hash: Hash,
    number: u64,
    txs: usize,
    pow_id: Option<u32>,
}
/// Decode with quai-primitives. `full` checks every body commitment for the
/// context (block view); otherwise only the body-header binding (header view
/// carries no transactions or manifest).
fn decode_object(wo: &ProtoWorkObject, context: Context, full: bool) -> Result<Decoded, String> {
    let object = WorkObject::decode_auxiliary(&wo.encode_to_vec(), SCHEDULE)?;
    let header = object.auxiliary_work_header(SCHEDULE)?;
    if full {
        object.verify_commitments(context)?;
    } else if header
        .wire()
        .header_hash
        .as_ref()
        .map(|h| h.value.as_slice())
        != Some(object.header().hash().as_slice())
    {
        return Err("body-header commitment mismatch".into());
    }
    let body = &object.header().wire().number;
    let number = match context {
        Context::Zone => be_u64(header.wire().number.as_deref().unwrap_or_default())?,
        Context::Prime => be_u64(&body[0])?,
        Context::Region => be_u64(&body[1])?,
    };
    let pow_id = match &header.wire().aux_pow {
        Some(aux) => canonical_pow(aux)?.chain_id,
        None => None,
    };
    Ok(Decoded {
        hash: header.block_hash()?,
        number,
        txs: object.transactions().len(),
        pow_id,
    })
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Hash,
    Block,
    Header,
    Range,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Hash => "block_hash",
            Self::Block => "block",
            Self::Header => "header",
            Self::Range => "blocks",
        }
    }
    fn request(self) -> rq::Request {
        match self {
            Self::Hash => rq::Request::BlockHash(ProtoHash::default()),
            Self::Block => rq::Request::WorkObjectBlock(Default::default()),
            Self::Header => rq::Request::WorkObjectHeader(Default::default()),
            Self::Range => rq::Request::WorkObjectBlocks(Default::default()),
        }
    }
}
enum Query {
    Number(u64),
    Hash(Hash),
}

struct Session<'a> {
    probe: &'a ReadOnlyPeer,
    peer: libp2p::PeerId,
    stream: Option<WireStream>,
    book: RequestBook,
    next_id: u32,
    checks: Vec<Value>,
}
impl Session<'_> {
    async fn ask(
        &mut self,
        location: &[u8],
        query: &Query,
        kind: Kind,
    ) -> Res<(rs::Response, usize)> {
        if self.stream.is_none() {
            self.stream = Some(self.probe.open(self.peer).await?);
        }
        self.next_id = self.next_id.wrapping_add(1);
        let request = QuaiRequestMessage {
            id: self.next_id,
            location: Some(ProtoLocation {
                value: location.to_vec(),
            }),
            data: Some(match query {
                Query::Number(n) => rq::Data::Number(number_bytes(*n)),
                Query::Hash(h) => rq::Data::Hash(ProtoHash { value: h.to_vec() }),
            }),
            request: Some(kind.request()),
        };
        let size = request.encoded_len();
        let stream = self.stream.as_mut().unwrap();
        match exchange(stream, &mut self.book, request).await {
            Ok(reply) => Ok((reply.response.ok_or("response without payload")?, size)),
            Err(e) => {
                // A failed stream is poisoned; open a new one for the next request.
                self.stream = None;
                Err(e.into())
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    async fn check(
        &mut self,
        context: (&str, &[u8], Context),
        query: Query,
        kind: Kind,
        expected: &[Hash],
        first: u64,
        expect_empty: bool,
    ) {
        let (name, location, ctx) = context;
        let started = Instant::now();
        let (by, n) = match &query {
            Query::Number(n) => ("number", json!(n)),
            Query::Hash(h) => ("hash", json!(encode_hex(h))),
        };
        let outcome: Result<Value, String> = async {
            let (response, _) = self
                .ask(location, &query, kind)
                .await
                .map_err(|e| e.to_string())?;
            let objects: Vec<Option<ProtoWorkObject>> = match (kind, response) {
                (Kind::Hash, rs::Response::BlockHash(h)) => {
                    if expect_empty {
                        return if h.value.is_empty() {
                            Ok(json!({"empty":true}))
                        } else {
                            Err("expected an empty reply".into())
                        };
                    }
                    let got = encode_hex(&h.value);
                    return if h.value.as_slice() == expected[0] {
                        Ok(json!({"hash":got}))
                    } else {
                        Err(format!("hash {got} != {}", encode_hex(&expected[0])))
                    };
                }
                (Kind::Block, rs::Response::WorkObjectBlockView(v)) => vec![v.work_object],
                (Kind::Header, rs::Response::WorkObjectHeaderView(v)) => vec![v.work_object],
                (Kind::Range, rs::Response::WorkObjectBlocksView(v)) => {
                    v.work_objects.into_iter().map(|v| v.work_object).collect()
                }
                _ => return Err("response kind does not match the request".into()),
            };
            if expect_empty {
                return if objects.iter().all(Option::is_none) {
                    Ok(json!({"empty":true}))
                } else {
                    Err("expected an empty reply".into())
                };
            }
            if objects.len() != expected.len() {
                return Err(format!(
                    "{} objects, expected {}",
                    objects.len(),
                    expected.len()
                ));
            }
            let mut hashes = vec![];
            let mut txs = 0;
            for (i, (object, want)) in objects.iter().zip(expected).enumerate() {
                let object = object
                    .as_ref()
                    .ok_or("peer replied empty (block not found)")?;
                let d = decode_object(object, ctx, !matches!(kind, Kind::Header))?;
                if d.hash != *want {
                    return Err(format!(
                        "hash {} != {}",
                        encode_hex(&d.hash),
                        encode_hex(want)
                    ));
                }
                if d.number != first + i as u64 {
                    return Err(format!("number {} != {}", d.number, first + i as u64));
                }
                txs += d.txs;
                hashes.push(encode_hex(&d.hash));
            }
            Ok(json!({"hashes":hashes,"txs":txs}))
        }
        .await;
        let mut v = json!({
            "check":"request","peer":self.peer.to_string(),"context":name,"kind":kind.name(),
            "by":by,"query":n,"ok":outcome.is_ok(),"ms":started.elapsed().as_millis() as u64,
        });
        match outcome {
            Ok(detail) => v["detail"] = detail,
            Err(e) => v["error"] = json!(e),
        }
        emit(v.clone());
        self.checks.push(v);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn request_suite(
    probe: &ReadOnlyPeer,
    peer: libp2p::PeerId,
    host: &str,
    heads: &[u64],
    local: bool,
) -> Res<Vec<Value>> {
    let mut s = Session {
        probe,
        peer,
        stream: None,
        book: RequestBook::new(1)?,
        next_id: 0,
        checks: vec![],
    };
    for ((name, location, port, ctx), head) in CONTEXTS.into_iter().zip(heads) {
        let c = (name, location, ctx);
        let mut numbers = vec![1, head / 2, head.saturating_sub(1)];
        numbers.retain(|n| *n >= 1);
        numbers.dedup();
        for n in numbers {
            let want = rpc_hash(host, port, n).await?;
            s.check(c, Query::Number(n), Kind::Hash, &[want], n, false)
                .await;
            s.check(c, Query::Number(n), Kind::Block, &[want], n, false)
                .await;
            s.check(c, Query::Number(n), Kind::Header, &[want], n, false)
                .await;
            s.check(c, Query::Hash(want), Kind::Block, &[want], n, false)
                .await;
            s.check(c, Query::Hash(want), Kind::Header, &[want], n, false)
                .await;
        }
        if *head > RANGE_BLOCKS {
            let mut want = vec![];
            for n in 1..=RANGE_BLOCKS {
                want.push(rpc_hash(host, port, n).await?);
            }
            s.check(c, Query::Hash(want[0]), Kind::Range, &want, 1, false)
                .await;
        }
        let unknown = [0x5a; 32];
        s.check(c, Query::Hash(unknown), Kind::Block, &[], 0, true)
            .await;
        if local {
            // The local node is still syncing: far-future numbers are absent.
            let n = head + 1_000_000;
            s.check(c, Query::Number(n), Kind::Block, &[], 0, true)
                .await;
            s.check(c, Query::Number(n), Kind::Hash, &[], 0, true).await;
        }
    }
    if let Some(mut stream) = s.stream.take() {
        let _ = stream.close().await;
    }
    Ok(s.checks)
}

fn decode_gossip(kind: TopicKind, data: &[u8]) -> Result<Value, String> {
    match kind {
        TopicKind::Blocks => {
            schema::preflight(".block.ProtoWorkObjectBlockView", data)?;
            let view = ProtoWorkObjectBlockView::decode(data).map_err(|e| e.to_string())?;
            let wo = view.work_object.ok_or("empty block view")?;
            let d = decode_object(&wo, Context::Zone, true)?;
            Ok(json!({"hash":encode_hex(&d.hash),"number":d.number,"txs":d.txs,"pow_id":d.pow_id}))
        }
        TopicKind::Workshares => {
            schema::preflight(".block.ProtoWorkObjectShareView", data)?;
            let view = ProtoWorkObjectShareView::decode(data).map_err(|e| e.to_string())?;
            let wo = view.work_object.ok_or("empty workshare view")?;
            let raw = wo
                .wo_header
                .as_ref()
                .ok_or("missing work header")?
                .encode_to_vec();
            let header = Header::decode_auxiliary(&raw, SCHEDULE)?;
            let pow_id = match &header.wire().aux_pow {
                Some(aux) => canonical_pow(aux)?.chain_id,
                None => None,
            };
            let txs = wo
                .wo_body
                .as_ref()
                .and_then(|b| b.transactions.as_ref())
                .map_or(0, |t| t.transactions.len());
            Ok(json!({
                "hash":encode_hex(&header.block_hash()?),
                "number":be_u64(header.wire().number.as_deref().unwrap_or_default())?,
                "txs":txs,"pow_id":pow_id,
            }))
        }
        _ => Err("topic kind not probed".into()),
    }
}

#[derive(Default)]
struct State {
    connected: BTreeMap<libp2p::PeerId, String>,
    identified: BTreeMap<libp2p::PeerId, Value>,
    subscribed: BTreeMap<String, BTreeSet<libp2p::PeerId>>,
    gossip: BTreeMap<String, Vec<Value>>,
    gossip_seen: BTreeMap<String, usize>,
    kad: Vec<Value>,
    kad_done: BTreeSet<&'static str>,
    dial_failures: Vec<Value>,
    disconnects: Vec<Value>,
    max_connected: usize,
}

fn collect(
    probe: Arc<ReadOnlyPeer>,
    state: Arc<Mutex<State>>,
    topics: BTreeMap<String, TopicKind>,
    limit: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Ok(event) = probe.next().await {
            let mut st = state.lock().unwrap();
            match event {
                Event::Connected { peer, address } => {
                    emit(
                        json!({"event":"connected","peer":peer.to_string(),"address":address.to_string()}),
                    );
                    st.connected.insert(peer, address.to_string());
                    st.max_connected = st.max_connected.max(st.connected.len());
                }
                Event::Disconnected { peer, cause } => {
                    let v = json!({"event":"disconnected","peer":peer.to_string(),"cause":cause});
                    emit(v.clone());
                    st.connected.remove(&peer);
                    st.disconnects.push(v);
                }
                Event::DialFailed { peer, error } => {
                    let v = json!({"event":"dial_failed","peer":peer.map(|p| p.to_string()),"error":error});
                    emit(v.clone());
                    st.dial_failures.push(v);
                }
                Event::DeadConnection { peer, error } => {
                    emit(json!({"event":"dead_connection","peer":peer.to_string(),"error":error}));
                }
                Event::Identified { peer, info } => {
                    let protocols: Vec<String> =
                        info.protocols.iter().map(|p| p.to_string()).collect();
                    let v = json!({
                        "agent":info.agent_version,"protocol_version":info.protocol_version,
                        "protocols":protocols,"listen_addrs":info.listen_addrs.len(),
                        "observed_addr":info.observed_addr.to_string(),
                    });
                    emit(json!({"event":"identified","peer":peer.to_string(),"info":v}));
                    st.identified.insert(peer, v);
                }
                Event::Subscribed { peer, topic } => {
                    if topics.contains_key(&topic) {
                        emit(
                            json!({"event":"remote_subscribed","peer":peer.to_string(),"topic":topic}),
                        );
                    }
                    st.subscribed.entry(topic).or_default().insert(peer);
                }
                Event::Gossip {
                    peer,
                    source,
                    topic,
                    data,
                    ..
                } => {
                    *st.gossip_seen.entry(topic.clone()).or_default() += 1;
                    let Some(kind) = topics.get(&topic) else {
                        continue;
                    };
                    if st.gossip.get(&topic).map_or(0, Vec::len) >= limit {
                        continue;
                    }
                    let mut v = json!({
                        "event":"gossip","topic":topic,"from":peer.to_string(),
                        "source":source.map(|p| p.to_string()),"bytes":data.len(),
                    });
                    match decode_gossip(*kind, &data) {
                        Ok(d) => {
                            v["ok"] = json!(true);
                            v["decoded"] = d;
                        }
                        Err(e) => {
                            v["ok"] = json!(false);
                            v["error"] = json!(e);
                        }
                    }
                    emit(v.clone());
                    st.gossip.entry(topic).or_default().push(v);
                }
                Event::Kad(p) => {
                    let v = json!({
                        "event":"kad","query":p.query,"peers":p.peers.len(),
                        "peer_ids":p.peers.iter().take(8).map(|p| p.to_string()).collect::<Vec<_>>(),
                        "last":p.last,"requests":p.requests,"successes":p.successes,
                        "failures":p.failures,"error":p.error,
                    });
                    emit(v.clone());
                    if p.last {
                        st.kad_done.insert(p.query);
                    }
                    st.kad.push(v);
                }
                // The read-only profile has no slots, relays or NAT checks.
                _ => {}
            }
        }
    })
}

async fn wait_until(deadline: Instant, mut ready: impl FnMut() -> bool) -> bool {
    while Instant::now() < deadline {
        if ready() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    ready()
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Res<()> {
    let args = parse_args()?;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(args.deadline_secs);
    let gateway = if args.public {
        Some(check_gateway()?)
    } else {
        None
    };
    let mut heads = vec![];
    for (_, _, port, _) in CONTEXTS {
        heads.push(rpc_head(&args.rpc_host, port).await?);
    }
    let policy = if args.public {
        DialPolicy::Public
    } else {
        DialPolicy::Loopback
    };
    let targets: Vec<libp2p::Multiaddr> = if args.public {
        COLOSSEUM_BOOTNODES
            .iter()
            .map(|a| a.parse())
            .collect::<Result<_, _>>()?
    } else {
        vec![args.peer.parse()?]
    };
    let target_ids: Vec<libp2p::PeerId> = targets
        .iter()
        .map(|a| {
            a.iter()
                .find_map(|p| match p {
                    libp2p::multiaddr::Protocol::P2p(id) => Some(id),
                    _ => None,
                })
                .ok_or("target needs a /p2p/ peer ID")
        })
        .collect::<Result<_, _>>()?;
    let probe = Arc::new(ReadOnlyPeer::start(policy)?);
    emit(json!({
        "event":"start","mode":if args.public {"public"} else {"local"},"peer_id":probe.id.to_string(),
        "policy":format!("{policy:?}"),"max_peers":MAX_PEERS,"gateway":gateway,
        "rpc_heads":{"prime":heads[0],"region":heads[1],"zone":heads[2]},
        "targets":targets.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
    }));
    // Subscribe before dialing, so the first RPC to each peer carries it.
    let genesis: Hash = decode_hex(COLOSSEUM_TOPIC_GENESIS)?
        .try_into()
        .map_err(|_| "genesis length")?;
    let mut topics = BTreeMap::new();
    for kind in [TopicKind::Blocks, TopicKind::Workshares] {
        let name = topic(&genesis, &[0, 0], kind)?;
        probe.subscribe(name.clone()).await?;
        topics.insert(name, kind);
    }
    emit(json!({"event":"subscribed","topics":topics.keys().collect::<Vec<_>>()}));
    let state = Arc::new(Mutex::new(State::default()));
    let collector = collect(
        probe.clone(),
        state.clone(),
        topics.clone(),
        args.gossip_messages,
    );

    // Dial one target at a time: the pending-dial limit is 2.
    for (address, id) in targets.iter().zip(&target_ids) {
        for attempt in 0..3 {
            if let Err(e) = probe.dial(address.clone()).await {
                emit(
                    json!({"event":"dial_error","target":address.to_string(),"error":e.to_string()}),
                );
                break;
            }
            let until = Instant::now() + Duration::from_secs(15);
            if wait_until(until.min(deadline), || probe.is_connected(id)).await {
                break;
            }
            emit(json!({"event":"dial_retry","target":address.to_string(),"attempt":attempt + 1}));
        }
    }
    let connected: Vec<libp2p::PeerId> = target_ids
        .iter()
        .copied()
        .filter(|id| probe.is_connected(id))
        .collect();
    // Identify arrives shortly after the connection.
    let until = (Instant::now() + Duration::from_secs(10)).min(deadline);
    wait_until(until, || {
        let st = state.lock().unwrap();
        connected.iter().all(|id| st.identified.contains_key(id))
    })
    .await;

    // Kademlia: seed only the connected targets (manual routing table), then
    // ask for the providers of the zone block topic and for our own neighbours.
    for (address, id) in targets.iter().zip(&target_ids) {
        if connected.contains(id) {
            let bare: libp2p::Multiaddr = address
                .iter()
                .filter(|p| !matches!(p, libp2p::multiaddr::Protocol::P2p(_)))
                .collect();
            probe.kad_add(*id, bare).await?;
        }
    }
    let blocks_topic = topic(&genesis, &[0, 0], TopicKind::Blocks)?;
    if !connected.is_empty() {
        probe.providers(topic_provider_key(&blocks_topic)).await?;
        probe.closest_peers(probe.id.to_bytes()).await?;
    }

    // Stream protocol: the Go request types in each context.
    let mut checks = vec![];
    for id in &connected {
        let supports = state
            .lock()
            .unwrap()
            .identified
            .get(id)
            .and_then(|v| v["protocols"].as_array().cloned())
            .is_some_and(|p| p.iter().any(|p| p == STREAM_PROTOCOL));
        if !supports {
            emit(
                json!({"event":"skip_requests","peer":id.to_string(),"reason":"no /quai/1.0.0 in identify"}),
            );
            continue;
        }
        match tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            request_suite(&probe, *id, &args.rpc_host, &heads, !args.public),
        )
        .await
        {
            Ok(Ok(c)) => checks.extend(c),
            Ok(Err(e)) => {
                emit(json!({"event":"suite_error","peer":id.to_string(),"error":e.to_string()}))
            }
            Err(_) => emit(json!({"event":"suite_error","peer":id.to_string(),"error":"deadline"})),
        }
    }

    // Gossip: wait for the first messages on both topics, then for Kademlia.
    let until = (Instant::now() + Duration::from_secs(args.gossip_secs)).min(deadline);
    wait_until(until, || {
        let st = state.lock().unwrap();
        topics
            .keys()
            .all(|t| st.gossip.get(t).map_or(0, Vec::len) >= args.gossip_messages)
    })
    .await;
    let until = (Instant::now() + Duration::from_secs(25)).min(deadline);
    wait_until(until, || {
        state.lock().unwrap().kad_done.len() >= 2 || connected.is_empty()
    })
    .await;
    collector.abort();

    let st = state.lock().unwrap();
    let passed = checks.iter().filter(|c| c["ok"] == true).count();
    let mut by_kind: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for c in &checks {
        let key = format!(
            "{}/{}/{}",
            c["context"].as_str().unwrap_or(""),
            c["kind"].as_str().unwrap_or(""),
            c["by"].as_str().unwrap_or("")
        );
        let e = by_kind.entry(key).or_default();
        e.0 += 1;
        e.1 += usize::from(c["ok"] == true);
    }
    let gossip: BTreeMap<&String, Value> = topics
        .keys()
        .map(|t| {
            let decoded = st.gossip.get(t).map_or(vec![], Clone::clone);
            let ok = decoded.iter().filter(|v| v["ok"] == true).count();
            (
                t,
                json!({
                    "received":st.gossip_seen.get(t).copied().unwrap_or(0),
                    "decoded":decoded.len(),"decoded_ok":ok,
                    "remote_subscribers":st.subscribed.get(t).map_or(0, BTreeSet::len),
                }),
            )
        })
        .collect();
    let kad_ok = st
        .kad
        .iter()
        .any(|v| v["successes"].as_u64().unwrap_or(0) > 0);
    let providers: BTreeSet<String> = st
        .kad
        .iter()
        .filter(|v| v["query"] == "providers")
        .flat_map(|v| v["peer_ids"].as_array().cloned().unwrap_or_default())
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let identified = connected
        .iter()
        .filter(|id| st.identified.contains_key(id))
        .count();
    let kad_supported = st
        .identified
        .values()
        .filter(|v| {
            v["protocols"]
                .as_array()
                .is_some_and(|p| p.iter().any(|p| p == KAD_PROTOCOL))
        })
        .count();
    // Every identified peer, and whether it announced a probe topic: the
    // Kademlia protocol ID is shared by all Quai networks.
    let peers: BTreeMap<String, Value> = st
        .identified
        .iter()
        .map(|(id, v)| {
            let on_topic = topics
                .keys()
                .any(|t| st.subscribed.get(t).is_some_and(|s| s.contains(id)));
            (
                id.to_string(),
                json!({"agent":v["agent"],"colosseum_topics":on_topic,"target":target_ids.contains(id)}),
            )
        })
        .collect();
    let ok = !connected.is_empty()
        && identified == connected.len()
        && passed == checks.len()
        && !checks.is_empty();
    emit(json!({
        "summary":{
            "mode":if args.public {"public"} else {"local"},
            "ok":ok,
            "targets":targets.len(),"connected":connected.len(),"identified":identified,
            "kad_protocol_advertised":kad_supported,"kad_query_success":kad_ok,
            "zone_block_topic_providers":providers,
            "requests":{"total":checks.len(),"passed":passed,"by_context_kind":by_kind.iter().map(|(k,(t,p))| (k.clone(), json!(format!("{p}/{t}")))).collect::<serde_json::Map<_,_>>()},
            "gossip":gossip,
            "max_simultaneous_connections":st.max_connected,"peers":peers,
            "dial_failures":st.dial_failures.len(),"disconnects":st.disconnects,
            "seconds":started.elapsed().as_secs_f64(),
        }
    }));
    drop(st);
    if ok {
        Ok(())
    } else {
        Err("probe checks failed; see the summary".into())
    }
}
