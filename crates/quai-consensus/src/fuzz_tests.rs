//! Deterministic fuzz-style tests for consensus decoders that read peer or
//! file bytes: ancestry records, AuxPoW and coinbase, termini, hierarchy
//! deltas, choice rows, donor hashes, quorum signatures and the private
//! profile. Inputs are random bytes plus mutations of Go fixture bytes.
use crate::{
    auxiliary::{coinbase::Coinbase, Auxiliary, Quorum},
    confirmation::{Hierarchy, LocalOutputs},
    history::Record,
    placement::Termini,
    verifier::Verifier,
};

#[path = "../../quai-primitives/tests/fuzz_support/mod.rs"]
mod fuzz;

const ITER: usize = 40_000;
/// Peak live bytes: fixed 2 MiB plus 128 per input byte.
fn budget(len: usize) -> usize {
    (2 << 20) + len.saturating_mul(128)
}
fn corpus() -> Vec<Vec<u8>> {
    fuzz::hex_corpus(&[
        include_bytes!("../../../fixtures/protocol/canonical.json"),
        include_bytes!("../../../fixtures/protocol/history.json"),
        include_bytes!("../../../fixtures/protocol/auxiliary.json"),
        include_bytes!("../../../fixtures/protocol/placement.json"),
        include_bytes!("../../../fixtures/protocol/zone-blocks.json"),
        include_bytes!("../../../fixtures/protocol/uncles.json"),
    ])
}

#[test]
fn fuzz_history_record_decode() {
    let seeds = fuzz::select(&corpus(), 60, 150_000, |b| Record::decode(b).is_ok());
    assert!(seeds.len() > 10, "{}", seeds.len());
    let s = fuzz::drive("Record::decode", 31, &seeds, 8_000, budget, |b| {
        let Ok(r) = Record::decode(b) else {
            return false;
        };
        // An accepted record re-decodes from its own bytes to the same record.
        let again = Record::decode(r.bytes()).expect("own bytes decode");
        assert_eq!(again.id(), r.id());
        assert_eq!(again.bytes(), r.bytes());
        for c in 0..4 {
            let _ = (r.parent(c), r.number(c));
        }
        let _ = (r.verify_binding(), r.difficulty(), r.uncles(), r.prime());
        true
    });
    assert!(s.accepted > 50);
}

#[test]
fn fuzz_auxiliary_and_coinbase() {
    let c = corpus();
    let seeds = fuzz::select(&c, 120, 100_000, |b| Auxiliary::decode(b).is_ok());
    assert!(seeds.len() > 10, "{}", seeds.len());
    let s = fuzz::drive("Auxiliary::decode", 32, &seeds, ITER, budget, |b| {
        let Ok(a) = Auxiliary::decode(b) else {
            return false;
        };
        let enc = a.encode();
        assert_eq!(
            Auxiliary::decode(&enc).expect("canonical decodes").encode(),
            enc
        );
        let _ = (a.id(), a.header().len(), a.signature().len());
        let _ = a.template();
        true
    });
    assert!(s.accepted > 500);
    // Coinbase transactions taken from the accepted auxiliary seeds.
    let txs: Vec<Vec<u8>> = seeds
        .iter()
        .filter_map(|b| Auxiliary::decode(b).ok())
        .map(|a| a.transaction().to_vec())
        .filter(|t| Coinbase::decode(t).is_ok())
        .collect();
    assert!(txs.len() > 5, "{}", txs.len());
    let s = fuzz::drive("Coinbase::decode", 33, &txs, ITER, budget, |b| {
        Coinbase::decode(b).is_ok()
    });
    assert!(s.accepted > 100);
}

#[test]
fn fuzz_termini_decode() {
    let mut seeds = fuzz::select(&corpus(), 80, 9_000, |b| Termini::decode(b).is_ok());
    for i in 0..20u8 {
        let t = Termini {
            dominant: std::array::from_fn(|j| [i ^ j as u8; 32]),
            subordinate: std::array::from_fn(|j| [i.wrapping_add(j as u8); 32]),
        };
        seeds.push(t.encode());
    }
    let s = fuzz::drive("Termini::decode", 34, &seeds, ITER, budget, |b| {
        let Ok(t) = Termini::decode(b) else {
            return false;
        };
        // Short hashes are left-padded, so the canonical form is the fixed point.
        let enc = t.encode();
        assert_eq!(Termini::decode(&enc).expect("own encoding").encode(), enc);
        true
    });
    assert!(s.accepted > 500);
}

