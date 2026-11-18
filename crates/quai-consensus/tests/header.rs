#[allow(dead_code)]
#[path = "header_support/mod.rs"]
mod support;
use quai_consensus::{
    economics,
    history::History,
    rules::Uint,
    verifier::{Verifier, PINNED_SCHEDULE},
    zone_header, Error,
};
use quai_primitives::{decode_hex, Header};
fn corpus() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/header.json")).unwrap()
}
#[test]
fn frozen_zone_header_and_economic_samples_match_go() {
    let mut c = corpus();
    c["quotes"] = serde_json::Value::Array(
        c["quotes"]
            .as_array()
            .unwrap()
            .iter()
            .step_by(7)
            .cloned()
            .collect(),
    );
    c["headers"] = serde_json::Value::Array(
        c["headers"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .filter(|(i, _)| i / 46 == 0 || i / 46 == 20 || i / 46 == 30)
            .map(|(_, v)| v.clone())
            .collect(),
    );
    let r = support::verify_policy(&c).unwrap();
    assert_eq!(r["mismatches"], 0, "{r}");
}
#[test]
fn base_fee_doubling_and_qi_activation_boundaries_are_exact() {
    let period = 365u64 * 17280 * 269 / 100;
    assert_eq!(
        economics::one_over_kqi(1_220_000),
        Uint::from_u64((period + 1_220_000) * 26_000_000 / period)
    );
    assert_eq!(
        economics::one_over_kqi(period),
        Uint::from_u64(16_000_000_000)
    );
    assert_eq!(
        economics::one_over_kqi(2 * period),
        Uint::from_u64(32_000_000_000)
    );
    assert_eq!(
        economics::one_over_kqi(u64::MAX),
        Uint::from_u64(32_000_000_000)
    );
}
#[test]
fn unavailable_history_and_invalid_clock_never_produce_header_authority() {
    let c = corpus();
    let v = &c["native"][0];
    let b = support::history::record(&v["block"]).unwrap();
    let p = support::profile(v).unwrap();
    let mut verifier = Verifier::default();
    assert!(matches!(
        zone_header::verify_zone_header_components(
            &mut verifier,
            &History::default(),
            &b,
            &p,
            1769112020,
            false
        ),
        Err(Error::Unavailable(_))
    ));
    let h = support::history::history(&v["records"]).unwrap();
    let zero = zone_header::ParentEntropyProjection {
        order: 0,
        total: Uint::ZERO,
        delta: Uint::ZERO,
        uncled_delta: Uint::ZERO,
    };
    assert!(matches!(
        zone_header::check_zone_header_projection(
            &mut verifier,
            &h,
            &b,
            &p,
            u64::MAX,
            false,
            &zero
        ),
        Err(Error::Unsupported(_))
    ));
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
fn conversion_fee_signal_is_fork_selected_and_amounts_are_bounded() {
    let c = corpus();
    for prime in [2236999, 2237000] {
        let v = c["quotes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| {
                let h = Header::decode_auxiliary(
                    &decode_hex(v["header"].as_str().unwrap()).unwrap(),
                    PINNED_SCHEDULE,
                )
                .unwrap();
                quai_consensus::share_rules::number(
                    h.wire().prime_terminus_number.as_deref().unwrap(),
                )
                .unwrap()
                    == prime
            })
            .unwrap();
        let h = Header::decode_auxiliary(
            &decode_hex(v["header"].as_str().unwrap()).unwrap(),
            PINNED_SCHEDULE,
        )
        .unwrap();
        let quote = economics::reward_quote(&h, Uint::ONE, Uint::from_u64(10)).unwrap();
        if prime == 2236999 {
            assert_eq!(quote.conversion_quai, quote.quai)
        } else {
            assert_eq!(
                quote.conversion_quai,
                quote.quai.wrapping_add(&Uint::from_u64(20))
            );
            assert!(matches!(
                economics::reward_quote(&h, Uint::MAX, Uint::MAX),
                Err(Error::Unsupported(_))
            ));
        }
    }
}
#[test]
fn header_policy_checks_signatures_before_any_candidate_proof_claim() {
    let c = corpus();
    let v = c["native"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["prime"] == 1171500 && v["variant"] == 4)
        .unwrap();
    let b = support::history::record(&v["block"]).unwrap();
    let h = support::history::history(&v["records"]).unwrap();
    assert!(matches!(
        zone_header::verify_zone_header_components(
            &mut Verifier::default(),
            &h,
            &b,
            &support::profile(v).unwrap(),
            v["now"].as_u64().unwrap(),
            false
        ),
        Err(Error::Invalid("auxiliary quorum signature"))
    ));
}

#[test]
fn negative_or_zero_reward_inputs_fail_with_explicit_domains() {
    use prost::Message;
    let c = corpus();
    let v = c["native"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["prime"] == 1171500 && v["variant"] == 0)
        .unwrap();
    let b = support::history::record(&v["block"]).unwrap();
    assert!(matches!(
        economics::reward_quote(b.header(), Uint::ONE, Uint::ZERO),
        Err(Error::Unsupported("negative reward logarithm"))
    ));
    let mut wire = b.header().wire().clone();
    wire.difficulty = Some(vec![]);
    let h = Header::decode_auxiliary(&wire.encode_to_vec(), PINNED_SCHEDULE).unwrap();
    assert!(matches!(
        economics::reward_quote(&h, Uint::ONE, Uint::ZERO),
        Err(Error::Invalid(_))
    ));
}
#[test]
fn missing_prime_exchange_context_is_unavailable() {
    let c = corpus();
    let v = c["headers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["mode"] == 4 && v["variant"] == 0)
        .unwrap();
    let all = support::history::history(&v["records"]).unwrap();
    let b = support::history::record(&v["block"]).unwrap();
    let parent_id: quai_primitives::Hash = b
        .header()
        .wire()
        .parent_hash
        .as_ref()
        .unwrap()
        .value
        .as_slice()
        .try_into()
        .unwrap();
    let parent = all.get(&parent_id).unwrap();
    let mut missing = History::default();
    for r in v["records"].as_array().unwrap().iter().skip(1) {
        missing
            .insert(support::history::record(r).unwrap())
            .unwrap();
    }
    assert!(matches!(
        economics::expected_base_fee(&missing, &parent, &support::profile(v).unwrap()),
        Err(Error::Unavailable(_))
    ));
}
