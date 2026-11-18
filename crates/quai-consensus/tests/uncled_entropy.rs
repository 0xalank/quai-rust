//! `validation::uncled_log_entropy` is core/poem.go UncledLogEntropy. The
//! pipeline uses it after KawPow; the rule itself has no fork condition, so the
//! Go values recorded for the pre-KawPow fixture chain (canonical.json, both
//! branches) check it, and the pre-KawPow share entropy must agree with it.
use quai_consensus::{
    history::{History, Record},
    prefork_shares, validation,
    verifier::Verifier,
};
use quai_primitives::decode_hex;

#[test]
fn uncled_log_entropy_equals_go_on_the_fixture_chain() {
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json")).unwrap();
    let mut verifier = Verifier::default();
    let mut checked = 0;
    for branch in corpus["branches"].as_array().unwrap() {
        let mut history = History::default();
        history
            .insert(
                Record::decode(&decode_hex(branch["genesis"].as_str().unwrap()).unwrap()).unwrap(),
            )
            .unwrap();
        for row in branch["vectors"].as_array().unwrap() {
            let block =
                Record::decode(&decode_hex(row["work_object"].as_str().unwrap()).unwrap()).unwrap();
            let expected = row["uncled_entropy"].as_str().unwrap();
            let actual = validation::uncled_log_entropy(&mut verifier, &block).unwrap();
            assert_eq!(
                quai_consensus::block::uint(actual),
                expected,
                "{}",
                row["id"]
            );
            let shares =
                prefork_shares::native_components(&mut verifier, &history, &block).unwrap();
            assert_eq!(shares.uncled, actual, "{}", row["id"]);
            if expected != "0" {
                checked += 1;
            }
            history.insert(block).unwrap();
        }
    }
    assert_eq!(checked, 12);
}
