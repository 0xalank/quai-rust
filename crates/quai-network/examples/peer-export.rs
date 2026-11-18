//! Peer exporter: writes the canonical go-quai mainnet chain (zone [0 0]) in
//! the QEX1 format (docs/mainnet-export-format.md) from go-quai peers over
//! `/quai/1.0.0`, so the Rust replay is not limited by the local Go node's
//! sync. Peers do not serve receipts, so the export has `"receipts": false`.
//!
//!   peer-export [export] [OPTIONS]     write or resume an export
//!   peer-export compare OURS OTHER     compare two exports record by record,
//!                                      ignoring only the receipts field
//!   peer-export verify DIR             check an export again from its files
//!   peer-export prune DIR --below ZONE delete the files of complete segments
//!                                      that end below ZONE (rolling replay)
//!   peer-export status DIR             print the manifest summary as JSON
//!
//! Export options:
//!   --out DIR          output (default $QUAI_MAINNET_DIR/peer-export, with
//!                      QUAI_MAINNET_DIR default ~/quai-data/mainnet-v0.57.0)
//!   --to ZONE          last zone height (default: as far as the peers allow)
//!   --sources S        local, public or all (default all: the local node first)
//!   --peer ADDR        the local go-quai node (default 127.0.0.1:4002)
//!   --max-peers N      at most N peers (default and maximum 8)
//!   --inflight N       requests in flight per peer (default 3, at most 4)
//!   --margin N         request only zone heights N below a peer's head (default 1000)
//!   --min-free-gb N    stop below N GiB free on the output volume (default 30)
//!   --max-gb N         stop when the export holds N GiB
//!   --max-seconds N    stop after N seconds
//!
//! The rules are those of the Go exporter (reference/mainnet-export/main.go):
//! every record is checked before it is written (hash recomputed from the
//! bytes, body roots, hash links in all three contexts, dominant parents) and
//! segments are written to `.pending` and renamed. A planned stop (a limit,
//! or a file named STOP in the output directory) writes the last partial
//! segment and the manifest; the next run resumes from it. The exporter
//! refuses a directory that holds an export with receipts (a Go export).
//! `sources/<segment>.json` records which peer supplied each block range.
//! When the export already reaches `--to`, or the peers have no block above
//! it, the export prints an `up_to_date` event and exits with status 0.
//!
//! `prune` keeps a long replay within the disk: a replay that resumes from a
//! checkpoint at or above ZONE never reads a segment that ends below ZONE.
//! Pass the oldest checkpoint that the replay keeps
//! (scripts/mainnet/rolling_replay.sh). The manifest keeps the entry with
//! `"pruned": true`. The newest complete segment and the last segment are
//! always kept: a resumed export reads its chain tips from the newest complete
//! segment and rewrites a partial last segment.
//!
//! Read-only, as `quai_network::public` enforces: dial out only, no inbound
//! requests served, no gossip published. Public peers are not used when the
//! default gateway is an iPhone hotspot (172.20.10.x) unless ALLOW_METERED=1.
use libp2p::{Multiaddr, PeerId};
use prost::Message;
use quai_consensus::verifier::PINNED_SCHEDULE as SCHEDULE;
use quai_network::{
    decode_envelope,
    public::{DialPolicy, Event, ReadOnlyPeer, COLOSSEUM_BOOTNODES},
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
    Hash, REFERENCE_REVISION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const USAGE: &str = "\
Usage: peer-export [export] [OPTIONS]     write or resume a QEX1 export from go-quai peers
       peer-export compare OURS OTHER     compare two exports record by record (receipts ignored)
       peer-export verify DIR             check an export again from its files
       peer-export prune DIR --below ZONE delete the files of complete segments that end
                                          below ZONE; keep the newest complete and the last
       peer-export status DIR             print the manifest summary as one JSON line

Export options:
  --out DIR          output (default $QUAI_MAINNET_DIR/peer-export,
                     QUAI_MAINNET_DIR default ~/quai-data/mainnet-v0.57.0)
  --to ZONE          last zone height (default: as far as the peers allow)
  --sources S        local, public or all (default all: the local node first)
  --peer ADDR        the local go-quai node (default 127.0.0.1:4002)
  --max-peers N      at most N peers (default and maximum 8)
  --inflight N       requests in flight per peer (default 3, at most 4)
  --margin N         request only zone heights N below a peer's head (default 1000)
  --min-free-gb N    stop below N GiB free on the output volume (default 30)
  --max-gb N         stop when the export holds N GiB
  --max-seconds N    stop after N seconds
A file named STOP in the output directory stops the export at the next check;
a stopped export resumes on the next run. ALLOW_METERED=1 allows public peers
on an iPhone hotspot gateway (172.20.10.x). An export that already reaches
--to, or that the peers cannot extend, prints an up_to_date event (status 0).
";

/// `params.ProgpowColosseumGenesisHash`.
const GENESIS: Hash = [
    0xac, 0x81, 0xc2, 0x8f, 0x1a, 0x72, 0x59, 0x1b, 0x87, 0xb5, 0xf1, 0x6c, 0x97, 0x93, 0xcd, 0xc0,
    0xe8, 0x7c, 0x45, 0xc6, 0xd4, 0x26, 0xd1, 0xa3, 0x64, 0xc3, 0xb8, 0xf6, 0x38, 0x6b, 0x5b, 0x8b,
];
const LOCAL_PEER: &str =
    "/ip4/127.0.0.1/tcp/4002/p2p/12D3KooWShkvuVk9997YBBtqNdEHdsgzeiZ9DpFsktoxjKVMnRdJ";
const ENVIRONMENT: &str = "colosseum";
const FORMAT: &str = "QEX1";
const MAGIC: &[u8; 5] = b"QEX1\n";
const SEGMENT_SPAN: u64 = 100_000;
const KIND_BLOCK: u8 = 1;
/// go-quai `C_NumPrimeBlocksToDownload`: a WorkObjectBlocks reply holds 10 blocks.
const BATCH: u64 = 10;
const NAMES: [&str; 3] = ["prime", "region", "zone"];
const LOCATIONS: [&[u8]; 3] = [&[], &[0], &[0, 0]];
const ZONE: usize = 2;
/// Attempts per batch before the export stops.
const MAX_ATTEMPTS: u32 = 8;
/// Consecutive faults before a peer is dropped.
const MAX_STRIKES: u32 = 6;

fn emit(v: Value) {
    println!("{v}");
}
fn hex(bytes: &[u8]) -> String {
    encode_hex(bytes)
}
fn utc(secs: u64) -> String {
    // Civil date from days since 1970-01-01 (proleptic Gregorian).
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
fn now_utc() -> String {
    utc(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0))
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
/// WorkObjectBlockView reply frame, unchanged. go-quai marshals a nested
/// message exactly as it marshals the message alone, so these are the bytes of
/// `proto.Marshal(wo.ProtoEncode(types.BlockObject))`.
fn reply_objects(frame: &[u8], blocks: bool) -> Result<Vec<Vec<u8>>, String> {
    let reply = delimited(frame, 2)?; // QuaiMessage.response
    let [reply] = reply.as_slice() else {
        return Err("not one response".into());
    };
    let views = if blocks {
        // QuaiResponseMessage.work_object_blocks_view, then repeated views.
        let outer = delimited(reply, 5)?;
        let [outer] = outer.as_slice() else {
            return Err("not a WorkObjectBlocksView reply".into());
        };
        delimited(outer, 1)?
    } else {
        delimited(reply, 4)? // QuaiResponseMessage.work_object_block_view
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
struct Block {
    ctx: usize,
    /// `wo.NumberU64(ctx)`.
    number: u64,
    hash: Hash,
    /// `wo.ParentHash(ctx)`.
    parent: Hash,
    /// Prime and region parents from the body header.
    dom_parents: [Hash; 2],
    /// Prime, region and zone numbers of the block.
    numbers: [u64; 3],
    location: Vec<u8>,
    txs: usize,
    raw: Vec<u8>,
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
/// Decode with quai-primitives and apply the record rules of the Go exporter
/// (`decodeBlock`, `checkBody` and the per-record part of `chainChecker`):
/// the bytes decode and encode back to the same bytes, the header hash binds
/// the body header, the body matches the header roots for the context, the
/// expansion number is 0 and the location is zone [0 0] after genesis.
fn decode_block(raw: Vec<u8>, ctx: usize) -> Result<Block, String> {
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
        return Err(format!(
            "expansion number {e}: QEX1 covers one active zone only"
        ));
    }
    let location = w
        .location
        .as_ref()
        .map(|l| l.value.clone())
        .unwrap_or_default();
    // checkBody: the work object header hash binds the body header.
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
        txs: object.transactions().len(),
        raw,
    })
}

// ---------------------------------------------------------------------------
// The chain rules of an export (Go `chainChecker`), per zone height.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tip {
    number: u64,
    hash: Hash,
}
#[derive(Default)]
struct Checker {
    last: [Option<Tip>; 3],
    next_zone: u64,
    records: [u64; 3],
    txs: u64,
}
impl Checker {
    /// `records` holds the prime, region and zone records of one zone height
    /// in that order (prime and region only when coincident).
    fn height(&mut self, records: &[&Block]) -> Result<(), String> {
        let Some((zone, dominant)) = records.split_last() else {
            return Err("no records".into());
        };
        let what = |b: &Block| format!("{} block {} ({})", NAMES[b.ctx], b.number, hex(&b.hash));
        if zone.ctx != ZONE {
            return Err(format!(
                "{}: the last record of a height must be zone",
                what(zone)
            ));
        }
        if zone.number != self.next_zone {
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
                    "{}: the {} record is of another block",
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
                if r.hash != GENESIS {
                    return Err(format!(
                        "{}: not the mainnet genesis {}",
                        what(r),
                        hex(&GENESIS)
                    ));
                }
                if self.last[r.ctx].is_some() {
                    return Err(format!("{}: a second genesis record", what(r)));
                }
            } else {
                let Some(prev) = self.last[r.ctx] else {
                    return Err(format!("{}: no previous {} record", what(r), NAMES[r.ctx]));
                };
                if prev.number + 1 != r.number {
                    return Err(format!(
                        "{}: the previous {} record is {}",
                        what(r),
                        NAMES[r.ctx],
                        prev.number
                    ));
                }
                if r.parent != prev.hash {
                    return Err(format!(
                        "{}: parent hash {} is not the previous {} record {}",
                        what(r),
                        hex(&r.parent),
                        NAMES[r.ctx],
                        hex(&prev.hash)
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
            self.dominant_parents(zone, seen, &what(zone))?;
        }
        for r in records {
            self.last[r.ctx] = Some(Tip {
                number: r.number,
                hash: r.hash,
            });
            self.records[r.ctx] += 1;
        }
        self.txs += zone.txs as u64;
        self.next_zone += 1;
        Ok(())
    }
    /// A zone block points at the last exported prime and region blocks in
    /// the contexts where it has no record of its own.
    fn dominant_parents(&self, zone: &Block, own: [bool; 2], what: &str) -> Result<(), String> {
        for ctx in 0..2 {
            if own[ctx] {
                continue;
            }
            let Some(prev) = self.last[ctx] else {
                return Err(format!("{what}: no {} record yet", NAMES[ctx]));
            };
            if zone.dom_parents[ctx] != prev.hash || zone.numbers[ctx] != prev.number + 1 {
                return Err(format!(
                    "{what}: its {} parent is {} ({} height {}), but the last exported {} block is {} ({}): a {} block is not coincident with a zone [0 0] block, which QEX1 does not cover",
                    NAMES[ctx], hex(&zone.dom_parents[ctx]), NAMES[ctx], zone.numbers[ctx].wrapping_sub(1),
                    NAMES[ctx], hex(&prev.hash), prev.number, NAMES[ctx]
                ));
            }
        }
        Ok(())
    }
    /// Start the rules at a zone block after a pruned prefix: the block's own
    /// parent links (zone parent, prime and region parents) stand for the
    /// pruned blocks. Every later record is then checked as usual.
    fn anchor(&mut self, zone: &Block) {
        let tip = |number: u64, hash: Hash| {
            Some(Tip {
                number: number.saturating_sub(1),
                hash,
            })
        };
        self.last = [
            tip(zone.numbers[0], zone.dom_parents[0]),
            tip(zone.numbers[1], zone.dom_parents[1]),
            tip(zone.number, zone.parent),
        ];
        self.next_zone = zone.number;
    }
    /// Go `checkTip`: the next zone block links to the export, and the next
    /// prime and region blocks are zone [0 0] blocks above it.
    fn tip(&self, child: &Block, next: [&Block; 2]) -> Result<(), String> {
        let last = self.last[ZONE].ok_or("empty export")?;
        let what = format!(
            "zone block {} ({}) after the export",
            child.number,
            hex(&child.hash)
        );
        if child.number != last.number + 1 || child.parent != last.hash {
            return Err(format!(
                "{what}: its parent is not the last exported zone block"
            ));
        }
        self.dominant_parents(child, [false; 2], &what)?;
        for (ctx, b) in next.iter().enumerate() {
            let prev = self.last[ctx].ok_or("empty export")?;
            if b.number != prev.number + 1 {
                return Err(format!(
                    "next {} block is {}, want {}",
                    NAMES[ctx],
                    b.number,
                    prev.number + 1
                ));
            }
            if b.location != LOCATIONS[ZONE] {
                return Err(format!(
                    "{} block {} is from zone {:?} and not coincident with a zone [0 0] block",
                    NAMES[ctx], b.number, b.location
                ));
            }
            if b.numbers[ZONE] <= last.number {
                return Err(format!(
                    "{} block {} has zone height {} inside the exported range",
                    NAMES[ctx], b.number, b.numbers[ZONE]
                ));
            }
        }
        Ok(())
    }
}

/// The chain tips after one zone height (`records` in prime, region, zone
/// order): its own records, and for prime and region without a record, the
/// parents that its zone block names. The export checked those links
/// (`Checker::dominant_parents`) when it wrote the block.
fn tips_after(records: &[Block]) -> [Option<Tip>; 3] {
    let mut tips = [None; 3];
    if let Some(zone) = records.last().filter(|b| b.ctx == ZONE) {
        for (c, tip) in tips.iter_mut().take(2).enumerate() {
            *tip = Some(Tip {
                number: zone.numbers[c].wrapping_sub(1),
                hash: zone.dom_parents[c],
            });
        }
    }
    for r in records {
        tips[r.ctx] = Some(Tip {
            number: r.number,
            hash: r.hash,
        });
    }
    tips
}

// ---------------------------------------------------------------------------
// Export directory: manifest, segments, resume (as the Go exporter).

#[derive(Clone, Debug, PartialEq, Eq)]
struct SegmentInfo {
    file: String,
    first: u64,
    last: u64,
    records: u64,
    sha256: String,
    /// `"pruned": true`: the file was deleted after the replay checkpointed
    /// past the segment. The entry keeps its range, count and hash.
    pruned: bool,
}
impl SegmentInfo {
    fn complete(&self, span: u64) -> bool {
        self.last - self.first + 1 == span
    }
}
#[derive(Clone, Debug)]
struct Manifest {
    receipts: bool,
    last_zone: u64,
    segments: Vec<SegmentInfo>,
}
fn segment_name(first: u64, last: u64) -> String {
    format!("segments/{first:010}-{last:010}.qex")
}
fn sources_name(first: u64, last: u64) -> String {
    format!("sources/{first:010}-{last:010}.json")
}
/// Go `readManifest` and `checkManifest`, plus pruned entries: only a prefix
/// of the segments can be pruned, never the last. `span` is the segment span
/// (`SEGMENT_SPAN`; tests use a smaller one).
fn read_manifest(dir: &Path, span: u64) -> Res<Option<Manifest>> {
    let path = dir.join("manifest.json");
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let v: Value = serde_json::from_slice(&bytes)?;
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    if s("format") != FORMAT || s("reference_revision") != REFERENCE_REVISION {
        return Err(format!(
            "{}: not a QEX1 export of revision {REFERENCE_REVISION}",
            path.display()
        )
        .into());
    }
    if s("environment") != ENVIRONMENT
        || v["slice"] != json!([0, 0])
        || s("genesis_hash") != hex(&GENESIS)
    {
        return Err(format!("{}: not a colosseum [0 0] export", path.display()).into());
    }
    let receipts = v["receipts"].as_bool().ok_or("manifest receipts")?;
    let last_zone = v["last_zone"].as_u64().ok_or("manifest last_zone")?;
    let list = v["segments"].as_array().ok_or("manifest segments")?;
    if list.is_empty() {
        return Err("manifest has no segments".into());
    }
    let mut segments = vec![];
    let mut next = 0;
    let mut kept = false;
    for (i, seg) in list.iter().enumerate() {
        let info = SegmentInfo {
            file: seg["file"].as_str().unwrap_or("").to_string(),
            first: seg["first_zone"].as_u64().ok_or("segment first_zone")?,
            last: seg["last_zone"].as_u64().ok_or("segment last_zone")?,
            records: seg["records"].as_u64().ok_or("segment records")?,
            sha256: seg["sha256"].as_str().unwrap_or("").to_string(),
            pruned: match &seg["pruned"] {
                Value::Null => false,
                Value::Bool(b) => *b,
                _ => return Err("segment pruned is not a boolean".into()),
            },
        };
        let width = info.last.wrapping_sub(info.first).wrapping_add(1);
        let last = i + 1 == list.len();
        if info.file != segment_name(info.first, info.last)
            || info.first != next
            || info.last < info.first
            || width > span
            || (width < span && !last)
            || info.records < width
            || info.sha256.len() != 64
        {
            return Err(format!("manifest segment {} is not valid", info.file).into());
        }
        if info.pruned && (kept || last) {
            return Err(format!(
                "manifest segment {} is pruned, but only segments before every kept segment can be pruned, and never the last",
                info.file
            )
            .into());
        }
        kept |= !info.pruned;
        next = info.last + 1;
        segments.push(info);
    }
    if last_zone + 1 != next {
        return Err("manifest last_zone does not match its segments".into());
    }
    Ok(Some(Manifest {
        receipts,
        last_zone,
        segments,
    }))
}
fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}
fn write_file_sync(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut f = File::create(path)?;
    f.write_all(data)?;
    f.sync_all()
}
fn write_manifest(dir: &Path, segments: &[SegmentInfo]) -> io::Result<()> {
    let last = segments.last().map_or(0, |s| s.last);
    let manifest = json!({
        "format": FORMAT,
        "reference_revision": REFERENCE_REVISION,
        "environment": ENVIRONMENT,
        "slice": [0, 0],
        "genesis_hash": hex(&GENESIS),
        "receipts": false,
        "last_zone": last,
        "segments": segments.iter().map(|s| {
            let mut v = json!({
                "file": s.file, "first_zone": s.first, "last_zone": s.last,
                "records": s.records, "sha256": s.sha256,
            });
            if s.pruned {
                v["pruned"] = json!(true);
            }
            v
        }).collect::<Vec<_>>(),
        "exported_utc": now_utc(),
    });
    let mut bytes = serde_json::to_vec_pretty(&manifest)?;
    bytes.push(b'\n');
    let path = dir.join("manifest.json");
    let tmp = dir.join("manifest.json.tmp");
    write_file_sync(&tmp, &bytes)?;
    fs::rename(&tmp, &path)?;
    sync_dir(dir)
}
fn file_sha256(path: &Path) -> io::Result<String> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize())[2..].to_string())
}
/// Go `removeUnlisted`: delete `.pending` files and segments the manifest
/// does not list (and their source files). The file of a pruned segment is
/// deleted too (left by an interrupted prune); its source file is kept.
fn remove_unlisted(dir: &Path, listed: &[SegmentInfo]) -> io::Result<()> {
    let keep: HashSet<String> = listed
        .iter()
        .filter(|s| !s.pruned)
        .map(|s| s.file.clone())
        .collect();
    let keep_sources: HashSet<String> = listed
        .iter()
        .map(|s| sources_name(s.first, s.last))
        .collect();
    for (sub, ext, keep) in [
        ("segments", "qex", &keep),
        ("sources", "json", &keep_sources),
    ] {
        let path = dir.join(sub);
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            let name = format!("{sub}/{}", entry.file_name().to_string_lossy());
            let base = name.strip_suffix(".pending").unwrap_or(&name);
            let ours = base.len() == sub.len() + 1 + 21 + 1 + ext.len() && base.ends_with(ext);
            if keep.contains(&name) || !ours || entry.file_type()?.is_dir() {
                continue;
            }
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

struct RawRecord {
    ctx: usize,
    number: u64,
    hash: Hash,
    block: Vec<u8>,
    receipts: Vec<u8>,
}
struct SegmentReader {
    r: BufReader<File>,
    offset: u64,
}
impl SegmentReader {
    fn open(path: &Path) -> Res<Self> {
        let mut r = BufReader::with_capacity(1 << 20, File::open(path)?);
        let mut magic = [0; 5];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(format!("{}: no QEX1 magic", path.display()).into());
        }
        Ok(Self { r, offset: 5 })
    }
    fn next(&mut self) -> Res<Option<RawRecord>> {
        let mut head = [0u8; 46];
        match self.r.read(&mut head[..1])? {
            0 => return Ok(None),
            _ => self.r.read_exact(&mut head[1..])?,
        }
        if head[0] != KIND_BLOCK || head[1] > 2 {
            return Err(format!("bad record header at byte {}", self.offset).into());
        }
        let field = |r: &mut BufReader<File>, len: u32| -> io::Result<Vec<u8>> {
            let mut v = Vec::with_capacity((len as usize).min(1 << 20));
            r.take(u64::from(len)).read_to_end(&mut v)?;
            if v.len() != len as usize {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            Ok(v)
        };
        let block_len = u32::from_be_bytes(head[42..46].try_into()?);
        let block = field(&mut self.r, block_len)?;
        let mut len = [0u8; 4];
        self.r.read_exact(&mut len)?;
        let receipts_len = u32::from_be_bytes(len);
        let receipts = field(&mut self.r, receipts_len)?;
        self.offset += 46 + u64::from(block_len) + 4 + u64::from(receipts_len);
        Ok(Some(RawRecord {
            ctx: head[1] as usize,
            number: u64::from_be_bytes(head[2..10].try_into()?),
            hash: head[10..42].try_into()?,
            block,
            receipts,
        }))
    }
}
/// The records of all segments of a manifest, in order. Each segment's
/// SHA-256 is checked before its records are read; a pruned segment is an
/// error.
struct ExportStream {
    dir: PathBuf,
    segments: Vec<SegmentInfo>,
    index: usize,
    reader: Option<SegmentReader>,
}
impl ExportStream {
    fn new(dir: &Path, segments: Vec<SegmentInfo>) -> Self {
        Self {
            dir: dir.to_path_buf(),
            segments,
            index: 0,
            reader: None,
        }
    }
    fn next(&mut self) -> Res<Option<RawRecord>> {
        loop {
            if self.reader.is_none() {
                let Some(info) = self.segments.get(self.index) else {
                    return Ok(None);
                };
                if info.pruned {
                    return Err(format!("{} is pruned: its file was deleted", info.file).into());
                }
                let path = self.dir.join(&info.file);
                if file_sha256(&path)? != info.sha256 {
                    return Err(format!("{}: sha256 does not match the manifest", info.file).into());
                }
                self.reader = Some(SegmentReader::open(&path)?);
            }
            if let Some(r) = self.reader.as_mut().ok_or("no reader")?.next()? {
                return Ok(Some(r));
            }
            self.reader = None;
            self.index += 1;
        }
    }
}

/// A segment written to `<name>.pending` and renamed when it is complete.
struct SegmentWriter {
    first: u64,
    pending: PathBuf,
    out: BufWriter<File>,
    sha: Sha256,
    records: u64,
    bytes: u64,
}
impl SegmentWriter {
    fn create(dir: &Path, first: u64, planned_last: u64) -> io::Result<Self> {
        let pending = dir.join(format!("{}.pending", segment_name(first, planned_last)));
        let mut w = Self {
            first,
            out: BufWriter::with_capacity(8 << 20, File::create(&pending)?),
            pending,
            sha: Sha256::new(),
            records: 0,
            bytes: 0,
        };
        w.put(MAGIC)?;
        Ok(w)
    }
    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.sha.update(bytes);
        self.bytes += bytes.len() as u64;
        Ok(())
    }
    fn push(&mut self, ctx: usize, number: u64, hash: &Hash, block: &[u8]) -> io::Result<()> {
        let len = u32::try_from(block.len()).map_err(|_| io::Error::other("block over 4 GiB"))?;
        let mut head = Vec::with_capacity(46);
        head.push(KIND_BLOCK);
        head.push(ctx as u8);
        head.extend_from_slice(&number.to_be_bytes());
        head.extend_from_slice(hash);
        head.extend_from_slice(&len.to_be_bytes());
        self.put(&head)?;
        self.put(block)?;
        self.put(&0u32.to_be_bytes())?; // receipts: none from peers
        self.records += 1;
        Ok(())
    }
    fn finish(mut self, dir: &Path, last: u64) -> io::Result<SegmentInfo> {
        self.out.flush()?;
        let file = self.out.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        drop(file);
        let name = segment_name(self.first, last);
        fs::rename(&self.pending, dir.join(&name))?;
        sync_dir(&dir.join("segments"))?;
        Ok(SegmentInfo {
            file: name,
            first: self.first,
            last,
            records: self.records,
            sha256: hex(&self.sha.finalize())[2..].to_string(),
            pruned: false,
        })
    }
}

/// Which source supplied each block range of a segment.
#[derive(Default)]
struct Provenance {
    ranges: Vec<(usize, u64, u64, String)>,
}
impl Provenance {
    fn add(&mut self, ctx: usize, number: u64, source: &str) {
        if let Some(r) = self
            .ranges
            .iter_mut()
            .rev()
            .find(|r| r.0 == ctx)
            .filter(|r| r.2 + 1 == number && r.3 == source)
        {
            r.2 = number;
            return;
        }
        self.ranges.push((ctx, number, number, source.to_string()));
    }
    fn write(&self, dir: &Path, info: &SegmentInfo) -> io::Result<()> {
        fs::create_dir_all(dir.join("sources"))?;
        let ranges: Vec<Value> = self
            .ranges
            .iter()
            .map(|(c, f, l, s)| json!({"context":NAMES[*c],"first":f,"last":l,"source":s}))
            .collect();
        let v = json!({"segment":info.file,"first_zone":info.first,"last_zone":info.last,"ranges":ranges});
        write_file_sync(
            &dir.join(sources_name(info.first, info.last)),
            &serde_json::to_vec_pretty(&v)?,
        )
    }
    fn load(dir: &Path, info: &SegmentInfo) -> Self {
        let mut p = Self::default();
        let Ok(bytes) = fs::read(dir.join(sources_name(info.first, info.last))) else {
            return p;
        };
        let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
            return p;
        };
        for r in v["ranges"].as_array().into_iter().flatten() {
            let ctx = NAMES.iter().position(|n| r["context"] == *n);
            if let (Some(ctx), Some(f), Some(l), Some(s)) = (
                ctx,
                r["first"].as_u64(),
                r["last"].as_u64(),
                r["source"].as_str(),
            ) {
                p.ranges.push((ctx, f, l, s.to_string()));
            }
        }
        p
    }
}

