//! Deterministic fuzz-style tests for the network frame, envelope and
//! request-correlation code. Inputs are random bytes plus bit flips,
//! truncation, extension, length-field edits and splices of valid messages.
use prost::Message;
use quai_network::{
    decode_envelope, read_envelope, read_frame, response, write_envelope, write_frame, RequestBook,
    MAX_FRAME_BYTES,
};
use quai_primitives::schema::{
    block::{ProtoWorkObject, ProtoWorkObjectBlockView},
    common::{ProtoHash, ProtoLocation},
    quaiprotocol::{
        quai_message::Payload, quai_request_message as rq, quai_response_message as rs, QuaiMessage,
        QuaiRequestMessage, QuaiResponseMessage,
    },
};
use std::io::Cursor;

#[path = "../../quai-primitives/tests/fuzz_support/mod.rs"]
mod fuzz;

const ITER: usize = 60_000;
fn budget(len: usize) -> usize {
    // The frame buffer grows with the bytes that arrive, so memory follows the
    // input size (Vec doubling, plus a decoded copy in the envelope test).
    (1 << 20) + len.saturating_mul(8)
}
fn env_budget(len: usize) -> usize {
    (1 << 20) + len.saturating_mul(128)
}

fn corpus() -> Vec<Vec<u8>> {
    fuzz::hex_corpus(&[
        include_bytes!("../../../fixtures/protocol/foundation.json"),
        include_bytes!("../../../fixtures/protocol/forward-wire.json"),
        include_bytes!("../../../fixtures/protocol/canonical.json"),
    ])
}
/// Valid envelopes: Go schema vectors plus built requests and responses.
fn seeds() -> Vec<Vec<u8>> {
    let c = corpus();
    let mut out = fuzz::select(&c, 80, 50_000, |b| decode_envelope(b).is_ok());
    let location = Some(ProtoLocation { value: vec![0, 0] });
    let mk = |payload| QuaiMessage { payload: Some(payload) }.encode_to_vec();
    out.push(mk(Payload::Request(QuaiRequestMessage {
        id: 9,
        location: location.clone(),
        data: Some(rq::Data::Hash(ProtoHash { value: vec![7; 32] })),
        request: Some(rq::Request::WorkObjectBlock(ProtoWorkObjectBlockView::default())),
    })));
    out.push(mk(Payload::Request(QuaiRequestMessage {
        id: 10,
        location: location.clone(),
        data: Some(rq::Data::Number(vec![1, 2, 3])),
        request: Some(rq::Request::BlockHash(ProtoHash::default())),
    })));
    // Responses carrying real work objects (any fixture work object that decodes).
    for b in fuzz::select(&c, 20, 200_000, |b| {
        ProtoWorkObject::decode(b).is_ok()
            && quai_primitives::schema::preflight(".block.ProtoWorkObject", b).is_ok()
            && b.len() > 200
    }) {
        out.push(mk(Payload::Response(QuaiResponseMessage {
            id: 3,
            location: location.clone(),
            response: Some(rs::Response::WorkObjectBlockView(ProtoWorkObjectBlockView {
                work_object: ProtoWorkObject::decode(b.as_slice()).ok(),
            })),
        })));
    }
    out
}

#[test]
fn fuzz_envelope_decode_and_stream() {
    let seeds = seeds();
    assert!(seeds.len() > 30, "{}", seeds.len());
    let s = fuzz::drive("decode_envelope", 21, &seeds, ITER, env_budget, |b| {
        let Ok(env) = decode_envelope(b) else {
            return false;
        };
        // The accepted envelope writes and reads back to a fixed point.
        let mut out = vec![];
        write_envelope(&mut out, &env).expect("accepted envelope writes");
        let back = read_envelope(&mut Cursor::new(&out)).expect("framed envelope reads");
        assert_eq!(back.encode_to_vec(), env.encode_to_vec());
        let _ = response(env);
        true
    });
    assert!(s.accepted > 1_000);
}

