//! Forward acquisition in bounded windows. Number/hash replies locate raw
//! objects; only native canonical import can authorize their state or ancestry.
use super::*;
use quai_consensus::private_canonical::CanonicalView;
pub const MAX_WINDOW_BLOCKS: usize = 128;
const MAX_NEXT_BLOCK_BYTES: usize = 3 * crate::MAX_FRAME_BYTES + 1024;
#[derive(Clone, Debug)]
pub struct Plan {
    last_height: u64,
    last: Hash,
    target_height: u64,
    target: Hash,
    pub discovery_requests: usize,
    pub discovery_bytes: usize,
}
pub struct Window {
    pub acquired: Acquired,
    pub next: Plan,
}
impl Plan {
    pub fn done(&self) -> bool {
        self.last_height == self.target_height
    }
    pub fn height(&self) -> u64 {
        self.last_height
    }
}
fn number(n: u64) -> Vec<u8> {
    let b = n.to_be_bytes();
    b[b.iter().position(|v| *v != 0).unwrap_or(8)..].to_vec()
}
fn number_request(id: u32, height: u64) -> QuaiRequestMessage {
    QuaiRequestMessage {
        id,
        location: Some(ProtoLocation { value: location(2) }),
        data: Some(rq::Data::Number(number(height))),
        request: Some(rq::Request::BlockHash(ProtoHash::default())),
    }
}
fn block_request(id: u32, height: u64) -> QuaiRequestMessage {
    QuaiRequestMessage {
        id,
        location: Some(ProtoLocation { value: location(2) }),
        data: Some(rq::Data::Number(number(height))),
        request: Some(rq::Request::WorkObjectBlock(
            ProtoWorkObjectBlockView::default(),
        )),
    }
}
async fn block_at(
    stream: &mut WireStream,
    book: &mut RequestBook,
    id: u32,
    height: u64,
    bytes: &mut usize,
) -> io::Result<Vec<u8>> {
    let req = block_request(id, height);
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
        .ok_or_else(|| bad("forward byte overflow"))?;
    need(*bytes <= MAX_SYNC_BYTES, "forward response byte budget")?;
    let reply = response(decode_envelope(&raw).map_err(bad)?).map_err(bad)?;
    book.complete(&reply).map_err(bad)?;
    let Some(rs::Response::WorkObjectBlockView(view)) = reply.response else {
        return Err(bad("missing number/block response"));
    };
    let object = view.work_object.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "peer has no canonical block at requested height",
        )
    })?;
    let raw = object.encode_to_vec();
    let record = Record::decode(&raw).map_err(bad)?;
    need(
        record.number(2).map_err(bad)? == height,
        "peer returned another block number",
    )?;
    check_scope(&record)?;
    Ok(raw)
}
async fn hash_at(
    stream: &mut WireStream,
    book: &mut RequestBook,
    id: u32,
    height: u64,
    bytes: &mut usize,
) -> io::Result<Hash> {
    let req = number_request(id, height);
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
        .ok_or_else(|| bad("forward byte overflow"))?;
    need(*bytes <= MAX_SYNC_BYTES, "forward discovery byte budget")?;
    let reply = response(decode_envelope(&raw).map_err(bad)?).map_err(bad)?;
    book.complete(&reply).map_err(bad)?;
    let Some(rs::Response::BlockHash(hash)) = reply.response else {
        return Err(bad("missing number/hash response"));
    };
    if hash.value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "peer has no canonical hash at requested height",
        ));
    }
    hash.value
        .try_into()
        .map_err(|_| bad("number/hash response length"))
}
/// Locate a shared native local ancestor. The peer's number/hash answers are
/// discovery hints. Every downloaded block still needs exact hash, linked
/// parent/height, native work and complete canonical admission checks.
pub async fn begin(
    peer: &PrivatePeer,
    remote: libp2p::PeerId,
    local: &CanonicalView,
    target: Hash,
    target_height: u64,
) -> io::Result<Plan> {
    if local.hash_at(target_height)? == Some(target) {
        return Ok(Plan {
            last_height: target_height,
            last: target,
            target_height,
            target,
            discovery_requests: 0,
            discovery_bytes: 0,
        });
    }
    let mut stream = peer.open(remote).await?;
    let result = tokio::time::timeout(SYNC_DEADLINE, async {
        let mut book = RequestBook::new(1).map_err(bad)?;
        let mut bytes = 0;
        let mut id = 1;
        let genesis = hash_at(&mut stream, &mut book, id, 0, &mut bytes).await?;
        id += 1;
        need(
            local.hash_at(0)? == Some(genesis),
            "forward peer genesis mismatch",
        )?;
        let tip = hash_at(&mut stream, &mut book, id, target_height, &mut bytes).await?;
        id += 1;
        need(
            tip == target,
            "forward target is not the peer canonical hash",
        )?;
        let mut low = 0;
        let mut high = local.height().min(target_height);
        while low < high {
            let d = high - low;
            let mid = low + d / 2 + d % 2;
            let remote_hash = hash_at(&mut stream, &mut book, id, mid, &mut bytes).await?;
            id += 1;
            if local.hash_at(mid)? == Some(remote_hash) {
                low = mid
            } else {
                high = mid - 1
            }
        }
        let last = local
            .hash_at(low)?
            .ok_or_else(|| bad("forward local ancestor unavailable"))?;
        need(book.is_empty(), "forward discovery leaked request")?;
        Ok(Plan {
            last_height: low,
            last,
            target_height,
            target,
            discovery_requests: (id - 1) as usize,
            discovery_bytes: bytes,
        })
    })
    .await
    .map_err(bad)?;
    let _ = tokio::time::timeout(IO_TIMEOUT, stream.close()).await;
    result
}
/// Retain at most 128 raw blocks and 32 MiB of responses. The next cursor must
/// be used only after this window completes native per-block publication.
/// A changed/malicious number index cannot substitute a different parent or
/// target; failure preserves any previously admitted valid prefix.
pub async fn window(
    peer: &PrivatePeer,
    remote: libp2p::PeerId,
    plan: &Plan,
    limit: usize,
) -> io::Result<Window> {
    window_with_verifier(peer, remote, plan, limit, &mut Verifier::default()).await
}
/// Keep the locally generated epoch cache across windows. The exported memo
/// contains only exact strong header proofs, never a peer-supplied permit.
pub async fn window_with_verifier(
    peer: &PrivatePeer,
    remote: libp2p::PeerId,
    plan: &Plan,
    limit: usize,
    verifier: &mut Verifier,
) -> io::Result<Window> {
    need(
        (1..=MAX_WINDOW_BLOCKS).contains(&limit),
        "forward window block bound",
    )?;
    need(!plan.done(), "forward plan already complete")?;
    let mut stream = peer.open(remote).await?;
    let result = tokio::time::timeout(SYNC_DEADLINE, async {
        let mut next = plan.clone();
        let mut blocks = vec![];
        let mut book = RequestBook::new(1).map_err(bad)?;
        let mut bytes = 0;
        let mut id = 1;
        let profile = zone_admission::profile().map_err(bad)?;
        while !next.done() && blocks.len() < limit {
            if !blocks.is_empty() && bytes > MAX_SYNC_BYTES - MAX_NEXT_BLOCK_BYTES {
                break;
            }
            let height = next
                .last_height
                .checked_add(1)
                .ok_or_else(|| bad("forward height overflow"))?;
            let zone = block_at(&mut stream, &mut book, id, height, &mut bytes).await?;
            id += 1;
            let record = Record::decode(&zone).map_err(bad)?;
            let hash = record.id();
            need(
                record.number(2).map_err(bad)? == height
                    && record.parent(2).map_err(bad)? == next.last,
                "forward peer changed branch or returned nonmonotonic ancestry",
            )?;
            let order = placement::verify_placement(verifier, &record, &profile)
                .map_err(bad)?
                .order;
            let mut contexts = vec![];
            for c in order as usize..2 {
                let raw = request(&mut stream, &mut book, id, hash, c, &mut bytes).await?;
                id += 1;
                contexts.push((c as u8, raw));
            }
            blocks.push(BlockBundle { zone, contexts });
            next.last_height = height;
            next.last = hash;
        }
        need(
            !blocks.is_empty() && book.is_empty(),
            "empty forward window or leaked request",
        )?;
        if next.done() {
            need(next.last == next.target, "forward final target mismatch")?
        }
        Ok(Window {
            acquired: Acquired {
                blocks,
                requests: (id - 1) as usize,
                bytes,
                header_proofs: verifier.take_header_proof_memo(),
            },
            next,
        })
    })
    .await
    .map_err(bad)?;
    let _ = tokio::time::timeout(IO_TIMEOUT, stream.close()).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use quai_primitives::{decode_hex, encode_hex};
    use serde_json::Value;
    fn corpus() -> Value {
        serde_json::from_str(include_str!(
            "../../../../fixtures/protocol/forward-wire.json"
        ))
        .unwrap()
    }
    #[test]
    fn canonical_number_hash_and_typed_absence_bytes_match_actual_go() {
        let v = corpus();
        assert_eq!(v["reference_revision"], quai_primitives::REFERENCE_REVISION);
        for row in v["vectors"].as_array().unwrap() {
            let id = row["id"].as_u64().unwrap() as u32;
            let req = number_request(id, row["height"].as_u64().unwrap());
            assert_eq!(
                encode_hex(
                    &QuaiMessage {
                        payload: Some(Payload::Request(req.clone()))
                    }
                    .encode_to_vec()
                ),
                row["request"]
            );
            assert_eq!(
                encode_hex(
                    &QuaiMessage {
                        payload: Some(Payload::Request(block_request(
                            id,
                            row["height"].as_u64().unwrap()
                        )))
                    }
                    .encode_to_vec()
                ),
                row["block_request"]
            );
            for absent in [false, true] {
                let hash = if absent {
                    vec![]
                } else {
                    decode_hex(row["hash"].as_str().unwrap()).unwrap()
                };
                let reply = QuaiResponseMessage {
                    id,
                    location: req.location.clone(),
                    response: Some(rs::Response::BlockHash(ProtoHash { value: hash })),
                };
                let raw = QuaiMessage {
                    payload: Some(Payload::Response(reply.clone())),
                }
                .encode_to_vec();
                assert_eq!(
                    encode_hex(&raw),
                    row[if absent {
                        "absent_response"
                    } else {
                        "present_response"
                    }]
                );
                let mut book = RequestBook::new(1).unwrap();
                book.register(&req).unwrap();
                book.complete(&reply).unwrap();
                assert!(book.is_empty());
            }
        }
    }
    #[test]
    fn canonical_number_service_uses_native_receipts_and_never_aliases_context_or_height() {
        let dir = std::env::temp_dir().join(format!(
            "quai-forward-service-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let v: Value =
            serde_json::from_str(include_str!("../../../../fixtures/protocol/canonical.json"))
                .unwrap();
        let now = v["now"].as_u64().unwrap();
        let mut chain = PrivateCanonical::open_execution_v2(&dir, now).unwrap();
        let row = &v["branches"][0]["vectors"][0];
        chain
            .import(
                BlockBundle {
                    zone: decode_hex(row["work_object"].as_str().unwrap()).unwrap(),
                    contexts: vec![],
                },
                now,
            )
            .unwrap();
        let checkpoint = chain.durable_checkpoint().unwrap();
        for row in corpus()["vectors"].as_array().unwrap() {
            let raw = decode_hex(row["request"].as_str().unwrap()).unwrap();
            let response = super::super::serve(&chain, &raw).unwrap();
            let expected = if row["height"].as_u64().unwrap() > 1 {
                "absent_response"
            } else {
                "present_response"
            };
            assert_eq!(encode_hex(&response), row[expected]);
            let raw = decode_hex(row["block_request"].as_str().unwrap()).unwrap();
            let response = super::super::serve(&chain, &raw).unwrap();
            if row["height"].as_u64().unwrap() > 1 {
                assert_eq!(encode_hex(&response), row["block_absent_response"]);
            } else {
                let reply = crate::response(crate::decode_envelope(&response).unwrap()).unwrap();
                let Some(rs::Response::WorkObjectBlockView(body)) = reply.response else {
                    panic!("wrong reply kind")
                };
                let record = Record::decode(&body.work_object.unwrap().encode_to_vec()).unwrap();
                assert_eq!(record.number(2).unwrap(), row["height"].as_u64().unwrap());
                assert_eq!(encode_hex(&record.id()), row["hash"]);
            }
        }
        let mut req = number_request(31, 1);
        req.location = Some(ProtoLocation { value: vec![] });
        assert!(super::super::serve(
            &chain,
            &QuaiMessage {
                payload: Some(Payload::Request(req))
            }
            .encode_to_vec()
        )
        .is_err());
        let mut req = number_request(32, 1);
        req.data = Some(rq::Data::Number(vec![1; 9]));
        assert!(super::super::serve(
            &chain,
            &QuaiMessage {
                payload: Some(Payload::Request(req))
            }
            .encode_to_vec()
        )
        .is_err());
        assert_eq!(checkpoint, chain.durable_checkpoint().unwrap());
        drop(chain);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn forward_valid_work_go_rejected_block_keeps_native_prefix_and_can_resume() {
        use crate::transport::Event;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let corpus: Value =
            serde_json::from_str(include_str!("../../../../fixtures/protocol/canonical.json"))
                .unwrap();
        let now = corpus["now"].as_u64().unwrap();
        let branch = corpus["first_branch"].as_u64().unwrap() as usize;
        let root = std::env::temp_dir().join(format!(
            "quai-forward-reject-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fn bundle(row: &Value) -> BlockBundle {
            BlockBundle {
                zone: decode_hex(row["work_object"].as_str().unwrap()).unwrap(),
                contexts: row["contexts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|c| {
                        let role = c["context"].as_u64().unwrap() as u8;
                        (role < 2).then(|| {
                            (
                                role,
                                decode_hex(c["work_object"].as_str().unwrap()).unwrap(),
                            )
                        })
                    })
                    .collect(),
            }
        }
        let mut source = PrivateCanonical::open_execution_v2(&root.join("source"), now).unwrap();
        for row in corpus["branches"][branch]["vectors"].as_array().unwrap() {
            source.import(bundle(row), now).unwrap();
        }
        let target = source.view().unwrap().block();
        let bad = corpus["negatives"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "duplicate" && r["at_height"] == 7)
            .unwrap();
        let bad = bundle(bad);
        let bad_id = bad.checked().unwrap().id();
        let bad_zone = bad.zone.clone();
        let peer = PrivatePeer::bind().await.unwrap();
        let remote = PrivatePeer::bind().await.unwrap();
        let remote_id = remote.id;
        let address = remote.address.clone();
        peer.enroll(remote_id).unwrap();
        remote.enroll(peer.id).unwrap();
        let attack = Arc::new(AtomicBool::new(true));
        let active = attack.clone();
        let server = tokio::spawn(async move {
            loop {
                let event = remote.next().await.unwrap();
                let Event::Stream { mut stream, .. } = event else {
                    continue;
                };
                while let Ok(raw) = stream.read().await {
                    let env = decode_envelope(&raw).unwrap();
                    let Some(Payload::Request(req)) = env.payload else {
                        panic!("request expected")
                    };
                    let height = if let Some(rq::Data::Number(n)) = &req.data {
                        Some(n.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b)))
                    } else {
                        None
                    };
                    let override_body = active.load(Ordering::Acquire)
                        && matches!(req.request, Some(rq::Request::WorkObjectBlock(_)))
                        && height == Some(7);
                    let bytes = if override_body {
                        let wo = ProtoWorkObject::decode(bad_zone.as_slice()).unwrap();
                        QuaiMessage {
                            payload: Some(Payload::Response(QuaiResponseMessage {
                                id: req.id,
                                location: req.location,
                                response: Some(rs::Response::WorkObjectBlockView(
                                    ProtoWorkObjectBlockView {
                                        work_object: Some(wo),
                                    },
                                )),
                            })),
                        }
                        .encode_to_vec()
                    } else {
                        super::super::serve(&source, &raw).unwrap()
                    };
                    if stream.write(&bytes).await.is_err() {
                        break;
                    }
                }
            }
        });
        peer.dial(remote_id, address).await.unwrap();
        let mut receiver =
            PrivateCanonical::open_execution_v2(&root.join("receiver"), now).unwrap();
        let plan = begin(&peer, remote_id, &receiver.view().unwrap(), target, 16)
            .await
            .unwrap();
        let first = window(&peer, remote_id, &plan, 7).await.unwrap();
        assert_eq!(first.acquired.blocks.len(), 7);
        let before = receiver.header_proof_memo_stats();
        assert!(first.acquired.apply(&mut receiver, now).is_err());
        assert!(receiver.header_proof_memo_stats().hits > before.hits);
        assert_eq!(receiver.view().unwrap().height(), 6);
        assert_eq!(receiver.admitted_count(), 7);
        assert!(!receiver.contains(&bad_id).unwrap());
        assert_eq!(
            encode_hex(&receiver.view().unwrap().roots().evm),
            corpus["branches"][branch]["vectors"][5]["evm_root"]
        );
        drop(receiver);
        let mut receiver =
            PrivateCanonical::open_execution_v2(&root.join("receiver"), now).unwrap();
        assert_eq!(receiver.view().unwrap().height(), 6);
        attack.store(false, Ordering::Release);
        let mut plan = begin(&peer, remote_id, &receiver.view().unwrap(), target, 16)
            .await
            .unwrap();
        assert_eq!(plan.height(), 6);
        let mut verifier = Verifier::default();
        while !plan.done() {
            let next = window_with_verifier(&peer, remote_id, &plan, 7, &mut verifier)
                .await
                .unwrap();
            let applied = next.acquired.apply(&mut receiver, now).unwrap();
            assert!(applied.reused_header_proofs > 0);
            assert!(applied.header_proof_hits >= applied.imported_blocks as u64);
            plan = next.next;
        }
        assert_eq!(receiver.view().unwrap().block(), target);
        assert_eq!(receiver.view().unwrap().height(), 16);
        drop(receiver);
        server.abort();
        let _ = server.await;
        std::fs::remove_dir_all(root).unwrap();
    }
}
