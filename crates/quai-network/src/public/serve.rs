//! Inbound `/quai/1.0.0` requests.
//!
//! go-quai (`p2p/protocol/handler.go`) answers from its database. This node
//! has no validated store yet, so [`NotFound`] answers every request as
//! go-quai answers for data it does not hold: an empty view or hash of the
//! requested type, with the request's ID and location. Request types that
//! go-quai does not answer (AuxPoW templates, or a block hash by hash) get no
//! answer. A peer over the rate limit has its stream closed, as in Go.
use super::peers::Book;
use crate::{decode_envelope, MAX_FRAME_BYTES};
use futures::{AsyncReadExt, AsyncWriteExt};
use libp2p::PeerId;
use prost::Message;
use quai_primitives::schema::{
    common::{ProtoHash, ProtoLocation},
    quaiprotocol::{
        quai_message::Payload, quai_request_message as rq, quai_response_message as rs,
        QuaiMessage, QuaiRequestMessage, QuaiResponseMessage,
    },
};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// A source of validated data for answers.
pub trait Store: Send + Sync {
    /// The answer to one request, or None when go-quai gives no answer.
    fn answer(&self, request: &QuaiRequestMessage) -> Option<rs::Response>;
}
/// No validated data yet: go-quai's "not found" answer of the requested type.
pub struct NotFound;
impl Store for NotFound {
    fn answer(&self, request: &QuaiRequestMessage) -> Option<rs::Response> {
        match (request.request.as_ref()?, request.data.as_ref()?) {
            (rq::Request::WorkObjectBlock(_), _) => {
                Some(rs::Response::WorkObjectBlockView(Default::default()))
            }
            (rq::Request::WorkObjectHeader(_), _) => {
                Some(rs::Response::WorkObjectHeaderView(Default::default()))
            }
            (rq::Request::WorkObjectBlocks(_), _) => {
                Some(rs::Response::WorkObjectBlocksView(Default::default()))
            }
            // handleBlockNumberRequest: a block hash is asked by number only.
            (rq::Request::BlockHash(_), rq::Data::Number(_)) => {
                Some(rs::Response::BlockHash(ProtoHash::default()))
            }
            (rq::Request::TrieNode(_), rq::Data::Hash(_)) => {
                Some(rs::Response::TrieNode(Default::default()))
            }
            (rq::Request::TrieNodes(_), rq::Data::Hashes(_)) => {
                Some(rs::Response::TrieNodes(Default::default()))
            }
            _ => None,
        }
    }
}
/// The answer frame for one request frame, or None (no answer).
pub fn answer(store: &dyn Store, frame: &[u8]) -> Option<Vec<u8>> {
    let message = decode_envelope(frame).ok()?;
    let Some(Payload::Request(request)) = message.payload else {
        return None;
    };
    let response = store.answer(&request)?;
    let location = Some(ProtoLocation {
        value: request.location.map(|l| l.value).unwrap_or_default(),
    });
    Some(
        QuaiMessage {
            payload: Some(Payload::Response(QuaiResponseMessage {
                id: request.id,
                location,
                response: Some(response),
            })),
        }
        .encode_to_vec(),
    )
}

/// Counters of the inbound side.
#[derive(Default)]
pub struct Served {
    pub requests: AtomicU64,
    pub answered: AtomicU64,
    pub rate_limited: AtomicU64,
}

/// Time a peer may keep an inbound stream open without a request. go-quai
/// keeps its outbound stream to a peer cached and reuses it.
pub const IDLE: Duration = Duration::from_secs(300);
/// Time to finish reading one frame once its length arrived, and to write.
pub const FRAME: Duration = Duration::from_secs(10);

