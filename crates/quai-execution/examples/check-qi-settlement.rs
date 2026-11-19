use quai_execution::{
    evm::word::{bytes, from_bytes},
    qi::{self, OutPoint, Utxo},
    qi_settlement::{self, Context},
    utxo::{self, UtxoState},
};
use quai_primitives::{decode_hex, encode_hex, transaction::Transaction};
use serde_json::{json, Value};
fn b(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
pub fn verify(corpus: &Value) -> Value {
    verify_observed(corpus, None)
}
pub fn verify_observed(
    corpus: &Value,
    mut observer: Option<
        &mut dyn FnMut(
            &Value,
            &mut quai_execution::evm::state::World,
            &mut quai_execution::utxo::UtxoState,
            u8,
        ),
    >,
) -> Value {
    assert_eq!(
        corpus["reference_revision"],
        quai_primitives::REFERENCE_REVISION
    );
    let mut checks = 0;
    let mut failures = vec![];
    let cases = corpus["vectors"].as_array().unwrap();
    for row in cases {
        let mut state = UtxoState::default();
        for v in row["utxos"].as_array().unwrap() {
            let p = OutPoint {
                hash: b(&v["hash"]).try_into().unwrap(),
                index: v["index"].as_u64().unwrap() as u16,
            };
            let u = Utxo {
                denomination: v["denomination"].as_u64().unwrap() as u8,
                address: qi::UtxoAddress::new(&b(&v["address"])).unwrap(),
                lock: b(&v["lock"]),
            };
            checks += 1;
            if encode_hex(&utxo::utxo_hash(p, &u)) != v["utxo_hash"].as_str().unwrap() {
                failures.push(json!({"id":row["id"],"field":"utxo_hash"}));
            }
            state.insert(p, u).unwrap();
        }
        let mut cmp = |field: &str, actual: Value| {
            checks += 1;
            if actual != row[field] {
                failures.push(
                    json!({"id":row["id"],"field":field,"expected":row[field],"actual":actual}),
                );
            }
        };
        let mut ctx = Context {
            inputs: qi::Context {
                chain_id: vec![5, 57],
                block_height: row["height"].as_u64().unwrap(),
                location: [
                    row["location"][0].as_u64().unwrap() as u8,
                    row["location"][1].as_u64().unwrap() as u8,
                ],
            },
            prime_number: row["prime_number"].as_u64().unwrap(),
            scaling_factor: if row["scaling_factor"] == "-inf" {
                f64::NEG_INFINITY
            } else {
                row["scaling_factor"].as_f64().unwrap()
            },
            base_fee: from_bytes(&b(&row["base_fee"])),
            quai_conversion_reward: from_bytes(&b(&row["quai_reward"])),
            qi_reward: from_bytes(&b(&row["qi_reward"])),
            eligible_slices: b(&row["eligible_slices"]).try_into().unwrap(),
            first_qi: row["first_qi"].as_bool().unwrap(),
            block_gas_limit: row["block_gas_limit"].as_u64().unwrap_or(30000000),
            gas_remaining: row["gas_pool_initial"].as_u64().unwrap(),
            used_gas: row["used_initial"].as_u64().unwrap(),
            region_etx_remaining: row["region_initial"].as_u64().unwrap(),
            prime_etx_remaining: row["prime_initial"].as_u64().unwrap(),
        };
        cmp("before_root", json!(encode_hex(&state.root())));
        cmp(
            "before_accumulator",
            json!(encode_hex(&state.accumulator())),
        );
        let tx = Transaction::decode(&b(&row["transaction"])).unwrap();
        let mut storage_world = quai_execution::evm::state::World::default();
        if let Some(observe) = observer.as_deref_mut() {
            observe(row, &mut storage_world, &mut state, 0);
        }
        let before = state.clone();
        let initial = ctx.clone();
        match qi_settlement::apply_qi(&mut state, &tx, &mut ctx) {
            Ok(out) => {
                cmp("accepted", json!(true));
                cmp("tx_hash", json!(encode_hex(&tx.id(None).unwrap())));
                cmp("gas_pool", json!(ctx.gas_remaining));
                cmp("used_gas", json!(ctx.used_gas));
                cmp("gas_used", json!(out.gas_used));
                cmp("receipt_cumulative", json!(out.receipt.cumulative_gas_used));
                cmp("region_remaining", json!(ctx.region_etx_remaining));
                cmp("prime_remaining", json!(ctx.prime_etx_remaining));
                cmp("fee_qit", json!(out.fee_qit));
                cmp("fee_quai", json!(encode_hex(&bytes(out.fee_quai))));
                cmp("added_qit", json!(out.added_qit));
                cmp("removed_qit", json!(out.removed_qit));
                cmp(
                    "outbound_etxs",
                    json!(out
                        .outbound
                        .iter()
                        .map(|t| encode_hex(&t.encode()))
                        .collect::<Vec<_>>()),
                );
                cmp("created",json!(state.materialize().unwrap().iter().map(|(p,v)|json!({"hash":encode_hex(&p.hash),"index":p.index,"denomination":v.denomination,"address":encode_hex(v.address.as_bytes()),"lock":encode_hex(&v.lock)})).collect::<Vec<_>>()));
                // types.CalculateBlockQiTxGas, the gas that prices the
                // transaction in a block (rows with other address lengths).
                if !row["block_qi_gas"].is_null() {
                    cmp(
                        "block_qi_gas",
                        json!(quai_execution::gas::block_qi_gas(
                            &tx,
                            ctx.scaling_factor,
                            ctx.inputs.location
                        )
                        .ok()),
                    );
                }
                if out.receipt.typed_rlp().is_err() {
                    cmp("receipt_encodable", json!(false));
                }
            }
            Err(e) => {
                cmp("accepted", json!(false));
                if row["accepted"] == true {
                    cmp("native_rejection", json!(e.to_string()));
                }
                assert_eq!(state, before, "rejected Qi changed state");
                assert_eq!(ctx.gas_remaining, initial.gas_remaining);
                assert_eq!(ctx.used_gas, initial.used_gas);
                assert_eq!(ctx.region_etx_remaining, initial.region_etx_remaining);
                assert_eq!(ctx.prime_etx_remaining, initial.prime_etx_remaining);
                assert_eq!(ctx.first_qi, initial.first_qi);
            }
        }
        if let Some(observe) = observer.as_deref_mut() {
            observe(row, &mut storage_world, &mut state, 1);
        }
        cmp("after_root", json!(encode_hex(&state.root())));
        cmp("after_accumulator", json!(encode_hex(&state.accumulator())));
    }
    let out = json!({"passed":failures.is_empty(),"cases":cases.len(),"checks":checks,"failures":failures,"reference_revision":quai_primitives::REFERENCE_REVISION,"full_block_replay":false});
    out
}
fn main() {
    let path = std::env::args().nth(1).expect("qi-settlement corpus");
    let corpus: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let result = verify(&corpus);
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    if result["passed"] != true {
        std::process::exit(1);
    }
}
