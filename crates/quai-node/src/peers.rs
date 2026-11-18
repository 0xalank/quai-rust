//! Zone heights from go-quai peers for `quai-node run`: a [`BlockSource`]
//! over `/quai/1.0.0` (`quai_network::public::PublicNode`, read-only).
//!
//! The fetch logic is the one of `crates/quai-network/examples/peer-export.rs`
//! (that example keeps its own copy and CLI): each peer is probed for its
//! zone head; ten-block batches by number (BlockHash, then
//! WorkObjectBlocks) are fetched ahead of the processor in each context;
//! every block is decoded and checked as the Go exporter does (hash
//! recomputed from the bytes, body roots, hash links in all three contexts,
//! dominant parents), and a height whose records break a chain rule is
//! fetched again from another peer. A peer that fails repeatedly is dropped.
//!
//! The source delivers zone heights in order from the processor's start
//! height, up to the best peer head less `margin` (so that it never fetches
//! a block that a reorganization may still remove), and then polls the
//! peers' heads. Public peers are not used when the default gateway is an
//! iPhone hotspot (172.20.10.x) unless ALLOW_METERED=1.
use crate::processor::{BlockSource, Next, SourceError};
use crate::qex::{Context, Height, RawRecord};
use libp2p::{Multiaddr, PeerId};
use prost::Message;
use quai_consensus::verifier::PINNED_SCHEDULE as SCHEDULE;
use quai_network::{
    decode_envelope,
    public::{Event, PublicNode, COLOSSEUM_BOOTNODES},
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
    /// The peer's zone head, after the network check; `from` is a height the
    /// peer is known to have.
    async fn head(&mut self, genesis: Hash, from: u64) -> Result<u64, Fault> {
        match self.block_hash(ZONE, 0).await? {
            Some(h) if h == genesis => {}
            Some(h) => {
                return Err(Fault::Bad(format!(
                    "zone genesis {} is not this network's",
                    hex(&h)
                )))
            }
            None => return Err(Fault::Missing("no zone genesis".into())),
        }
        let (mut lo, mut step) = (from, 1024u64);
        let mut hi = lo.saturating_add(step);
        while self.block_hash(ZONE, hi).await?.is_some() {
            lo = hi;
            step = step.saturating_mul(2);
            hi = hi.saturating_add(step);
            if hi >= 1 << 40 {
                return Err(Fault::Bad("no head".into()));
            }
        }
        if lo > 0 && self.block_hash(ZONE, lo).await?.is_none() {
            lo = 0;
        }
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if self.block_hash(ZONE, mid).await?.is_some() {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }
    /// One batch: BlockHash(start), then the blocks from that hash, checked
    /// and hash-linked. A dominant batch at the peer's head can be short.
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
    head: AtomicU64,
    alive: AtomicBool,
    stats: Mutex<Stats>,
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
    /// The last zone height the source delivers: no batch starts above it.
    to: Option<u64>,
    write_pos: u64,
    error: Option<String>,
}
impl Sched {
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
    fn take(&mut self, source: usize, limit: u64) -> Option<(Job, bool)> {
        let (found, single) = self.pending.iter().find_map(|&(need, job)| {
            let st = self.state.get(&job)?;
            (!st.excluded.contains(&source)
                && need <= limit
                && (job.ctx != ZONE || job.start + BATCH - 1 <= limit))
                .then_some(((need, job), st.single))
        })?;
        self.pending.remove(&found);
        Some((found.1, single))
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
        let Some(st) = self.state.get_mut(&job) else {
            return;
        };
        st.attempts += 1;
        if exclude {
            st.excluded.insert(source);
        }
        st.single |= single;
        if st.attempts >= MAX_ATTEMPTS {
            if job.start + BATCH > self.zone_end {
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
    /// Move the fetch limit with the best peer head.
    fn update_end(&self) {
        let best = self
            .alive()
            .iter()
            .map(|s| s.head.load(Ordering::Relaxed))
            .max()
            .unwrap_or(0);
        self.status.head.store(best, Ordering::Relaxed);
        self.status
            .peers
            .store(self.alive().len(), Ordering::Relaxed);
        let end = best.saturating_sub(self.margin) + 1;
        let mut s = lock(&self.sched);
        if end > s.zone_end {
            s.zone_end = end;
            s.fill();
        }
        drop(s);
        self.notify.notify_waiters();
    }
    /// Probe a connected peer and start its workers.
    async fn add(self: &Arc<Self>, node: Arc<PublicNode>, peer: PeerId) -> Result<(), String> {
        // One probe per peer at a time: a peer that connects while it is
        // being added must not become two sources.
        if lock(&self.sources).iter().any(|s| s.peer == peer) || !lock(&self.probing).insert(peer)
        {
            return Err("already a source".into());
        }
        let probed = async {
            let mut conn = Conn::new(node.clone(), peer)?;
            tokio::time::timeout(Duration::from_secs(120), conn.head(self.genesis, 0))
                .await
                .map_err(|_| "head probe timed out".to_string())?
                .map_err(|e| e.to_string())
        }
        .await;
        lock(&self.probing).remove(&peer);
        let head = probed?;
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
                head: AtomicU64::new(head),
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
        let limit = src
            .head
            .load(Ordering::Relaxed)
            .saturating_sub(shared.margin);
        let job = lock(&shared.sched).take(src.index, limit);
        let Some((job, single)) = job else {
            let _ =
                tokio::time::timeout(Duration::from_millis(500), shared.notify.notified()).await;
            continue;
        };
        let result = conn.batch(job, single).await;
        let mut stats = lock(&src.stats);
        match result {
            Ok(blocks) => {
                stats.blocks += blocks.len() as u64;
                stats.strikes = 0;
                shared
                    .status
                    .blocks
                    .fetch_add(blocks.len() as u64, Ordering::Relaxed);
                drop(stats);
                lock(&shared.sched).complete(job, blocks, src.index);
            }
            Err(fault) => {
                stats.faults += 1;
                shared.status.faults.fetch_add(1, Ordering::Relaxed);
                stats.last_fault = Some(format!("{} {}: {fault}", NAMES[job.ctx], job.start));
                let (exclude, single) = match fault {
                    Fault::TooLarge => (false, true),
                    Fault::Io(_) => (false, false),
                    Fault::Missing(_) | Fault::Bad(_) => (true, false),
                };
                if !matches!(fault, Fault::TooLarge) {
                    stats.strikes += 1;
                    let delay = Duration::from_secs(1 << stats.strikes.min(6));
                    stats.backoff_until = Some(Instant::now() + delay);
                }
                if stats.strikes >= MAX_STRIKES {
                    src.alive.store(false, Ordering::Relaxed);
                }
                let why = fault.to_string();
                drop(stats);
                lock(&shared.sched).retry(job, src.index, exclude, single, &why);
                if !src.alive.load(Ordering::Relaxed) {
                    shared.update_end();
                }
            }
        }
        shared.notify.notify_waiters();
    }
}
/// Follow a peer's head.
async fn head_poll(src: Arc<Source>, shared: Arc<Shared>) {
    let Ok(mut conn) = Conn::new(src.node.clone(), src.peer) else {
        return;
    };
    while !shared.stop.load(Ordering::Relaxed) && src.alive.load(Ordering::Relaxed) {
        tokio::time::sleep(HEAD_POLL).await;
        let from = src.head.load(Ordering::Relaxed);
        match tokio::time::timeout(Duration::from_secs(60), conn.head(shared.genesis, from)).await {
            Ok(Ok(head)) if head >= from => {
                src.head.store(head, Ordering::Relaxed);
                shared.update_end();
            }
            _ => {}
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
    /// End the source once it delivered every height up to the best peer
    /// head less `margin` (the node then follows the head through gossip).
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
        let next_dom = [o.heads[0].1 + 1, o.heads[1].1 + 1];
        let start_dom = if o.start == 0 { [0, 0] } else { next_dom };
        let shared = Arc::new(Shared {
            sources: Mutex::new(vec![]),
            probing: Mutex::new(HashSet::new()),
            sched: Mutex::new(Sched {
                pending: BTreeSet::new(),
                state: HashMap::new(),
                zone: BTreeMap::new(),
                dom: [BTreeMap::new(), BTreeMap::new()],
                dom_next: [start_dom[0] / BATCH * BATCH, start_dom[1] / BATCH * BATCH],
                zone_next: o.start / BATCH * BATCH,
                zone_end: 0,
                to: o.to,
                write_pos: o.start,
                error: None,
            }),
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
    let events = node.clone();
    let candidates = shared.clone();
    tokio::spawn(async move {
        let mut tried = HashSet::new();
        while let Ok(event) = events.next().await {
            if candidates.stop.load(Ordering::Relaxed) {
                return;
            }
            if let Event::Connected { peer, .. } = event {
                if !tried.insert(peer) {
                    continue;
                }
                let (shared, node) = (candidates.clone(), events.clone());
                tokio::spawn(async move {
                    match shared.add(node, peer).await {
                        Ok(()) => log.info(&format!("peer {peer}: source")),
                        Err(e) => log.debug(&format!("peer {peer}: not a source: {e}")),
                    }
                });
            }
        }
    });
    // Peers connected before this source started (by a bootstrap) send no
    // new Connected event.
    for peer in node.verified_peers() {
        let (shared, node) = (shared.clone(), node.clone());
        tokio::spawn(async move {
            match shared.add(node, peer).await {
                Ok(()) => log.info(&format!("peer {peer}: source")),
                Err(e) => log.debug(&format!("peer {peer}: not a source: {e}")),
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
    let mut chain = Chain {
        last: [None; 3],
        next_zone: o.start,
        genesis: o.genesis,
    };
    if o.start > 0 {
        chain.last = o.heads.map(|(hash, number)| Some(Tip { number, hash }));
        chain.next_zone = o.start + 1;
    }
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
        enum Step {
            Wait,
            Height(Vec<Fetched>),
        }
        let step = {
            let mut s = lock(&shared.sched);
            s.write_pos = z;
            s.fill();
            let start = z / BATCH * BATCH;
            match s
                .zone
                .get(&start)
                .and_then(|b| b.get((z - start) as usize))
                .cloned()
            {
                None => Step::Wait,
                Some(zone) if z == o.start && z > 0 => Step::Height(vec![zone]),
                Some(zone) => {
                    // A record at the zone block's own number in a dominant
                    // context is coincident when it has the zone block's hash.
                    let n = [zone.block.numbers[0], zone.block.numbers[1]];
                    for (c, number) in n.iter().enumerate() {
                        s.need_dominant(c, number + 1, start);
                    }
                    match [s.dom[0].get(&n[0]).cloned(), s.dom[1].get(&n[1]).cloned()] {
                        [Some(p), Some(r)] => {
                            let mut records = vec![];
                            for d in [p, r] {
                                if d.block.hash == zone.block.hash {
                                    records.push(d);
                                }
                            }
                            records.push(zone);
                            Step::Height(records)
                        }
                        _ => Step::Wait,
                    }
                }
            }
        };
        match step {
            Step::Wait => {
                let caught_up = {
                    let s = lock(&shared.sched);
                    s.zone_end > 0 && z >= s.zone_end
                };
                if o.handoff && caught_up {
                    log.info(&format!(
                        "peers: every height to {} delivered (peers' head {} less margin {})",
                        z.saturating_sub(1),
                        shared.status.head.load(Ordering::Relaxed),
                        o.margin
                    ));
                    return;
                }
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
                        let mut s = lock(&shared.sched);
                        let start = z / BATCH * BATCH;
                        let below: Vec<u64> = s.zone.range(..start).map(|(k, _)| *k).collect();
                        for k in below {
                            s.zone.remove(&k);
                        }
                        for c in 0..2 {
                            if let Some(t) = chain.last[c] {
                                s.dom[c] = s.dom[c].split_off(&t.number);
                            }
                        }
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

// ---------------------------------------------------------------------------
// Single requests and ranges for the trusted bootstrap (`bootstrap.rs`).

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
                return Err(format!("{} range starts at {}, not {from}", NAMES[ctx], b.number));
            }
            out.push(b);
        }
    }
    if out.last().map(|b| b.number) != Some(to) {
        return Err(format!("{} range {from}..={to} is incomplete", NAMES[ctx]));
    }
    Ok(out)
}
