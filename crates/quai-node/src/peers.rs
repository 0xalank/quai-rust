//! Zone heights from go-quai peers for `quai-node run`: a [`BlockSource`]
//! over `/quai/1.0.0` (`quai_network::public::PublicNode`, read-only).
//!
//! The fetch logic is the one of `crates/quai-network/examples/peer-export.rs`
//! (that example keeps its own copy and CLI): each peer is probed for its
//! prime, region and zone heads; ten-block batches by number (BlockHash, then
//! WorkObjectBlocks) are fetched ahead of the processor in each context;
//! every block is decoded and checked as the Go exporter does (hash
//! recomputed from the bytes, body roots, hash links in all three contexts,
//! dominant parents), and a height whose records break a chain rule is
//! fetched again from another peer. A peer that fails repeatedly is dropped.
//!
//! go-quai answers WorkObjectBlocks only with all ten blocks
//! (`GetWorkObjectsFrom` returns nothing when one is missing), so a batch is
//! asked only of a peer whose head in that context covers its last block.
//! A peer that lacks a prime or region batch near the best head is not at
//! fault: the batch waits for the heads to move.
//!
//! The source delivers zone heights in order from the processor's start
//! height, up to the best peer head less `margin` (so that it never fetches
//! a block that a reorganization may still remove), and then polls the
//! peers' heads. With `handoff` it ends instead at the first height that
//! needs a batch past the peers' heads, and the node follows the head
//! through gossip. Public peers are not used when the default gateway is an
//! iPhone hotspot (172.20.10.x) unless ALLOW_METERED=1.
use crate::processor::{BlockSource, Next, SourceError};
use crate::qex::{Context, Height, RawRecord};
use libp2p::{Multiaddr, PeerId};
use prost::Message;
use quai_consensus::verifier::PINNED_SCHEDULE as SCHEDULE;
use quai_network::{
    decode_envelope,
    public::{PublicNode, COLOSSEUM_BOOTNODES},
    response,
    transport::WireStream,
    RequestBook,
};
use quai_primitives::{
    encode_hex,
    schema::{
        self,
        block::ProtoWorkObject,
        common::{ProtoHash, ProtoLocation},
        quaiprotocol::{
            quai_message::Payload, quai_request_message as rq, quai_response_message as rs,
            QuaiMessage, QuaiRequestMessage,
        },
    },
    work::WorkObject,
    Hash,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// go-quai `C_NumPrimeBlocksToDownload`: a WorkObjectBlocks reply holds 10 blocks.
const BATCH: u64 = 10;
const NAMES: [&str; 3] = ["prime", "region", "zone"];
const LOCATIONS: [&[u8]; 3] = [&[], &[0], &[0, 0]];
const ZONE: usize = 2;
/// Attempts per batch before the source gives up.
const MAX_ATTEMPTS: u32 = 8;
/// Consecutive faults before a peer is dropped.
const MAX_STRIKES: u32 = 6;
/// Batches fetched ahead of the processor.
const WINDOW: u64 = 64;
/// Heights queued for the processor.
const QUEUE: usize = 512;
/// Time between head probes.
const HEAD_POLL: Duration = Duration::from_secs(15);
/// First distance probed above the last known head in a head poll.
const HEAD_STEP: u64 = 16;
/// Prime and region blocks below the best peer head within which a peer
/// that lacks a batch is not at fault: a dominant head can move back in a
/// reorganization, and a head poll is up to `HEAD_POLL` old.
const HEAD_SLACK: u64 = BATCH;

fn hex(b: &[u8]) -> String {
    encode_hex(b)
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Protobuf bytes as the peer sent them.

fn varint(input: &mut &[u8]) -> Result<u64, String> {
    let mut out = 0u64;
    for shift in (0..64).step_by(7) {
        let (&b, rest) = input.split_first().ok_or("truncated varint")?;
        *input = rest;
        out |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(out);
        }
    }
    Err("varint overflow".into())
}
/// The length-delimited fields with number `field` of one message, in order.
fn delimited(mut msg: &[u8], field: u64) -> Result<Vec<&[u8]>, String> {
    let mut out = vec![];
    while !msg.is_empty() {
        let key = varint(&mut msg)?;
        match key & 7 {
            0 => {
                varint(&mut msg)?;
            }
            1 => msg = msg.get(8..).ok_or("truncated field")?,
            2 => {
                let n = usize::try_from(varint(&mut msg)?).map_err(|_| "length overflow")?;
                if n > msg.len() {
                    return Err("truncated field".into());
                }
                let (value, rest) = msg.split_at(n);
                if key >> 3 == field {
                    out.push(value);
                }
                msg = rest;
            }
            5 => msg = msg.get(4..).ok_or("truncated field")?,
            _ => return Err("unsupported protobuf wire type".into()),
        }
    }
    Ok(out)
}
/// The work object bytes of a WorkObjectBlocksView (`blocks`) or
/// WorkObjectBlockView reply frame, unchanged.
fn reply_objects(frame: &[u8], blocks: bool) -> Result<Vec<Vec<u8>>, String> {
    let reply = delimited(frame, 2)?;
    let [reply] = reply.as_slice() else {
        return Err("not one response".into());
    };
    let views = if blocks {
        let outer = delimited(reply, 5)?;
        let [outer] = outer.as_slice() else {
            return Err("not a WorkObjectBlocksView reply".into());
        };
        delimited(outer, 1)?
    } else {
        delimited(reply, 4)?
    };
    let mut out = vec![];
    for view in views {
        for object in delimited(view, 1)? {
            out.push(object.to_vec());
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// One block in one context, decoded and checked as the Go exporter does.

#[derive(Clone, Debug)]
pub struct Block {
    pub ctx: usize,
    /// `wo.NumberU64(ctx)`.
    pub number: u64,
    pub hash: Hash,
    /// `wo.ParentHash(ctx)`.
    pub parent: Hash,
    /// Prime and region parents from the body header.
    pub dom_parents: [Hash; 2],
    /// Prime, region and zone numbers of the block.
    pub numbers: [u64; 3],
    pub location: Vec<u8>,
    pub raw: Vec<u8>,
}
fn be_u64(bytes: &[u8]) -> Result<u64, String> {
    if bytes.len() > 8 {
        return Err("number exceeds u64".into());
    }
    Ok(bytes.iter().fold(0, |n, b| (n << 8) | u64::from(*b)))
}
fn h32(bytes: &[u8], what: &str) -> Result<Hash, String> {
    bytes
        .try_into()
        .map_err(|_| format!("{what}: {} bytes, want 32", bytes.len()))
}
/// Decode with quai-primitives and apply the record rules of the Go
/// exporter: the bytes decode and encode back to the same bytes, the header
/// hash binds the body header, the body matches the header roots for the
/// context, the expansion number is 0 and the location is zone [0 0] after
/// genesis.
pub fn decode_block(raw: Vec<u8>, ctx: usize) -> Result<Block, String> {
    schema::preflight(".block.ProtoWorkObject", &raw)?;
    let wire = ProtoWorkObject::decode(raw.as_slice()).map_err(|e| e.to_string())?;
    if wire.encode_to_vec() != raw {
        return Err("block bytes do not encode back to the same bytes".into());
    }
    let object = WorkObject::decode_auxiliary(&raw, SCHEDULE)?;
    let header = object.auxiliary_work_header(SCHEDULE)?;
    let hash = header.block_hash()?;
    let w = header.wire();
    let b = object.header().wire();
    let number_at = |i: usize| -> Result<u64, String> {
        be_u64(b.number.get(i).ok_or("body header number missing")?)
    };
    let numbers = [
        number_at(0)?,
        number_at(1)?,
        be_u64(w.number.as_deref().ok_or("work header number missing")?)?,
    ];
    let parent_at = |i: usize| -> Result<Hash, String> {
        h32(
            &b.parent_hash
                .get(i)
                .ok_or("body parent hash missing")?
                .value,
            "parent hash",
        )
    };
    let dom_parents = [parent_at(0)?, parent_at(1)?];
    let zone_parent = h32(
        &w.parent_hash
            .as_ref()
            .ok_or("work parent hash missing")?
            .value,
        "parent hash",
    )?;
    if let Some(e) = b.expansion_number.filter(|e| *e != 0) {
        return Err(format!("expansion number {e}: one active zone only"));
    }
    let location = w
        .location
        .as_ref()
        .map(|l| l.value.clone())
        .unwrap_or_default();
    let committed = &w
        .header_hash
        .as_ref()
        .ok_or("work header hash missing")?
        .value;
    if committed.as_slice() != object.header().hash() {
        return Err(format!(
            "work object header hash {} is not the body header hash {}",
            hex(committed),
            hex(&object.header().hash())
        ));
    }
    let root = |r: Result<Hash, String>, committed: Option<&ProtoHash>, what: &str| {
        let got = r?;
        let want = committed.ok_or(format!("{what} missing from the header"))?;
        if got.as_slice() == want.value.as_slice() {
            Ok(())
        } else {
            Err(format!(
                "{what} {}, header has {}",
                hex(&got),
                hex(&want.value)
            ))
        }
    };
    let uncles = wire
        .wo_body
        .as_ref()
        .and_then(|body| body.uncles.as_ref())
        .map_or(0, |u| u.wo_headers.len());
    if ctx == ZONE {
        root(
            object.transaction_root(),
            b.tx_hash.as_ref(),
            "transaction root",
        )?;
        root(
            object.outbound_root(),
            b.outbound_etx_hash.as_ref(),
            "outbound ETX root",
        )?;
        root(Ok(object.uncle_root()), b.uncle_hash.as_ref(), "uncle hash")?;
    } else {
        if !object.transactions().is_empty() || !object.outbound_etxs().is_empty() || uncles != 0 {
            return Err("a dominant body has transactions, outbound ETXs or uncles".into());
        }
        root(
            object.manifest_root(),
            b.manifest_hash.get(ctx + 1),
            "manifest root",
        )?;
        if ctx == 0 {
            root(
                object.interlink_root(),
                b.interlink_root_hash.as_ref(),
                "interlink root",
            )?;
        }
    }
    let number = numbers[ctx];
    if number != 0 && location != LOCATIONS[ZONE] {
        return Err(format!("location {location:?} is not zone [0 0]"));
    }
    Ok(Block {
        ctx,
        number,
        hash,
        parent: if ctx == ZONE {
            zone_parent
        } else {
            dom_parents[ctx]
        },
        dom_parents,
        numbers,
        location,
        raw,
    })
}

// ---------------------------------------------------------------------------
// The chain rules of one zone height (Go exporter `chainChecker`).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tip {
    number: u64,
    hash: Hash,
}
struct Chain {
    last: [Option<Tip>; 3],
    next_zone: u64,
    genesis: Hash,
}
impl Chain {
    /// The checker of a source that starts at zone height `start` with the
    /// heads `heads` there (prime, region, zone).
    fn new(start: u64, heads: [(Hash, u64); 3], genesis: Hash) -> Self {
        if start == 0 {
            return Self {
                last: [None; 3],
                next_zone: 0,
                genesis,
            };
        }
        Self {
            last: heads.map(|(hash, number)| Some(Tip { number, hash })),
            next_zone: start + 1,
            genesis,
        }
    }
    /// `records`: the prime, region and zone records of one zone height in
    /// that order (prime and region only when coincident).
    fn height(&mut self, records: &[&Block]) -> Result<(), String> {
        let Some((zone, dominant)) = records.split_last() else {
            return Err("no records".into());
        };
        let what = |b: &Block| format!("{} block {} ({})", NAMES[b.ctx], b.number, hex(&b.hash));
        if zone.ctx != ZONE || zone.number != self.next_zone {
            return Err(format!(
                "{}: expected zone height {}",
                what(zone),
                self.next_zone
            ));
        }
        let mut seen = [false; 2];
        for (i, r) in dominant.iter().enumerate() {
            if r.ctx >= ZONE || seen[r.ctx] || (i == 1 && r.ctx == 0) {
                return Err(format!("{}: records out of order", what(r)));
            }
            seen[r.ctx] = true;
            if r.hash != zone.hash {
                return Err(format!(
                    "{}: the {} record is of another block (not coincident)",
                    what(zone),
                    NAMES[r.ctx]
                ));
            }
        }
        if seen[0] && !seen[1] {
            return Err(format!(
                "{}: a prime block without a region record",
                what(zone)
            ));
        }
        for r in records {
            if r.number == 0 {
                if r.hash != self.genesis {
                    return Err(format!("{}: not the genesis", what(r)));
                }
            } else {
                let Some(prev) = self.last[r.ctx] else {
                    return Err(format!("{}: no previous {} record", what(r), NAMES[r.ctx]));
                };
                if prev.number + 1 != r.number || r.parent != prev.hash {
                    return Err(format!(
                        "{}: parent {} is not the previous {} record {} at {}",
                        what(r),
                        hex(&r.parent),
                        NAMES[r.ctx],
                        hex(&prev.hash),
                        prev.number
                    ));
                }
            }
        }
        if zone.number == 0 {
            if !seen[0] || !seen[1] {
                return Err(format!(
                    "{}: genesis needs prime and region records",
                    what(zone)
                ));
            }
        } else {
            for ctx in 0..2 {
                if seen[ctx] {
                    continue;
                }
                let prev = self.last[ctx].ok_or_else(|| format!("no {} record", NAMES[ctx]))?;
                if zone.dom_parents[ctx] != prev.hash || zone.numbers[ctx] != prev.number + 1 {
                    return Err(format!(
                        "{}: its {} parent {} is not the previous {} record {} (previous coincident reference)",
                        what(zone),
                        NAMES[ctx],
                        hex(&zone.dom_parents[ctx]),
                        NAMES[ctx],
                        hex(&prev.hash)
                    ));
                }
            }
        }
        for r in records {
            self.last[r.ctx] = Some(Tip {
                number: r.number,
                hash: r.hash,
            });
        }
        self.next_zone += 1;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Requests to one peer.

enum Fault {
    /// Stream or timeout error: retry, maybe with another peer.
    Io(String),
    /// The peer replied that it has no such block.
    Missing(String),
    /// The peer sent data that fails a check.
    Bad(String),
    /// The reply frame is over 3 MiB: fetch one block at a time.
    TooLarge,
}
impl Fault {
    fn io(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::InvalidData && e.to_string().contains("exceeds 3 MiB") {
            Self::TooLarge
        } else {
            Self::Io(e.to_string())
        }
    }
    fn label(&self) -> &'static str {
        match self {
            Self::Io(_) => "io",
            Self::Missing(_) => "missing",
            Self::Bad(_) => "bad",
            Self::TooLarge => "too_large",
        }
    }
}
impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) | Self::Missing(e) | Self::Bad(e) => write!(f, "{}: {e}", self.label()),
            Self::TooLarge => write!(f, "too_large: reply over 3 MiB"),
        }
    }
}
fn number_bytes(n: u64) -> Vec<u8> {
    let b = n.to_be_bytes();
    b[b.iter().position(|v| *v != 0).unwrap_or(8)..].to_vec()
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Job {
    ctx: usize,
    start: u64,
}

/// One `/quai/1.0.0` stream with sequential, correlated requests.
struct Conn {
    node: Arc<PublicNode>,
    peer: PeerId,
    stream: Option<WireStream>,
    book: RequestBook,
    id: u32,
}
impl Conn {
    fn new(node: Arc<PublicNode>, peer: PeerId) -> Result<Self, String> {
        Ok(Self {
            node,
            peer,
            stream: None,
            book: RequestBook::new(1).map_err(|e| e.to_string())?,
            id: 0,
        })
    }
    async fn ask(
        &mut self,
        ctx: usize,
        data: rq::Data,
        request: rq::Request,
    ) -> Result<(rs::Response, Vec<u8>), Fault> {
        if self.stream.is_none() {
            self.stream = Some(self.node.open(self.peer).await.map_err(Fault::io)?);
        }
        self.id = self.id.wrapping_add(1);
        let id = self.id;
        let message = QuaiRequestMessage {
            id,
            location: Some(ProtoLocation {
                value: LOCATIONS[ctx].to_vec(),
            }),
            data: Some(data),
            request: Some(request),
        };
        self.book
            .register(&message)
            .map_err(|e| Fault::Bad(e.to_string()))?;
        let bytes = QuaiMessage {
            payload: Some(Payload::Request(message)),
        }
        .encode_to_vec();
        let Conn { stream, book, .. } = self;
        let result = async {
            let stream = stream.as_mut().ok_or(Fault::Io("no stream".into()))?;
            stream.write(&bytes).await.map_err(Fault::io)?;
            let raw = stream.read().await.map_err(Fault::io)?;
            let envelope = decode_envelope(&raw).map_err(|e| Fault::Bad(e.to_string()))?;
            let reply = response(envelope).map_err(|e| Fault::Bad(e.to_string()))?;
            book.complete(&reply)
                .map_err(|e| Fault::Bad(e.to_string()))?;
            let payload = reply
                .response
                .ok_or(Fault::Bad("reply without payload".into()))?;
            Ok((payload, raw))
        }
        .await;
        if result.is_err() {
            self.book.cancel(id);
            self.stream = None;
        }
        result
    }
    /// The canonical hash at `number` in `ctx`, or None when the peer has none.
    async fn block_hash(&mut self, ctx: usize, number: u64) -> Result<Option<Hash>, Fault> {
        let (reply, _) = self
            .ask(
                ctx,
                rq::Data::Number(number_bytes(number)),
                rq::Request::BlockHash(ProtoHash::default()),
            )
            .await?;
        match reply {
            rs::Response::BlockHash(h) if h.value.is_empty() => Ok(None),
            rs::Response::BlockHash(h) => h32(&h.value, "block hash").map(Some).map_err(Fault::Bad),
            _ => Err(Fault::Bad("reply kind is not BlockHash".into())),
        }
    }
    async fn objects(
        &mut self,
        ctx: usize,
        hash: Hash,
        blocks: bool,
    ) -> Result<Vec<Vec<u8>>, Fault> {
        let request = if blocks {
            rq::Request::WorkObjectBlocks(Default::default())
        } else {
            rq::Request::WorkObjectBlock(Default::default())
        };
        let (_, raw) = self
            .ask(
                ctx,
                rq::Data::Hash(ProtoHash {
                    value: hash.to_vec(),
                }),
                request,
            )
            .await?;
        reply_objects(&raw, blocks).map_err(Fault::Bad)
    }
    /// The network check: the peer's zone genesis is this network's.
    async fn genesis_check(&mut self, genesis: Hash) -> Result<(), Fault> {
        match self.block_hash(ZONE, 0).await? {
            Some(h) if h == genesis => Ok(()),
            Some(h) => Err(Fault::Bad(format!(
                "zone genesis {} is not this network's",
                hex(&h)
            ))),
            None => Err(Fault::Missing("no zone genesis".into())),
        }
    }
    /// The peer's head in `ctx`: the highest number with a canonical hash.
    /// `from` is a number the peer is believed to have (a guess below its
    /// head is cheapest); `step` is the first distance probed above it.
    async fn head_in(&mut self, ctx: usize, from: u64, step: u64) -> Result<u64, Fault> {
        let (mut lo, mut step) = (from, step.max(1));
        let mut hi = lo.saturating_add(step);
        while self.block_hash(ctx, hi).await?.is_some() {
            lo = hi;
            step = step.saturating_mul(2);
            hi = hi.saturating_add(step);
            if hi >= 1 << 40 {
                return Err(Fault::Bad("no head".into()));
            }
        }
        if lo > 0 && self.block_hash(ctx, lo).await?.is_none() {
            lo = 0;
        }
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if self.block_hash(ctx, mid).await?.is_some() {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }
    /// The peer's zone head, after the network check; `from` is a height the
    /// peer is known to have.
    async fn head(&mut self, genesis: Hash, from: u64) -> Result<u64, Fault> {
        self.genesis_check(genesis).await?;
        self.head_in(ZONE, from, 1024).await
    }
    /// The peer's prime, region and zone heads, after the network check.
    /// go-quai answers a batch only when it holds all ten blocks
    /// (`GetWorkObjectsFrom`), so the source asks a peer only for batches
    /// below its head in the batch's context.
    async fn heads(&mut self, genesis: Hash, from: [u64; 3], step: u64) -> Result<[u64; 3], Fault> {
        self.genesis_check(genesis).await?;
        let mut out = [0; 3];
        for (ctx, head) in out.iter_mut().enumerate() {
            *head = self.head_in(ctx, from[ctx], step).await?;
        }
        Ok(out)
    }
    /// One batch: BlockHash(start), then the blocks from that hash, checked
    /// and hash-linked. A dominant batch fetched block by block (`single`)
    /// can be short at the peer's head; WorkObjectBlocks gives all ten
    /// blocks or none.
    async fn batch(&mut self, job: Job, single: bool) -> Result<Vec<Block>, Fault> {
        let first = self
            .block_hash(job.ctx, job.start)
            .await?
            .ok_or_else(|| Fault::Missing(format!("{} {}", NAMES[job.ctx], job.start)))?;
        let raws = if single {
            let mut raws = vec![];
            for i in 0..BATCH {
                let hash = if i == 0 {
                    first
                } else {
                    match self.block_hash(job.ctx, job.start + i).await? {
                        Some(h) => h,
                        None if job.ctx != ZONE => break,
                        None => {
                            return Err(Fault::Missing(format!(
                                "{} {}",
                                NAMES[job.ctx],
                                job.start + i
                            )))
                        }
                    }
                };
                let mut one = self.objects(job.ctx, hash, false).await?;
                if one.len() != 1 {
                    return Err(Fault::Missing(format!(
                        "{} {}",
                        NAMES[job.ctx],
                        job.start + i
                    )));
                }
                raws.append(&mut one);
            }
            raws
        } else {
            self.objects(job.ctx, first, true).await?
        };
        if raws.is_empty() {
            return Err(Fault::Missing(format!(
                "{} {} blocks",
                NAMES[job.ctx], job.start
            )));
        }
        if raws.len() as u64 > BATCH || (job.ctx == ZONE && raws.len() as u64 != BATCH) {
            return Err(Fault::Bad(format!(
                "{} blocks in a batch reply",
                raws.len()
            )));
        }
        let mut out: Vec<Block> = Vec::with_capacity(raws.len());
        for (i, raw) in raws.into_iter().enumerate() {
            let n = job.start + i as u64;
            let b = decode_block(raw, job.ctx)
                .map_err(|e| Fault::Bad(format!("{} {n}: {e}", NAMES[job.ctx])))?;
            if b.number != n {
                return Err(Fault::Bad(format!(
                    "{} {n}: block has number {}",
                    NAMES[job.ctx], b.number
                )));
            }
            match out.last() {
                None if b.hash != first => {
                    return Err(Fault::Bad(format!(
                        "{} {n}: hash {} is not the hash the peer gave {}",
                        NAMES[job.ctx],
                        hex(&b.hash),
                        hex(&first)
                    )))
                }
                Some(prev) if b.parent != prev.hash => {
                    return Err(Fault::Bad(format!(
                        "{} {n}: parent link broken",
                        NAMES[job.ctx]
                    )))
                }
                _ => {}
            }
            out.push(b);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Peers and scheduling.

#[derive(Default)]
struct Stats {
    blocks: u64,
    faults: u64,
    strikes: u32,
    backoff_until: Option<Instant>,
    last_fault: Option<String>,
}
struct Source {
    index: usize,
    peer: PeerId,
    node: Arc<PublicNode>,
    /// The prime, region and zone heads the peer reported (a dominant head
    /// is lowered when the peer does not have a batch below it).
    heads: [AtomicU64; 3],
    alive: AtomicBool,
    stats: Mutex<Stats>,
}
impl Source {
    fn heads(&self) -> [u64; 3] {
        [0, 1, 2].map(|c| self.heads[c].load(Ordering::Relaxed))
    }
}
struct JobState {
    need: u64,
    excluded: HashSet<usize>,
    attempts: u32,
    single: bool,
}
#[derive(Clone)]
struct Fetched {
    block: Arc<Block>,
    source: usize,
}
struct Sched {
    pending: BTreeSet<(u64, Job)>,
    state: HashMap<Job, JobState>,
    zone: BTreeMap<u64, Vec<Fetched>>,
    dom: [BTreeMap<u64, Fetched>; 2],
    dom_next: [u64; 2],
    zone_next: u64,
    /// Zone heights below this can be fetched (best peer head less margin, +1).
    zone_end: u64,
    /// Prime and region numbers below this can be fetched (best peer head
    /// in the context, +1; 0 while no peer reported one).
    dom_end: [u64; 2],
    /// The last zone height the source delivers: no batch starts above it.
    to: Option<u64>,
    write_pos: u64,
    error: Option<String>,
}
/// What the next zone height waits for.
enum Step {
    /// Batches that the peers have and that are not fetched yet.
    Wait,
    /// A batch past the peers' head (zone: less the margin): only part of it
    /// exists. A source that hands off to gossip ends here.
    Head { ctx: usize, start: u64 },
    /// The records of the height: prime and region when coincident, zone.
    Height(Vec<Fetched>),
}
/// The result of one request for the peer that answered it.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The batch arrived with this many blocks.
    Blocks(u64),
    /// The peer does not have the whole batch yet: a dominant batch at the
    /// head. Not a fault; the batch waits for a peer whose head covers it.
    NotYet,
    /// A fault of the peer. `strike`: it counts toward dropping the peer.
    Fault { strike: bool },
}
impl Sched {
    /// The scheduler of a source that starts at zone height `start` with the
    /// heads `heads` there (prime, region, zone).
    fn new(start: u64, heads: [(Hash, u64); 3], to: Option<u64>) -> Self {
        let dom = if start == 0 {
            [0, 0]
        } else {
            [heads[0].1 + 1, heads[1].1 + 1]
        };
        Self {
            pending: BTreeSet::new(),
            state: HashMap::new(),
            zone: BTreeMap::new(),
            dom: [BTreeMap::new(), BTreeMap::new()],
            dom_next: dom.map(|n| n / BATCH * BATCH),
            zone_next: start / BATCH * BATCH,
            zone_end: 0,
            dom_end: [0, 0],
            to,
            write_pos: start,
            error: None,
        }
    }
    /// The fetch limit in each context for a peer with these heads: the zone
    /// head less the margin, the prime and region heads.
    fn limits(heads: [u64; 3], margin: u64) -> [u64; 3] {
        [heads[0], heads[1], heads[ZONE].saturating_sub(margin)]
    }
    /// Move the fetch limits with the best peer heads.
    fn set_heads(&mut self, best: [u64; 3], margin: u64) {
        let end = best[ZONE].saturating_sub(margin) + 1;
        if end > self.zone_end {
            self.zone_end = end;
            self.fill();
        }
        for (end, best) in self.dom_end.iter_mut().zip(best) {
            *end = if best > 0 { best + 1 } else { 0 };
        }
    }
    /// A batch that the best peer head does not cover yet, or only just:
    /// zone, its last height at or above the fetch limit; prime and region,
    /// its last block within `HEAD_SLACK` of the best head (a dominant head
    /// can move back a few blocks in a reorganization).
    fn at_head(&self, job: Job) -> bool {
        let last = job.start + BATCH - 1;
        if job.ctx == ZONE {
            last >= self.zone_end
        } else {
            last + HEAD_SLACK >= self.dom_end[job.ctx]
        }
    }
    fn push(&mut self, job: Job, need: u64) {
        let st = self.state.entry(job).or_insert(JobState {
            need,
            excluded: HashSet::new(),
            attempts: 0,
            single: false,
        });
        self.pending.insert((st.need, job));
    }
    fn fill(&mut self) {
        // A batch is ten blocks, so the one that holds `to` may end above
        // it: it is fetched when the peers have its last block.
        while self.zone_next + BATCH <= self.zone_end
            && self.zone_next < self.write_pos + WINDOW * BATCH
            && self.to.is_none_or(|to| self.zone_next <= to)
        {
            let job = Job {
                ctx: ZONE,
                start: self.zone_next,
            };
            self.push(job, job.start);
            self.zone_next += BATCH;
        }
    }
    fn need_dominant(&mut self, ctx: usize, upto: u64, need: u64) {
        while self.dom_next[ctx] <= upto {
            let job = Job {
                ctx,
                start: self.dom_next[ctx],
            };
            self.push(job, need);
            self.dom_next[ctx] += BATCH;
        }
    }
    /// The next job for `source`, whose fetch limits are `limits`
    /// ([`Sched::limits`]): a batch is asked only of a peer that holds all
    /// of it.
    fn take(&mut self, source: usize, limits: [u64; 3]) -> Option<(Job, bool)> {
        let (found, single) = self.pending.iter().find_map(|&(need, job)| {
            let st = self.state.get(&job)?;
            (!st.excluded.contains(&source)
                && need <= limits[ZONE]
                && job.start + BATCH - 1 <= limits[job.ctx])
                .then_some(((need, job), st.single))
        })?;
        self.pending.remove(&found);
        Some((found.1, single))
    }
    /// Book the answer of `source` (whose reported heads are `heads`) to
    /// `job`.
    fn settle(
        &mut self,
        source: usize,
        heads: &[AtomicU64; 3],
        job: Job,
        result: Result<Vec<Block>, Fault>,
    ) -> Outcome {
        match result {
            Ok(blocks) => {
                let n = blocks.len() as u64;
                self.complete(job, blocks, source);
                Outcome::Blocks(n)
            }
            // go-quai answers BlockHash and WorkObjectBlocks only for blocks
            // it holds (GetWorkObjectsFrom: all ten or nothing). At the head
            // of a dominant chain that means "not yet": the peer's head is
            // below the batch's last block, and the batch waits for a peer
            // whose head covers it, without an attempt or a strike.
            Err(Fault::Missing(_)) if job.ctx != ZONE && self.at_head(job) => {
                heads[job.ctx].fetch_min(job.start + BATCH - 2, Ordering::Relaxed);
                if let Some(st) = self.state.get(&job) {
                    self.pending.insert((st.need, job));
                }
                Outcome::NotYet
            }
            Err(fault) => {
                let (exclude, single) = match fault {
                    Fault::TooLarge => (false, true),
                    Fault::Io(_) => (false, false),
                    Fault::Missing(_) | Fault::Bad(_) => (true, false),
                };
                self.retry(job, source, exclude, single, &fault.to_string());
                Outcome::Fault {
                    strike: !matches!(fault, Fault::TooLarge),
                }
            }
        }
    }
    fn complete(&mut self, job: Job, blocks: Vec<Block>, source: usize) {
        let need = self.state.remove(&job).map_or(job.start, |s| s.need);
        if job.ctx == ZONE {
            let top = [0, 1].map(|c| blocks.iter().map(|b| b.numbers[c]).max().unwrap_or(0));
            for (c, t) in top.into_iter().enumerate() {
                self.need_dominant(c, t + 1, job.start);
            }
            let fetched = blocks
                .into_iter()
                .map(|b| Fetched {
                    block: Arc::new(b),
                    source,
                })
                .collect();
            self.zone.insert(job.start, fetched);
        } else {
            let got = blocks.len() as u64;
            for b in blocks {
                self.dom[job.ctx].insert(
                    b.number,
                    Fetched {
                        block: Arc::new(b),
                        source,
                    },
                );
            }
            // A short batch at the head: fetch the rest again later.
            let end = job.start + BATCH;
            if job.start + got < end && job.start + got < self.dom_next[job.ctx] {
                self.push(
                    Job {
                        ctx: job.ctx,
                        start: job.start + got,
                    },
                    need,
                );
            }
        }
    }
    fn retry(&mut self, job: Job, source: usize, exclude: bool, single: bool, why: &str) {
        let at_head = self.at_head(job);
        let Some(st) = self.state.get_mut(&job) else {
            return;
        };
        st.attempts += 1;
        if exclude {
            st.excluded.insert(source);
        }
        st.single |= single;
        if st.attempts >= MAX_ATTEMPTS {
            if at_head {
                // At the peers' head: try again from the start later.
                st.attempts = 0;
                st.excluded.clear();
            } else {
                self.error = Some(format!(
                    "{} batch {}: no valid reply after {} attempts (last: {why})",
                    NAMES[job.ctx], job.start, st.attempts
                ));
            }
        }
        self.pending.insert((st.need, job));
    }
    /// Drop a fetched batch after a chain-rule failure and fetch it again
    /// from another peer.
    fn invalidate(&mut self, job: Job, why: &str) {
        let source = if job.ctx == ZONE {
            self.zone
                .remove(&job.start)
                .and_then(|b| b.first().map(|f| f.source))
        } else {
            let mut source = None;
            for n in job.start..job.start + BATCH {
                if let Some(f) = self.dom[job.ctx].remove(&n) {
                    source = Some(f.source);
                }
            }
            source
        };
        let Some(source) = source else {
            return;
        };
        self.state.insert(
            job,
            JobState {
                need: self.write_pos,
                excluded: HashSet::new(),
                attempts: 0,
                single: false,
            },
        );
        self.retry(job, source, true, false, why);
    }
    /// What zone height `z` waits for; `first`: it is the processor's start
    /// block, which comes without dominant records.
    fn step(&mut self, z: u64, first: bool) -> Step {
        self.write_pos = z;
        self.fill();
        let start = z / BATCH * BATCH;
        let Some(zone) = self
            .zone
            .get(&start)
            .and_then(|b| b.get((z - start) as usize))
            .cloned()
        else {
            // `fill` asks only for whole batches below the fetch limit.
            return if self.zone_end > 0 && start + BATCH > self.zone_end {
                Step::Head { ctx: ZONE, start }
            } else {
                Step::Wait
            };
        };
        if first {
            return Step::Height(vec![zone]);
        }
        // A record at the zone block's own number in a dominant context is
        // coincident when it has the zone block's hash. For a zone block
        // that is not coincident, the record at that number is a later
        // block, which may not exist yet.
        let n = [zone.block.numbers[0], zone.block.numbers[1]];
        for (c, number) in n.iter().enumerate() {
            self.need_dominant(c, number + 1, start);
        }
        let dom = [
            self.dom[0].get(&n[0]).cloned(),
            self.dom[1].get(&n[1]).cloned(),
        ];
        if let [Some(p), Some(r)] = dom {
            let mut records = vec![];
            for d in [p, r] {
                if d.block.hash == zone.block.hash {
                    records.push(d);
                }
            }
            records.push(zone);
            return Step::Height(records);
        }
        for (c, number) in n.iter().enumerate() {
            let start = number / BATCH * BATCH;
            if dom[c].is_none() && self.dom_end[c] > 0 && start + BATCH > self.dom_end[c] {
                return Step::Head { ctx: c, start };
            }
        }
        Step::Wait
    }
    /// Drop the batches below the next zone height `z` and the dominant
    /// records below the last ones delivered.
    fn delivered(&mut self, z: u64, last: &[Option<Tip>; 3]) {
        let start = z / BATCH * BATCH;
        let below: Vec<u64> = self.zone.range(..start).map(|(k, _)| *k).collect();
        for k in below {
            self.zone.remove(&k);
        }
        for (c, tip) in last.iter().take(2).enumerate() {
            if let Some(t) = tip {
                self.dom[c] = self.dom[c].split_off(&t.number);
            }
        }
    }
}

/// Counters for logs and metrics.
#[derive(Default)]
pub struct Status {
    /// The best peer zone head.
    pub head: AtomicU64,
    /// Peers in use.
    pub peers: AtomicUsize,
    /// Blocks fetched and faults seen.
    pub blocks: AtomicU64,
    pub faults: AtomicU64,
    /// Heights fetched again after a chain-rule failure.
    pub refetched: AtomicU64,
}

struct Shared {
    sources: Mutex<Vec<Arc<Source>>>,
    /// Peers whose head is being probed before they become sources.
    probing: Mutex<HashSet<PeerId>>,
    sched: Mutex<Sched>,
    notify: Notify,
    stop: Arc<AtomicBool>,
    status: Arc<Status>,
    genesis: Hash,
    margin: u64,
    max_peers: usize,
    inflight: usize,
}
impl Shared {
    fn alive(&self) -> Vec<Arc<Source>> {
        lock(&self.sources)
            .iter()
            .filter(|s| s.alive.load(Ordering::Relaxed))
            .cloned()
            .collect()
    }
    /// Move the fetch limits with the best peer heads.
    fn update_end(&self) {
        let alive = self.alive();
        let best = [0, 1, 2].map(|c| {
            alive
                .iter()
                .map(|s| s.heads[c].load(Ordering::Relaxed))
                .max()
                .unwrap_or(0)
        });
        self.status.head.store(best[ZONE], Ordering::Relaxed);
        self.status.peers.store(alive.len(), Ordering::Relaxed);
        lock(&self.sched).set_heads(best, self.margin);
        self.notify.notify_waiters();
    }
    /// Probe a connected peer and start its workers. `hints`: numbers the
    /// peer probably has (the processor's start heads).
    async fn add(
        self: &Arc<Self>,
        node: Arc<PublicNode>,
        peer: PeerId,
        hints: [u64; 3],
    ) -> Result<(), String> {
        // One probe per peer at a time: a peer that connects while it is
        // being added must not become two sources.
        if lock(&self.sources).iter().any(|s| s.peer == peer) || !lock(&self.probing).insert(peer) {
            return Err("already a source".into());
        }
        let probed = async {
            let mut conn = Conn::new(node.clone(), peer)?;
            tokio::time::timeout(
                Duration::from_secs(120),
                conn.heads(self.genesis, hints, 1024),
            )
            .await
            .map_err(|_| "head probe timed out".to_string())?
            .map_err(|e| e.to_string())
        }
        .await;
        lock(&self.probing).remove(&peer);
        let heads = probed?;
        let src = {
            let mut sources = lock(&self.sources);
            if sources
                .iter()
                .filter(|s| s.alive.load(Ordering::Relaxed))
                .count()
                >= self.max_peers
            {
                return Err("peer limit reached".into());
            }
            let src = Arc::new(Source {
                index: sources.len(),
                peer,
                node,
                heads: heads.map(AtomicU64::new),
                alive: AtomicBool::new(true),
                stats: Mutex::new(Stats::default()),
            });
            sources.push(src.clone());
            src
        };
        self.update_end();
        for _ in 0..self.inflight {
            tokio::spawn(worker(src.clone(), self.clone()));
        }
        tokio::spawn(head_poll(src, self.clone()));
        Ok(())
    }
}

async fn worker(src: Arc<Source>, shared: Arc<Shared>) {
    let Ok(mut conn) = Conn::new(src.node.clone(), src.peer) else {
        return;
    };
    while !shared.stop.load(Ordering::Relaxed) && src.alive.load(Ordering::Relaxed) {
        let wait = lock(&src.stats)
            .backoff_until
            .filter(|t| *t > Instant::now());
        if let Some(t) = wait {
            tokio::time::sleep_until(t.into()).await;
            continue;
        }
        let limits = Sched::limits(src.heads(), shared.margin);
        let job = lock(&shared.sched).take(src.index, limits);
        let Some((job, single)) = job else {
            let _ =
                tokio::time::timeout(Duration::from_millis(500), shared.notify.notified()).await;
            continue;
        };
        let result = conn.batch(job, single).await;
        let fault = result
            .as_ref()
            .err()
            .map(|f| format!("{} {}: {f}", NAMES[job.ctx], job.start));
        let outcome = lock(&shared.sched).settle(src.index, &src.heads, job, result);
        let mut stats = lock(&src.stats);
        match outcome {
            Outcome::Blocks(n) => {
                stats.blocks += n;
                stats.strikes = 0;
                shared.status.blocks.fetch_add(n, Ordering::Relaxed);
            }
            Outcome::NotYet => {
                drop(stats);
                // The best head may be lower now: the source may hand off.
                shared.update_end();
            }
            Outcome::Fault { strike } => {
                stats.faults += 1;
                shared.status.faults.fetch_add(1, Ordering::Relaxed);
                stats.last_fault = fault;
                if strike {
                    stats.strikes += 1;
                    let delay = Duration::from_secs(1 << stats.strikes.min(6));
                    stats.backoff_until = Some(Instant::now() + delay);
                }
                if stats.strikes >= MAX_STRIKES {
                    src.alive.store(false, Ordering::Relaxed);
                }
                drop(stats);
                if !src.alive.load(Ordering::Relaxed) {
                    shared.update_end();
                }
            }
        }
        shared.notify.notify_waiters();
    }
}
/// Follow a peer's heads.
async fn head_poll(src: Arc<Source>, shared: Arc<Shared>) {
    let Ok(mut conn) = Conn::new(src.node.clone(), src.peer) else {
        return;
    };
    while !shared.stop.load(Ordering::Relaxed) && src.alive.load(Ordering::Relaxed) {
        tokio::time::sleep(HEAD_POLL).await;
        let from = src.heads();
        let probe = conn.heads(shared.genesis, from, HEAD_STEP);
        if let Ok(Ok(heads)) = tokio::time::timeout(Duration::from_secs(60), probe).await {
            // The zone head only moves up, as before; a dominant head is the
            // peer's answer (it can move back in a reorganization).
            if heads[ZONE] >= from[ZONE] {
                src.heads[ZONE].store(heads[ZONE], Ordering::Relaxed);
            }
            for (head, value) in src.heads.iter().zip(heads).take(2) {
                head.store(value, Ordering::Relaxed);
            }
            shared.update_end();
        }
    }
}

fn peer_of(a: &Multiaddr) -> Option<PeerId> {
    a.iter().find_map(|p| match p {
        libp2p::multiaddr::Protocol::P2p(id) => Some(id),
        _ => None,
    })
}
async fn wait_connected(node: &PublicNode, peer: &PeerId, secs: u64) -> bool {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        if node.is_connected(peer) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

pub fn default_gateway() -> Option<String> {
    let out = std::process::Command::new("route")
        .args(["-n", "get", "default"])
        .output();
    match out {
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
}
/// Public peers are allowed unless the gateway is an iPhone hotspot (or
/// cannot be read), as in scripts/mainnet/node_guard.sh.
pub fn public_allowed() -> Result<String, String> {
    let gateway = default_gateway().ok_or("cannot read the default gateway")?;
    if gateway.starts_with("172.20.10.") && std::env::var("ALLOW_METERED").as_deref() != Ok("1") {
        return Err(format!(
            "default gateway {gateway} is an iPhone hotspot (metered)"
        ));
    }
    Ok(gateway)
}

/// What the source fetches.
#[derive(Clone, Debug)]
pub struct Options {
    /// The processor's start height and its heads there (prime, region, zone).
    pub start: u64,
    pub heads: [(Hash, u64); 3],
    pub genesis: Hash,
    /// Stop after this zone height.
    pub to: Option<u64>,
    /// Fetch only zone heights at least this far below the best peer head.
    pub margin: u64,
    pub max_peers: usize,
    /// Requests in flight per peer.
    pub inflight: usize,
    /// Peers to dial (for example the local go-quai node).
    pub peers: Vec<Multiaddr>,
    /// Use the colosseum boot peers and the peers they lead to.
    pub public: bool,
    /// End the source at the first height that needs a batch past the
    /// peers' head (zone: the best head less `margin`; prime and region: the
    /// best head), before the last partial batch; the node then follows the
    /// head through gossip. Without it the source waits there for the heads
    /// to move.
    pub handoff: bool,
}

enum Item {
    Height(Box<Height>),
    Error(SourceError),
}
/// The peer block source.
pub struct PeerSource {
    rx: Receiver<Item>,
    peeked: Option<Item>,
    status: Arc<Status>,
    stop: Arc<AtomicBool>,
    to: Option<u64>,
    delivered: Option<u64>,
}
impl PeerSource {
    /// Start fetching on `runtime`. `node` is the libp2p node to use.
    pub fn start(
        runtime: &tokio::runtime::Handle,
        node: Arc<PublicNode>,
        options: Options,
        log: crate::log::Log,
    ) -> Result<Self, String> {
        let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
        let status = Arc::new(Status::default());
        let stop = Arc::new(AtomicBool::new(false));
        let o = options.clone();
        let shared = Arc::new(Shared {
            sources: Mutex::new(vec![]),
            probing: Mutex::new(HashSet::new()),
            sched: Mutex::new(Sched::new(o.start, o.heads, o.to)),
            notify: Notify::new(),
            stop: stop.clone(),
            status: status.clone(),
            genesis: o.genesis,
            margin: o.margin,
            max_peers: o.max_peers.max(1),
            inflight: o.inflight.clamp(1, 4),
        });
        runtime.spawn(connect(node.clone(), shared.clone(), o.clone(), log));
        runtime.spawn(assemble(shared, o, tx, log));
        Ok(Self {
            rx,
            peeked: None,
            status,
            stop,
            to: options.to,
            delivered: None,
        })
    }
    pub fn status(&self) -> Arc<Status> {
        self.status.clone()
    }
    fn take(&mut self, item: Item) -> Result<Next, SourceError> {
        match item {
            Item::Height(h) => {
                self.delivered = Some(h.zone_height);
                Ok(Next::Height(h))
            }
            Item::Error(e) => Err(e),
        }
    }
}
impl Drop for PeerSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
impl BlockSource for PeerSource {
    fn next(&mut self) -> Result<Next, SourceError> {
        if let Some(item) = self.peeked.take() {
            return self.take(item);
        }
        if self
            .to
            .is_some_and(|to| self.delivered.is_some_and(|d| d >= to))
        {
            return Ok(Next::End);
        }
        match self.rx.try_recv() {
            Ok(item) => self.take(item),
            Err(TryRecvError::Empty) => Ok(Next::Wait),
            Err(TryRecvError::Disconnected) => Ok(Next::End),
        }
    }
    fn wait(&mut self, stop: &AtomicBool) {
        if self.peeked.is_some() || stop.load(Ordering::Relaxed) {
            return;
        }
        match self.rx.recv_timeout(Duration::from_millis(500)) {
            Ok(item) => self.peeked = Some(item),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {}
        }
    }
    fn target(&self) -> Option<u64> {
        Some(self.status.head.load(Ordering::Relaxed))
    }
    fn receipts(&self) -> bool {
        false
    }
}

/// Dial the configured and the public peers; every connected peer that
/// answers the genesis check becomes a source.
async fn connect(node: Arc<PublicNode>, shared: Arc<Shared>, o: Options, log: crate::log::Log) {
    // Numbers every synced peer has: the start of the head probes.
    let hints = [o.heads[0].1, o.heads[1].1, o.start];
    // Every connected peer, the ones a bootstrap or the gossip intake
    // connected included, is tried once as a source. The peer list is
    // polled: the node's event queue belongs to the gossip intake, which
    // runs beside this source when the node catches up in tip mode.
    {
        let (node, shared) = (node.clone(), shared.clone());
        tokio::spawn(async move {
            let mut tried = HashSet::new();
            while !shared.stop.load(Ordering::Relaxed) {
                for peer in node.connected_peers() {
                    if !tried.insert(peer) {
                        continue;
                    }
                    let (shared, node) = (shared.clone(), node.clone());
                    tokio::spawn(async move {
                        match shared.add(node, peer, hints).await {
                            Ok(()) => log.info(&format!("peer {peer}: source")),
                            Err(e) => log.debug(&format!("peer {peer}: not a source: {e}")),
                        }
                    });
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
    }
    let mut targets: Vec<Multiaddr> = o.peers.clone();
    if o.public {
        match public_allowed() {
            Ok(gateway) => {
                log.info(&format!("public peers allowed (gateway {gateway})"));
                targets.extend(COLOSSEUM_BOOTNODES.iter().filter_map(|a| a.parse().ok()));
            }
            Err(e) => log.warn(&format!("public peers not used: {e}")),
        }
    }
    for address in targets {
        if let Err(e) = node.dial(address.clone()).await {
            // A peer that a bootstrap already connected is not news.
            let e = e.to_string();
            if e.contains("already connected") {
                log.debug(&format!("dial {address}: {e}"));
            } else {
                log.warn(&format!("dial {address}: {e}"));
            }
            continue;
        }
        let Some(peer) = peer_of(&address) else {
            continue;
        };
        if wait_connected(&node, &peer, 15).await && o.public {
            let bare: Multiaddr = address
                .iter()
                .filter(|p| !matches!(p, libp2p::multiaddr::Protocol::P2p(_)))
                .collect();
            let _ = node.kad_add(peer, bare).await;
        }
    }
    if o.public {
        let _ = node.closest_peers(node.id.to_bytes()).await;
    }
    // Look for more peers while fewer than the limit are in use.
    while !shared.stop.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_secs(60)).await;
        if o.public && shared.alive().len() < shared.max_peers {
            let _ = node.closest_peers(node.id.to_bytes()).await;
        }
    }
}

/// Put the fetched batches together, height by height, and queue them.
async fn assemble(shared: Arc<Shared>, o: Options, tx: SyncSender<Item>, log: crate::log::Log) {
    let mut chain = Chain::new(o.start, o.heads, o.genesis);
    let mut z = o.start;
    let mut failures: HashMap<u64, u32> = HashMap::new();
    let send = |item: Item| -> bool {
        let mut item = item;
        loop {
            match tx.try_send(item) {
                Ok(()) => return true,
                Err(TrySendError::Disconnected(_)) => return false,
                Err(TrySendError::Full(back)) => {
                    if shared.stop.load(Ordering::Relaxed) {
                        return false;
                    }
                    item = back;
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    };
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        if o.to.is_some_and(|to| z > to) {
            return;
        }
        if let Some(e) = lock(&shared.sched).error.take() {
            send(Item::Error(SourceError {
                context: "peers".into(),
                check: "peer data".into(),
                expected: "a valid reply from some peer".into(),
                actual: e,
            }));
            return;
        }
        let step = lock(&shared.sched).step(z, z == o.start && z > 0);
        match step {
            Step::Head { ctx, start } if o.handoff => {
                // Before the last partial batch: gossip delivers the head.
                let s = lock(&shared.sched);
                let head = if ctx == ZONE {
                    s.zone_end.saturating_sub(1)
                } else {
                    s.dom_end[ctx].saturating_sub(1)
                };
                drop(s);
                log.info(&format!(
                    "peers: every height to {} delivered; zone height {z} needs the {} batch {start}, which ends past the peers' {} head {head}{}: following the head through gossip",
                    z.saturating_sub(1),
                    NAMES[ctx],
                    NAMES[ctx],
                    if ctx == ZONE {
                        format!(" (best head {} less margin {})", shared.status.head.load(Ordering::Relaxed), o.margin)
                    } else {
                        String::new()
                    },
                ));
                return;
            }
            Step::Wait | Step::Head { .. } => {
                let _ = tokio::time::timeout(Duration::from_millis(200), shared.notify.notified())
                    .await;
            }
            Step::Height(records) => {
                let blocks: Vec<&Block> = records.iter().map(|f| f.block.as_ref()).collect();
                let checked = if z == o.start && z > 0 {
                    // The checkpoint block: the processor checks its hash.
                    Ok(())
                } else {
                    chain.height(&blocks)
                };
                match checked {
                    Ok(()) => {
                        let mut height = Height {
                            zone_height: z,
                            records: [None, None, None],
                        };
                        for f in &records {
                            let b = &f.block;
                            height.records[b.ctx] = Some(RawRecord {
                                context: Context::from_u8(b.ctx as u8).unwrap_or(Context::Zone),
                                number: b.number,
                                hash: b.hash,
                                block: b.raw.clone(),
                                receipts: vec![],
                            });
                        }
                        if !send(Item::Height(Box::new(height))) {
                            return;
                        }
                        z += 1;
                        lock(&shared.sched).delivered(z, &chain.last);
                    }
                    Err(e) => {
                        let count = failures.entry(z).or_default();
                        *count += 1;
                        shared.status.refetched.fetch_add(1, Ordering::Relaxed);
                        log.warn(&format!(
                            "zone height {z}: {e}; fetching again from other peers (attempt {count})"
                        ));
                        if *count >= 3 {
                            send(Item::Error(SourceError {
                                context: "peers".into(),
                                check: "chain rules".into(),
                                expected: "linked records from the peers".into(),
                                actual: format!("zone height {z}: {e}"),
                            }));
                            return;
                        }
                        let mut s = lock(&shared.sched);
                        let start = z / BATCH * BATCH;
                        s.invalidate(Job { ctx: ZONE, start }, &e);
                        for f in records.iter().filter(|f| f.block.ctx != ZONE) {
                            s.invalidate(
                                Job {
                                    ctx: f.block.ctx,
                                    start: f.block.number / BATCH * BATCH,
                                },
                                &e,
                            );
                        }
                        drop(s);
                        shared.notify.notify_waiters();
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Single requests and ranges for the shadow bootstrap (`bootstrap.rs`).

/// Dial the configured peers and, when allowed, the colosseum boot peers,
/// and start a Kademlia walk for more.
pub async fn dial_targets(
    node: Arc<PublicNode>,
    peers: Vec<Multiaddr>,
    public: bool,
    log: crate::log::Log,
) {
    let mut targets = peers;
    if public {
        match public_allowed() {
            Ok(gateway) => {
                log.info(&format!("public peers allowed (gateway {gateway})"));
                targets.extend(COLOSSEUM_BOOTNODES.iter().filter_map(|a| a.parse().ok()));
            }
            Err(e) => log.warn(&format!("public peers not used: {e}")),
        }
    }
    for address in targets {
        if let Err(e) = node.dial(address.clone()).await {
            // A peer that a bootstrap already connected is not news.
            let e = e.to_string();
            if e.contains("already connected") {
                log.debug(&format!("dial {address}: {e}"));
            } else {
                log.warn(&format!("dial {address}: {e}"));
            }
            continue;
        }
        let Some(peer) = peer_of(&address) else {
            continue;
        };
        if wait_connected(&node, &peer, 15).await && public {
            let bare: Multiaddr = address
                .iter()
                .filter(|p| !matches!(p, libp2p::multiaddr::Protocol::P2p(_)))
                .collect();
            let _ = node.kad_add(peer, bare).await;
        }
    }
    if public {
        let _ = node.closest_peers(node.id.to_bytes()).await;
    }
}
/// The canonical hash at `number` in context `ctx` of one peer (None: the
/// peer has no block there).
pub async fn peer_hash_at(
    node: &Arc<PublicNode>,
    peer: PeerId,
    ctx: usize,
    number: u64,
) -> Result<Option<Hash>, String> {
    let mut conn = Conn::new(node.clone(), peer)?;
    tokio::time::timeout(Duration::from_secs(30), conn.block_hash(ctx, number))
        .await
        .map_err(|_| "time-out".to_string())?
        .map_err(|f| f.to_string())
}
/// A peer's zone head, after its zone genesis is this network's.
pub async fn peer_head(node: &Arc<PublicNode>, peer: PeerId, genesis: Hash) -> Result<u64, String> {
    let mut conn = Conn::new(node.clone(), peer)?;
    tokio::time::timeout(Duration::from_secs(120), conn.head(genesis, 0))
        .await
        .map_err(|_| "head probe timed out".to_string())?
        .map_err(|f| f.to_string())
}
/// One block by hash in context `ctx` from one peer, decoded and checked as
/// the Go exporter does.
pub async fn peer_block(
    node: &Arc<PublicNode>,
    peer: PeerId,
    ctx: usize,
    hash: Hash,
) -> Result<Block, String> {
    let mut conn = Conn::new(node.clone(), peer)?;
    let raws = tokio::time::timeout(Duration::from_secs(30), conn.objects(ctx, hash, false))
        .await
        .map_err(|_| "time-out".to_string())?
        .map_err(|f| f.to_string())?;
    let [raw] = <[Vec<u8>; 1]>::try_from(raws).map_err(|_| "not one block".to_string())?;
    let b = decode_block(raw, ctx)?;
    if b.hash != hash {
        return Err(format!("block {} is not {}", hex(&b.hash), hex(&hash)));
    }
    Ok(b)
}
/// The canonical blocks `from..=to` of context `ctx`, by number, from
/// `peers` in parallel (ten per request), each decoded and checked, and
/// every block linked to the one before it. A batch that fails is asked of
/// another peer; the range fails after `MAX_ATTEMPTS` tries of one batch.
pub async fn fetch_range(
    node: &Arc<PublicNode>,
    peers: &[PeerId],
    ctx: usize,
    from: u64,
    to: u64,
    log: crate::log::Log,
) -> Result<Vec<Block>, String> {
    if peers.is_empty() || from > to {
        return Err("no peers or an empty range".into());
    }
    let jobs: Vec<u64> = (from..=to).step_by(BATCH as usize).collect();
    let queue = Arc::new(Mutex::new(
        jobs.iter()
            .map(|s| (*s, 0u32, HashSet::<usize>::new()))
            .collect::<std::collections::VecDeque<_>>(),
    ));
    let done: Arc<Mutex<BTreeMap<u64, Vec<Block>>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let failed: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mut workers = vec![];
    for (index, peer) in peers.iter().enumerate() {
        for _ in 0..3 {
            let (node, queue, done, failed) =
                (node.clone(), queue.clone(), done.clone(), failed.clone());
            let peer = *peer;
            let peers_n = peers.len();
            let total = jobs.len();
            workers.push(tokio::spawn(async move {
                let Ok(mut conn) = Conn::new(node, peer) else {
                    return;
                };
                let mut strikes = 0u32;
                loop {
                    if lock(&failed).is_some() || strikes >= MAX_STRIKES {
                        return;
                    }
                    let job = {
                        let mut q = lock(&queue);
                        let at = q.iter().position(|(_, _, ex)| {
                            !ex.contains(&index) || ex.len() >= peers_n
                        });
                        at.and_then(|i| q.remove(i))
                    };
                    let Some((start, attempts, mut excluded)) = job else {
                        if lock(&done).len() >= total {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    };
                    let reply =
                        tokio::time::timeout(Duration::from_secs(60), conn.batch(Job { ctx, start }, false))
                            .await
                            .unwrap_or_else(|_| Err(Fault::Io("time-out".into())));
                    match reply {
                        Ok(blocks) if !blocks.is_empty() => {
                            strikes = 0;
                            let keep: Vec<Block> =
                                blocks.into_iter().filter(|b| b.number <= to).collect();
                            lock(&done).insert(start, keep);
                        }
                        other => {
                            strikes += 1;
                            let why = match other {
                                Err(f) => f.to_string(),
                                Ok(_) => "empty batch".into(),
                            };
                            if attempts + 1 >= MAX_ATTEMPTS {
                                *lock(&failed) = Some(format!(
                                    "{} batch {start}: no valid reply after {} attempts (last: {why})",
                                    NAMES[ctx],
                                    attempts + 1
                                ));
                                return;
                            }
                            excluded.insert(index);
                            lock(&queue).push_back((start, attempts + 1, excluded));
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                    }
                }
            }));
        }
    }
    let started = Instant::now();
    let mut last = Instant::now();
    loop {
        if let Some(e) = lock(&failed).clone() {
            return Err(e);
        }
        let got = lock(&done).len();
        if got == jobs.len() {
            break;
        }
        if workers.iter().all(|w| w.is_finished()) {
            return Err(format!(
                "{} {from}..={to}: the peers stopped answering ({got} of {} batches)",
                NAMES[ctx],
                jobs.len()
            ));
        }
        if last.elapsed() >= Duration::from_secs(30) {
            last = Instant::now();
            log.info(&format!(
                "bootstrap: {} {from}..={to}: {got}/{} batches ({:.0}s)",
                NAMES[ctx],
                jobs.len(),
                started.elapsed().as_secs_f64()
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    for w in workers {
        w.abort();
    }
    let done = std::mem::take(&mut *lock(&done));
    let mut out: Vec<Block> = Vec::with_capacity((to - from + 1) as usize);
    for (_, blocks) in done {
        for b in blocks {
            if let Some(prev) = out.last() {
                if b.number != prev.number + 1 || b.parent != prev.hash {
                    return Err(format!(
                        "{} {}: parent {} is not the previous block {} at {}",
                        NAMES[ctx],
                        b.number,
                        hex(&b.parent),
                        hex(&prev.hash),
                        prev.number
                    ));
                }
            } else if b.number != from {
                return Err(format!(
                    "{} range starts at {}, not {from}",
                    NAMES[ctx], b.number
                ));
            }
            out.push(b);
        }
    }
    if out.last().map(|b| b.number) != Some(to) {
        return Err(format!("{} range {from}..={to} is incomplete", NAMES[ctx]));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The batch that holds `to` is fetched even when `to` does not end a
    /// ten-block batch (a source stopped at `to` waited for it forever).
    #[test]
    fn the_batch_that_holds_to_is_fetched() {
        let mut s = Sched {
            pending: BTreeSet::new(),
            state: HashMap::new(),
            zone: BTreeMap::new(),
            dom: [BTreeMap::new(), BTreeMap::new()],
            dom_next: [0, 0],
            zone_next: 0,
            zone_end: 100,
            dom_end: [0, 0],
            to: Some(37),
            write_pos: 0,
            error: None,
        };
        s.fill();
        let starts: Vec<u64> = s.pending.iter().map(|(_, j)| j.start).collect();
        assert_eq!(starts, vec![0, 10, 20, 30]);
        // Without `to`, every batch below the fetch limit.
        s.to = None;
        s.fill();
        assert_eq!(s.pending.len(), 10);
    }
    // -----------------------------------------------------------------
    // A simulated slice and peers that answer as go-quai does.

    fn hash_of(z: u64) -> Hash {
        let mut h = [0x5a; 32];
        h[..8].copy_from_slice(&z.to_be_bytes());
        h
    }
    /// Zone heights `zg..=z1` of a slice. The block at `zg` is a prime
    /// block with prime number `pg` and region number `rg`; `order(z)` is
    /// 0 (prime), 1 (region) or 2 (zone) for the others. Numbers, parents
    /// and dominant parents as go-quai sets them: a block's prime and region
    /// numbers are its dominant parents' numbers plus one.
    struct Slice {
        zone: BTreeMap<u64, Block>,
        dom: [BTreeMap<u64, Block>; 2],
        /// The tips (hash, number) of each context at each zone height.
        tips: BTreeMap<u64, [(Hash, u64); 3]>,
    }
    impl Slice {
        fn new(zg: u64, pg: u64, rg: u64, z1: u64, order: impl Fn(u64) -> usize) -> Self {
            let mut s = Slice {
                zone: BTreeMap::new(),
                dom: [BTreeMap::new(), BTreeMap::new()],
                tips: BTreeMap::new(),
            };
            let mut tips = [(hash_of(zg), pg), (hash_of(zg), rg), (hash_of(zg), zg)];
            s.tips.insert(zg, tips);
            for z in zg + 1..=z1 {
                let hash = hash_of(z);
                let numbers = [tips[0].1 + 1, tips[1].1 + 1, z];
                let dom_parents = [tips[0].0, tips[1].0];
                let record = |ctx: usize, parent: Hash| Block {
                    ctx,
                    number: numbers[ctx],
                    hash,
                    parent,
                    dom_parents,
                    numbers,
                    location: LOCATIONS[ZONE].to_vec(),
                    raw: vec![],
                };
                for ctx in order(z)..3 {
                    let parent = if ctx == ZONE {
                        tips[ZONE].0
                    } else {
                        dom_parents[ctx]
                    };
                    let map = if ctx == ZONE {
                        &mut s.zone
                    } else {
                        &mut s.dom[ctx]
                    };
                    map.insert(numbers[ctx], record(ctx, parent));
                    tips[ctx] = (hash, numbers[ctx]);
                }
                s.tips.insert(z, tips);
            }
            s
        }
        /// The heads of a peer whose zone head is `z`.
        fn heads_at(&self, z: u64) -> [u64; 3] {
            self.tips[&z].map(|t| t.1)
        }
    }
    /// go-quai's answer to the batch `job` of a peer whose heads are
    /// `heads`: BlockHash(start) (none above its head), then
    /// WorkObjectBlocks from that hash, which holds all ten blocks or none
    /// (p2p/node/api.go `GetWorkObjectsFrom`).
    fn go_answer(slice: &Slice, heads: [u64; 3], job: Job) -> Result<Vec<Block>, Fault> {
        let chain = if job.ctx == ZONE {
            &slice.zone
        } else {
            &slice.dom[job.ctx]
        };
        if job.start > heads[job.ctx] || !chain.contains_key(&job.start) {
            return Err(Fault::Missing(format!("{} {}", NAMES[job.ctx], job.start)));
        }
        let blocks: Vec<Block> = (job.start..job.start + BATCH)
            .filter(|n| *n <= heads[job.ctx])
            .filter_map(|n| chain.get(&n).cloned())
            .collect();
        if blocks.len() as u64 != BATCH {
            return Err(Fault::Missing(format!(
                "{} {} blocks",
                NAMES[job.ctx], job.start
            )));
        }
        Ok(blocks)
    }
    /// The scheduler and the chain checker driven as the workers and the
    /// assembler drive them, with simulated peers.
    struct Sim {
        s: Sched,
        chain: Chain,
        start: u64,
        z: u64,
        margin: u64,
        /// The heads each peer reported, as `Source::heads`.
        reported: Vec<[AtomicU64; 3]>,
        strikes: Vec<u32>,
        faults: u64,
        not_yet: u64,
    }
    impl Sim {
        fn new(slice: &Slice, start: u64, margin: u64, peers: usize) -> Self {
            let heads = slice.tips[&start];
            Sim {
                s: Sched::new(start, heads, None),
                chain: Chain::new(start, heads, hash_of(0)),
                start,
                z: start,
                margin,
                reported: (0..peers).map(|_| [0, 0, 0].map(AtomicU64::new)).collect(),
                strikes: vec![0; peers],
                faults: 0,
                not_yet: 0,
            }
        }
        /// Head polls: peer `i` reports `heads[i]`.
        fn report(&mut self, heads: &[[u64; 3]]) {
            for (r, h) in self.reported.iter().zip(heads) {
                for (head, value) in r.iter().zip(h) {
                    head.store(*value, Ordering::Relaxed);
                }
            }
            self.update_end();
        }
        /// `Shared::update_end`.
        fn update_end(&mut self) {
            let best = [0, 1, 2].map(|c| {
                self.reported
                    .iter()
                    .map(|r| r[c].load(Ordering::Relaxed))
                    .max()
                    .unwrap_or(0)
            });
            self.s.set_heads(best, self.margin);
        }
        /// The workers take jobs until none is left for any peer (`actual`:
        /// the heads the peers really have), the assembler delivers heights,
        /// and again until nothing moves. Returns where the assembler
        /// stopped, or the source's error.
        fn pass(&mut self, slice: &Slice, actual: &[[u64; 3]]) -> Result<Step, String> {
            loop {
                let mut taken = 0;
                loop {
                    let mut busy = false;
                    for (i, heads) in actual.iter().enumerate() {
                        let reported =
                            [0, 1, 2].map(|c| self.reported[i][c].load(Ordering::Relaxed));
                        let Some((job, _)) = self.s.take(i, Sched::limits(reported, self.margin))
                        else {
                            continue;
                        };
                        busy = true;
                        taken += 1;
                        let answer = go_answer(slice, *heads, job);
                        match self.s.settle(i, &self.reported[i], job, answer) {
                            Outcome::Blocks(_) => self.strikes[i] = 0,
                            Outcome::NotYet => {
                                self.not_yet += 1;
                                self.update_end();
                            }
                            Outcome::Fault { strike } => {
                                self.faults += 1;
                                self.strikes[i] += u32::from(strike);
                            }
                        }
                        if let Some(e) = self.s.error.take() {
                            return Err(e);
                        }
                    }
                    if !busy {
                        break;
                    }
                }
                let before = self.z;
                let step = loop {
                    match self.s.step(self.z, self.z == self.start && self.z > 0) {
                        Step::Height(records) => {
                            let blocks: Vec<&Block> =
                                records.iter().map(|f| f.block.as_ref()).collect();
                            if self.z != self.start {
                                self.chain.height(&blocks)?;
                            }
                            self.z += 1;
                            self.s.delivered(self.z, &self.chain.last);
                        }
                        other => break other,
                    }
                };
                if taken == 0 && self.z == before {
                    return Ok(step);
                }
            }
        }
    }
    fn head_of(step: &Step) -> Option<(usize, u64)> {
        match step {
            Step::Head { ctx, start } => Some((*ctx, *start)),
            _ => None,
        }
    }
    /// The slice of the first shadow run's stop (2026-11-19): zone 11,212,902
    /// is prime block 2,480,939; prime blocks every 4 zone heights before it
    /// and every 16 after it, region blocks every 2.
    fn shadow_stop_slice() -> Slice {
        Slice::new(11_212_502, 2_480_839, 6_043_000, 11_213_400, |z| {
            if (z <= 11_212_902 && z % 4 == 2) || z % 16 == 6 {
                0
            } else if z % 2 == 0 {
                1
            } else {
                2
            }
        })
    }

    /// The first shadow run stopped at zone 11,212,903, 70 below the peers'
    /// head 11,212,973: "prime batch 2480940: no valid reply after 8
    /// attempts (last: missing: prime 2480940 blocks)". The zone block's
    /// prime number was 2,480,940, the batch 2,480,940..=2,480,949 ended past
    /// the peers' prime head, and go-quai answers WorkObjectBlocks only with
    /// all ten blocks. The source asked every peer for it and failed. Now it
    /// asks no peer whose prime head is below the batch's end, a "missing"
    /// answer there is not a fault, and at that height a source that hands
    /// off to gossip ends; one that does not waits for the prime head.
    #[test]
    fn a_prime_batch_past_the_peers_head_waits_and_hands_off() {
        let slice = shadow_stop_slice();
        assert_eq!(slice.zone[&11_212_902].numbers[0], 2_480_939);
        assert_eq!(slice.dom[0][&2_480_939].hash, hash_of(11_212_902));
        assert_eq!(slice.zone[&11_212_903].numbers[0], 2_480_940);
        let head = slice.heads_at(11_212_973);
        assert!(
            head[0] < 2_480_949,
            "prime head {} covers the batch",
            head[0]
        );
        let peers = [head; 8];
        // go-quai's answer, as the peers gave it.
        let job = Job {
            ctx: 0,
            start: 2_480_940,
        };
        assert!(
            matches!(go_answer(&slice, head, job), Err(Fault::Missing(m)) if m == "prime 2480940 blocks")
        );

        // The node resumed at 11,211,933 in the shadow run; here from a
        // checkpoint at 11,212,600, margin 64.
        let mut sim = Sim::new(&slice, 11_212_600, 64, peers.len());
        sim.report(&peers);
        assert_eq!(sim.s.zone_end, 11_212_973 - 64 + 1);
        let stop = sim.pass(&slice, &peers).expect("no failure at the head");
        assert_eq!(head_of(&stop), Some((0, 2_480_940)));
        assert_eq!(sim.z, 11_212_903, "every height to 11,212,902 delivered");
        assert_eq!(
            (sim.faults, sim.not_yet),
            (0, 0),
            "no peer was asked for the batch"
        );
        // The job waits in the queue for a peer whose head covers it.
        assert!(sim.s.pending.iter().any(|(_, j)| *j == job));

        // Without a hand-off the source waits: polls at the same heads
        // change nothing and fail nothing.
        for _ in 0..20 {
            sim.report(&peers);
            let stop = sim.pass(&slice, &peers).expect("no failure");
            assert_eq!(head_of(&stop), Some((0, 2_480_940)));
        }
        assert_eq!((sim.z, sim.faults), (11_212_903, 0));

        // Head polls that are older than the peers' last reorganization
        // claim a prime head that covers the batch: each peer answers
        // "missing" once, is not at fault, and its prime head is lowered
        // below the batch's end.
        let stale: Vec<[u64; 3]> = peers.iter().map(|h| [2_480_949, h[1], h[2]]).collect();
        sim.report(&stale);
        let stop = sim
            .pass(&slice, &peers)
            .expect("a stale head is not a failure");
        assert_eq!(head_of(&stop), Some((0, 2_480_940)));
        assert_eq!((sim.not_yet, sim.faults), (8, 0));
        assert!(sim.strikes.iter().all(|s| *s == 0));
        assert!(sim
            .reported
            .iter()
            .all(|r| r[0].load(Ordering::Relaxed) == 2_480_948));

        // The prime chain moves on: every waiting height is delivered.
        // At zone head 11,213,300 the prime head is 2,480,963: the heights
        // to 11,213,222 (prime block 2,480,959) are delivered, and zone
        // height 11,213,223 (prime number 2,480,960) waits for prime batch
        // 2,480,960..=2,480,969.
        let later = [slice.heads_at(11_213_300); 8];
        assert_eq!(later[0][0], 2_480_963);
        sim.report(&later);
        let stop = sim.pass(&slice, &later).expect("no failure");
        assert_eq!(head_of(&stop), Some((0, 2_480_960)));
        assert_eq!(sim.z, 11_213_223);
        assert_eq!(sim.faults, 0);
    }

    /// A zone batch that ends past the best head less the margin is the last
    /// partial batch: a source that hands off ends before it.
    #[test]
    fn the_source_hands_off_before_the_last_partial_zone_batch() {
        // Every block a prime block: the dominant batches never wait.
        let slice = Slice::new(1_000, 100, 200, 2_000, |_| 0);
        for head in [1_500, 1_503, 1_509] {
            let peers = [slice.heads_at(head); 3];
            let mut sim = Sim::new(&slice, 1_200, 64, peers.len());
            sim.report(&peers);
            let stop = sim.pass(&slice, &peers).unwrap();
            // The fetch limit is head - 64; whole batches only.
            let last_batch = (head - 64 + 1) / BATCH * BATCH;
            assert_eq!(head_of(&stop), Some((ZONE, last_batch)), "head {head}");
            assert_eq!(sim.z, last_batch, "head {head}");
            assert_eq!((sim.faults, sim.not_yet), (0, 0));
        }
    }

    /// A batch well below the peers' heads that every peer answers
    /// "missing" for still fails the source after `MAX_ATTEMPTS`: waiting
    /// is only for batches at the head.
    #[test]
    fn a_missing_batch_below_the_head_is_still_a_fault() {
        let slice = shadow_stop_slice();
        let head = slice.heads_at(11_213_300);
        let mut sim = Sim::new(&slice, 11_212_600, 64, 8);
        sim.report(&[head; 8]);
        // The peers claim the heads but hold nothing from prime block
        // 2,480,940 on (as a peer with a damaged database would).
        let broken: Vec<[u64; 3]> = (0..8).map(|_| [2_480_939, head[1], head[2]]).collect();
        let e = loop {
            match sim.pass(&slice, &broken) {
                Err(e) => break e,
                Ok(_) => sim.report(&[head; 8]),
            }
        };
        assert_eq!(
            e,
            "prime batch 2480940: no valid reply after 8 attempts (last: missing: prime 2480940)"
        );
    }

    #[test]
    fn reply_frames_and_numbers() {
        assert_eq!(number_bytes(0), Vec::<u8>::new());
        assert_eq!(number_bytes(256), vec![1, 0]);
        // QuaiMessage{response{work_object_blocks_view{views[{wo}]}}} by hand.
        let wo = vec![0x0a, 0x00];
        let view = [vec![0x0a, wo.len() as u8], wo.clone()].concat();
        let outer = [vec![0x0a, view.len() as u8], view].concat();
        let reply = [vec![0x2a, outer.len() as u8], outer].concat();
        let frame = [vec![0x12, reply.len() as u8], reply].concat();
        assert_eq!(reply_objects(&frame, true).unwrap(), vec![wo]);
        assert!(reply_objects(&[0x12, 0x05, 0x00], true).is_err());
    }
}
