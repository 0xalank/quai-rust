#[allow(dead_code)]
#[path = "../examples/check-activation.rs"]
mod checker;
use quai_consensus::{
    private_canonical::{BlockBundle, PrivateCanonical},
    verifier::Verifier,
    zone_admission,
    zone_execution::{self, Profile},
    Error,
};
use quai_primitives::{decode_hex, transaction::keccak};
use serde_json::Value;
fn corpus() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/activation.json")).unwrap()
}
fn directory(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "quai-activation-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&p).unwrap();
    p
}
fn first(v: &Value) -> quai_consensus::history::Record {
    quai_consensus::history::Record::decode(
        &decode_hex(v["vectors"][0]["work_object"].as_str().unwrap()).unwrap(),
    )
    .unwrap()
}
#[test]
fn sealed_activated_mixed_ledger_append_execution_and_persistence_match_go() {
    let p = directory("replay");
    let report = checker::verify(&corpus(), &p);
    assert_eq!(report["passed"], true);
    assert_eq!(report["valid_work_rejections"], 33);
    assert_eq!(report["rejected_attempts"], 66);
    println!("ACTIVATION_CHECKS {report}");
    std::fs::remove_dir_all(p).unwrap();
}
#[test]
fn local_append_and_execution_must_bind_the_same_profile() {
    let v = corpus();
    let (mut state, history, index, past) = checker::inputs(&v);
    let before = state.clone();
    let mut placement = zone_admission::profile().unwrap();
    placement.genesis.insert(keccak(b"foreign-genesis"));
    let result = zone_execution::inspect_local_components(
        &mut Verifier::default(),
        &mut state,
        &history,
        &past,
        &index,
        &first(&v),
        &Profile::reviewed_admission().unwrap(),
        &placement,
        &[],
        v["now"].as_u64().unwrap(),
    );
    assert!(matches!(
        result,
        Err(Error::Invalid("append/execution profile binding"))
    ));
    assert_eq!(state, before);
}
#[test]
fn absent_activation_redemption_history_is_unavailable_and_atomic() {
    let v = corpus();
    let (mut state, history, index, _) = checker::inputs(&v);
    let before = state.clone();
    let result = zone_execution::inspect_local_components(
        &mut Verifier::default(),
        &mut state,
        &history,
        &checker::Past::default(),
        &index,
        &first(&v),
        &Profile::reviewed_admission().unwrap(),
        &zone_admission::profile().unwrap(),
        &[],
        v["now"].as_u64().unwrap(),
    );
    assert!(
        matches!(result, Err(Error::Unavailable(_))),
        "{:?}",
        result.err()
    );
    assert_eq!(state, before);
}
#[test]
fn supplied_activation_checkpoint_cannot_acquire_canonical_authority() {
    let v = corpus();
    let p = directory("no-authority");
    let mut chain = PrivateCanonical::open(&p, v["now"].as_u64().unwrap()).unwrap();
    let before = chain.view().unwrap();
    let head = before.block();
    drop(before);
    let first = first(&v);
    let b = BlockBundle {
        zone: first.bytes().to_vec(),
        contexts: vec![],
    };
    assert!(chain.import(b, v["now"].as_u64().unwrap()).is_err());
    assert_eq!(chain.view().unwrap().block(), head);
    assert_eq!(chain.view().unwrap().height(), 0);
    // Zone 259,201 does not extend the bare genesis.
    let refused = quai_consensus::block::validate_on_genesis(
        &mut Verifier::default(),
        quai_consensus::chain::Chain::private_hierarchy().unwrap(),
        [None, None, Some(std::sync::Arc::new(first.clone()))],
        1_800_000_000,
    )
    .err()
    .unwrap();
    assert_eq!(refused.check, "parent hash");
    drop(chain);
    std::fs::remove_dir_all(p).unwrap();
}

#[test]
fn prepared_hierarchy_cannot_execute_another_block_or_chain() {
    let v = corpus();
    let (mut state, history, index, past) = checker::inputs(&v);
    let before = state.clone();
    let graph = checker::graph(&v, &index);
    let mut verifier = Verifier::default();
    let prepared = graph
        .prepare_activated_local_components(&mut verifier, &first(&v), v["now"].as_u64().unwrap())
        .unwrap();
    let wrong = quai_consensus::history::Record::decode(
        &decode_hex(v["negatives"][0]["work_object"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let result = zone_execution::execute_prepared_components(
        &mut verifier,
        &mut state,
        &history,
        &past,
        &wrong,
        &Profile::reviewed_admission().unwrap(),
        &prepared,
    );
    assert!(matches!(
        result,
        Err(Error::Invalid("hierarchy/execution block binding"))
    ));
    assert_eq!(state, before);
    let placement = zone_admission::profile().unwrap();
    let foreign = Profile::official(
        quai_consensus::rules::Uint::from_u64(9),
        [0, 0],
        placement.genesis,
    )
    .unwrap();
    let result = zone_execution::execute_prepared_components(
        &mut verifier,
        &mut state,
        &history,
        &past,
        &first(&v),
        &foreign,
        &prepared,
    );
    assert!(matches!(
        result,
        Err(Error::Invalid("hierarchy/execution profile binding"))
    ));
    assert_eq!(state, before);
    let placement = zone_admission::profile().unwrap();
    let foreign_policy = Profile::official(
        quai_consensus::rules::Uint::from_u64(1337),
        [0, 0],
        placement.genesis,
    )
    .unwrap();
    let result = zone_execution::execute_prepared_components(
        &mut verifier,
        &mut state,
        &history,
        &past,
        &first(&v),
        &foreign_policy,
        &prepared,
    );
    assert!(matches!(
        result,
        Err(Error::Invalid("hierarchy/execution profile binding"))
    ));
    assert_eq!(state, before);
}
