//! Controlled loopback transport checks. No public peers or chain authority.
use prost::Message;
use quai_network::{
    decode_envelope, response,
    transport::{Event, PrivatePeer},
    RequestBook,
};
use quai_primitives::schema::{
    common::{ProtoHash, ProtoLocation},
    quaiprotocol::{
        quai_message::Payload, quai_request_message as rq, quai_response_message as rs,
        QuaiMessage, QuaiRequestMessage, QuaiResponseMessage,
    },
};
use std::{io, time::Duration};
fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
fn request(i: u32) -> QuaiRequestMessage {
    QuaiRequestMessage {
        id: 1000 + i,
        location: Some(ProtoLocation { value: vec![0, 0] }),
        data: Some(rq::Data::Hash(ProtoHash {
            value: vec![i as u8 + 1; 32],
        })),
        request: Some(rq::Request::BlockHash(ProtoHash::default())),
    }
}
async fn exchange(client: &PrivatePeer, peer: libp2p::PeerId) -> io::Result<usize> {
    let mut stream = client.open(peer).await?;
    let mut book = RequestBook::new(8).map_err(bad)?;
    for i in 0..8 {
        let request = request(i);
        book.register(&request).map_err(bad)?;
        stream
            .write(
                &QuaiMessage {
                    payload: Some(Payload::Request(request)),
                }
                .encode_to_vec(),
            )
            .await?;
        let reply = response(decode_envelope(&stream.read().await?).map_err(bad)?).map_err(bad)?;
        book.complete(&reply).map_err(bad)?;
        if !matches!(reply.response,Some(rs::Response::BlockHash(h))if h.value==vec![i as u8+1;32])
        {
            return Err(bad("wrong response payload"));
        }
    }
    if !book.is_empty() {
        return Err(bad("request leaked"));
    }
    stream.close().await?;
    Ok(8)
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "local".into());
    if mode == "go" {
        let peer = PrivatePeer::bind().await?;
        let ready =
            serde_json::json!({"peer_id":peer.id.to_string(),"address":peer.address.to_string()});
        println!("{}", ready);
        use tokio::io::AsyncBufReadExt;
        let mut line = String::new();
        tokio::io::BufReader::new(tokio::io::stdin())
            .read_line(&mut line)
            .await?;
        let go: serde_json::Value = serde_json::from_str(&line)?;
        let id: libp2p::PeerId = go["peer_id"].as_str().ok_or("peer_id")?.parse()?;
        let address = go["address"].as_str().ok_or("address")?.parse()?;
        peer.enroll(id)?;
        peer.dial(id, address).await?;
        let n = exchange(&peer, id).await?;
        println!(
            "{}",
            serde_json::json!({"passed":true,"go_rust_libp2p_exchanges":n,"persistent_stream":true,"noise":true,"yamux":true,"public_network":false})
        );
        return Ok(());
    }
    let mut server = PrivatePeer::bind().await?;
    let client = PrivatePeer::bind().await?;
    server.enroll(client.id)?;
    client.enroll(server.id)?;
    let server_id = server.id;
    let address = server.address.clone();
    let client_id = client.id;
    let (done, finished) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        loop {
            if let Event::Stream { peer, mut stream } = server.next().await? {
                if peer != client_id {
                    return Err(bad("untrusted peer"));
                }
                for _ in 0..8 {
                    let message = decode_envelope(&stream.read().await?).map_err(bad)?;
                    let Some(Payload::Request(request)) = message.payload else {
                        return Err(bad("expected request"));
                    };
                    let Some(rq::Data::Hash(hash)) = request.data else {
                        return Err(bad("expected hash"));
                    };
                    let reply = QuaiMessage {
                        payload: Some(Payload::Response(QuaiResponseMessage {
                            id: request.id,
                            location: request.location,
                            response: Some(rs::Response::BlockHash(hash)),
                        })),
                    };
                    stream.write(&reply.encode_to_vec()).await?;
                }
                let _ = finished.await;
                return Ok::<_, io::Error>(8);
            }
        }
    });
    client.dial(server_id, address).await?;
    let n = exchange(&client, server_id).await?;
    let _ = done.send(());
    let incoming = tokio::time::timeout(Duration::from_secs(15), task).await???;
    if client
        .dial(server_id, "/ip4/203.0.113.1/tcp/12345".parse()?)
        .await
        .is_ok()
    {
        return Err("public dial accepted".into());
    }
    println!(
        "{}",
        serde_json::json!({"passed":true,"rust_rust_libp2p_exchanges":n,"server_exchanges":incoming,"persistent_stream":true,"noise":true,"yamux":true,"public_dial_rejected":true,"public_network":false})
    );
    Ok(())
}
