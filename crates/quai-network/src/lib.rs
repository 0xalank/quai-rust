//! Quai application messages, stream framing and per-peer request correlation.
//! Optional `native-transport` adds enrolled loopback-only libp2p components.
//! Complete block admission and native chain synchronization remain unavailable.
use prost::Message;
use quai_primitives::{
    encode_hex,
    schema::{
        self,
        quaiprotocol::{
            self as p, quai_message, quai_request_message as rq, quai_response_message as rs,
        },
    },
    Hash,
};
use std::{
    collections::BTreeMap,
    fmt,
    io::{self, Read, Write},
};
pub const STREAM_PROTOCOL: &str = "/quai/1.0.0";
pub const DHT_PROTOCOL: &str = "/quai";
pub const MAX_FRAME_BYTES: usize = 3 << 20;
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Limit,
    Invalid(String),
    Correlation,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "stream: {e}"),
            Self::Limit => write!(f, "Quai stream frame exceeds 3 MiB"),
            Self::Invalid(e) => write!(f, "invalid Quai message: {e}"),
            Self::Correlation => write!(f, "unexpected response ID, location or type"),
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
/// Read the advertised length, check it against the limit, then read the payload in steps. Deadlines and
/// connection/resource management belong to the eventual transport.
pub fn read_frame(reader: &mut impl Read) -> Result<Vec<u8>, Error> {
    let mut prefix = [0; 4];
    reader.read_exact(&mut prefix)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(Error::Limit);
    }
    // Grow with the bytes that arrive. A peer that declares 3 MiB and sends
    // nothing does not make this side reserve 3 MiB.
    let mut bytes = Vec::with_capacity(length.min(64 * 1024));
    reader.take(length as u64).read_to_end(&mut bytes)?;
    if bytes.len() != length {
        return Err(Error::Io(io::ErrorKind::UnexpectedEof.into()));
    }
    Ok(bytes)
}
pub fn write_frame(writer: &mut impl Write, bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(Error::Limit);
    }
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(bytes)?;
    Ok(())
}
pub fn decode_envelope(bytes: &[u8]) -> Result<p::QuaiMessage, Error> {
    schema::preflight(".quaiprotocol.QuaiMessage", bytes).map_err(Error::Invalid)?;
    let envelope = p::QuaiMessage::decode(bytes).map_err(|e| Error::Invalid(e.to_string()))?;
    if envelope.payload.is_none() {
        return Err(Error::Invalid("missing request/response payload".into()));
    }
    Ok(envelope)
}
pub fn read_envelope(reader: &mut impl Read) -> Result<p::QuaiMessage, Error> {
    decode_envelope(&read_frame(reader)?)
}
pub fn write_envelope(writer: &mut impl Write, envelope: &p::QuaiMessage) -> Result<(), Error> {
    if envelope.encoded_len() > MAX_FRAME_BYTES {
        return Err(Error::Limit);
    }
    let bytes = envelope.encode_to_vec();
    decode_envelope(&bytes)?;
    write_frame(writer, &bytes)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopicKind {
    Headers,
    Blocks,
    Workshares,
    AuxTemplate,
}
impl TopicKind {
    fn suffix(self) -> &'static str {
        match self {
            Self::Headers => "headers",
            Self::Blocks => "blocks",
            Self::Workshares => "worksharev2",
            Self::AuxTemplate => "auxtemplate",
        }
    }
}
pub fn topic(genesis: &Hash, location: &[u8], kind: TopicKind) -> Result<String, Error> {
    if location.len() > 2 || location.iter().any(|n| *n > 15) {
        return Err(Error::Invalid("invalid chain location".into()));
    }
    let path = location
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!(
        "{}/{}/{}",
        encode_hex(genesis),
        path,
        kind.suffix()
    ))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponseKind {
    Header,
    Block,
    Blocks,
    Hash,
    Aux,
}
fn request_kind(request: &rq::Request) -> ResponseKind {
    match request {
        rq::Request::WorkObjectHeader(_) => ResponseKind::Header,
        rq::Request::WorkObjectBlock(_) => ResponseKind::Block,
        rq::Request::WorkObjectBlocks(_) => ResponseKind::Blocks,
        rq::Request::BlockHash(_) => ResponseKind::Hash,
        rq::Request::AuxTemplate(_) => ResponseKind::Aux,
    }
}
fn response_kind(response: &rs::Response) -> ResponseKind {
    match response {
        rs::Response::WorkObjectHeaderView(_) => ResponseKind::Header,
        rs::Response::WorkObjectBlockView(_) => ResponseKind::Block,
        rs::Response::WorkObjectBlocksView(_) => ResponseKind::Blocks,
        rs::Response::BlockHash(_) => ResponseKind::Hash,
        rs::Response::AuxTemplate(_) => ResponseKind::Aux,
    }
}
/// A bounded book for one authenticated peer/connection. The transport must
/// cancel timed-out requests and destroy the book on connection teardown.
/// Correlation never establishes validity of returned blocks or state.
pub struct RequestBook {
    pending: BTreeMap<u32, (Vec<u8>, ResponseKind)>,
    limit: usize,
}
impl RequestBook {
    pub fn new(limit: usize) -> Result<Self, Error> {
        if limit == 0 || limit > 4096 {
            return Err(Error::Invalid(
                "request limit outside supported bounds".into(),
            ));
        }
        Ok(Self {
            pending: BTreeMap::new(),
            limit,
        })
    }
    pub fn register(&mut self, request: &p::QuaiRequestMessage) -> Result<(), Error> {
        let location = &request
            .location
            .as_ref()
            .ok_or_else(|| Error::Invalid("missing request location".into()))?
            .value;
        if location.len() > 2 || location.iter().any(|n| *n > 15) {
            return Err(Error::Invalid("invalid request location".into()));
        }
        let kind = request_kind(
            request
                .request
                .as_ref()
                .ok_or_else(|| Error::Invalid("missing request kind".into()))?,
        );
        if self.pending.contains_key(&request.id) || self.pending.len() == self.limit {
            return Err(Error::Invalid(
                "duplicate request ID or full request book".into(),
            ));
        }
        self.pending.insert(request.id, (location.clone(), kind));
        Ok(())
    }
    pub fn complete(&mut self, response: &p::QuaiResponseMessage) -> Result<(), Error> {
        let (location, kind) = self.pending.get(&response.id).ok_or(Error::Correlation)?;
        if response
            .location
            .as_ref()
            .is_none_or(|v| &v.value != location)
            || response
                .response
                .as_ref()
                .is_none_or(|v| response_kind(v) != *kind)
        {
            return Err(Error::Correlation);
        }
        self.pending.remove(&response.id);
        Ok(())
    }
    pub fn cancel(&mut self, id: u32) -> bool {
        self.pending.remove(&id).is_some()
    }
    pub fn len(&self) -> usize {
        self.pending.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}
/// Extract a response for a caller that has already chosen the expected direction.
pub fn response(envelope: p::QuaiMessage) -> Result<p::QuaiResponseMessage, Error> {
    match envelope.payload {
        Some(quai_message::Payload::Response(v)) => Ok(v),
        _ => Err(Error::Invalid("expected response envelope".into())),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use quai_primitives::schema::common::{ProtoHash, ProtoLocation};
    use std::io::Cursor;
    #[test]
    fn partial_io_truncation_and_limit() {
        struct Fragmented(Cursor<Vec<u8>>);
        impl Read for Fragmented {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
                let n = b.len().min(1);
                self.0.read(&mut b[..n])
            }
        }
        impl Write for Fragmented {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.0.write(&b[..b.len().min(1)])
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Fragmented(Cursor::new(vec![]));
        write_frame(&mut writer, b"hello").unwrap();
        let mut reader = Fragmented(Cursor::new(writer.0.into_inner()));
        assert_eq!(read_frame(&mut reader).unwrap(), b"hello");
        assert!(read_frame(&mut Cursor::new(vec![0, 0, 0, 5, 1])).is_err());
        let mut reader = Cursor::new(((MAX_FRAME_BYTES + 1) as u32).to_be_bytes());
        assert!(matches!(read_frame(&mut reader), Err(Error::Limit)));
        assert_eq!(reader.position(), 4);
        assert!(decode_envelope(&[]).is_err());
    }
    #[test]
    fn responses_cannot_consume_other_pending_requests() {
        let location = Some(ProtoLocation { value: vec![0, 0] });
        let request = p::QuaiRequestMessage {
            id: 7,
            location: location.clone(),
            data: Some(rq::Data::Number(vec![1])),
            request: Some(rq::Request::BlockHash(ProtoHash::default())),
        };
        let mut book = RequestBook::new(1).unwrap();
        book.register(&request).unwrap();
        assert!(book.register(&request).is_err());
        let mut response = p::QuaiResponseMessage {
            id: 8,
            location: location.clone(),
            response: Some(rs::Response::BlockHash(ProtoHash::default())),
        };
        assert!(book.complete(&response).is_err());
        assert_eq!(book.len(), 1);
        response.id = 7;
        response.location = Some(ProtoLocation { value: vec![1, 1] });
        assert!(book.complete(&response).is_err());
        assert_eq!(book.len(), 1);
        response.location = location;
        response.response = Some(rs::Response::AuxTemplate(Default::default()));
        assert!(book.complete(&response).is_err());
        response.response = Some(rs::Response::BlockHash(ProtoHash::default()));
        book.complete(&response).unwrap();
        assert!(book.is_empty());
        assert!(book.complete(&response).is_err());
    }
    #[test]
    fn frozen_go_topics() {
        let corpus: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../../fixtures/protocol/foundation.json"))
                .unwrap();
        for v in corpus["topics"].as_array().unwrap() {
            let genesis = quai_primitives::decode_hex(v["genesis"].as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            let location = v["location"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u8)
                .collect::<Vec<_>>();
            let kind = match v["kind"].as_str().unwrap() {
                "headers" => TopicKind::Headers,
                "blocks" => TopicKind::Blocks,
                "worksharev2" => TopicKind::Workshares,
                "auxtemplate" => TopicKind::AuxTemplate,
                _ => panic!(),
            };
            assert_eq!(
                topic(&genesis, &location, kind).unwrap(),
                v["topic"].as_str().unwrap()
            );
        }
    }
}

#[cfg(feature = "native-transport")]
pub mod transport;

#[cfg(feature = "native-transport")]
pub mod private_sync;
