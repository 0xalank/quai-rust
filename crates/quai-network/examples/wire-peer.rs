//! Development-only loopback wire interoperability with the pinned Go framing.
use quai_network::{read_envelope, response, write_envelope, RequestBook, MAX_FRAME_BYTES};
use quai_primitives::schema::{
    common::{ProtoHash, ProtoLocation},
    quaiprotocol::{
        quai_message::Payload, quai_request_message as rq, quai_response_message as rs,
        QuaiMessage, QuaiRequestMessage,
    },
};
use std::{
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    time::Duration,
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let address: SocketAddr = std::env::args().nth(1).ok_or("LOOPBACK_ADDRESS")?.parse()?;
    if !matches!(address.ip(),IpAddr::V4(ip) if ip.is_loopback()) {
        return Err("development peer accepts IPv4 loopback only".into());
    }
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut book = RequestBook::new(4)?;
    for i in 0..3 {
        let hash = vec![i as u8 + 1; 32];
        let request = QuaiRequestMessage {
            id: 1001 + i,
            location: Some(ProtoLocation { value: vec![0, 0] }),
            data: Some(rq::Data::Hash(ProtoHash {
                value: hash.clone(),
            })),
            request: Some(rq::Request::BlockHash(ProtoHash::default())),
        };
        book.register(&request)?;
        write_envelope(
            &mut stream,
            &QuaiMessage {
                payload: Some(Payload::Request(request)),
            },
        )?;
        let response = response(read_envelope(&mut stream)?)?;
        book.complete(&response)?;
        if !matches!(response.response,Some(rs::Response::BlockHash(h)) if h.value==hash) {
            return Err("Go response hash mismatch".into());
        }
    }
    if !book.is_empty() {
        return Err("pending request leaked".into());
    }
    stream.write_all(&((MAX_FRAME_BYTES + 1) as u32).to_be_bytes())?;
    let mut eof = [0];
    if stream.read(&mut eof)? != 0 {
        return Err("oversize frame was not closed".into());
    }
    println!(
        "{{\"passed\":true,\"exchanges\":3,\"oversize_rejected\":true,\"libp2p_transport\":false}}"
    );
    Ok(())
}
