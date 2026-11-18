#[allow(dead_code)]
#[path = "support/mod.rs"]
mod support;
use quai_consensus::{
    pow::Algorithm,
    rules::{self, EntropyInput, Uint},
    verifier::Verifier,
    Error,
};
#[test]
fn frozen_difficulty_entropy_and_fork_rules_match() {
    let corpus =
        serde_json::from_str(include_str!("../../../fixtures/protocol/consensus.json")).unwrap();
    let result = support::verify_rules(&corpus).unwrap();
    assert_eq!(result["mismatches"], 0, "{result}");
    for order in result["hierarchy_orders"].as_array().unwrap() {
        assert!(order.as_u64().unwrap() > 0);
    }
}
#[test]
fn malformed_or_out_of_domain_rules_cannot_produce_validation() {
    assert!(rules::adjust_difficulty(Uint::ZERO, Uint::ONE, 1, 0, 5).is_err());
    assert!(rules::adjust_difficulty(Uint::ONE, Uint::ONE, 1, 0, 0).is_err());
    assert!(rules::share_target(Uint::ZERO, 1).is_err());
    assert!(rules::share_target(Uint::ONE, 0).is_err());
    assert!(matches!(
        rules::share_target(Uint::ONE, 256),
        Err(Error::Unsupported(_))
    ));
    assert!(rules::intrinsic([0; 32]).is_err());
    assert!(matches!(
        rules::adjust_difficulty(Uint::MAX, Uint::ONE, 1, 1, 5),
        Err(Error::Unsupported(_))
    ));
}
#[test]
fn hash_zero_one_and_overflow_boundaries_are_explicit() {
    let mut one = [0; 32];
    one[31] = 1;
    assert_eq!(
        rules::intrinsic(one).unwrap(),
        Uint::from_u64(256).shl_vartime(64)
    );
    let input = EntropyInput {
        hash: one,
        difficulty: Uint::from_u64(2),
        expansion: 0,
        parent_entropy: [Uint::MAX; 3],
        parent_delta: [Uint::MAX; 3],
        parent_uncled_delta: [Uint::MAX; 3],
        uncled_entropy: Uint::MAX,
        postfork_share_count: None,
        node_context: 2,
    };
    assert!(matches!(rules::entropy(&input), Err(Error::Unsupported(_))));
}
#[test]
fn invalid_heights_are_rejected_before_cache_allocation() {
    assert!(Algorithm::Progpow.sizes(1_301_101).is_err());
    assert!(Algorithm::Kawpow.sizes(14_000_001).is_err());
    assert!(Algorithm::Kawpow.sizes(u64::MAX).is_err());
    assert!(Algorithm::Progpow.sizes(1_301_100).is_ok());
    assert!(Algorithm::Kawpow.sizes(14_000_000).is_ok());
}
#[test]
fn incomplete_block_admission_never_returns_a_validated_result() {
    let mut verifier = Verifier::default();
    for data in [vec![], vec![0], vec![255; 1024]] {
        assert!(matches!(
            verifier.validate_block(&data, quai_primitives::work::Context::Zone),
            Err(Error::Unsupported(_))
        ));
    }
}

#[test]
fn donor_inputs_have_exact_length_and_use_no_unbounded_memory() {
    for n in [0, 1, 79, 81, 1000] {
        assert!(quai_consensus::donor::sha256d(&vec![0; n]).is_err());
        assert!(quai_consensus::donor::scrypt(&vec![0; n]).is_err());
    }
}
