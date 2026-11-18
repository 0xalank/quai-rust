#[allow(dead_code)]
#[path = "history_support/mod.rs"]
mod support;
use prost::Message;
use quai_consensus::{history::History, validation, verifier::Verifier, Error};
fn corpus() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/history.json")).unwrap()
}
#[test]
fn frozen_go_share_controllers_classification_and_history_match() {
    let r = support::verify(&corpus()).unwrap();
    assert_eq!(r["mismatches"], 0, "{r}");
    assert!(r["accepted_inclusion_projections"].as_u64().unwrap() > 0);
}
#[test]
fn missing_history_is_unavailable_and_duplicates_are_atomic() {
    let c = corpus();
    let c = &c["distances"][0];
    let h = support::history(&c["records"]).unwrap();
    let b = support::record(&c["block"]).unwrap();
    let share = b.header();
    assert!(matches!(
        History::default().workshare_distance(&b, share),
        Err(Error::Unavailable(_))
    ));
    let mut hist = h;
    let r = support::record(&c["records"][0]).unwrap();
    let n = hist.len();
    let bytes = hist.encoded_bytes();
    hist.insert(r.clone()).unwrap();
    assert_eq!((hist.len(), hist.encoded_bytes()), (n, bytes));
    let mut wire = quai_primitives::schema::block::ProtoWorkObject::decode(r.bytes()).unwrap();
    wire.wo_body
        .as_mut()
        .unwrap()
        .header
        .as_mut()
        .unwrap()
        .extra = Some(vec![1]);
    let altered = quai_consensus::history::Record::decode(&wire.encode_to_vec()).unwrap();
    assert_eq!(altered.id(), r.id());
    assert!(matches!(hist.insert(altered), Err(Error::Invalid(_))));
    assert_eq!((hist.len(), hist.encoded_bytes()), (n, bytes));
}
#[test]
fn native_inclusions_reject_forged_mix_and_retain_no_chain_authority() {
    let c = corpus();
    let c = &c["inclusions"][0];
    let b = support::record(&c["block"]).unwrap();
    let h = support::history(&c["records"]).unwrap();
    let mut v = Verifier::default();
    assert!(matches!(
        validation::verify_inclusions(&mut v, &h, &b, &support::profile()),
        Err(Error::Invalid("mix hash"))
    ));
    // Without its ancestry a block fails validation.
    assert!(quai_consensus::block::validate_on_genesis(
        &mut v,
        quai_consensus::chain::Chain::private_hierarchy().unwrap(),
        [None, None, Some(std::sync::Arc::new(b.clone()))],
        1_800_000_000,
    )
    .is_err());
}
#[test]
fn controller_boundaries_and_invalid_inputs_are_explicit() {
    use quai_consensus::{rules::Uint, share_rules::*};
    assert_eq!(inclusion_depth(1_329_999), 3);
    assert_eq!(inclusion_depth(1_330_000), 4);
    assert_eq!(count_limits(1_530_499), (16, 8, 8));
    assert_eq!(count_limits(1_530_500), (32, 16, 16));
    assert!(kawpow_share_difficulty(
        Uint::ZERO,
        Uint::ZERO,
        Uint::ZERO,
        Uint::ZERO,
        Uint::ZERO,
        Uint::ZERO
    )
    .is_err());
    assert!(next_pow_triple(
        Uint::ONE,
        &Triple::default(),
        Uint::ONE,
        0,
        0,
        1171499,
        true
    )
    .is_err());
    assert!(support::profile().validate().is_ok());
    let mut p = support::profile();
    p.location.clear();
    assert!(p.validate().is_err());
}

#[test]
fn header_ancestry_component_checks_preserve_partial_status() {
    let c = corpus();
    let c = &c["inclusions"][0];
    let b = support::record(&c["block"]).unwrap();
    let h = support::history(&c["records"]).unwrap();
    let r = validation::check_header_ancestry(&h, &b, &support::profile(), 5000, false).unwrap();
    assert!(!r.fully_validated);
    assert!(matches!(
        validation::check_header_ancestry(&h, &b, &support::profile(), 0, false),
        Err(Error::Invalid("future block"))
    ));
    assert!(matches!(
        validation::check_header_ancestry(
            &History::default(),
            &b,
            &support::profile(),
            5000,
            false
        ),
        Err(Error::Unavailable(_))
    ));
}

#[test]
fn snapshot_record_capacity_rejects_without_mutating_history() {
    use quai_consensus::history::{Record, MAX_RECORDS};
    let c = corpus();
    let base = support::record(&c["inclusions"][0]["block"]).unwrap();
    let mut wire = quai_primitives::schema::block::ProtoWorkObject::decode(base.bytes()).unwrap();
    let mut h = History::default();
    for n in 0..MAX_RECORDS {
        wire.wo_header.as_mut().unwrap().nonce = Some(n as u64);
        h.insert(Record::decode(&wire.encode_to_vec()).unwrap())
            .unwrap();
    }
    let bytes = h.encoded_bytes();
    wire.wo_header.as_mut().unwrap().nonce = Some(MAX_RECORDS as u64);
    assert!(matches!(
        h.insert(Record::decode(&wire.encode_to_vec()).unwrap()),
        Err(Error::ResourceLimit(_))
    ));
    assert_eq!((h.len(), h.encoded_bytes()), (MAX_RECORDS, bytes));
}

#[test]
fn donor_target_equality_is_rejected_and_missing_candidate_context_is_unavailable() {
    use crypto_bigint::Encoding;
    use quai_consensus::{
        rules::Uint,
        share_rules::{self, Classification},
        verifier::PINNED_SCHEDULE,
    };
    let c = corpus();
    let sha = c["classifications"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|v| {
            let h = quai_primitives::Header::decode_auxiliary(
                &quai_primitives::decode_hex(v["header"].as_str().unwrap()).unwrap(),
                PINNED_SCHEDULE,
            )
            .ok()?;
            let aux =
                quai_consensus::auxiliary::Auxiliary::decode(h.wire().aux_pow.as_ref()?).ok()?;
            if aux.id() == 2
                && share_rules::number(h.wire().prime_terminus_number.as_ref()?).ok()? == 1171500
            {
                Some(h)
            } else {
                None
            }
        })
        .unwrap();
    let target = Uint::ONE.shl_vartime(246);
    assert_eq!(
        share_rules::classify(&sha, target.to_be_bytes(), 7).unwrap(),
        Classification::Invalid
    );
    assert_eq!(
        share_rules::classify(&sha, target.wrapping_sub(&Uint::ONE).to_be_bytes(), 7).unwrap(),
        Classification::Valid
    );
    let b = support::record(&c["inclusions"][0]["block"]).unwrap();
    let uncles = b.uncles().unwrap();
    let hashes = vec![[255; 32]; uncles.len()];
    assert!(matches!(
        validation::check_inclusions_projection(
            &History::default(),
            &b,
            &support::profile(),
            &uncles,
            &hashes,
            |_| Ok(2)
        ),
        Err(Error::Unavailable(_))
    ));
}
