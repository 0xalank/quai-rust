//! Bounded peer acquisition for the reviewed private canonical profile. Peers
//! supply raw objects only; native admission/execution decides every publication.
use crate::{
    decode_envelope, response,
    transport::{PrivatePeer, WireStream, IO_TIMEOUT},
    RequestBook,
};
use prost::Message;
use quai_consensus::{
    history::Record,
    placement,
    private_canonical::{check_scope, AdmittedView, BlockBundle, PrivateCanonical},
    verifier::{HeaderProofMemo, Verifier},
    zone_admission,
};
use quai_primitives::{
    schema::{
        block::{ProtoWorkObject, ProtoWorkObjectBlockView},
        common::{ProtoHash, ProtoLocation},
        quaiprotocol::{
            quai_message::Payload, quai_request_message as rq, quai_response_message as rs,
            QuaiMessage, QuaiRequestMessage, QuaiResponseMessage,
        },
    },
    Hash,
};
use std::{
    collections::BTreeSet,
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
pub mod forward;
pub const MAX_SYNC_BLOCKS: usize = 4095;
pub const MAX_SYNC_BYTES: usize = 32 << 20;
pub const SYNC_DEADLINE: Duration = Duration::from_secs(60);
fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}
fn need(ok: bool, e: &str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(bad(e))
    }
}
#[derive(Debug)]
pub struct Acquired {
    pub blocks: Vec<BlockBundle>,
    pub requests: usize,
    pub bytes: usize,
    header_proofs: HeaderProofMemo,
}
#[derive(Debug)]
pub struct Applied {
    pub fetched_blocks: usize,
    pub imported_blocks: usize,
    pub selected_changes: usize,
    pub reorgs: usize,
    pub requests: usize,
    pub bytes: usize,
    pub head: Hash,
    pub height: u64,
    pub reused_header_proofs: usize,
    pub header_proof_hits: u64,
    pub header_proof_misses: u64,
    pub global_full_consensus: bool,
    pub public_sync_qualified: bool,
}
impl Acquired {
    /// Raw inputs carry no reusable proof until checked by a native verifier.
    pub fn new(blocks: Vec<BlockBundle>, requests: usize, bytes: usize) -> Self {
        Self {
            blocks,
            requests,
            bytes,
            header_proofs: HeaderProofMemo::default(),
        }
    }
    pub fn apply(self, chain: &mut PrivateCanonical, now: u64) -> io::Result<Applied> {
        self.apply_while(chain, now, || true)
    }
    /// Cancellation is checked between atomic block publications. A completed
    /// valid prefix remains durable; an in-flight import completes atomically.
    pub fn apply_cancellable(
        self,
        chain: &mut PrivateCanonical,
        now: u64,
        cancelled: &AtomicBool,
    ) -> io::Result<Applied> {
        self.apply_while(chain, now, || !cancelled.load(Ordering::Acquire))
    }
    fn apply_while(
        self,
        chain: &mut PrivateCanonical,
        now: u64,
        proceed: impl Fn() -> bool,
    ) -> io::Result<Applied> {
        let fetched_blocks = self.blocks.len();
        let mut imported_blocks = 0;
        let mut selected_changes = 0;
        let mut reorgs = 0;
        let before = chain.header_proof_memo_stats();
        let reused_header_proofs = chain.reuse_header_proof_memo(self.header_proofs)?;
        for b in self.blocks {
            if !proceed() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "private import cancelled between blocks",
                ));
            }
            let r = chain.import(b, now)?;
            imported_blocks += usize::from(!r.duplicate);
            selected_changes += usize::from(r.selected);
            reorgs += usize::from(!r.detached.is_empty());
        }
        let view = chain.view()?;
        let after = chain.header_proof_memo_stats();
        Ok(Applied {
            fetched_blocks,
            imported_blocks,
            selected_changes,
            reorgs,
            requests: self.requests,
            bytes: self.bytes,
            head: view.block(),
            height: view.height(),
            reused_header_proofs,
            header_proof_hits: after.hits.saturating_sub(before.hits),
            header_proof_misses: after.misses.saturating_sub(before.misses),
            global_full_consensus: false,
            public_sync_qualified: false,
        })
    }
}
fn location(context: usize) -> Vec<u8> {
    vec![0; context]
}
async fn request(
    stream: &mut WireStream,
    book: &mut RequestBook,
    id: u32,
    hash: Hash,
    context: usize,
    bytes: &mut usize,
) -> io::Result<Vec<u8>> {
    let req = QuaiRequestMessage {
        id,
        location: Some(ProtoLocation {
            value: location(context),
        }),
        data: Some(rq::Data::Hash(ProtoHash {
            value: hash.to_vec(),
        })),
        request: Some(rq::Request::WorkObjectBlock(
            ProtoWorkObjectBlockView::default(),
        )),
    };
    book.register(&req).map_err(bad)?;
    stream
        .write(
            &QuaiMessage {
                payload: Some(Payload::Request(req)),
            }
            .encode_to_vec(),
        )
        .await?;
    let raw = stream.read().await?;
    *bytes = bytes
        .checked_add(raw.len())
        .ok_or_else(|| bad("sync byte overflow"))?;
    need(*bytes <= MAX_SYNC_BYTES, "sync acquisition byte budget")?;
    let reply = response(decode_envelope(&raw).map_err(bad)?).map_err(bad)?;
    book.complete(&reply).map_err(bad)?;
    let Some(rs::Response::WorkObjectBlockView(v)) = reply.response else {
        return Err(bad("missing block response"));
    };
    let raw = v
        .work_object
        .ok_or_else(|| bad("peer missing object"))?
        .encode_to_vec();
    let record = Record::decode(&raw).map_err(bad)?;
    need(record.id() == hash, "peer returned another block hash")?;
    check_scope(&record)?;
    Ok(raw)
}
/// Membership is local and fallible. A database-backed generation avoids
/// collecting all admitted hashes before every acquisition.
pub trait KnownBlocks: Send + Sync {
    fn is_empty(&self) -> io::Result<bool>;
    fn contains(&self, hash: &Hash) -> io::Result<bool>;
}
impl KnownBlocks for BTreeSet<Hash> {
    fn is_empty(&self) -> io::Result<bool> {
        Ok(BTreeSet::is_empty(self))
    }
    fn contains(&self, hash: &Hash) -> io::Result<bool> {
        Ok(BTreeSet::contains(self, hash))
    }
}
impl KnownBlocks for AdmittedView {
    fn is_empty(&self) -> io::Result<bool> {
        Ok(AdmittedView::is_empty(self))
    }
    fn contains(&self, hash: &Hash) -> io::Result<bool> {
        AdmittedView::contains(self, hash)
    }
}
/// Fetch backward to an already admitted local ancestor, checking requested
/// hashes, scope, native candidate work/order and strict height descent. No
/// peer-advertised height, entropy, checkpoint or state image is trusted.
pub async fn acquire<K: KnownBlocks + ?Sized>(
    peer: &PrivatePeer,
    remote: libp2p::PeerId,
    head: Hash,
    known: &K,
) -> io::Result<Acquired> {
    need(!known.is_empty()?, "missing local admitted genesis")?;
    if known.contains(&head)? {
        return Ok(Acquired {
            blocks: vec![],
            requests: 0,
            bytes: 0,
            header_proofs: HeaderProofMemo::default(),
        });
    }
    let mut stream = peer.open(remote).await?;
    let result = tokio::time::timeout(SYNC_DEADLINE, async {
        let mut book = RequestBook::new(1).map_err(bad)?;
        let started = Instant::now();
        let mut hash = head;
        let mut seen = BTreeSet::new();
        let mut blocks = vec![];
        let mut bytes = 0;
        let mut id = 1;
        let mut child_height = None;
        let mut verifier = Verifier::default();
        let profile = zone_admission::profile().map_err(bad)?;
        while !known.contains(&hash)? {
            need(
                started.elapsed() < SYNC_DEADLINE,
                "sync acquisition deadline",
            )?;
            need(
                blocks.len() < MAX_SYNC_BLOCKS && seen.insert(hash),
                "sync ancestry bound/cycle",
            )?;
            let zone = request(&mut stream, &mut book, id, hash, 2, &mut bytes).await?;
            id += 1;
            let record = Record::decode(&zone).map_err(bad)?;
            let height = record.number(2).map_err(bad)?;
            need(
                height > 0 && child_height.is_none_or(|h| height.checked_add(1) == Some(h)),
                "sync unknown genesis/nonmonotonic ancestry",
            )?;
            let order = placement::verify_placement(&mut verifier, &record, &profile)
                .map_err(bad)?
                .order;
            let mut contexts = vec![];
            for c in order as usize..2 {
                let raw = request(&mut stream, &mut book, id, hash, c, &mut bytes).await?;
                id += 1;
                contexts.push((c as u8, raw));
            }
            hash = record.parent(2).map_err(bad)?;
            child_height = Some(height);
            blocks.push(BlockBundle { zone, contexts });
        }
        need(book.is_empty(), "sync leaked request")?;
        blocks.reverse();
        Ok(Acquired {
            blocks,
            requests: (id - 1) as usize,
            bytes,
            header_proofs: verifier.take_header_proof_memo(),
        })
    })
    .await
    .map_err(bad)?;
    // Semantic/correlation failures cannot leave a stream eligible for reuse.
    let _ = tokio::time::timeout(IO_TIMEOUT, stream.close()).await;
    result
}
/// Serve only locally admitted raw views. This sends no local state images or
/// admission receipts and makes no claim that the receiving peer can trust them.
pub fn serve(chain: &PrivateCanonical, raw: &[u8]) -> io::Result<Vec<u8>> {
    let env = decode_envelope(raw).map_err(bad)?;
    let Some(Payload::Request(req)) = env.payload else {
        return Err(bad("expected peer request"));
    };
    let loc = req
        .location
        .as_ref()
        .ok_or_else(|| bad("missing request location"))?;
    need(
        loc.value.len() <= 2 && loc.value == location(loc.value.len()),
        "inactive request location",
    )?;
    if matches!(req.request, Some(rq::Request::BlockHash(_))) {
        need(
            loc.value.len() == 2,
            "number/hash lookup requires the zone context",
        )?;
        let Some(rq::Data::Number(number)) = req.data else {
            return Err(bad("expected canonical block number"));
        };
        need(number.len() <= 8, "canonical block number magnitude")?;
        let height = number.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b));
        let hash = chain.view()?.hash_at(height)?;
        let reply = QuaiResponseMessage {
            id: req.id,
            location: req.location,
            response: Some(rs::Response::BlockHash(ProtoHash {
                value: hash.map_or_else(Vec::new, |h| h.to_vec()),
            })),
        };
        return Ok(QuaiMessage {
            payload: Some(Payload::Response(reply)),
        }
        .encode_to_vec());
    }
    need(
        matches!(req.request, Some(rq::Request::WorkObjectBlock(_))),
        "unsupported canonical request",
    )?;
    let id = match req.data {
        Some(rq::Data::Hash(hash)) => Some(hash.value.as_slice().try_into().map_err(bad)?),
        Some(rq::Data::Number(number)) => {
            need(
                loc.value.len() == 2 && number.len() <= 8,
                "block-number context or magnitude",
            )?;
            let height = number.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b));
            chain.view()?.hash_at(height)?
        }
        _ => return Err(bad("expected block hash or zone number")),
    };
    let Some(id): Option<Hash> = id else {
        let reply = QuaiResponseMessage {
            id: req.id,
            location: req.location,
            response: Some(rs::Response::WorkObjectBlockView(
                ProtoWorkObjectBlockView::default(),
            )),
        };
        return Ok(QuaiMessage {
            payload: Some(Payload::Response(reply)),
        }
        .encode_to_vec());
    };
    let bundle = chain.bundle(id)?;
    let bytes = if loc.value.len() == 2 {
        bundle.zone
    } else {
        bundle
            .contexts
            .iter()
            .find(|(c, _)| *c as usize == loc.value.len())
            .ok_or_else(|| bad("unavailable higher-context view"))?
            .1
            .clone()
    };
    let wo = ProtoWorkObject::decode(bytes.as_slice()).map_err(bad)?;
    let reply = QuaiResponseMessage {
        id: req.id,
        location: req.location,
        response: Some(rs::Response::WorkObjectBlockView(
            ProtoWorkObjectBlockView {
                work_object: Some(wo),
            },
        )),
    };
    Ok(QuaiMessage {
        payload: Some(Payload::Response(reply)),
    }
    .encode_to_vec())
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    static FIXTURE_SERIAL: AtomicUsize = AtomicUsize::new(0);
    fn fixture() -> (serde_json::Value, std::path::PathBuf) {
        let v = serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json"))
            .unwrap();
        let d = std::env::temp_dir().join(format!(
            "quai-cancel-{}-{}-{}",
            std::process::id(),
            FIXTURE_SERIAL.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&d).unwrap();
        (v, d)
    }
    fn block(v: &serde_json::Value) -> BlockBundle {
        BlockBundle {
            zone: quai_primitives::decode_hex(v["work_object"].as_str().unwrap()).unwrap(),
            contexts: v["contexts"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|r| {
                    let c = r["context"].as_u64().unwrap() as u8;
                    (c < 2).then(|| {
                        (
                            c,
                            quai_primitives::decode_hex(r["work_object"].as_str().unwrap())
                                .unwrap(),
                        )
                    })
                })
                .collect(),
        }
    }
    #[test]
    fn local_acquisition_proof_handoff_reduces_native_misses_without_changing_results() {
        let (v, d) = fixture();
        let now = v["now"].as_u64().unwrap();
        let rows = v["branches"][v["first_branch"].as_u64().unwrap() as usize]["vectors"]
            .as_array()
            .unwrap();
        let blocks: Vec<_> = rows.iter().map(block).collect();
        let mut verifier = Verifier::default();
        let profile = zone_admission::profile().unwrap();
        for bundle in &blocks {
            let record = Record::decode(&bundle.zone).unwrap();
            placement::verify_placement(&mut verifier, &record, &profile).unwrap();
        }
        let mut baseline = PrivateCanonical::open(&d.join("without-handoff"), now).unwrap();
        let mut candidate = PrivateCanonical::open(&d.join("with-handoff"), now).unwrap();
        let baseline_result = Acquired::new(blocks.clone(), 0, 0)
            .apply(&mut baseline, now)
            .unwrap();
        let candidate_result = Acquired {
            blocks,
            requests: 0,
            bytes: 0,
            header_proofs: verifier.take_header_proof_memo(),
        }
        .apply(&mut candidate, now)
        .unwrap();
        assert_eq!(baseline_result.head, candidate_result.head);
        assert_eq!(
            baseline.view().unwrap().roots(),
            candidate.view().unwrap().roots()
        );
        assert_eq!(baseline_result.imported_blocks, rows.len());
        assert_eq!(candidate_result.imported_blocks, rows.len());
        assert_eq!(candidate_result.reused_header_proofs, rows.len());
        assert_eq!(baseline_result.reused_header_proofs, 0);
        assert_eq!(
            baseline_result.header_proof_misses - candidate_result.header_proof_misses,
            rows.len() as u64
        );
        println!(
            "{}",
            serde_json::json!({"kind":"local-header-proof-handoff-comparison","blocks":rows.len(),"baseline_admission_misses":baseline_result.header_proof_misses,"candidate_admission_misses":candidate_result.header_proof_misses,"reused_header_proofs":candidate_result.reused_header_proofs,"head":quai_primitives::encode_hex(&candidate_result.head),"same_native_roots":true,"timing_measured":false})
        );
        drop(baseline);
        drop(candidate);
        std::fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn cancelled_acquisition_cannot_start_native_publication() {
        let (v, d) = fixture();
        let now = v["now"].as_u64().unwrap();
        let mut chain = PrivateCanonical::open(&d, now).unwrap();
        let old = chain.view().unwrap().checkpoint();
        let got = Acquired {
            blocks: vec![block(&v["branches"][0]["vectors"][0])],
            requests: 1,
            bytes: 0,
            header_proofs: HeaderProofMemo::default(),
        };
        assert_eq!(
            got.apply_cancellable(&mut chain, now, &AtomicBool::new(true))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(chain.view().unwrap().checkpoint(), old);
        drop(chain);
        assert_eq!(
            PrivateCanonical::open(&d, now)
                .unwrap()
                .view()
                .unwrap()
                .height(),
            0
        );
        std::fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn cancellation_keeps_exact_completed_prefix_and_can_resume() {
        let (v, d) = fixture();
        let now = v["now"].as_u64().unwrap();
        let rows = &v["branches"][0]["vectors"];
        let mut chain = PrivateCanonical::open(&d, now).unwrap();
        let calls = AtomicUsize::new(0);
        let got = Acquired {
            blocks: vec![block(&rows[0]), block(&rows[1])],
            requests: 2,
            bytes: 0,
            header_proofs: HeaderProofMemo::default(),
        };
        assert_eq!(
            got.apply_while(&mut chain, now, || calls.fetch_add(1, Ordering::Relaxed)
                == 0)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        let committed = chain.view().unwrap().block();
        assert_eq!(chain.view().unwrap().height(), 1);
        drop(chain);
        let mut chain = PrivateCanonical::open(&d, now).unwrap();
        assert_eq!(chain.view().unwrap().block(), committed);
        let done = Acquired {
            blocks: vec![block(&rows[0]), block(&rows[1])],
            requests: 2,
            bytes: 0,
            header_proofs: HeaderProofMemo::default(),
        }
        .apply_cancellable(&mut chain, now, &AtomicBool::new(false))
        .unwrap();
        assert_eq!(done.imported_blocks, 1);
        assert_eq!(done.height, 2);
        assert_eq!(
            quai_primitives::encode_hex(&chain.view().unwrap().roots().evm),
            rows[1]["evm_root"]
        );
        drop(chain);
        std::fs::remove_dir_all(d).unwrap();
    }
}

#[cfg(test)]
mod known_head_tests {
    use super::*;
    #[tokio::test]
    async fn admitted_head_needs_no_stream_or_enrolled_peer() {
        let peer = PrivatePeer::bind().await.unwrap();
        let unknown = libp2p::PeerId::random();
        let head = [11; 32];
        let got = acquire(&peer, unknown, head, &BTreeSet::from([head]))
            .await
            .unwrap();
        assert!(got.blocks.is_empty());
        assert_eq!(got.requests, 0);
        assert_eq!(got.bytes, 0);
        assert!(acquire(&peer, unknown, head, &BTreeSet::new())
            .await
            .is_err());
    }
}