#[test]
fn fuzz_frames() {
    // Raw stream bytes: any prefix/length combination. The read must stop at
    // the declared length, never above MAX_FRAME_BYTES, never panic.
    let mut good = vec![];
    write_frame(&mut good, b"hello world").unwrap();
    let mut env = vec![];
    for s in seeds().iter().take(10) {
        write_frame(&mut env, s).unwrap();
    }
    let pool = vec![good.clone(), env];
    let s = fuzz::drive("read_frame", 22, &pool, ITER, budget, |b| {
        let mut cur = Cursor::new(b);
        match read_frame(&mut cur) {
            Ok(frame) => {
                // A frame that read fully re-frames to the bytes consumed.
                let mut again = vec![];
                write_frame(&mut again, &frame).unwrap();
                assert_eq!(&b[..cur.position() as usize], &again[..]);
                true
            }
            Err(_) => false,
        }
    });
    assert!(s.accepted > 100);
    // Declared lengths just around the limit, with and without payload.
    for len in [0u32, 1, MAX_FRAME_BYTES as u32 - 1, MAX_FRAME_BYTES as u32, MAX_FRAME_BYTES as u32 + 1, u32::MAX] {
        let mut b = len.to_be_bytes().to_vec();
        b.extend([5u8; 16]);
        let (r, peak) = fuzz::peak_during(|| read_frame(&mut Cursor::new(&b)));
        // Lengths 0 and 1 fit the 16 payload bytes; the others are cut short or too large.
        assert_eq!(r.is_ok(), len <= 16, "{len}");
        assert!(peak <= 128 * 1024, "{len}: {peak}");
    }
}

#[test]
fn fuzz_request_book() {
    // Decode arbitrary envelopes and feed requests and responses to a book.
    let seeds = seeds();
    fuzz::drive("RequestBook", 23, &seeds, ITER, env_budget, |b| {
        let Ok(env) = decode_envelope(b) else {
            return false;
        };
        let mut book = RequestBook::new(4).unwrap();
        match env.payload {
            Some(Payload::Request(r)) => {
                let _ = book.register(&r);
                let _ = book.register(&r);
                book.cancel(r.id);
            }
            Some(Payload::Response(r)) => {
                let _ = book.complete(&r);
            }
            None => {}
        }
        assert!(book.len() <= 4);
        true
    });
}

#[cfg(feature = "native-transport")]
#[test]
fn fuzz_private_sync_serve() {
    use quai_consensus::private_canonical::PrivateCanonical;
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json")).unwrap();
    let now = fixture["now"].as_u64().unwrap();
    let dir = std::env::temp_dir().join(format!("quai-fuzz-serve-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    let chain = PrivateCanonical::open(&dir, now).unwrap();
    let seeds = seeds();
    let genesis_hash = chain.view().unwrap().block();
    let mut seeds = seeds;
    for loc in [vec![], vec![0], vec![0, 0]] {
        for data in [
            rq::Data::Hash(ProtoHash { value: genesis_hash.to_vec() }),
            rq::Data::Number(vec![0]),
            rq::Data::Number(vec![0, 0, 0, 0, 0, 0, 0, 0, 1]),
        ] {
            for request in [
                rq::Request::WorkObjectBlock(ProtoWorkObjectBlockView::default()),
                rq::Request::BlockHash(ProtoHash::default()),
            ] {
                seeds.push(
                    QuaiMessage {
                        payload: Some(Payload::Request(QuaiRequestMessage {
                            id: 1,
                            location: Some(ProtoLocation { value: loc.clone() }),
                            data: Some(data.clone()),
                            request: Some(request),
                        })),
                    }
                    .encode_to_vec(),
                );
            }
        }
    }
    let s = fuzz::drive("private_sync::serve", 24, &seeds, 20_000, env_budget, |b| {
        match quai_network::private_sync::serve(&chain, b) {
            Ok(reply) => {
                // The reply is itself a valid response envelope.
                let env = decode_envelope(&reply).expect("reply decodes");
                assert!(matches!(env.payload, Some(Payload::Response(_))));
                true
            }
            Err(_) => false,
        }
    });
    assert!(s.accepted > 50);
    drop(chain);
    let _ = std::fs::remove_dir_all(dir);
}