/// The output side of an export: checks every height and writes it.
struct Export {
    dir: PathBuf,
    _lock: File,
    /// Complete segments, pruned ones included (they stay listed).
    segments: Vec<SegmentInfo>,
    current: Option<SegmentWriter>,
    provenance: Provenance,
    checker: Checker,
    end: u64,
    /// Bytes of the segment files on disk (pruned segments have none).
    bytes: u64,
    span: u64,
}
/// Take the export directory's lock (one writer: an export or a prune).
fn lock_dir(dir: &Path) -> Res<File> {
    let lock = File::open(dir)?;
    lock.try_lock()
        .map_err(|e| format!("another export is writing {}: {e}", dir.display()))?;
    Ok(lock)
}
impl Export {
    fn open(dir: &Path) -> Res<Self> {
        Self::open_with(dir, SEGMENT_SPAN)
    }
    /// Open or resume. Complete segments are kept after their hash is
    /// checked; pruned segments stay listed and are not read. A partial last
    /// segment is checked record by record and copied into the new pending
    /// segment (the export rewrites it). The chain tips come from the newest
    /// complete segment, which a prune never deletes.
    fn open_with(dir: &Path, span: u64) -> Res<Self> {
        fs::create_dir_all(dir.join("segments"))?;
        let lock = lock_dir(dir)?;
        let old = read_manifest(dir, span)?;
        if old.as_ref().is_some_and(|m| m.receipts) {
            return Err(format!(
                "{} holds an export with receipts (a Go export); peer exports go to another directory",
                dir.display()
            )
            .into());
        }
        let listed = old.as_ref().map_or(vec![], |m| m.segments.clone());
        remove_unlisted(dir, &listed)?;
        let mut e = Self {
            dir: dir.to_path_buf(),
            _lock: lock,
            segments: vec![],
            current: None,
            provenance: Provenance::default(),
            checker: Checker::default(),
            end: u64::MAX,
            bytes: 0,
            span,
        };
        let Some(old) = old else {
            return Ok(e);
        };
        let mut partial = None;
        for s in old.segments {
            if s.pruned {
                // No file: a replay checkpointed past it. It stays listed.
                e.segments.push(s);
                continue;
            }
            let path = dir.join(&s.file);
            if file_sha256(&path)? != s.sha256 {
                return Err(format!("{}: sha256 does not match the manifest", s.file).into());
            }
            if s.complete(span) {
                e.bytes += fs::metadata(&path)?.len();
                e.segments.push(s);
            } else {
                partial = Some(s);
            }
        }
        // Chain state at the end of the complete segments, from the last zone
        // height of the newest one. That segment is enough, and a prune
        // always keeps it.
        if let Some(last) = e.segments.last() {
            if last.pruned {
                return Err(format!(
                    "cannot resume: the chain tips are read from the newest complete segment {}, which is pruned",
                    last.file
                )
                .into());
            }
            let mut r = SegmentReader::open(&dir.join(&last.file))?;
            let mut height: Vec<RawRecord> = vec![];
            while let Some(rec) = r.next()? {
                if height.last().is_some_and(|p| p.ctx == ZONE) {
                    height.clear();
                }
                height.push(rec);
            }
            let mut blocks = vec![];
            for rec in height {
                let block = decode_block(rec.block, rec.ctx).map_err(|err| {
                    format!("{}: {} {}: {err}", last.file, NAMES[rec.ctx], rec.number)
                })?;
                if block.hash != rec.hash || block.number != rec.number {
                    return Err(format!(
                        "{}: record {} does not match its block",
                        last.file, rec.number
                    )
                    .into());
                }
                blocks.push(block);
            }
            let tips = tips_after(&blocks);
            if tips.iter().any(Option::is_none) || tips[ZONE].map(|t| t.number) != Some(last.last) {
                return Err(
                    format!("{} does not end with zone height {}", last.file, last.last).into(),
                );
            }
            e.checker.last = tips;
            e.checker.next_zone = last.last + 1;
        }
        if let Some(p) = partial {
            let provenance = Provenance::load(dir, &p);
            let mut copied = 0;
            let mut r = SegmentReader::open(&dir.join(&p.file))?;
            let mut height: Vec<Block> = vec![];
            while let Some(rec) = r.next()? {
                let block = decode_block(rec.block, rec.ctx).map_err(|err| {
                    format!("{}: {} {}: {err}", p.file, NAMES[rec.ctx], rec.number)
                })?;
                if block.hash != rec.hash || block.number != rec.number {
                    return Err(format!(
                        "{}: record {} does not match its block",
                        p.file, rec.number
                    )
                    .into());
                }
                let zone = block.ctx == ZONE;
                height.push(block);
                if zone {
                    e.write_height(&height.iter().collect::<Vec<_>>(), &[])?;
                    height.clear();
                    copied += 1;
                }
            }
            if !height.is_empty() || copied != p.last - p.first + 1 {
                return Err(format!("{}: does not end with zone height {}", p.file, p.last).into());
            }
            e.provenance = provenance;
            emit(json!({"event":"resume_partial","segment":p.file,"zone_heights":copied}));
        }
        Ok(e)
    }
    fn next_zone(&self) -> u64 {
        self.checker.next_zone
    }
    /// Check one zone height and append it. `sources` names the supplier of
    /// each record (empty when copied from a partial segment).
    fn write_height(&mut self, records: &[&Block], sources: &[String]) -> Res<()> {
        self.checker.height(records)?;
        let z = self.checker.next_zone - 1;
        if self.current.is_none() {
            let first = z;
            let planned = (first + self.span - 1).min(self.end);
            self.current = Some(SegmentWriter::create(&self.dir, first, planned)?);
        }
        let seg = self.current.as_mut().ok_or("no segment")?;
        let before = seg.bytes;
        for (i, b) in records.iter().enumerate() {
            seg.push(b.ctx, b.number, &b.hash, &b.raw)?;
            if let Some(s) = sources.get(i) {
                self.provenance.add(b.ctx, b.number, s);
            }
        }
        self.bytes += seg.bytes - before;
        if z + 1 - seg.first == self.span {
            self.finish_segment(z)?;
        }
        Ok(())
    }
    fn finish_segment(&mut self, last: u64) -> Res<()> {
        let Some(seg) = self.current.take() else {
            return Ok(());
        };
        let info = seg.finish(&self.dir, last)?;
        self.provenance.write(&self.dir, &info)?;
        self.provenance = Provenance::default();
        self.segments.push(info.clone());
        write_manifest(&self.dir, &self.segments)?;
        remove_unlisted(&self.dir, &self.segments)?;
        emit(
            json!({"event":"segment","file":info.file,"first_zone":info.first,"last_zone":info.last,"records":info.records,"sha256":info.sha256}),
        );
        Ok(())
    }
    /// Planned stop: write the partial segment so the export is complete to
    /// the last written height.
    fn close(&mut self) -> Res<()> {
        let last = self.next_zone().saturating_sub(1);
        if self.current.as_ref().is_some_and(|s| s.records > 0) {
            self.finish_segment(last)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Peers.

#[derive(Debug)]
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
    fn io(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::InvalidData && e.to_string().contains("exceeds 3 MiB") {
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

/// One `/quai/1.0.0` stream with sequential, correlated requests.
struct Conn {
    node: Arc<ReadOnlyPeer>,
    peer: PeerId,
    stream: Option<WireStream>,
    book: RequestBook,
    id: u32,
    requests: u64,
    bytes: u64,
}
impl Conn {
    fn new(node: Arc<ReadOnlyPeer>, peer: PeerId) -> Res<Self> {
        Ok(Self {
            node,
            peer,
            stream: None,
            book: RequestBook::new(1)?,
            id: 0,
            requests: 0,
            bytes: 0,
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
        self.requests += 1;
        match &result {
            Ok((_, raw)) => self.bytes += raw.len() as u64,
            Err(_) => {
                self.book.cancel(id);
                self.stream = None;
            }
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
    /// Highest zone height the peer has, after the network check.
    async fn probe(&mut self) -> Result<u64, Fault> {
        match self.block_hash(ZONE, 0).await? {
            Some(h) if h == GENESIS => {}
            Some(h) => {
                return Err(Fault::Bad(format!(
                    "zone genesis {} is not mainnet",
                    hex(&h)
                )))
            }
            None => return Err(Fault::Missing("no zone genesis".into())),
        }
        let (mut lo, mut hi) = (0u64, 1024u64);
        while self.block_hash(ZONE, hi).await?.is_some() {
            lo = hi;
            hi = hi
                .checked_mul(2)
                .filter(|h| *h < 1 << 40)
                .ok_or(Fault::Bad("no head".into()))?;
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
    /// One batch: BlockHash(start), then the blocks from that hash. Every
    /// block is decoded and checked; the batch is a hash-linked run.
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
                    self.block_hash(job.ctx, job.start + i)
                        .await?
                        .ok_or_else(|| {
                            Fault::Missing(format!("{} {}", NAMES[job.ctx], job.start + i))
                        })?
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
        if raws.len() as u64 != BATCH {
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

#[derive(Default)]
struct Stats {
    batches: u64,
    blocks: u64,
    requests: u64,
    bytes: u64,
    faults: BTreeMap<&'static str, u64>,
    last_fault: Option<String>,
    strikes: u32,
    backoff_until: Option<Instant>,
}
struct Source {
    index: usize,
    label: String,
    peer: PeerId,
    node: Arc<ReadOnlyPeer>,
    head: u64,
    /// Highest zone height this peer is asked for: head minus the margin.
    limit: u64,
    alive: AtomicBool,
    stats: Mutex<Stats>,
}
impl Source {
    fn name(&self) -> String {
        format!("{}:{}", self.label, self.peer)
    }
    fn summary(&self) -> Value {
        let s = lock(&self.stats);
        json!({"source":self.label,"peer":self.peer.to_string(),"head":self.head,"limit":self.limit,
            "alive":self.alive.load(Ordering::Relaxed),"batches":s.batches,"blocks":s.blocks,
            "requests":s.requests,"bytes":s.bytes,"faults":s.faults,"last_fault":s.last_fault})
    }
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Work scheduling: 10-block batches per context, fetched ahead of the writer.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Job {
    ctx: usize,
    start: u64,
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
    zone_end: u64,
    write_pos: u64,
    window: u64,
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
        while self.zone_next <= self.zone_end
            && self.zone_next < self.write_pos + self.window * BATCH
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
        self.state.remove(&job);
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
            for b in blocks {
                self.dom[job.ctx].insert(
                    b.number,
                    Fetched {
                        block: Arc::new(b),
                        source,
                    },
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
            self.error = Some(format!(
                "{} batch {}: no valid reply after {} attempts (last: {why})",
                NAMES[job.ctx], job.start, st.attempts
            ));
        }
        self.pending.insert((st.need, job));
    }
    /// Drop a fetched batch after a chain-rule failure and fetch it again
    /// from another peer. A batch that is not stored (pending, in flight or
    /// already dropped) is left alone.
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

async fn worker(
    src: Arc<Source>,
    sched: Arc<Mutex<Sched>>,
    notify: Arc<Notify>,
    stop: Arc<AtomicBool>,
) {
    let Ok(mut conn) = Conn::new(src.node.clone(), src.peer) else {
        return;
    };
    while !stop.load(Ordering::Relaxed) && src.alive.load(Ordering::Relaxed) {
        let wait = lock(&src.stats)
            .backoff_until
            .filter(|t| *t > Instant::now());
        if let Some(t) = wait {
            tokio::time::sleep_until(t.into()).await;
            continue;
        }
        let job = lock(&sched).take(src.index, src.limit);
        let Some((job, single)) = job else {
            let _ = tokio::time::timeout(Duration::from_millis(200), notify.notified()).await;
            continue;
        };
        let (requests, bytes) = (conn.requests, conn.bytes);
        let result = conn.batch(job, single).await;
        let mut stats = lock(&src.stats);
        stats.requests += conn.requests - requests;
        stats.bytes += conn.bytes - bytes;
        match result {
            Ok(blocks) => {
                stats.batches += 1;
                stats.blocks += blocks.len() as u64;
                stats.strikes = 0;
                drop(stats);
                lock(&sched).complete(job, blocks, src.index);
            }
            Err(fault) => {
                *stats.faults.entry(fault.label()).or_default() += 1;
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
                emit(
                    json!({"event":"fault","source":src.name(),"context":NAMES[job.ctx],"start":job.start,"fault":why}),
                );
                lock(&sched).retry(job, src.index, exclude, single, &why);
            }
        }
        notify.notify_waiters();
    }
}

// ---------------------------------------------------------------------------
// Environment checks (scripts/mainnet/node_guard.sh rules).

fn default_gateway() -> Option<String> {
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
fn public_allowed() -> Result<String, String> {
    let gateway = default_gateway().ok_or("cannot read the default gateway")?;
    if gateway.starts_with("172.20.10.") && std::env::var("ALLOW_METERED").as_deref() != Ok("1") {
        return Err(format!(
            "default gateway {gateway} is an iPhone hotspot (metered)"
        ));
    }
    Ok(gateway)
}
fn free_bytes(dir: &Path) -> Res<u64> {
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(dir)
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1).ok_or("df: no output")?;
    let avail: u64 = line
        .split_whitespace()
        .nth(3)
        .ok_or("df: no column")?
        .parse()?;
    Ok(avail * 1024)
}

// ---------------------------------------------------------------------------
// Export command.

struct Args {
    out: PathBuf,
    to: Option<u64>,
    local: bool,
    public: bool,
    peer: String,
    max_peers: usize,
    inflight: usize,
    margin: u64,
    min_free: u64,
    max_bytes: Option<u64>,
    max_seconds: Option<u64>,
}
fn parse_export(mut it: impl Iterator<Item = String>) -> Res<Args> {
    let data = std::env::var("QUAI_MAINNET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join("quai-data/mainnet-v0.57.0")
        });
    let mut a = Args {
        out: data.join("peer-export"),
        to: None,
        local: true,
        public: true,
        peer: LOCAL_PEER.into(),
        max_peers: 8,
        inflight: 3,
        margin: 1000,
        min_free: 30 << 30,
        max_bytes: None,
        max_seconds: None,
    };
    while let Some(flag) = it.next() {
        let value = it.next().ok_or(format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--out" => a.out = PathBuf::from(value),
            "--to" => a.to = Some(value.parse()?),
            "--sources" => {
                (a.local, a.public) = match value.as_str() {
                    "local" => (true, false),
                    "public" => (false, true),
                    "all" => (true, true),
                    _ => return Err("--sources: local, public or all".into()),
                }
            }
            "--peer" => a.peer = value,
            "--max-peers" => a.max_peers = value.parse::<usize>()?.clamp(1, 8),
            "--inflight" => a.inflight = value.parse::<usize>()?.clamp(1, 4),
            "--margin" => a.margin = value.parse()?,
            "--min-free-gb" => a.min_free = value.parse::<u64>()? << 30,
            "--max-gb" => {
                a.max_bytes = Some((value.parse::<f64>()? * f64::from(1u32 << 30)) as u64)
            }
            "--max-seconds" => a.max_seconds = Some(value.parse()?),
            _ => return Err(format!("unknown option {flag}").into()),
        }
    }
    Ok(a)
}

struct Pool {
    sources: Mutex<Vec<Arc<Source>>>,
    sched: Arc<Mutex<Sched>>,
    notify: Arc<Notify>,
    stop: Arc<AtomicBool>,
    inflight: usize,
    margin: u64,
    max_peers: usize,
}
impl Pool {
    /// Probe a connected peer and start its workers.
    async fn add(
        &self,
        node: Arc<ReadOnlyPeer>,
        peer: PeerId,
        label: &str,
    ) -> Result<Arc<Source>, String> {
        if lock(&self.sources).iter().any(|s| s.peer == peer) {
            return Err("already a source".into());
        }
        let mut conn = Conn::new(node.clone(), peer).map_err(|e| e.to_string())?;
        let head = tokio::time::timeout(Duration::from_secs(120), conn.probe())
            .await
            .map_err(|_| "head probe timed out".to_string())?
            .map_err(|e| e.to_string())?;
        let mut sources = lock(&self.sources);
        if sources.len() >= self.max_peers {
            return Err("peer limit reached".into());
        }
        let src = Arc::new(Source {
            index: sources.len(),
            label: label.into(),
            peer,
            node,
            head,
            limit: head.saturating_sub(self.margin),
            alive: AtomicBool::new(true),
            stats: Mutex::new(Stats {
                requests: conn.requests,
                bytes: conn.bytes,
                ..Default::default()
            }),
        });
        sources.push(src.clone());
        drop(sources);
        emit(
            json!({"event":"source","source":src.label,"peer":peer.to_string(),"head":head,"limit":src.limit,"probe_requests":conn.requests}),
        );
        for _ in 0..self.inflight {
            tokio::spawn(worker(
                src.clone(),
                self.sched.clone(),
                self.notify.clone(),
                self.stop.clone(),
            ));
        }
        Ok(src)
    }
    fn alive(&self) -> Vec<Arc<Source>> {
        lock(&self.sources)
            .iter()
            .filter(|s| s.alive.load(Ordering::Relaxed))
            .cloned()
            .collect()
    }
}

async fn wait_connected(node: &ReadOnlyPeer, peer: &PeerId, secs: u64) -> bool {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        if node.is_connected(peer) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}
fn peer_of(a: &Multiaddr) -> Option<PeerId> {
    a.iter().find_map(|p| match p {
        libp2p::multiaddr::Protocol::P2p(id) => Some(id),
        _ => None,
    })
}

/// Public peers: the colosseum boot peers, then peers that they lead to.
async fn start_public(pool: Arc<Pool>) -> Res<Arc<ReadOnlyPeer>> {
    let node = Arc::new(ReadOnlyPeer::start(DialPolicy::Public)?);
    // Every newly connected peer is a candidate; Kademlia leads to them.
    let events = node.clone();
    let candidates = pool.clone();
    tokio::spawn(async move {
        let mut tried = HashSet::new();
        while let Ok(event) = events.next().await {
            if let Event::Connected { peer, .. } = event {
                if !tried.insert(peer) || candidates.stop.load(Ordering::Relaxed) {
                    continue;
                }
                let (pool, node) = (candidates.clone(), events.clone());
                tokio::spawn(async move {
                    if let Err(e) = pool.add(node, peer, "public").await {
                        emit(json!({"event":"source_rejected","peer":peer.to_string(),"reason":e}));
                    }
                });
            }
        }
    });
    for address in COLOSSEUM_BOOTNODES {
        let address: Multiaddr = address.parse()?;
        let peer = peer_of(&address).ok_or("boot peer without peer ID")?;
        if let Err(e) = node.dial(address.clone()).await {
            emit(json!({"event":"dial_error","target":address.to_string(),"error":e.to_string()}));
            continue;
        }
        if wait_connected(&node, &peer, 15).await {
            let bare: Multiaddr = address
                .iter()
                .filter(|p| !matches!(p, libp2p::multiaddr::Protocol::P2p(_)))
                .collect();
            node.kad_add(peer, bare).await?;
        }
    }
    // A FIND_NODE walk for our own ID finds the peers near us.
    node.closest_peers(node.id.to_bytes()).await?;
    Ok(node)
}

async fn run_export(args: Args) -> Res<()> {
    let started = Instant::now();
    // An export that already reaches --to needs no peers (and no rehash).
    if let (Some(to), Some(m)) = (args.to, read_manifest(&args.out, SEGMENT_SPAN)?) {
        if to <= m.last_zone && !m.receipts {
            emit(
                json!({"event":"up_to_date","out":args.out.display().to_string(),"last_zone":m.last_zone,"to":to,
                "reason":"the export already reaches --to"}),
            );
            return Ok(());
        }
    }
    let mut export = Export::open(&args.out)?;
    let stop_file = args.out.join("STOP");
    if stop_file.exists() {
        fs::remove_file(&stop_file)?;
    }
    let resume_at = export.next_zone();
    let tips = export.checker.last;
    let pruned = export.segments.iter().filter(|s| s.pruned).count();
    emit(
        json!({"event":"start","out":args.out.display().to_string(),"resume_at":resume_at,
        "kept_segments":export.segments.len() - pruned,"pruned_segments":pruned,
        "local":args.local,"public":args.public,
        "inflight":args.inflight,"margin":args.margin,"max_peers":args.max_peers}),
    );
    let sched = Arc::new(Mutex::new(Sched {
        pending: BTreeSet::new(),
        state: HashMap::new(),
        zone: BTreeMap::new(),
        dom: [BTreeMap::new(), BTreeMap::new()],
        dom_next: [0, 1].map(|c| tips[c].map_or(0, |t| (t.number + 1) / BATCH * BATCH)),
        zone_next: resume_at / BATCH * BATCH,
        zone_end: 0,
        write_pos: resume_at,
        window: 64,
        error: None,
    }));
    let pool = Arc::new(Pool {
        sources: Mutex::new(vec![]),
        sched: sched.clone(),
        notify: Arc::new(Notify::new()),
        stop: Arc::new(AtomicBool::new(false)),
        inflight: args.inflight,
        margin: args.margin,
        max_peers: args.max_peers,
    });
    // Sources: the local node first, then public peers.
    let mut _local = None;
    if args.local {
        let node = Arc::new(ReadOnlyPeer::start(DialPolicy::Loopback)?);
        let address: Multiaddr = args.peer.parse()?;
        let peer = peer_of(&address).ok_or("--peer needs a /p2p/ peer ID")?;
        node.dial(address).await?;
        if wait_connected(&node, &peer, 15).await {
            if let Err(e) = pool.add(node.clone(), peer, "local").await {
                emit(json!({"event":"source_rejected","peer":peer.to_string(),"reason":e}));
            }
        } else {
            emit(
                json!({"event":"source_rejected","peer":peer.to_string(),"reason":"no connection"}),
            );
        }
        _local = Some(node);
    }
    let mut public_node = None;
    if args.public {
        match public_allowed() {
            Ok(gateway) => {
                emit(json!({"event":"public_peers","gateway":gateway}));
                public_node = Some(start_public(pool.clone()).await?);
                // Let the boot peers' head probes finish before choosing the end.
                let until = Instant::now() + Duration::from_secs(60);
                while Instant::now() < until
                    && lock(&pool.sources)
                        .iter()
                        .filter(|s| s.label == "public")
                        .count()
                        < 2
                {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
            Err(e) => emit(json!({"event":"public_peers_refused","reason":e})),
        }
    }
    let sources = pool.alive();
    let best = sources
        .iter()
        .map(|s| s.limit)
        .max()
        .ok_or("no usable peer")?;
    // Every batch ends at or below a peer's limit; one more height is fetched
    // for the tip check after the last exported height.
    let top = ((best + 1) / BATCH * BATCH)
        .checked_sub(1)
        .ok_or("peers too short")?;
    let end = args.to.unwrap_or(u64::MAX).min(top.saturating_sub(1));
    if end < resume_at {
        // At the peers' head (less --margin): nothing to add yet. The
        // manifest still lists the old partial segment; drop its copy.
        pool.stop.store(true, Ordering::Relaxed);
        if let Some(seg) = export.current.take() {
            let _ = fs::remove_file(&seg.pending);
        }
        emit(
            json!({"event":"up_to_date","out":args.out.display().to_string(),"last_zone":resume_at.saturating_sub(1),
            "to":args.to,"peers_allow":top.saturating_sub(1),
            "reason":"the peers have no block above the export (less --margin)"}),
        );
        return Ok(());
    }
    export.end = end;
    {
        let mut s = lock(&sched);
        s.zone_end = end + 1;
        s.fill();
    }
    pool.notify.notify_waiters();
    // A resumed export must still be canonical: ask the peer with the highest
    // head for the tip hashes.
    if resume_at > 0 {
        let src = sources
            .iter()
            .max_by_key(|s| s.limit)
            .ok_or("no usable peer")?;
        let mut conn = Conn::new(src.node.clone(), src.peer)?;
        for (ctx, tip) in tips.iter().enumerate() {
            let tip = tip.ok_or("resume without a tip")?;
            let got = conn
                .block_hash(ctx, tip.number)
                .await
                .map_err(|e| e.to_string())?;
            if got != Some(tip.hash) {
                return Err(format!(
                    "the last exported {} block {} ({}) is no longer canonical at {}: remove the last segment from the manifest and export again",
                    NAMES[ctx], tip.number, hex(&tip.hash), src.name()
                ).into());
            }
        }
    }
    emit(
        json!({"event":"plan","from":resume_at,"to":end,"sources":sources.iter().map(|s| s.summary()).collect::<Vec<_>>()}),
    );

    let mut z = resume_at;
    let mut stop_reason: Option<String> = None;
    let mut last_report = Instant::now();
    let mut last_check = Instant::now();
    let mut report_z = z;
    let mut height_failures: HashMap<u64, u32> = HashMap::new();
    let mut tip_checked = false;
    loop {
        if lock(&sched).error.is_some() {
            break;
        }
        if last_check.elapsed() >= Duration::from_secs(10) {
            last_check = Instant::now();
            if stop_file.exists() {
                stop_reason = Some("STOP file".into());
            } else if args
                .max_seconds
                .is_some_and(|m| started.elapsed().as_secs() >= m)
            {
                stop_reason = Some("--max-seconds".into());
            } else if args.max_bytes.is_some_and(|m| export.bytes >= m) {
                stop_reason = Some("--max-gb".into());
            } else if let Some(why) = match free_bytes(&args.out) {
                Ok(free) if free < args.min_free => {
                    Some(format!("free space below {} GiB", args.min_free >> 30))
                }
                Ok(_) => None,
                Err(e) => Some(format!("cannot read the free space: {e}")),
            } {
                stop_reason = Some(why);
            } else if public_node.is_some() {
                if let Err(e) = public_allowed() {
                    for s in pool.alive().iter().filter(|s| s.label == "public") {
                        s.alive.store(false, Ordering::Relaxed);
                    }
                    public_node = None;
                    emit(json!({"event":"public_peers_dropped","reason":e}));
                }
            }
            if pool.alive().is_empty() {
                stop_reason = Some("no usable peers left".into());
            }
            if stop_reason.is_some() {
                break;
            }
        }
        if last_report.elapsed() >= Duration::from_secs(10) {
            let rate = (z - report_z) as f64 / last_report.elapsed().as_secs_f64();
            emit(
                json!({"event":"progress","zone":z,"end":end,"heights_per_s":(rate * 10.0).round() / 10.0,
                "overall_heights_per_s":((z - resume_at) as f64 / started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
                "bytes":export.bytes,"sources":pool.alive().iter().map(|s| s.summary()).collect::<Vec<_>>()}),
            );
            last_report = Instant::now();
            report_z = z;
        }
        // Assemble the records of zone height z from fetched batches.
        enum Step {
            Wait,
            Height(Vec<Fetched>),
            Tip(Fetched, [Fetched; 2]),
        }
        let step = {
            let mut s = lock(&sched);
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
                Some(zone) if z > end => {
                    let next = [0, 1].map(|c| {
                        let n = export.checker.last[c].map_or(0, |t| t.number + 1);
                        s.need_dominant(c, n, z);
                        s.dom[c].get(&n).cloned()
                    });
                    match next {
                        [Some(p), Some(r)] => Step::Tip(zone, [p, r]),
                        _ => Step::Wait,
                    }
                }
                Some(zone) => {
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
                let _ =
                    tokio::time::timeout(Duration::from_millis(100), pool.notify.notified()).await;
            }
            Step::Tip(child, next) => {
                export
                    .checker
                    .tip(&child.block, [&next[0].block, &next[1].block])?;
                tip_checked = true;
                break;
            }
            Step::Height(records) => {
                let blocks: Vec<&Block> = records.iter().map(|f| f.block.as_ref()).collect();
                let names: Vec<String> = {
                    let sources = lock(&pool.sources);
                    records
                        .iter()
                        .map(|f| sources.get(f.source).map_or("unknown".into(), |s| s.name()))
                        .collect()
                };
                match export.write_height(&blocks, &names) {
                    Ok(()) => {
                        z += 1;
                        let mut s = lock(&sched);
                        let start = z / BATCH * BATCH;
                        let below: Vec<u64> = s.zone.range(..start).map(|(k, _)| *k).collect();
                        for k in below {
                            s.zone.remove(&k);
                        }
                        for c in 0..2 {
                            if let Some(t) = export.checker.last[c] {
                                s.dom[c] = s.dom[c].split_off(&t.number);
                            }
                        }
                    }
                    Err(e)
                        if e.to_string().contains("parent")
                            || e.to_string().contains("coincident")
                            || e.to_string().contains("previous") =>
                    {
                        // Refetch the batches of this height from other peers.
                        let count = height_failures.entry(z).or_default();
                        *count += 1;
                        emit(
                            json!({"event":"chain_fault","zone":z,"attempt":*count,"error":e.to_string()}),
                        );
                        if *count >= 3 {
                            return Err(format!(
                                "zone height {z}: {e} (after refetching from other peers)"
                            )
                            .into());
                        }
                        let mut s = lock(&sched);
                        let start = z / BATCH * BATCH;
                        s.invalidate(Job { ctx: ZONE, start }, &e.to_string());
                        for f in records.iter().filter(|f| f.block.ctx != ZONE) {
                            s.invalidate(
                                Job {
                                    ctx: f.block.ctx,
                                    start: f.block.number / BATCH * BATCH,
                                },
                                &e.to_string(),
                            );
                        }
                        let zone_block = records.last().map(|f| f.block.numbers);
                        if let Some(n) = zone_block {
                            for (c, number) in n.iter().take(2).enumerate() {
                                s.invalidate(
                                    Job {
                                        ctx: c,
                                        start: number / BATCH * BATCH,
                                    },
                                    &e.to_string(),
                                );
                            }
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
    pool.stop.store(true, Ordering::Relaxed);
    if let Some(err) = lock(&sched).error.take() {
        // As the Go exporter: an error removes the pending segment.
        if let Some(seg) = export.current.take() {
            let _ = fs::remove_file(&seg.pending);
        }
        return Err(err.into());
    }
    export.close()?;
    let elapsed = started.elapsed().as_secs_f64();
    let written = export.next_zone() - resume_at;
    emit(json!({
        "event": if tip_checked { "complete" } else { "stopped" },
        "reason": stop_reason,
        "first_zone": resume_at, "last_zone": export.next_zone().saturating_sub(1),
        "tip_checked": tip_checked,
        "zone_heights": written,
        "heights_per_s": (written as f64 / elapsed * 10.0).round() / 10.0,
        "seconds": (elapsed * 10.0).round() / 10.0,
        "records": {"prime": export.checker.records[0], "region": export.checker.records[1], "zone": export.checker.records[2]},
        "transactions": export.checker.txs,
        "bytes": export.bytes,
        "segments": export.segments.iter().map(|s| json!({"file":s.file,"records":s.records,"sha256":s.sha256})).collect::<Vec<_>>(),
        "sources": lock(&pool.sources).iter().map(|s| s.summary()).collect::<Vec<_>>(),
    }));
    Ok(())
}

// ---------------------------------------------------------------------------
// compare and verify.

fn load(dir: &Path, span: u64) -> Res<Manifest> {
    read_manifest(dir, span)?
        .ok_or_else(|| format!("{} has no manifest.json", dir.display()).into())
}
/// Record-by-record comparison up to the shorter export; receipts ignored.
/// Both exports must hold every segment from genesis.
fn compare(ours: &Path, other: &Path) -> Res<bool> {
    let (a, b) = (load(ours, SEGMENT_SPAN)?, load(other, SEGMENT_SPAN)?);
    let last = a.last_zone.min(b.last_zone);
    let (mut ra, mut rb) = (
        ExportStream::new(ours, a.segments),
        ExportStream::new(other, b.segments),
    );
    let mut records = 0u64;
    let mut receipts_ignored = 0u64;
    loop {
        let (x, y) = (ra.next()?, rb.next()?);
        let (Some(x), Some(y)) = (x, y) else {
            return Err("an export ended before the compared range".into());
        };
        let same = x.ctx == y.ctx && x.number == y.number && x.hash == y.hash && x.block == y.block;
        if !same {
            emit(json!({"event":"difference","record":records,
                "ours":{"context":NAMES[x.ctx],"number":x.number,"hash":hex(&x.hash),"block_len":x.block.len()},
                "other":{"context":NAMES[y.ctx],"number":y.number,"hash":hex(&y.hash),"block_len":y.block.len()}}));
            return Ok(false);
        }
        receipts_ignored += u64::from(x.receipts != y.receipts);
        records += 1;
        if x.ctx == ZONE && x.number == last {
            break;
        }
    }
    emit(
        json!({"event":"compared","identical":true,"zone_heights":last + 1,"records":records,
        "records_with_different_receipts":receipts_ignored,"ours_receipts":a.receipts,"other_receipts":b.receipts}),
    );
    Ok(true)
}
/// Check an export again from its files: manifest, hashes, and every record
/// with the same rules as the export (Go `-verify`). After a pruned prefix
/// the chain rules start at the first kept zone block, whose own parent links
/// stand for the pruned blocks.
fn verify(dir: &Path, span: u64) -> Res<()> {
    let m = load(dir, span)?;
    let started = Instant::now();
    let mut checker = Checker::default();
    let pruned = m.segments.iter().filter(|s| s.pruned).count();
    let from_zone = m.segments.get(pruned).map_or(0, |s| s.first);
    for s in m.segments.iter().filter(|s| !s.pruned) {
        let mut stream = ExportStream::new(dir, vec![s.clone()]);
        let mut height: Vec<Block> = vec![];
        let mut records = 0;
        while let Some(rec) = stream.next()? {
            if !m.receipts && !rec.receipts.is_empty() {
                return Err(format!("{}: receipts in an export without receipts", s.file).into());
            }
            let block = decode_block(rec.block, rec.ctx)
                .map_err(|e| format!("{}: {} {}: {e}", s.file, NAMES[rec.ctx], rec.number))?;
            if block.hash != rec.hash || block.number != rec.number {
                return Err(format!(
                    "{}: {} record {} does not match its block",
                    s.file, NAMES[rec.ctx], rec.number
                )
                .into());
            }
            records += 1;
            let zone = block.ctx == ZONE;
            if zone && checker.last[ZONE].is_none() && block.number > 0 {
                checker.anchor(&block);
            }
            height.push(block);
            if zone {
                checker
                    .height(&height.iter().collect::<Vec<_>>())
                    .map_err(|e| format!("{}: {e}", s.file))?;
                height.clear();
            }
        }
        if !height.is_empty() || checker.next_zone != s.last + 1 || records != s.records {
            return Err(format!("{}: records do not match the manifest", s.file).into());
        }
        emit(
            json!({"event":"verified_segment","file":s.file,"records":records,"seconds":started.elapsed().as_secs()}),
        );
    }
    emit(
        json!({"event":"verified","from_zone":from_zone,"last_zone":m.last_zone,"pruned_segments":pruned,
        "records":checker.records,"transactions":checker.txs,"receipts":m.receipts}),
    );
    Ok(())
}

/// The segments that `prune` deletes: complete segments that end below
/// `below`, except the newest complete segment, from which a resumed export
/// reads its chain tips. The last segment is that segment or the partial
/// one, so it is kept too. Only a prefix of the segments qualifies.
fn prunable(segments: &[SegmentInfo], below: u64, span: u64) -> Vec<usize> {
    let newest = segments.iter().rposition(|s| s.complete(span));
    segments
        .iter()
        .enumerate()
        .filter(|(i, s)| !s.pruned && s.complete(span) && s.last < below && Some(*i) != newest)
        .map(|(i, _)| i)
        .collect()
}
/// `prune DIR --below ZONE`: mark the prunable segments `"pruned": true` in
/// the manifest, then delete their files. A replay that resumes from a
/// checkpoint at or above ZONE never reads them. The manifest is written
/// first, so an interruption leaves only files that the next prune or export
/// deletes, never a listed segment without its file.
fn prune(dir: &Path, below: u64, span: u64) -> Res<Value> {
    let _lock = lock_dir(dir)?;
    let mut m = load(dir, span)?;
    if m.receipts {
        return Err(format!(
            "{} holds an export with receipts (a Go export); prune only peer exports",
            dir.display()
        )
        .into());
    }
    let chosen = prunable(&m.segments, below, span);
    for &i in &chosen {
        m.segments[i].pruned = true;
    }
    if !chosen.is_empty() {
        write_manifest(dir, &m.segments)?;
    }
    // Files of segments pruned now, and of segments pruned by an
    // interrupted earlier run.
    let size = |s: &SegmentInfo| fs::metadata(dir.join(&s.file)).map_or(0, |md| md.len());
    let freed: u64 = m.segments.iter().filter(|s| s.pruned).map(size).sum();
    remove_unlisted(dir, &m.segments)?;
    sync_dir(&dir.join("segments"))?;
    let kept: Vec<&SegmentInfo> = m.segments.iter().filter(|s| !s.pruned).collect();
    Ok(json!({
        "event": "pruned", "below": below,
        "pruned_now": chosen.iter().map(|&i| m.segments[i].file.clone()).collect::<Vec<_>>(),
        "freed_bytes": freed,
        "pruned_segments": m.segments.len() - kept.len(),
        "kept": kept.iter().map(|s| s.file.clone()).collect::<Vec<_>>(),
        "kept_bytes": kept.iter().map(|s| size(s)).sum::<u64>(),
        "first_kept_zone": kept.first().map(|s| s.first),
        "last_zone": m.last_zone,
    }))
}
/// `status DIR`: the manifest summary as one JSON line (for scripts).
fn status(dir: &Path) -> Res<Value> {
    let m = load(dir, SEGMENT_SPAN)?;
    let kept: Vec<&SegmentInfo> = m.segments.iter().filter(|s| !s.pruned).collect();
    let bytes: u64 = kept
        .iter()
        .map(|s| fs::metadata(dir.join(&s.file)).map_or(0, |md| md.len()))
        .sum();
    Ok(json!({
        "event": "status", "last_zone": m.last_zone, "receipts": m.receipts,
        "segments": m.segments.len(), "pruned_segments": m.segments.len() - kept.len(),
        "first_kept_zone": kept.first().map(|s| s.first), "kept_bytes": bytes,
    }))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Res<()> {
    let mut args = std::env::args().skip(1).peekable();
    let result = match args.peek().map(String::as_str) {
        Some("help" | "-h" | "--help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some("compare") => {
            args.next();
            let (Some(a), Some(b)) = (args.next(), args.next()) else {
                return Err("usage: peer-export compare OURS OTHER".into());
            };
            match compare(Path::new(&a), Path::new(&b))? {
                true => Ok(()),
                false => Err("the exports differ".into()),
            }
        }
        Some("verify") => {
            args.next();
            let dir = args.next().ok_or("usage: peer-export verify DIR")?;
            verify(Path::new(&dir), SEGMENT_SPAN)
        }
        Some("prune") => {
            args.next();
            let usage = "usage: peer-export prune DIR --below ZONE";
            let (Some(dir), Some("--below"), Some(zone), None) = (
                args.next(),
                args.next().as_deref(),
                args.next(),
                args.next(),
            ) else {
                return Err(usage.into());
            };
            let below = zone.parse().map_err(|_| usage)?;
            prune(Path::new(&dir), below, SEGMENT_SPAN).map(emit)
        }
        Some("status") => {
            args.next();
            let (Some(dir), None) = (args.next(), args.next()) else {
                return Err("usage: peer-export status DIR".into());
            };
            status(Path::new(&dir)).map(emit)
        }
        Some("export") => {
            args.next();
            run_export(parse_export(args)?).await
        }
        _ => run_export(parse_export(args)?).await,
    };
    if let Err(e) = &result {
        eprintln!("peer-export: error: {e}");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use quai_primitives::schema::{
        block::{ProtoWorkObjectBlockView, ProtoWorkObjectBlocksView},
        quaiprotocol::QuaiResponseMessage,
    };

    fn fixture() -> Vec<RawRecord> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mainnet/qex1-0-8");
        let m = load(&dir, SEGMENT_SPAN).unwrap();
        let mut s = ExportStream::new(&dir, m.segments);
        let mut out = vec![];
        while let Some(r) = s.next().unwrap() {
            out.push(r);
        }
        out
    }
    fn heights(records: &[RawRecord]) -> Vec<Vec<Block>> {
        let mut out = vec![];
        let mut h = vec![];
        for r in records {
            let b = decode_block(r.block.clone(), r.ctx).unwrap();
            assert_eq!((b.hash, b.number), (r.hash, r.number));
            let zone = b.ctx == ZONE;
            h.push(b);
            if zone {
                out.push(std::mem::take(&mut h));
            }
        }
        out
    }
    fn tempdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("peer-export-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn constants_and_time() {
        assert_eq!(
            hex(&GENESIS),
            "0xac81c28f1a72591b87b5f16c9793cdc0e87c45c6d426d1a364c3b8f6386b5b8b"
        );
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_794_970_210), "2026-11-18T02:50:10Z");
        assert_eq!(utc(1_735_689_599), "2024-12-31T23:59:59Z");
        assert_eq!(number_bytes(0), Vec::<u8>::new());
        assert_eq!(number_bytes(0x0102), vec![1, 2]);
    }

    #[test]
    fn reply_objects_are_the_bytes_the_peer_sent() {
        let records = fixture();
        let zone: Vec<&RawRecord> = records.iter().filter(|r| r.ctx == ZONE).collect();
        let views: Vec<ProtoWorkObjectBlockView> = zone
            .iter()
            .map(|r| ProtoWorkObjectBlockView {
                work_object: Some(ProtoWorkObject::decode(r.block.as_slice()).unwrap()),
            })
            .collect();
        let frame = QuaiMessage {
            payload: Some(Payload::Response(QuaiResponseMessage {
                id: 1,
                location: Some(ProtoLocation { value: vec![0, 0] }),
                response: Some(rs::Response::WorkObjectBlocksView(
                    ProtoWorkObjectBlocksView {
                        work_objects: views.clone(),
                    },
                )),
            })),
        }
        .encode_to_vec();
        let got = reply_objects(&frame, true).unwrap();
        assert_eq!(got.len(), zone.len());
        for (g, r) in got.iter().zip(&zone) {
            assert_eq!(g, &r.block);
        }
        // An empty reply (block not found) has no objects.
        let empty = QuaiMessage {
            payload: Some(Payload::Response(QuaiResponseMessage {
                id: 1,
                location: Some(ProtoLocation { value: vec![0, 0] }),
                response: Some(rs::Response::WorkObjectBlockView(Default::default())),
            })),
        }
        .encode_to_vec();
        assert!(reply_objects(&empty, false).unwrap().is_empty());
    }

    #[test]
    fn go_rules_accept_the_mainnet_fixture_and_reject_breaks() {
        let records = fixture();
        assert_eq!(records.len(), 17);
        let hs = heights(&records);
        assert_eq!(hs.len(), 9);
        let mut c = Checker::default();
        for h in &hs {
            c.height(&h.iter().collect::<Vec<_>>()).unwrap();
        }
        assert_eq!(c.records, [3, 5, 9]);
        // Tip rule: the fixture's next zone height is not included, so check
        // the rule on height 8 with the state after height 7.
        let mut c = Checker::default();
        for h in &hs[..8] {
            c.height(&h.iter().collect::<Vec<_>>()).unwrap();
        }
        let child = hs[8].last().unwrap();
        // A zone-only child must point at the last prime and region blocks.
        assert!(c.dominant_parents(child, [false; 2], "child").is_ok());
        // A skipped height, a wrong parent and a dropped region record fail.
        let mut c = Checker::default();
        c.height(&hs[0].iter().collect::<Vec<_>>()).unwrap();
        assert!(c.height(&hs[2].iter().collect::<Vec<_>>()).is_err());
        let mut broken = hs[1].last().unwrap().clone();
        broken.parent = [9; 32];
        assert!(c.height(&[&broken]).unwrap_err().contains("parent hash"));
        // A coincident block written without its dominant records breaks the
        // dominant chains at the next dominant record.
        let mut c = Checker::default();
        c.height(&hs[0].iter().collect::<Vec<_>>()).unwrap();
        c.height(&hs[1].iter().collect::<Vec<_>>()).unwrap();
        c.height(&[hs[2].last().unwrap()]).unwrap();
        assert!(c
            .height(&hs[3].iter().collect::<Vec<_>>())
            .unwrap_err()
            .contains("previous"));
        // A tampered block fails decode: the hash binding or the roots.
        let zone7 = records
            .iter()
            .find(|r| r.ctx == ZONE && r.number == 7)
            .unwrap();
        let mut wire = ProtoWorkObject::decode(zone7.block.as_slice()).unwrap();
        wire.wo_body.as_mut().unwrap().transactions = Some(Default::default());
        assert!(decode_block(wire.encode_to_vec(), ZONE)
            .unwrap_err()
            .contains("transaction root"));
    }

    #[test]
    fn writer_resumes_a_partial_segment_and_refuses_receipts() {
        let records = fixture();
        let hs = heights(&records);
        let dir = tempdir("resume");
        let mut e = Export::open(&dir).unwrap();
        e.end = 8;
        for h in &hs[..5] {
            e.write_height(
                &h.iter().collect::<Vec<_>>(),
                &vec!["test:a".to_string(); h.len()],
            )
            .unwrap();
        }
        e.close().unwrap();
        drop(e);
        let m = read_manifest(&dir, SEGMENT_SPAN).unwrap().unwrap();
        assert_eq!((m.last_zone, m.segments.len(), m.receipts), (4, 1, false));
        // A stale pending file is removed on open.
        fs::write(dir.join("segments/0000000000-0000000099.qex.pending"), b"x").unwrap();
        let mut e = Export::open(&dir).unwrap();
        assert!(!dir
            .join("segments/0000000000-0000000099.qex.pending")
            .exists());
        assert_eq!(e.next_zone(), 5);
        e.end = 8;
        for h in &hs[5..] {
            e.write_height(
                &h.iter().collect::<Vec<_>>(),
                &vec!["test:b".to_string(); h.len()],
            )
            .unwrap();
        }
        e.close().unwrap();
        drop(e);
        let m = read_manifest(&dir, SEGMENT_SPAN).unwrap().unwrap();
        assert_eq!(m.segments[0].file, "segments/0000000000-0000000008.qex");
        assert!(!dir.join("segments/0000000000-0000000004.qex").exists());
        assert!(!dir.join("sources/0000000000-0000000004.json").exists());
        let sources = fs::read_to_string(dir.join("sources/0000000000-0000000008.json")).unwrap();
        assert!(sources.contains("test:a") && sources.contains("test:b"));
        verify(&dir, SEGMENT_SPAN).unwrap();
        // The same records as the Go fixture, without receipts.
        let fixture_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mainnet/qex1-0-8");
        assert!(compare(&dir, &fixture_dir).unwrap());
        // A directory with a receipts export is refused.
        let go = tempdir("go");
        for f in ["manifest.json", "segments/0000000000-0000000008.qex"] {
            fs::create_dir_all(go.join("segments")).unwrap();
            fs::copy(fixture_dir.join(f), go.join(f)).unwrap();
        }
        let err = Export::open(&go).err().unwrap().to_string();
        assert!(err.contains("receipts"), "{err}");
        assert!(go.join("segments/0000000000-0000000008.qex").exists());
        let err = prune(&go, u64::MAX, SEGMENT_SPAN).unwrap_err().to_string();
        assert!(err.contains("receipts"), "{err}");
        // An export that already reaches --to stops before it needs a peer.
        let args = parse_export(
            ["--out", dir.to_str().unwrap(), "--to", "8"]
                .map(String::from)
                .into_iter(),
        )
        .unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_export(args))
            .unwrap();
        let s = status(&dir).unwrap();
        assert_eq!(
            (&s["last_zone"], &s["segments"], &s["pruned_segments"]),
            (&json!(8), &json!(1), &json!(0))
        );
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&go);
    }

    fn info(first: u64, last: u64, pruned: bool) -> SegmentInfo {
        SegmentInfo {
            file: segment_name(first, last),
            first,
            last,
            records: last - first + 1,
            sha256: "00".repeat(32),
            pruned,
        }
    }

    #[test]
    fn prune_rule_keeps_the_newest_complete_and_the_last_segment() {
        let s = SEGMENT_SPAN;
        let complete = |i: u64, pruned: bool| info(i * s, (i + 1) * s - 1, pruned);
        // Four complete segments and a partial one.
        let mut segs: Vec<SegmentInfo> = (0..4).map(|i| complete(i, false)).collect();
        segs.push(info(4 * s, 4 * s + 9, false));
        let none: Vec<usize> = vec![];
        assert_eq!(prunable(&segs, 0, s), none);
        // A checkpoint inside segment 0 keeps it; one at its end + 1 does not.
        assert_eq!(prunable(&segs, s - 1, s), none);
        assert_eq!(prunable(&segs, s, s), [0]);
        assert_eq!(prunable(&segs, 2 * s + 5, s), [0, 1]);
        // Segment 3 is the newest complete segment, 4 the partial one.
        assert_eq!(prunable(&segs, u64::MAX, s), [0, 1, 2]);
        // Segments pruned before are not chosen again.
        segs[0].pruned = true;
        assert_eq!(prunable(&segs, u64::MAX, s), [1, 2]);
        // Without a partial segment, the last (newest complete) one is kept.
        segs.pop();
        assert_eq!(prunable(&segs, u64::MAX, s), [1, 2]);
        assert_eq!(prunable(&[complete(0, false)], u64::MAX, s), none);
        assert_eq!(prunable(&[info(0, 7, false)], u64::MAX, s), none);

        // The manifest round trip: "pruned" is written only when true.
        let dir = tempdir("manifest-pruned");
        let mut segs: Vec<SegmentInfo> = (0..3).map(|i| complete(i, i < 2)).collect();
        segs.push(info(3 * s, 3 * s + 4, false));
        write_manifest(&dir, &segs).unwrap();
        let text = fs::read_to_string(dir.join("manifest.json")).unwrap();
        assert_eq!(text.matches("\"pruned\": true").count(), 2);
        assert!(!text.contains("\"pruned\": false"));
        let m = read_manifest(&dir, s).unwrap().unwrap();
        assert_eq!(m.segments, segs);
        assert_eq!(m.last_zone, 3 * s + 4);
        // Only a prefix can be pruned, never the last segment.
        let mut gap = segs.clone();
        gap[0].pruned = false;
        let mut last = segs.clone();
        last[3].pruned = true;
        last[2].pruned = true;
        for bad in [gap, last] {
            write_manifest(&dir, &bad).unwrap();
            let e = read_manifest(&dir, s).unwrap_err().to_string();
            assert!(e.contains("pruned"), "{e}");
        }
        let mut v: Value = serde_json::from_str(&text).unwrap();
        v["segments"][0]["pruned"] = json!("yes");
        fs::write(dir.join("manifest.json"), v.to_string()).unwrap();
        assert!(read_manifest(&dir, s).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pruned_export_resumes_extends_and_verifies() {
        // Segments of 3 zone heights over the 9 mainnet fixture heights.
        const SPAN: u64 = 3;
        let records = fixture();
        let hs = heights(&records);
        // The tips that a resume takes from the last height of a segment are
        // the last exported records of each context.
        let mut c = Checker::default();
        for h in &hs {
            c.height(&h.iter().collect::<Vec<_>>()).unwrap();
            assert_eq!(tips_after(h), c.last);
        }
        let dir = tempdir("pruned");
        let write = |e: &mut Export, range: std::ops::Range<usize>| {
            for h in &hs[range] {
                e.write_height(
                    &h.iter().collect::<Vec<_>>(),
                    &vec!["test".to_string(); h.len()],
                )
                .unwrap();
            }
        };
        let files = |m: &Manifest| -> Vec<(String, bool)> {
            m.segments
                .iter()
                .map(|s| (s.file.clone(), s.pruned))
                .collect()
        };
        let mut e = Export::open_with(&dir, SPAN).unwrap();
        e.end = 8;
        write(&mut e, 0..8);
        e.close().unwrap();
        drop(e);
        let m = read_manifest(&dir, SPAN).unwrap().unwrap();
        assert_eq!(
            files(&m),
            [
                (segment_name(0, 2), false),
                (segment_name(3, 5), false),
                (segment_name(6, 7), false)
            ]
        );
        // A replay checkpointed at 7: 0-2 goes. 3-5 is the newest complete
        // segment and 6-7 the partial one: both stay.
        let v = prune(&dir, 7, SPAN).unwrap();
        assert_eq!(v["pruned_now"], json!([segment_name(0, 2)]));
        assert!(v["freed_bytes"].as_u64().unwrap() > 0);
        assert_eq!(v["first_kept_zone"], json!(3));
        assert!(!dir.join(segment_name(0, 2)).exists());
        assert!(dir.join(sources_name(0, 2)).exists());
        // Pruning again, even with no floor, changes nothing.
        let v = prune(&dir, u64::MAX, SPAN).unwrap();
        assert_eq!(v["pruned_now"], json!([]));
        assert_eq!(v["freed_bytes"], json!(0));
        // A file left at the pruned path (an interrupted prune) is deleted on
        // open and never read: the resume hashes and reads only 3-5 and 6-7.
        fs::write(dir.join(segment_name(0, 2)), b"left over").unwrap();
        // An export whose newest complete segment is pruned cannot resume.
        let lost = tempdir("pruned-tips");
        fs::create_dir_all(lost.join("segments")).unwrap();
        fs::copy(dir.join(segment_name(6, 7)), lost.join(segment_name(6, 7))).unwrap();
        let mut segs = read_manifest(&dir, SPAN).unwrap().unwrap().segments;
        segs[1].pruned = true;
        write_manifest(&lost, &segs).unwrap();
        let err = Export::open_with(&lost, SPAN).err().unwrap().to_string();
        assert!(
            err.contains("cannot resume") && err.contains(&segment_name(3, 5)),
            "{err}"
        );
        // Resume the pruned export and extend it by one height.
        let mut e = Export::open_with(&dir, SPAN).unwrap();
        assert!(!dir.join(segment_name(0, 2)).exists());
        assert_eq!(e.next_zone(), 8);
        e.end = 8;
        write(&mut e, 8..9);
        e.close().unwrap();
        drop(e);
        let m = read_manifest(&dir, SPAN).unwrap().unwrap();
        assert_eq!(m.last_zone, 8);
        assert_eq!(
            files(&m),
            [
                (segment_name(0, 2), true),
                (segment_name(3, 5), false),
                (segment_name(6, 8), false)
            ]
        );
        // verify checks every kept record, from the first kept block on.
        verify(&dir, SPAN).unwrap();
        // The kept records are the fixture's records for heights 3..=8.
        let kept: Vec<SegmentInfo> = m.segments.iter().filter(|s| !s.pruned).cloned().collect();
        let mut stream = ExportStream::new(&dir, kept);
        let mut got = vec![];
        while let Some(r) = stream.next().unwrap() {
            got.push((r.ctx, r.number, r.hash, r.block));
        }
        let want: Vec<_> = hs[3..]
            .iter()
            .flatten()
            .map(|b| (b.ctx, b.number, b.hash, b.raw.clone()))
            .collect();
        assert_eq!(got, want);
        // A pruned segment cannot be streamed (compare needs every segment).
        let mut stream = ExportStream::new(&dir, m.segments.clone());
        assert!(stream.next().err().unwrap().to_string().contains("pruned"));
        // Now 6-8 is the newest complete segment, so 3-5 can go, and the
        // export still resumes at 9 with the zone tip of height 8.
        let v = prune(&dir, 9, SPAN).unwrap();
        assert_eq!(v["pruned_now"], json!([segment_name(3, 5)]));
        let e = Export::open_with(&dir, SPAN).unwrap();
        assert_eq!(e.next_zone(), 9);
        assert_eq!(
            e.checker.last[ZONE].map(|t| t.hash),
            Some(hs[8].last().unwrap().hash)
        );
        drop(e);
        verify(&dir, SPAN).unwrap();
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&lost);
    }
}
