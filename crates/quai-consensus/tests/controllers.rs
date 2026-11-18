mod controller_support;
use quai_consensus::{controllers, rules::Uint, Error};
#[test]
fn official_prime_controller_arithmetic_matches_all_frozen_go_cases() {
    let v =
        serde_json::from_str(include_str!("../../../fixtures/protocol/controllers.json")).unwrap();
    let report = controller_support::verify(&v);
    assert_eq!(report["go_comparisons"], 3200);
    println!("{report}");
}
#[test]
fn controller_history_is_required_after_activation_and_zero_is_invalid() {
    assert_eq!(
        controllers::k_quai_discount(
            quai_primitives::spec::FORKS.controller_kick_in,
            Uint::ZERO,
            None,
            Uint::ZERO
        )
        .unwrap(),
        Uint::from_u64(100)
    );
    assert!(matches!(
        controllers::k_quai_discount(
            quai_primitives::spec::FORKS.controller_kick_in + 1,
            Uint::ZERO,
            None,
            Uint::ONE
        ),
        Err(Error::Unavailable(_))
    ));
    assert!(matches!(
        controllers::k_quai_discount(
            quai_primitives::spec::FORKS.controller_kick_in + 1,
            Uint::ZERO,
            Some(Uint::ZERO),
            Uint::ONE
        ),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn feedback_rejects_undefined_math_and_schedule_shortcuts_preserve_history() {
    let zero = [Uint::ZERO; controllers::WINDOW as usize];
    let one = [Uint::ONE; controllers::WINDOW as usize];
    for choices in [&zero, &one] {
        assert!(matches!(
            controllers::exchange_rate(266_000, Uint::ONE, Uint::ONE, choices),
            Err(Error::Invalid(_))
        ));
    }
    assert!(matches!(
        controllers::calculate_k_quai(266_000, Uint::ONE, Uint::ZERO, Uint::ONE),
        Err(Error::Invalid(_))
    ));
    // Official early/hold/freeze branches do not inspect a feedback window.
    assert_eq!(
        controllers::exchange_rate(265_999, Uint::ZERO, Uint::ZERO, &zero).unwrap(),
        Uint::from_u128(quai_consensus::economics::INITIAL_EXCHANGE_RATE)
    );
    for number in [752_001, 1_171_501, 1_755_001, 2_237_000] {
        assert_eq!(
            controllers::exchange_rate(number, Uint::from_u64(37), Uint::ZERO, &zero).unwrap(),
            Uint::from_u64(37)
        );
    }
}
