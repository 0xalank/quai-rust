#[path = "aux_support/mod.rs"]
mod support;
use prost::Message;
use quai_consensus::{
    auxiliary::{coinbase::Coinbase, Auxiliary, Quorum},
    verifier::{Verifier, PINNED_SCHEDULE},
    Error,
};
use quai_primitives::{decode_hex, schema::block::ProtoAuxPow, Header};
use serde_json::Value;
fn corpus() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/auxiliary.json")).unwrap()
}
#[test]
fn signed_templates_and_auxiliary_bindings_match_frozen_go() {
    let r = support::verify(&corpus(), false).unwrap();
    assert_eq!(r["mismatches"], 0, "{r}");
    assert!(r["valid_pinned_bindings"].as_u64().unwrap() > 0);
}
#[test]
fn every_signature_length_is_checked_before_the_curve_parser() {
    let quorum = Quorum::pinned();
    for n in 0..=130 {
        assert!(!quorum.verify([17; 32], &vec![0; n]), "length {n}");
    }
}
#[test]
fn truncated_coinbase_and_script_lengths_never_panic_or_allocate_unboundedly() {
    let c = corpus();
    let tx = decode_hex(c["auxiliaries"][0]["aux"].as_str().unwrap()).unwrap();
    let a = ProtoAuxPow::decode(tx.as_slice()).unwrap();
    let tx = a.transaction.unwrap();
    let original =
        Auxiliary::decode(&decode_hex(c["auxiliaries"][0]["aux"].as_str().unwrap()).unwrap())
            .unwrap();
    let seal: [u8; 32] = decode_hex(c["auxiliaries"][0]["quai_seal"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    for n in 0..tx.len() {
        let mut wire = ProtoAuxPow::decode(original.encode().as_slice()).unwrap();
        wire.transaction = Some(tx[..n].to_vec());
        let a = Auxiliary::decode(&wire.encode_to_vec()).unwrap();
        assert!(
            a.admit(
                seal,
                c["auxiliaries"][0]["quai_time"].as_u64().unwrap(),
                &Quorum::pinned()
            )
            .is_err(),
            "prefix {n}"
        );
    }
    let mut malicious = vec![0; 4];
    malicious.push(1);
    malicious.extend([0; 32]);
    malicious.extend([255; 4]);
    malicious.push(255);
    malicious.extend(u64::MAX.to_le_bytes());
    assert!(Coinbase::decode(&malicious).is_err());
}
#[test]
fn pinned_runtime_rejects_lab_quorum_before_proof_cache_generation() {
    let c = corpus();
    let v = c["golden"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["profile"] == "lab")
        .unwrap();
    let h = Header::decode_auxiliary(
        &decode_hex(v["quai_header"].as_str().unwrap()).unwrap(),
        PINNED_SCHEDULE,
    )
    .unwrap();
    let mut verifier = Verifier::default();
    assert!(matches!(
        verifier.verify_header_proof(&h),
        Err(Error::Invalid("auxiliary quorum signature"))
    ));
}
#[test]
fn untrusted_donor_height_does_not_trigger_an_epoch_cache() {
    let c = corpus();
    let v = c["golden"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["profile"] == "pinned")
        .unwrap();
    let h = Header::decode_auxiliary(
        &decode_hex(v["quai_header"].as_str().unwrap()).unwrap(),
        PINNED_SCHEDULE,
    )
    .unwrap();
    let mut wire = h.wire().clone();
    let mut aux = ProtoAuxPow::decode(wire.aux_pow.as_ref().unwrap().as_slice()).unwrap();
    aux.header.as_mut().unwrap()[76..80].copy_from_slice(&u32::MAX.to_le_bytes());
    wire.aux_pow = Some(aux.encode_to_vec());
    let bad = Header::decode_auxiliary(&wire.encode_to_vec(), PINNED_SCHEDULE).unwrap();
    assert!(matches!(
        Verifier::default().verify_header_proof(&bad),
        Err(Error::Invalid("auxiliary quorum signature"))
    ));
}
#[test]
fn auxiliary_budgets_and_full_admission_boundary_are_enforced() {
    let c = corpus();
    let v = &c["golden"][0];
    let mut p =
        ProtoAuxPow::decode(decode_hex(v["aux"].as_str().unwrap()).unwrap().as_slice()).unwrap();
    p.merkle_branch = vec![vec![0; 32]; 65];
    assert!(Auxiliary::decode(&p.encode_to_vec()).is_err());
    let bytes = decode_hex(v["quai_header"].as_str().unwrap()).unwrap();
    assert!(Header::decode(&bytes, PINNED_SCHEDULE).is_err());
    assert!(Header::decode_auxiliary(&bytes, PINNED_SCHEDULE).is_ok());
    // Without its ancestry a block fails validation.
    let object = decode_hex(v["work_object"].as_str().unwrap()).unwrap();
    assert!(
        quai_consensus::history::Record::decode(&object).map_or(true, |r| {
            quai_consensus::block::validate_on_genesis(
                &mut Verifier::default(),
                quai_consensus::chain::Chain::private_hierarchy().unwrap(),
                [None, None, Some(std::sync::Arc::new(r))],
                1_800_000_000,
            )
            .is_err()
        })
    );
    assert!(Quorum::from_trusted_keys([[0; 33]; 3]).is_err());
}