/// Serve one inbound stream until the peer closes it, goes idle, sends a
/// bad frame or exceeds the rate limit.
pub async fn serve_stream(
    mut stream: libp2p::Stream,
    peer: PeerId,
    store: Arc<dyn Store>,
    book: Arc<Mutex<Book>>,
    served: Arc<Served>,
) {
    loop {
        let mut prefix = [0u8; 4];
        match tokio::time::timeout(IDLE, stream.read_exact(&mut prefix)).await {
            Ok(Ok(())) => {}
            _ => break,
        }
        let n = u32::from_be_bytes(prefix) as usize;
        if n > MAX_FRAME_BYTES {
            break;
        }
        let mut frame = Vec::with_capacity(n.min(64 * 1024));
        let read =
            tokio::time::timeout(FRAME, (&mut stream).take(n as u64).read_to_end(&mut frame)).await;
        if !matches!(read, Ok(Ok(m)) if m == n) {
            break;
        }
        served.requests.fetch_add(1, Ordering::Relaxed);
        let allowed = book
            .lock()
            .map(|mut b| b.inbound_request(peer, Instant::now()))
            .unwrap_or(false);
        if !allowed {
            // handleRequest: "closing stream to over-chatty peer".
            served.rate_limited.fetch_add(1, Ordering::Relaxed);
            break;
        }
        // Run store lookups/redb I/O on Tokio's blocking pool so inbound
        // requests never stall the async P2P/RPC reactor threads.
        let store_for_task = store.clone();
        let reply = match tokio::task::spawn_blocking(move || {
            answer(store_for_task.as_ref(), &frame)
        })
        .await
        {
            Ok(Some(reply)) => reply,
            Ok(None) => continue,
            Err(_) => break,
        };
        if reply.len() > MAX_FRAME_BYTES {
            continue;
        }
        let Ok(len) = u32::try_from(reply.len()) else {
            continue;
        };
        let mut out = len.to_be_bytes().to_vec();
        out.extend_from_slice(&reply);
        let wrote = tokio::time::timeout(FRAME, async {
            stream.write_all(&out).await?;
            stream.flush().await
        })
        .await;
        if !matches!(wrote, Ok(Ok(()))) {
            break;
        }
        served.answered.fetch_add(1, Ordering::Relaxed);
    }
    let _ = stream.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{response, RequestBook};

    fn request(
        id: u32,
        location: Vec<u8>,
        data: rq::Data,
        kind: rq::Request,
    ) -> QuaiRequestMessage {
        QuaiRequestMessage {
            id,
            location: Some(ProtoLocation { value: location }),
            data: Some(data),
            request: Some(kind),
        }
    }
    fn frame(r: &QuaiRequestMessage) -> Vec<u8> {
        QuaiMessage {
            payload: Some(Payload::Request(r.clone())),
        }
        .encode_to_vec()
    }

    #[test]
    fn not_found_answers_match_go_and_correlate() {
        let by_number = rq::Data::Number(vec![1]);
        let by_hash = rq::Data::Hash(ProtoHash { value: vec![7; 32] });
        let cases = [
            (
                rq::Request::WorkObjectBlock(Default::default()),
                by_hash.clone(),
                true,
            ),
            (
                rq::Request::WorkObjectHeader(Default::default()),
                by_number.clone(),
                true,
            ),
            (
                rq::Request::WorkObjectBlocks(Default::default()),
                by_hash.clone(),
                true,
            ),
            (
                rq::Request::BlockHash(Default::default()),
                by_number.clone(),
                true,
            ),
            // go-quai answers neither of these.
            (
                rq::Request::BlockHash(Default::default()),
                by_hash.clone(),
                false,
            ),
            (
                rq::Request::AuxTemplate(Default::default()),
                by_number.clone(),
                false,
            ),
        ];
        for (i, (kind, data, answered)) in cases.into_iter().enumerate() {
            for location in [vec![], vec![0], vec![0, 0]] {
                let r = request(i as u32 + 10, location.clone(), data.clone(), kind.clone());
                let reply = answer(&NotFound, &frame(&r));
                assert_eq!(reply.is_some(), answered, "{i}");
                let Some(reply) = reply else { continue };
                let mut book = RequestBook::new(1).unwrap();
                book.register(&r).unwrap();
                let reply = response(decode_envelope(&reply).unwrap()).unwrap();
                // Same ID, same location, same type: the asker's book accepts it.
                book.complete(&reply).unwrap();
                let empty = match reply.response.unwrap() {
                    rs::Response::WorkObjectBlockView(v) => v.work_object.is_none(),
                    rs::Response::WorkObjectHeaderView(v) => v.work_object.is_none(),
                    rs::Response::WorkObjectBlocksView(v) => v.work_objects.is_empty(),
                    rs::Response::BlockHash(h) => h.value.is_empty(),
                    rs::Response::AuxTemplate(_) => false,
                    rs::Response::TrieNode(n) => n.node_data.is_empty(),
                    rs::Response::TrieNodes(ns) => ns.nodes.is_empty(),
                };
                assert!(empty);
            }
        }
        // A response or garbage gets no answer.
        assert!(answer(&NotFound, &[1, 2, 3]).is_none());
    }

    #[test]
    fn trie_node_not_found_and_classify_keccak_verification() {
        use super::super::{classify, Outcome, Query, Want};
        use quai_primitives::{
            schema::quaiprotocol::{ProtoHashList, ProtoTrieNode, ProtoTrieNodes},
            transaction::keccak,
        };

        let loc = vec![0u8, 0u8];
        let blob1 = b"evm-trie-node-alpha".to_vec();
        let blob2 = b"etx-trie-node-beta".to_vec();
        let h1 = keccak(&blob1);
        let h2 = keccak(&blob2);

        // NotFound answers empty TrieNode and TrieNodes
        let req_single = request(
            77,
            loc.clone(),
            rq::Data::Hash(ProtoHash { value: h1.to_vec() }),
            rq::Request::TrieNode(Default::default()),
        );
        let empty_single = answer(&NotFound, &frame(&req_single)).unwrap();
        let (outcome, val) = classify(Ok(empty_single), 77, &loc, &Query::Hash(h1), Want::TrieNode);
        assert_eq!(outcome, Outcome::Empty);
        assert!(val.is_none());

        let req_batch = request(
            78,
            loc.clone(),
            rq::Data::Hashes(ProtoHashList {
                hashes: vec![
                    ProtoHash { value: h1.to_vec() },
                    ProtoHash { value: h2.to_vec() },
                ],
            }),
            rq::Request::TrieNodes(Default::default()),
        );
        let empty_batch = answer(&NotFound, &frame(&req_batch)).unwrap();
        let (outcome, val) = classify(
            Ok(empty_batch),
            78,
            &loc,
            &Query::Hashes(vec![h1, h2]),
            Want::TrieNodes,
        );
        assert_eq!(outcome, Outcome::Empty);
        assert!(val.is_none());

        // Valid single TrieNode response passes Keccak256 check
        let ok_frame = QuaiMessage {
            payload: Some(Payload::Response(QuaiResponseMessage {
                id: 79,
                location: Some(ProtoLocation { value: loc.clone() }),
                response: Some(rs::Response::TrieNode(ProtoTrieNode {
                    node_data: blob1.clone(),
                })),
            })),
        }
        .encode_to_vec();
        let (outcome, val) = classify(Ok(ok_frame), 79, &loc, &Query::Hash(h1), Want::TrieNode);
        assert_eq!(outcome, Outcome::Valid);
        assert!(val.is_some());

        // Corrupted single TrieNode triggers Ban
        let bad_frame = QuaiMessage {
            payload: Some(Payload::Response(QuaiResponseMessage {
                id: 80,
                location: Some(ProtoLocation { value: loc.clone() }),
                response: Some(rs::Response::TrieNode(ProtoTrieNode {
                    node_data: b"corrupted".to_vec(),
                })),
            })),
        }
        .encode_to_vec();
        let (outcome, val) = classify(Ok(bad_frame), 80, &loc, &Query::Hash(h1), Want::TrieNode);
        assert!(matches!(outcome, Outcome::Ban(_)));
        assert!(val.is_none());

        // Valid partial batch passes, duplicate or unsolicited triggers Ban
        let ok_batch = QuaiMessage {
            payload: Some(Payload::Response(QuaiResponseMessage {
                id: 81,
                location: Some(ProtoLocation { value: loc.clone() }),
                response: Some(rs::Response::TrieNodes(ProtoTrieNodes {
                    nodes: vec![blob2.clone()],
                })),
            })),
        }
        .encode_to_vec();
        let (outcome, val) = classify(
            Ok(ok_batch),
            81,
            &loc,
            &Query::Hashes(vec![h1, h2]),
            Want::TrieNodes,
        );
        assert_eq!(outcome, Outcome::Valid);
        assert!(val.is_some());

        let dup_batch = QuaiMessage {
            payload: Some(Payload::Response(QuaiResponseMessage {
                id: 82,
                location: Some(ProtoLocation { value: loc.clone() }),
                response: Some(rs::Response::TrieNodes(ProtoTrieNodes {
                    nodes: vec![blob1.clone(), blob1],
                })),
            })),
        }
        .encode_to_vec();
        let (outcome, _) = classify(
            Ok(dup_batch),
            82,
            &loc,
            &Query::Hashes(vec![h1, h2]),
            Want::TrieNodes,
        );
        assert!(matches!(outcome, Outcome::Ban(_)));
    }
}
