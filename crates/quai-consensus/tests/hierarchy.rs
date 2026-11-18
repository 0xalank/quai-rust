#[allow(dead_code)]
#[path = "hierarchy_support/mod.rs"]
mod support;
use prost::Message;
use quai_consensus::{
    hierarchy_header::{self, Profile},
    history::{History, Record},
    rules::Uint,
    verifier::Verifier,
    Error,
};
fn corpus() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/hierarchy.json")).unwrap()
}
#[test]
fn frozen_prime_region_policy_and_arithmetic_samples_match_go() {
    let mut c = corpus();
    c["headers"] = serde_json::Value::Array(
        c["headers"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                i / 34 == 0
                    || i / 34 == 1
                    || i / 34 == 10
                    || i / 34 == 20
                    || i / 34 == 21
                    || i / 34 == 22
            })
            .map(|(_, v)| v.clone())
            .collect(),
    );
    let r = support::verify_policy(&c).unwrap();
    assert_eq!(r["mismatches"], 0, "{r}");
}
#[test]
fn efficiency_and_threshold_wrapping_stays_compatible() {
    let z = [Uint::ZERO; 2];
    assert_eq!(
        hierarchy_header::efficiency_score(65535, z, z, false).unwrap(),
        65535
    );
    assert_eq!(
        hierarchy_header::efficiency_score(65535, z, z, true).unwrap(),
        6552
    );
    assert!(matches!(
        hierarchy_header::efficiency_score(1, z, [Uint::ONE, Uint::ZERO], false),
        Err(Error::Unsupported(_))
    ));
    for (previous, score, expected) in [
        (0, 65535, 0),
        (143, 65535, 144),
        (143, 0, 0),
        (144, 0, 145),
        (1168, 65535, 0),
        (65535, 65535, 0),
    ] {
        assert_eq!(hierarchy_header::threshold_count(previous, score), expected)
    }
}
#[test]
fn etx_bitmap_only_changes_the_parent_slice_after_strict_activation() {
    for pos in 0..256 {
        let location = [(pos / 16) as u8, (pos % 16) as u8];
        let before = [0xa5; 32];
        let mut on = before;
        on[pos / 8] |= 1 << (pos % 8);
        assert_eq!(
            hierarchy_header::update_etx_eligible(before, &location, 259201).unwrap(),
            on
        );
        let mut off = before;
        off[pos / 8] &= !(1 << (pos % 8));
        assert_eq!(
            hierarchy_header::update_etx_eligible(before, &location, 259200).unwrap(),
            off
        )
    }
    for location in [vec![], vec![0], vec![16, 0], vec![0, 16]] {
        assert!(hierarchy_header::update_etx_eligible([0; 32], &location, 0).is_err())
    }
}
#[test]
fn miner_ema_uses_prime_height_and_has_no_intermediate_overflow() {
    let d = Uint::from_u64(4000);
    let m = Uint::from_u64(8000);
    assert_eq!(hierarchy_header::miner_difficulty(262000, d, m).unwrap(), d);
    assert_eq!(
        hierarchy_header::miner_difficulty(262001, d, m).unwrap(),
        Uint::from_u64(7999)
    );
    assert_eq!(
        hierarchy_header::miner_difficulty(u64::MAX, Uint::MAX, Uint::MAX).unwrap(),
        Uint::MAX
    );
}
#[test]
fn missing_history_clock_and_zone_profiles_fail_before_parent_work() {
    let c = corpus();
    let row = &c["native"][0];
    let b = support::history::record(&row["block"]).unwrap();
    let p = support::profile(row).unwrap();
    let mut verifier = Verifier::default();
    assert!(matches!(
        hierarchy_header::verify_header_components(
            &mut verifier,
            &History::default(),
            &b,
            &p,
            1769112040,
            false
        ),
        Err(Error::Unavailable(_))
    ));
    let h = support::history::history(&row["records"]).unwrap();
    assert!(matches!(
        hierarchy_header::verify_header_components(&mut verifier, &h, &b, &p, u64::MAX, false),
        Err(Error::Unsupported(_))
    ));
    let invalid = Profile {
        location: vec![0, 0],
        genesis: Default::default(),
    };
    assert!(invalid.validate().is_err());
    // Without its ancestry a block fails validation.
    assert!(quai_consensus::block::validate_on_genesis(
        &mut verifier,
        quai_consensus::chain::Chain::private_hierarchy().unwrap(),
        [None, None, Some(std::sync::Arc::new(b.clone()))],
        1_800_000_000,
    )
    .is_err());
}
#[test]
fn prime_and_region_do_not_inherit_zone_header_rules() {
    let mut c = corpus();
    let rows = c["headers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["mode"] == 0 && v["variant"] == 17)
        .cloned()
        .collect::<Vec<_>>();
    assert!(rows.iter().all(|v| v["valid"] == true));
    c["headers"] = serde_json::Value::Array(rows);
    c["thresholds"] = serde_json::json!([]);
    c["scores"] = serde_json::json!([]);
    c["miners"] = serde_json::json!([]);
    c["bitmaps"] = serde_json::json!([]);
    let r = support::verify_policy(&c).unwrap();
    assert_eq!(r["mismatches"], 0, "{r}");
}
#[test]
fn zero_context_parent_is_reported_without_a_work_or_authority_claim() {
    let c = corpus();
    for row in c["native"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["id"].as_str().unwrap().starts_with("zero-context"))
    {
        let b = support::history::record(&row["block"]).unwrap();
        let h = support::history::history(&row["records"]).unwrap();
        let r = hierarchy_header::verify_header_components(
            &mut Verifier::default(),
            &h,
            &b,
            &support::profile(row).unwrap(),
            1769112040,
            false,
        )
        .unwrap();
        assert!(
            !r.parent_work_checked
                && !r.candidate_work_checked
                && !r.candidate_order_checked
                && !r.history_verified
                && !r.fully_validated
        );
    }
}
#[test]
fn bad_quorum_does_not_become_a_candidate_work_pass() {
    let c = corpus();
    let row = c["native"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "zero-context/0/1171500/true")
        .unwrap();
    let b = support::history::record(&row["block"]).unwrap();
    let mut w = quai_primitives::schema::block::ProtoWorkObject::decode(b.bytes()).unwrap();
    let wh = w.wo_header.as_mut().unwrap();
    wh.aux_pow.as_mut().unwrap().signature.as_mut().unwrap()[0] ^= 1;
    let b = Record::decode(&w.encode_to_vec()).unwrap();
    let h = support::history::history(&row["records"]).unwrap();
    assert!(matches!(
        hierarchy_header::verify_header_components(
            &mut Verifier::default(),
            &h,
            &b,
            &support::profile(row).unwrap(),
            1769112040,
            false
        ),
        Err(Error::Invalid("auxiliary quorum signature"))
    ));
}
