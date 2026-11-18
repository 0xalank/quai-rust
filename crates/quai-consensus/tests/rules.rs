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
/// `Verifier::validate_block` against an explicit parent context runs the
/// replay pipeline: on the Go fixture chain (canonical.json branch 0, chain
/// 1337) every height validates, executes on its parent state, and confirms
/// and rolls up exactly the ETXs Go recorded; a block without its parent fails.
#[test]
fn validate_block_runs_the_replay_pipeline_on_the_go_fixture_chain() {
    use quai_consensus::{
        block::{self, Ancestry, NoPast, ParentState},
        chain::Chain,
        history::Record,
        zone_execution::State,
    };
    use quai_primitives::{decode_hex, encode_hex, transaction::Transaction};
    use std::sync::Arc;
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json")).unwrap();
    let now = corpus["now"].as_u64().unwrap();
    let chain = Chain::private_hierarchy().unwrap();
    assert_eq!(
        encode_hex(&chain.genesis_hash()),
        corpus["branches"][0]["genesis_hash"].as_str().unwrap()
    );
    let profile = chain.execution_profile().unwrap();
    let mut ancestry = Ancestry::new(chain.clone()).unwrap();
    let mut state = State::default();
    let mut verifier = Verifier::default();
    let txs = |v: &[Transaction]| -> serde_json::Value {
        v.iter().map(|t| encode_hex(&t.encode())).collect()
    };
    let rows = corpus["branches"][0]["vectors"].as_array().unwrap();
    let records = |row: &serde_json::Value| {
        let mut out: [Option<Arc<Record>>; 3] = [None, None, None];
        for c in row["contexts"].as_array().unwrap() {
            let raw = decode_hex(c["work_object"].as_str().unwrap()).unwrap();
            out[c["context"].as_u64().unwrap() as usize] =
                Some(Arc::new(Record::decode(&raw).unwrap()));
        }
        out
    };
    // A block that skips its parent fails its parent link.
    let skipped = verifier
        .validate_block::<NoPast>(&ancestry.parent(now), records(&rows[1]), None)
        .err()
        .unwrap();
    assert_eq!(skipped.check, "parent hash");
    for (i, row) in rows.iter().enumerate() {
        let mut parent_state = ParentState {
            state: &mut state,
            past: &NoPast,
            profile: &profile,
        };
        let validated = verifier
            .validate_block(&ancestry.parent(now), records(row), Some(&mut parent_state))
            .unwrap_or_else(|f| panic!("height {}: {f}", i + 1));
        assert_eq!(
            validated.checked.order as u64,
            row["order"].as_u64().unwrap()
        );
        assert_eq!(
            txs(&validated.checked.cross),
            row["cross_rollup"],
            "height {}",
            i + 1
        );
        let executed = validated.executed.as_ref().unwrap();
        assert_eq!(
            txs(&executed.confirmed),
            row["confirmed"],
            "height {}",
            i + 1
        );
        assert_eq!(
            encode_hex(&state.world.root().unwrap()),
            row["evm_root"].as_str().unwrap(),
            "height {}",
            i + 1
        );
        ancestry.commit(validated).unwrap();
    }
    assert_eq!(ancestry.heads()[2].1, rows.len() as u64);
    // Go-rejected variants of height 7 fail against the same parent context.
    let mut ancestry = Ancestry::new(chain).unwrap();
    for row in &rows[..6] {
        let v = verifier
            .validate_block::<NoPast>(&ancestry.parent(now), records(row), None)
            .unwrap();
        ancestry.commit(v).unwrap();
    }
    for bad in corpus["branches"][0]["negatives"].as_array().unwrap() {
        if bad["at_height"] == 7 {
            assert!(block::check(&mut verifier, &ancestry.parent(now), records(bad)).is_err());
        }
    }
}

#[test]
fn donor_inputs_have_exact_length_and_use_no_unbounded_memory() {
    for n in [0, 1, 79, 81, 1000] {
        assert!(quai_consensus::donor::sha256d(&vec![0; n]).is_err());
        assert!(quai_consensus::donor::scrypt(&vec![0; n]).is_err());
    }
}
