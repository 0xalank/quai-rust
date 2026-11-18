//! Actual private libp2p delivery + early execution replay + durable restart.
//! This example does not establish proof/hierarchy admission or full native sync.
use prost::Message;
use quai_execution::{checkpoint, early_replay::EarlyReplay};
use quai_network::{
    decode_envelope, response,
    transport::{Event, PrivatePeer},
    RequestBook,
};
use quai_primitives::{
    decode_hex,
    schema::{
        block::{ProtoWorkObject, ProtoWorkObjectBlockView},
        common::{ProtoHash, ProtoLocation},
        quaiprotocol::{
            quai_message::Payload, quai_request_message as rq, quai_response_message as rs,
            QuaiMessage, QuaiRequestMessage, QuaiResponseMessage,
        },
    },
    work::{BodyHeader, WorkObject},
    Schedule,
};
use quai_store::journal::{Checkpoint, ScratchJournal};
use std::{io, time::Duration};
fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let fixture = args.next().ok_or("early block corpus")?;
    let directory = args
        .next()
        .ok_or("dedicated private checkpoint directory")?;
    let data: serde_json::Value = serde_json::from_slice(&std::fs::read(fixture)?)?;
    if data["reference_revision"] != quai_primitives::REFERENCE_REVISION {
        return Err("wrong corpus revision".into());
    }
    let profile = quai_consensus::private_profile::PrivateChain::reviewed()?;
    let genesis = profile.genesis[2].id();
    let schedule = Schedule {
        kawpow_fork: 1_171_500,
        transition_period: 129600,
    };
    let genesis_object = WorkObject::decode(
        &decode_hex(data["genesis"].as_str().ok_or("genesis")?)?,
        schedule,
    )?;
    if genesis_object.work_header(schedule)?.block_hash()? != genesis {
        return Err("wrong genesis".into());
    }
    let blocks = data["vectors"]
        .as_array()
        .ok_or("blocks")?
        .iter()
        .map(|r| decode_hex(r["work_object"].as_str().unwrap()))
        .collect::<Result<Vec<_>, _>>()?;
    if blocks.len() != 3 {
        return Err("private replay requires three early blocks".into());
    }
    let mut store = ScratchJournal::open(std::path::Path::new(&directory), profile.fingerprint)?;
    if store.head().is_some() {
        return Err("private replay example requires a fresh store".into());
    }
    let mut state = EarlyReplay::empty(
        genesis,
        genesis_object.work_header(schedule)?.wire().time.unwrap(),
    );
    let g = store.stage(&Checkpoint {
        block: genesis,
        parent: None,
        height: 0,
        payload: checkpoint::encode(&state)?,
    })?;
    store.publish(None, g)?;
    drop(store);
    let mut server = PrivatePeer::bind().await?;
    let client = PrivatePeer::bind().await?;
    server.enroll(client.id)?;
    client.enroll(server.id)?;
    let peer = server.id;
    let address = server.address.clone();
    let client_id = client.id;
    let (done, finished) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        loop {
            if let Event::Stream { peer, mut stream } = server.next().await? {
                if peer != client_id {
                    return Err(bad("unenrolled peer"));
                }
                for i in 0..4 {
                    let envelope = decode_envelope(&stream.read().await?).map_err(bad)?;
                    let Some(Payload::Request(req)) = envelope.payload else {
                        return Err(bad("expected request"));
                    };
                    let n = if i == 0 { 0 } else { i - 1 };
                    if req.data != Some(rq::Data::Number(vec![n as u8 + 1]))
                        || !matches!(req.request, Some(rq::Request::WorkObjectBlock(_)))
                    {
                        return Err(bad("wrong block request"));
                    }
                    let mut block = ProtoWorkObject::decode(blocks[n].as_slice()).map_err(bad)?;
                    if i == 0 {
                        let h = block.wo_body.as_mut().unwrap().header.as_mut().unwrap();
                        h.gas_used = Some(h.gas_used.unwrap() + 1);
                        let root = BodyHeader::decode(&h.encode_to_vec()).map_err(bad)?.hash();
                        block.wo_header.as_mut().unwrap().header_hash = Some(ProtoHash {
                            value: root.to_vec(),
                        });
                    }
                    let reply = QuaiMessage {
                        payload: Some(Payload::Response(QuaiResponseMessage {
                            id: req.id,
                            location: req.location,
                            response: Some(rs::Response::WorkObjectBlockView(
                                ProtoWorkObjectBlockView {
                                    work_object: Some(block),
                                },
                            )),
                        })),
                    };
                    stream.write(&reply.encode_to_vec()).await?;
                }
                let _ = finished.await;
                return Ok::<_, io::Error>(());
            }
        }
    });
    client.dial(peer, address).await?;
    let mut stream = client.open(peer).await?;
    let mut book = RequestBook::new(1).map_err(bad)?;
    let mut rejected = 0;
    let mut restarts = 0;
    for i in 0..4 {
        let n = if i == 0 { 1 } else { i };
        let req = QuaiRequestMessage {
            id: 2000 + i,
            location: Some(ProtoLocation { value: vec![0, 0] }),
            data: Some(rq::Data::Number(vec![n as u8])),
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
        let reply = response(decode_envelope(&stream.read().await?).map_err(bad)?).map_err(bad)?;
        book.complete(&reply).map_err(bad)?;
        let Some(rs::Response::WorkObjectBlockView(view)) = reply.response else {
            return Err("wrong reply kind".into());
        };
        let object = WorkObject::decode(
            &view.work_object.ok_or("empty block")?.encode_to_vec(),
            schedule,
        )?;
        let before = checkpoint::encode(&state)?;
        if i == 0 {
            if state.apply(&object).is_ok() || checkpoint::encode(&state)? != before {
                return Err("invalid execution block was accepted or mutated state".into());
            }
            rejected += 1;
            continue;
        }
        state.apply(&object)?;
        let mut store =
            ScratchJournal::open(std::path::Path::new(&directory), profile.fingerprint)?;
        let prior = store.head();
        let next = store.stage(&Checkpoint {
            block: state.head(),
            parent: prior,
            height: state.height(),
            payload: checkpoint::encode(&state)?,
        })?;
        store.publish(prior, next)?;
        drop(store);
        let store = ScratchJournal::open(std::path::Path::new(&directory), profile.fingerprint)?;
        let committed = store.load(store.head().ok_or("lost head")?)?;
        let restored = checkpoint::decode(&committed.payload, genesis)?;
        if restored.head() != state.head()
            || restored.height() != state.height()
            || checkpoint::encode(&restored)? != checkpoint::encode(&state)?
        {
            return Err("restart state differs".into());
        }
        state = restored;
        restarts += 1;
    }
    stream.close().await?;
    let _ = done.send(());
    tokio::time::timeout(Duration::from_secs(15), task).await???;
    if !book.is_empty() {
        return Err("request leaked".into());
    }
    println!(
        "{}",
        serde_json::json!({"passed":true,"early_blocks_replayed":state.height(),"invalid_execution_blocks_rejected":rejected,"durable_restarts":restarts,"noise":true,"yamux":true,"persistent_stream":true,"profile_fingerprint":quai_primitives::encode_hex(&profile.fingerprint),"header_consensus_admission":false,"full_native_sync":false,"public_network":false})
    );
    Ok(())
}
