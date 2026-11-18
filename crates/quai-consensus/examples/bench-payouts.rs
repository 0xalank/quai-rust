//! Fixed offline repeated reward-distribution workload. No network or disk I/O in timing.
use quai_consensus::{
    history::{History, Record},
    payouts, share_rules,
    verifier::Verifier,
};
use quai_primitives::{decode_hex, encode_hex, work::transactions_root};
use serde_json::{json, Value};
use std::{hint::black_box, time::Instant};
fn main() {
    let path = std::env::args().nth(1).expect("frozen zone-block corpus");
    let corpus: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        corpus["reference_revision"],
        quai_primitives::REFERENCE_REVISION
    );
    let decode = |v: &Value| Record::decode(&decode_hex(v.as_str().unwrap()).unwrap()).unwrap();
    let genesis = decode(&corpus["genesis"]);
    let exchange = share_rules::uint(
        genesis
            .object()
            .header()
            .wire()
            .exchange_rate
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    let mut history = History::default();
    history.insert(genesis).unwrap();
    let rows = corpus["vectors"].as_array().unwrap();
    for row in &rows[..rows.len() - 1] {
        history.insert(decode(&row["work_object"])).unwrap();
    }
    let block = decode(&rows.last().unwrap()["work_object"]);
    let mut verifier = Verifier::default();
    verifier.verify_header_proof(block.header()).unwrap();
    let warm =
        payouts::native_components(&mut verifier, &history, &block, [0, 0], exchange, 0).unwrap();
    let root = transactions_root(&warm.outbound).unwrap();
    assert_eq!(
        root.as_slice(),
        block
            .object()
            .header()
            .wire()
            .outbound_etx_hash
            .as_ref()
            .unwrap()
            .value
    );
    assert!(!warm.fully_validated);
    let iterations = 400;
    let start = Instant::now();
    let mut count = 0;
    for _ in 0..iterations {
        let value = payouts::native_components(
            &mut verifier,
            black_box(&history),
            black_box(&block),
            [0, 0],
            exchange,
            0,
        )
        .unwrap();
        count += black_box(value.outbound.len());
        black_box(value);
    }
    let elapsed = start.elapsed().as_nanos();
    let checked =
        payouts::native_components(&mut verifier, &history, &block, [0, 0], exchange, 0).unwrap();
    assert_eq!(transactions_root(&checked.outbound).unwrap(), root);
    assert_eq!(count, iterations * warm.outbound.len());
    println!(
        "{}",
        json!({"iterations":iterations,"reward_count":warm.outbound.len(),"work_requests_per_operation":warm.work_evaluations,"outbound_root":encode_hex(&root),"total_rewards_observed":count,"ns_per_operation":elapsed / iterations as u128,"full_consensus":false,"scope":"Repeated distribution on identical verified history and warm epoch; parsing, epoch construction and root checking excluded from timer."})
    );
}