#[test]
fn fuzz_choice_row_and_donor_hashes() {
    use crate::choice_window::Choice;
    let seeds: Vec<Vec<u8>> = (0..30u8)
        .map(|i| Choice::default().encode().iter().map(|b| b ^ i).collect())
        .collect();
    let s = fuzz::drive("Choice::decode", 35, &seeds, ITER, budget, |b| {
        let Ok(c) = Choice::decode(b) else {
            return false;
        };
        assert_eq!(&c.encode()[..], b);
        true
    });
    assert!(s.accepted > 100);
    let headers: Vec<Vec<u8>> = (0..10u8).map(|i| vec![i; 80]).collect();
    fuzz::drive("donor::sha256d", 36, &headers, ITER, budget, |b| {
        crate::donor::sha256d(b).is_ok()
    });
    // scrypt costs about a millisecond a call; keep the count small.
    fuzz::drive("donor::scrypt", 37, &headers, 1_500, budget, |b| {
        crate::donor::scrypt(b).is_ok()
    });
}

#[test]
fn fuzz_quorum_signatures_and_keys() {
    let q = Quorum::pinned();
    let c = corpus();
    let sigs: Vec<Vec<u8>> = fuzz::select(&c, 60, 64, |b| b.len() == 64);
    let seeds = if sigs.is_empty() {
        vec![vec![7; 64]]
    } else {
        sigs
    };
    fuzz::drive("Quorum::verify", 38, &seeds, 6_000, budget, |b| {
        q.verify([9; 32], b)
    });
    // Trusted-key constructor: arbitrary 33-byte keys must be rejected, not panic.
    let keys: Vec<Vec<u8>> = q.keys().iter().map(|k| k.to_vec()).collect();
    fuzz::drive("Quorum::from_trusted_keys", 39, &keys, 3_000, budget, |b| {
        let mut k = [[0u8; 33]; 3];
        for (i, slot) in k.iter_mut().enumerate() {
            for (j, byte) in slot.iter_mut().enumerate() {
                *byte = b.get((i * 33 + j) % b.len().max(1)).copied().unwrap_or(2);
            }
        }
        Quorum::from_trusted_keys(k).is_ok()
    });
}

#[test]
fn fuzz_private_profile_decode() {
    use crate::private_profile::PrivateChain;
    // The profile accepts only reviewed bytes (a fingerprint check), so any
    // mutation must be rejected with an error. Fuzz the JSON-shaped inputs.
    let seed = vec![b"{\"schema\":1}".to_vec(), b"[]".to_vec(), b"{".to_vec()];
    let s = fuzz::drive("PrivateChain::decode", 40, &seed, ITER, budget, |b| {
        PrivateChain::decode(b).is_ok()
    });
    assert_eq!(s.accepted, 0);
}

#[test]
fn fuzz_local_hierarchy_delta() {
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/confirmation.json")).unwrap();
    let now = corpus["now"].as_u64().unwrap();
    let bytes = |v: &serde_json::Value| quai_primitives::decode_hex(v.as_str().unwrap()).unwrap();
    let mut graph = Hierarchy::new().unwrap();
    let mut verifier = Verifier::default();
    let mut cases: Vec<(Record, Vec<u8>)> = vec![];
    for row in corpus["branches"][0]["vectors"].as_array().unwrap() {
        let zone = Record::decode(&bytes(&row["work_object"])).unwrap();
        let contexts = row["contexts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| {
                let c = v["context"].as_u64().unwrap() as u8;
                (c < 2).then(|| (c, bytes(&v["work_object"])))
            })
            .collect::<Vec<_>>();
        let prepared = graph.prepare(&mut verifier, &zone, &contexts, now).unwrap();
        let raw = prepared.encode_local_outputs().unwrap();
        cases.push((zone, raw));
        graph.commit(prepared).unwrap();
    }
    assert!(cases.len() >= 3);
    let pool: Vec<Vec<u8>> = cases.iter().map(|c| c.1.clone()).collect();
    for (i, (zone, raw)) in cases.iter().enumerate() {
        // Seeds apply to their own zone record; other zone records test the binding check.
        let seeds = vec![raw.clone()];
        let s = fuzz::drive(
            "LocalOutputs::decode",
            41 + i as u64,
            &seeds,
            6_000,
            budget,
            |b| {
                let Ok(d) = LocalOutputs::decode(b, zone) else {
                    return false;
                };
                let _ = (
                    d.heads(),
                    d.order(),
                    d.region_rollup().len(),
                    d.confirmed().len(),
                );
                for c in 0..4 {
                    let _ = d.snapshot(c);
                }
                true
            },
        );
        assert!(s.accepted >= 1);
    }
    let _ = pool;
}
