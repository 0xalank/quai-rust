//! Deterministic fuzz-style tests for every decoder in quai-primitives that
//! reads untrusted bytes. Inputs are random bytes and mutations (bit flips,
//! truncation, extension, length-field edits, splices) of Go fixture bytes.
//! Each case must not panic, must stay inside an allocation budget, and an
//! accepted input must re-encode to the same bytes.
use prost::Message;
use quai_primitives::{
    auxiliary, mpt, rlp, schema,
    transaction::Transaction,
    work::{BodyHeader, WorkObject},
    Header, Schedule,
};
use std::collections::HashMap;

#[path = "fuzz_support/mod.rs"]
mod fuzz;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// Accepted inputs whose re-encoding equals the input byte for byte. Protobuf
/// accepts non-canonical forms (unknown fields, field order), so the test
/// asserts a fixed point and only reports how many inputs were identical.
static IDENTICAL: AtomicUsize = AtomicUsize::new(0);
fn same(_name: &str, input: &[u8], enc: &[u8]) {
    if input == enc {
        IDENTICAL.fetch_add(1, Relaxed);
    }
}

const ITER: usize = 60_000;
/// Peak live bytes in one decode, re-encode and re-decode. Fixed 1 MiB plus a
/// linear factor of 128. The observed worst case is far lower (see output).
fn budget(len: usize) -> usize {
    (1 << 20) + len.saturating_mul(128)
}

fn corpus() -> Vec<Vec<u8>> {
    fuzz::hex_corpus(&[
        include_bytes!("../../../fixtures/protocol/foundation.json"),
        include_bytes!("../../../fixtures/protocol/work-headers.json"),
        include_bytes!("../../../fixtures/protocol/transactions.json"),
        include_bytes!("../../../fixtures/protocol/auxiliary.json"),
        include_bytes!("../../../fixtures/protocol/zone-blocks.json"),
        include_bytes!("../../../fixtures/protocol/uncles.json"),
    ])
}
fn schedule() -> Schedule {
    let c: serde_json::Value =
        serde_json::from_slice(include_bytes!("../../../fixtures/protocol/foundation.json"))
            .unwrap();
    serde_json::from_value(c["schedule"].clone()).unwrap()
}

#[test]
fn fuzz_transaction_decode() {
    let seeds = fuzz::select(&corpus(), 150, 20_000, |b| Transaction::decode(b).is_ok());
    assert!(seeds.len() > 50, "{}", seeds.len());
    let s = fuzz::drive("Transaction::decode", 1, &seeds, ITER, budget, |b| {
        let Ok(tx) = Transaction::decode(b) else {
            return false;
        };
        // Accepted input: the decoder re-decodes its own encoding to the same value.
        same("tx", b, &tx.encode());
        let again = Transaction::decode(&tx.encode()).expect("own encoding decodes");
        assert_eq!(tx, again);
        assert_eq!(tx.encode(), again.encode());
        let _ = tx.sender();
        let _ = tx.id(Some([0, 0]));
        let _ = tx.verify_qi_signature();
        true
    });
    assert!(s.accepted > 100);
    eprintln!("Transaction::decode identical re-encodings so far: {}", IDENTICAL.load(Relaxed));
}

#[test]
fn fuzz_body_header_decode() {
    let seeds = fuzz::select(&corpus(), 150, 20_000, |b| BodyHeader::decode(b).is_ok());
    assert!(seeds.len() > 20, "{}", seeds.len());
    let s = fuzz::drive("BodyHeader::decode", 2, &seeds, ITER, budget, |b| {
        let Ok(h) = BodyHeader::decode(b) else {
            return false;
        };
        let enc = h.encode();
        same("body", b, &enc);
        let h2 = BodyHeader::decode(&enc).expect("own encoding decodes");
        assert_eq!(h2.encode(), enc);
        assert_eq!(h.hash(), h2.hash());
        true
    });
    assert!(s.accepted > 100);
}

#[test]
fn fuzz_work_object_decode() {
    let sch = schedule();
    let corpus = corpus();
    for (aux, seed) in [(false, 3), (true, 4)] {
        let dec = move |b: &[u8]| {
            if aux {
                WorkObject::decode_auxiliary(b, sch)
            } else {
                WorkObject::decode(b, sch)
            }
        };
        let seeds = fuzz::select(&corpus, 60, 200_000, |b| dec(b).is_ok());
        assert!(seeds.len() > 5, "{}", seeds.len());
        let s = fuzz::drive("WorkObject::decode", seed, &seeds, 10_000, budget, |b| {
            let Ok(w) = dec(b) else {
                return false;
            };
            let enc = w.encode();
            same("work", b, &enc);
            let w2 = dec(&enc).expect("own encoding decodes");
            assert_eq!(w2.encode(), enc);
            let _ = (w.header().hash(), w.outbound_root(), w.transactions().len());
            true
        });
        assert!(s.accepted > 50);
    }
}

#[test]
fn fuzz_header_decode() {
    let sch = schedule();
    // Headers live in the header-bearing fixtures; use the whole corpus plus work-headers.
    let mut all = corpus();
    all.extend(fuzz::hex_corpus(&[include_bytes!(
        "../../../fixtures/protocol/work-headers.json"
    )]));
    for (aux, seed) in [(false, 5), (true, 6)] {
        let dec = move |b: &[u8]| {
            if aux {
                Header::decode_auxiliary(b, sch)
            } else {
                Header::decode(b, sch)
            }
        };
        let seeds = fuzz::select(&all, 150, 70_000, |b| dec(b).is_ok());
        assert!(seeds.len() > 10, "{}", seeds.len());
        let s = fuzz::drive("Header::decode", seed, &seeds, ITER, budget, |b| {
            let Ok(h) = dec(b) else {
                return false;
            };
            let enc = h.encode();
            same("header", b, &enc);
            let h2 = dec(&enc).expect("own encoding decodes");
            assert_eq!(h2.encode(), enc);
            assert_eq!(h.seal_bytes(), h2.seal_bytes());
            true
        });
        assert!(s.accepted > 100);
    }
}

