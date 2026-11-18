//! Isolated native canonical acquisition over real private Noise/Yamux peers.
//! The runtime validates raw peer blocks locally; Go is never invoked here.
use prost::Message;
use quai_consensus::{
    history::Record,
    private_canonical::{BlockBundle, PrivateCanonical},
};
use quai_network::{
    decode_envelope, private_sync,
    transport::{Event, PrivatePeer},
};
use quai_primitives::{
    decode_hex, encode_hex,
    schema::{
        block::{ProtoWorkObject, ProtoWorkObjectBlockView},
        quaiprotocol::{
            quai_message::Payload, quai_request_message as rq, quai_response_message as rs,
            QuaiMessage, QuaiResponseMessage,
        },
    },
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
fn bundle(v: &Value) -> BlockBundle {
    BlockBundle {
        zone: decode_hex(v["work_object"].as_str().unwrap()).unwrap(),
        contexts: v["contexts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| {
                let c = r["context"].as_u64().unwrap() as u8;
                (c < 2).then(|| (c, decode_hex(r["work_object"].as_str().unwrap()).unwrap()))
            })
            .collect(),
    }
}
#[derive(Clone, Copy)]
enum Mode {
    Normal,
    WrongHash,
    WrongId,
    Disconnect,
}
fn start(
    mut peer: PrivatePeer,
    chain: Arc<Mutex<PrivateCanonical>>,
    mode: Mode,
    extra: BTreeMap<[u8; 32], BlockBundle>,
) -> (oneshot::Sender<()>, tokio::task::JoinHandle<io::Result<()>>) {
    let (tx, mut stop) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut jobs = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                      _=&mut stop=>break,
            _=jobs.join_next(),if !jobs.is_empty()=>{},
                      event=peer.next()=>{if let Event::Stream{mut stream,..}=event?{let chain=chain.clone();let extra=extra.clone();jobs.spawn(async move{
                       let mut count=0;
                       loop{let raw=match stream.read().await{Ok(v)=>v,Err(_)=>break};let reply=(||->io::Result<Vec<u8>>{
                        let env=decode_envelope(&raw).map_err(bad)?;let Some(Payload::Request(mut req))=env.payload else{return Err(bad("not request"));};let mut response_id=req.id;
                        if matches!(mode,Mode::WrongId){response_id+=1;}
                        if matches!(mode,Mode::WrongHash){let id=chain.lock().unwrap().view()?.block();req.data=Some(rq::Data::Hash(quai_primitives::schema::common::ProtoHash{value:id.to_vec()}));}
                        if let Some(rq::Data::Hash(h))=&req.data {let id:[u8;32]=h.value.as_slice().try_into().map_err(bad)?;if let Some(b)=extra.get(&id){let c=req.location.as_ref().ok_or_else(||bad("location"))?.value.len();let bytes=if c==2{b.zone.clone()}else{b.contexts.iter().find(|(n,_)|*n as usize==c).ok_or_else(||bad("missing view"))?.1.clone()};let wo=ProtoWorkObject::decode(bytes.as_slice()).map_err(bad)?;return Ok(QuaiMessage{payload:Some(Payload::Response(QuaiResponseMessage{id:response_id,location:req.location,response:Some(rs::Response::WorkObjectBlockView(ProtoWorkObjectBlockView{work_object:Some(wo)}))}))}.encode_to_vec());}}
                        let raw=QuaiMessage{payload:Some(Payload::Request(req))}.encode_to_vec();let mut reply=decode_envelope(&private_sync::serve(&chain.lock().unwrap(),&raw)?).map_err(bad)?;if let Some(Payload::Response(r))=&mut reply.payload{r.id=response_id;}Ok(reply.encode_to_vec())
                       })();let reply=match reply{Ok(v)=>v,Err(_)=>break};if stream.write(&reply).await.is_err(){break;}count+=1;if matches!(mode,Mode::Disconnect)&&count==1{let _=stream.close().await;break;}}
                       Ok::<(),io::Error>(())
                      });}}
                     }
        }
        jobs.abort_all();
        while jobs.join_next().await.is_some() {}
        Ok(())
    });
    (tx, task)
}
async fn fetch_apply(
    peer: &PrivatePeer,
    remote: libp2p::PeerId,
    head: [u8; 32],
    mut chain: PrivateCanonical,
    now: u64,
) -> io::Result<(PrivateCanonical, private_sync::Applied)> {
    let known = chain.admitted_view()?;
    let got = private_sync::acquire(peer, remote, head, &known).await?;
    tokio::task::spawn_blocking(move || {
        let report = got.apply(&mut chain, now)?;
        Ok((chain, report))
    })
    .await
    .map_err(bad)?
}
#[tokio::main(flavor = "multi_thread", worker_threads = 3)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let v: Value = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    let root = Path::new(&args[1]);
    std::fs::create_dir_all(root)?;
    let now = v["now"].as_u64().unwrap();
    let first = v["first_branch"].as_u64().unwrap() as usize;
    let winning = v["winning_branch"].as_u64().unwrap() as usize;
    let mut sources = vec![];
    for (name, index) in [("source-a", first), ("source-b", winning)] {
        let mut c = PrivateCanonical::open(&root.join(name), now)?;
        for row in v["branches"][index]["vectors"].as_array().unwrap() {
            c.import(bundle(row), now)?;
        }
        sources.push(Arc::new(Mutex::new(c)));
    }
    let client_peer = PrivatePeer::bind().await?;
    let mut tasks = vec![];
    let mut endpoints = vec![];
    for mode in [
        Mode::Normal,
        Mode::Normal,
        Mode::WrongHash,
        Mode::WrongId,
        Mode::Disconnect,
        Mode::Normal,
    ] {
        let source = PrivatePeer::bind().await?;
        source.enroll(client_peer.id)?;
        client_peer.enroll(source.id)?;
        let id = source.id;
        let address = source.address.clone();
        let source_chain = if endpoints.len() == 1 {
            sources[1].clone()
        } else {
            sources[0].clone()
        };
        let mut extra = BTreeMap::new();
        if endpoints.len() == 5 {
            let bad = v["negatives"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == "uncled-entropy")
                .unwrap();
            let b = bundle(bad);
            extra.insert(Record::decode(&b.zone)?.id(), b);
        }
        tasks.push(start(source, source_chain, mode, extra));
        client_peer.dial(id, address).await?;
        endpoints.push(id);
    }
    let directory = root.join("receiver");
    let mut client = PrivateCanonical::open(&directory, now)?;
    let head_a = sources[0].lock().unwrap().view()?.block();
    let head_b = sources[1].lock().unwrap().view()?.block();
    let genesis = client.view()?.block();
    let mut rejected = 0;
    for index in [2usize, 3, 4] {
        let before = client.view()?.checkpoint();
        assert!(private_sync::acquire(
            &client_peer,
            endpoints[index],
            head_a,
            &client.admitted_view()?
        )
        .await
        .is_err());
        assert_eq!(client.view()?.checkpoint(), before);
        assert_eq!(client.view()?.block(), genesis);
        rejected += 1;
    }
    let started = Instant::now();
    let (c, a) = fetch_apply(&client_peer, endpoints[0], head_a, client, now).await?;
    client = c;
    assert_eq!(client.view()?.block(), head_a);
    let pinned = client.view()?;
    drop(client);
    client = PrivateCanonical::open(&directory, now)?;
    assert_eq!(client.view()?.block(), head_a);
    let bad = v["negatives"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "uncled-entropy")
        .unwrap();
    let bad_id = Record::decode(&bundle(bad).zone)?.id();
    let acquired =
        private_sync::acquire(&client_peer, endpoints[5], bad_id, &client.admitted_view()?).await?;
    let before = client.view()?.checkpoint();
    assert!(acquired.apply(&mut client, now).is_err());
    assert_eq!(client.view()?.checkpoint(), before);
    rejected += 1;
    let (c, b) = fetch_apply(&client_peer, endpoints[1], head_b, client, now).await?;
    client = c;
    assert_eq!(client.view()?.block(), head_b);
    assert_eq!(pinned.block(), head_a);
    assert_eq!(pinned.height(), 16);
    assert!(b.reorgs > 0);
    let final_view = client.view()?;
    let expected = &v["branches"][winning]["vectors"][15];
    assert_eq!(encode_hex(&final_view.roots().evm), expected["evm_root"]);
    assert_eq!(encode_hex(&final_view.roots().etx), expected["etx_root"]);
    assert_eq!(encode_hex(&final_view.roots().utxo), expected["utxo_root"]);
    drop(client);
    client = PrivateCanonical::open(&directory, now)?;
    assert_eq!(client.view()?.block(), head_b);
    let empty =
        private_sync::acquire(&client_peer, endpoints[1], head_b, &client.admitted_view()?).await?;
    assert!(empty.blocks.is_empty());
    for (stop, task) in tasks {
        let _ = stop.send(());
        tokio::time::timeout(Duration::from_secs(5), task).await???;
    }
    let elapsed = started.elapsed().as_nanos();
    println!(
        "{}",
        json!({"passed":true,"unique_native_blocks_imported":a.imported_blocks+b.imported_blocks,"raw_object_requests":a.requests+b.requests,"response_payload_bytes":a.bytes+b.bytes,"receiver_restart_checks":2,"adversarial_peers_or_blocks_rejected":rejected,"canonical_reorgs":b.reorgs,"head":encode_hex(&client.view()?.block()),"height":client.view()?.height(),"noise":true,"yamux":true,"persistent_streams":true,"independent_local_stores":3,"peer_nodes_share_test_process":true,"runtime_go_dependency":false,"private_profile_canonical_sync_checked":true,"global_full_consensus":false,"canonical_public_sync":false,"public_network":false,"qualification_wall_ns_including_restart_checks":elapsed.to_string()})
    );
    Ok(())
}
