#[allow(dead_code)]
#[path = "../tests/vm_support/mod.rs"]
mod support;
use quai_execution::evm::calls::{execute_call_with_env, Environment};
use quai_primitives::{decode_hex, encode_hex, transaction::AccessTuple};
use serde_json::{json, Value};
pub fn verify(corpus: &Value) -> Value {
    assert_eq!(
        corpus["reference_revision"],
        quai_primitives::REFERENCE_REVISION
    );
    let mut checks = 0;
    let mut failures = vec![];
    let cases = corpus["vectors"].as_array().unwrap();
    for row in cases {
        let mut world = support::parse_world(row).unwrap();
        if let Some(rows) = row["lockups_before"].as_array() {
            for v in rows {
                let address = |name: &str| {
                    decode_hex(v[name].as_str().unwrap())
                        .unwrap()
                        .try_into()
                        .unwrap()
                };
                let key = quai_execution::lockup::Key {
                    owner: address("owner"),
                    beneficiary: address("beneficiary"),
                    lock: v["lock"].as_u64().unwrap() as u8,
                    epoch: v["epoch"].as_u64().unwrap() as u32,
                };
                world
                    .set_lockup(
                        key,
                        Some(quai_execution::lockup::Lockup {
                            balance: quai_execution::evm::word::from_bytes(
                                &decode_hex(v["balance"].as_str().unwrap()).unwrap(),
                            ),
                            unlock_height: v["unlock_height"].as_u64().unwrap() as u32,
                            elements: v["elements"].as_u64().unwrap() as u16,
                            delegate: address("delegate"),
                        }),
                    )
                    .unwrap();
            }
        }
        let context = support::parse_context(row).unwrap();
        let access = row["access_list"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| AccessTuple {
                address: decode_hex(a["address"].as_str().unwrap())
                    .unwrap()
                    .try_into()
                    .unwrap(),
                keys: a["keys"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| decode_hex(v.as_str().unwrap()).unwrap().try_into().unwrap())
                    .collect(),
            })
            .collect::<Vec<_>>();
        let mut compare = |name: &str, actual: Value| {
            checks += 1;
            if actual != row[name] {
                failures.push(json!({"id":row["id"],"field":name,"actual":actual,"expected":row[name],"go_error":row["reference_error"]}));
            }
        };
        compare("before_root", json!(encode_hex(&world.root().unwrap())));
        let input = decode_hex(row["input"].as_str().unwrap()).unwrap();
        let environment = Environment {
            prime_number: row["context"]["prime_number"].as_u64(),
            block_hashes: row["block_hashes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| {
                    (
                        v["number"].as_u64().unwrap(),
                        decode_hex(v["hash"].as_str().unwrap())
                            .unwrap()
                            .try_into()
                            .unwrap(),
                    )
                })
                .collect(),
            transaction_hash: row["transaction_hash"]
                .as_str()
                .map(|v| decode_hex(v).unwrap().try_into().unwrap()),
            eligible_slices: row["eligible_slices"]
                .as_str()
                .map(|v| decode_hex(v).unwrap().try_into().unwrap()),
            ..Default::default()
        };
        match execute_call_with_env(
            &mut world,
            &context,
            &access,
            &input,
            row["gas"].as_u64().unwrap(),
            &environment,
        ) {
            Err(e) => {
                failures.push(json!({"id":row["id"],"capability_error":e.to_string()}));
            }
            Ok(out) => {
                compare("output", json!(encode_hex(&out.output)));
                compare("error", json!(out.error.map(|e| e.class())));
                compare("reverted", json!(out.reverted));
                compare("gas_remaining", json!(out.gas_remaining));
                compare("state_gas", json!(out.state_gas));
                compare("refund", json!(out.refund));
                compare("after_root", json!(encode_hex(&world.root().unwrap())));
                if row["lockups_after"].is_array() {
                    compare("lockups_after",json!(world.lockups().unwrap().iter().map(|(k,v)|json!({"owner":encode_hex(&k.owner),"beneficiary":encode_hex(&k.beneficiary),"lock":k.lock,"epoch":k.epoch,"balance":encode_hex(&quai_execution::evm::word::bytes(v.balance)),"unlock_height":v.unlock_height,"elements":v.elements,"delegate":encode_hex(&v.delegate),"commitment":encode_hex(&v.commitment(*k))})).collect::<Vec<_>>()));
                }
                compare("trace_hash", json!(encode_hex(&out.trace_hash)));
                compare("trace_steps", json!(out.trace_steps));
                compare(
                    "outbound_etxs",
                    json!(out
                        .outbound_etxs
                        .iter()
                        .map(|tx| encode_hex(&tx.encode()))
                        .collect::<Vec<_>>()),
                );
                compare("logs",json!(out.logs.iter().map(|l|json!({"address":encode_hex(&l.address),"topics":l.topics.iter().map(|h|encode_hex(h)).collect::<Vec<_>>(),"data":encode_hex(&l.data)})).collect::<Vec<_>>()));
            }
        }
    }
    let result = json!({"passed":failures.is_empty(),"cases":cases.len(),"checks":checks,"failures":failures,"reference_revision":quai_primitives::REFERENCE_REVISION,"full_block_replay":false,"network_ready":false});
    result
}
fn main() {
    let path = std::env::args().nth(1).expect("stateful corpus");
    let corpus: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let result = verify(&corpus);
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    if result["passed"] != true {
        std::process::exit(1);
    }
}