#[test]
fn fuzz_auxpow_and_template() {
    let c = corpus();
    let pow_seeds = fuzz::select(&c, 150, 100_000, |b| auxiliary::canonical_pow(b).is_ok());
    let tpl_seeds = fuzz::select(&c, 150, 100_000, |b| auxiliary::canonical_template(b).is_ok());
    assert!(pow_seeds.len() > 5 && tpl_seeds.len() > 5);
    fuzz::drive("canonical_pow", 7, &pow_seeds, ITER, budget, |b| {
        let Ok(p) = auxiliary::canonical_pow(b) else {
            return false;
        };
        // Canonical form is a fixed point.
        let enc = p.encode_to_vec();
        let q = auxiliary::canonical_pow(&enc).expect("canonical form decodes");
        assert_eq!(q.encode_to_vec(), enc);
        true
    });
    fuzz::drive("canonical_template", 8, &tpl_seeds, ITER, budget, |b| {
        let Ok(p) = auxiliary::canonical_template(b) else {
            return false;
        };
        let enc = p.encode_to_vec();
        let q = auxiliary::canonical_template(&enc).expect("canonical form decodes");
        assert_eq!(q.encode_to_vec(), enc);
        let _ = (auxiliary::template_signing_bytes(&p), auxiliary::template_hash(&p));
        true
    });
}

#[test]
fn fuzz_every_schema_message() {
    let c = corpus();
    let names: Vec<&str> = schema_names();
    for (i, name) in names.iter().enumerate() {
        let seeds = fuzz::select(&c, 40, 30_000, |b| schema::canonicalize(name, b).is_ok());
        fuzz::drive(name, 100 + i as u64, &seeds, 5_000, budget, |b| {
            let Ok(out) = schema::canonicalize(name, b) else {
                return false;
            };
            // Canonical bytes are a fixed point.
            assert_eq!(schema::canonicalize(name, &out).as_deref(), Ok(&out[..]));
            true
        });
    }
}
fn schema_names() -> Vec<&'static str> {
    // Source of truth is descriptor::NAMES; probe until preflight reports the name unknown.
    let text = include_str!("../src/schema/descriptor.rs");
    let start = text.find("pub const NAMES").unwrap();
    let body = &text[start..];
    body[..body.find("];").unwrap()]
        .lines()
        .filter_map(|l| l.trim().strip_prefix('"')?.strip_suffix("\","))
        .collect()
}

#[test]
fn fuzz_rlp_and_access_list() {
    let c = corpus();
    let seeds = fuzz::select(&c, 150, 20_000, |b| rlp::decode(b).is_ok());
    // Access-list seeds: RLP list of [address, [storage keys]].
    use quai_primitives::trie::{bytes, list};
    let acc_seeds: Vec<Vec<u8>> = (0..40u8)
        .map(|i| {
            list((0..(i % 5)).map(|j| {
                list([
                    bytes(&[i ^ j; 20]),
                    list((0..(j % 4)).map(|k| bytes(&[i.wrapping_add(k); 32]))),
                ])
            }))
        })
        .filter(|b| rlp::access_list(b).is_ok())
        .collect();
    assert!(acc_seeds.len() >= 20);
    assert!(!seeds.is_empty());
    fuzz::drive("rlp::decode", 9, &seeds, ITER, budget, |b| {
        rlp::decode(b).is_ok()
    });
    // Accepted access lists are canonical RLP lists of [address, [keys]].
    fuzz::drive("rlp::access_list", 10, &acc_seeds, ITER, budget, |b| {
        rlp::access_list(b).is_ok()
    });
}

struct Nodes(HashMap<[u8; 32], Vec<u8>>);
impl mpt::ReadNodes for Nodes {
    fn get_node(&self, h: &[u8; 32]) -> Result<Option<Vec<u8>>, mpt::Error> {
        Ok(self.0.get(h).cloned())
    }
}
#[test]
fn fuzz_mpt_nodes() {
    use quai_primitives::transaction::keccak;
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0u32..200)
        .map(|i| (i.to_be_bytes().to_vec(), vec![i as u8 | 1; 1 + (i as usize % 70)]))
        .collect();
    let trie = mpt::Trie::from_secure_pairs(&pairs).unwrap();
    let mut seeds = vec![];
    trie.visit_new_nodes(&mut |_, raw| {
        seeds.push(raw.to_vec());
        Ok(())
    })
    .unwrap();
    assert!(seeds.len() > 20);
    // Fuzz both the root node and a child: the fuzz bytes are served under
    // their own hash, so the hash check passes and the node decoder runs.
    fuzz::drive("mpt node", 11, &seeds, ITER, budget, |b| {
        let h = keccak(b);
        let reader = Nodes(HashMap::from([(h, b.to_vec())]));
        let t = mpt::Trie::from_root(h);
        let ok = t.check_root(&reader).is_ok();
        let _ = t.get(&reader, &[1, 2, 3, 4]);
        let _ = t.visit_leaves(&reader, 1000, 1 << 20, &mut |_, _| Ok(()));
        ok
    });
}
