use quai_consensus::{
    economics,
    history::{History, Record},
    payouts,
    rules::Uint,
    share_rules, Error,
};
use quai_primitives::{decode_hex, encode_hex};
use serde_json::{json, Value};
fn bytes(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
fn number(v: &Value) -> Uint {
    share_rules::uint(&bytes(v)).unwrap()
}
pub fn verify(corpus: &Value) -> Value {
    assert_eq!(
        corpus["reference_revision"],
        quai_primitives::REFERENCE_REVISION
    );
    let rows = corpus["vectors"].as_array().unwrap();
    let mut checks = 0;
    let mut failures = vec![];
    let mut evaluations = 0;
    for row in rows {
        let mut history = History::default();
        for r in row["records"].as_array().unwrap() {
            history.insert(Record::decode(&bytes(r)).unwrap()).unwrap();
        }
        let block = Record::decode(&bytes(&row["block"])).unwrap();
        let b = block.object().header().wire();
        let exchange = share_rules::uint(b.exchange_rate.as_deref().unwrap()).unwrap();
        let avg = share_rules::uint(b.avg_tx_fees.as_deref().unwrap()).unwrap();
        let total = share_rules::uint(b.total_fees.as_deref().unwrap()).unwrap();
        let parent_hash = block
            .header()
            .wire()
            .parent_hash
            .as_ref()
            .unwrap()
            .value
            .as_slice()
            .try_into()
            .unwrap();
        let parent = history.get(&parent_hash).unwrap();
        let quote = economics::reward_quote(block.header(), exchange, avg).unwrap();
        let q = number(&row["quai_fees"]);
        let qi = number(&row["qi_fees"]);
        let expected_avg = payouts::average_fees(&block, &parent, false, &quote, q, qi).unwrap();
        let expected_total = q.wrapping_add(&payouts::convert_qi(&quote, qi).unwrap());
        let accepted = expected_avg == avg && expected_total == total;
        checks += 1;
        if accepted != (row["accepted"] == true) {
            failures.push(json!({"id":row["id"],"field":"accepted","expected":row["accepted"],"actual":accepted}));
        }
        if accepted {
            let location = [
                row["location"][0].as_u64().unwrap() as u8,
                row["location"][1].as_u64().unwrap() as u8,
            ];
            let result = payouts::project(
                &history,
                &block,
                location,
                exchange,
                row["first_index"].as_u64().unwrap() as usize,
                |h| {
                    h.wire()
                        .mix_hash
                        .as_ref()
                        .unwrap()
                        .value
                        .as_slice()
                        .try_into()
                        .map_err(|_| Error::Invalid("controlled work hash"))
                },
            );
            match result {
                Ok(r) => {
                    evaluations += r.work_evaluations;
                    let actual = json!(r
                        .outbound
                        .iter()
                        .map(|t| encode_hex(&t.encode()))
                        .collect::<Vec<_>>());
                    checks += 2;
                    if actual != row["outbound"] {
                        failures.push(json!({"id":row["id"],"field":"outbound","actual":actual,"expected":row["outbound"]}));
                    }
                    if r.fully_validated {
                        failures
                            .push(json!({"id":row["id"],"field":"projection_is_not_admission"}));
                    }
                }
                Err(e) => failures.push(json!({"id":row["id"],"error":e.to_string()})),
            }
        }
    }
    json!({"passed":failures.is_empty(),"cases":rows.len(),"checks":checks,"work_evaluations":evaluations,"failures":failures,"full_consensus":false,"scope":"Exact official fee/payout branch; controlled work arithmetic and projection boundary only."})
}
fn main() {
    let p = std::env::args().nth(1).expect("payout corpus");
    let v: Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
    let r = verify(&v);
    println!("{}", serde_json::to_string_pretty(&r).unwrap());
    if r["passed"] != true {
        std::process::exit(1);
    }
}
