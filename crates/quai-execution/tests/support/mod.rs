use quai_execution::{
    etx::EtxState,
    gas,
    qi::{self, OutPoint, Utxo},
};
use quai_primitives::{
    decode_hex, encode_hex,
    transaction::{AccessTuple, Transaction},
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
fn raw(v: &Value, key: &str) -> Result<Vec<u8>, String> {
    decode_hex(v[key].as_str().ok_or_else(|| format!("missing {key}"))?)
}
fn tx(v: &Value, key: &str) -> Result<Transaction, String> {
    Transaction::decode(&raw(v, key)?)
}
fn number(v: &Value, key: &str) -> Result<u64, String> {
    v[key].as_u64().ok_or_else(|| format!("invalid {key}"))
}
fn array<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>, String> {
    v[key].as_array().ok_or_else(|| format!("missing {key}"))
}
pub fn verify(corpus: &Value) -> Result<Value, String> {
    if corpus["reference_revision"] != quai_primitives::REFERENCE_REVISION
        || corpus["kind"] != "native-foundation"
    {
        return Err("unrecognized execution corpus".into());
    }
    let vectors = &corpus["execution"];
    let mut checks = 0;
    let mut failures = Vec::new();
    let mut compare = |id: &Value, field: &str, actual: Value, expected: &Value| {
        checks += 1;
        if &actual != expected {
            failures.push(json!({"id":id,"field":field,"actual":actual,"expected":expected}));
        }
    };
    for v in array(vectors, "intrinsic_gas")? {
        let access = array(v, "keys_per_address")?
            .iter()
            .map(|n| {
                Ok(AccessTuple {
                    address: [0; 20],
                    keys: vec![
                        [0; 32];
                        usize::try_from(n.as_u64().ok_or("bad key count")?)
                            .map_err(|_| "key count overflow")?
                    ],
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let actual = gas::intrinsic_gas(
            &raw(v, "data")?,
            &access,
            v["creation"].as_bool().ok_or("bad creation flag")?,
        )?;
        compare(&v["id"], "intrinsic gas", json!(actual), &v["gas"]);
    }
    for v in array(vectors, "qi_inputs")? {
        let transaction = tx(v, "transaction")?;
        let mut view = BTreeMap::new();
        for u in array(v, "utxos")? {
            view.insert(
                OutPoint {
                    hash: raw(u, "hash")?.try_into().map_err(|_| "bad UTXO hash")?,
                    index: u16::try_from(number(u, "index")?).map_err(|_| "bad UTXO index")?,
                },
                Utxo {
                    denomination: u8::try_from(number(u, "denomination")?)
                        .map_err(|_| "bad denomination")?,
                    address: quai_execution::qi::UtxoAddress::new(&raw(u, "address")?)
                        .ok_or("bad UTXO address")?,
                    lock: raw(u, "lock")?,
                },
            );
        }
        let location = array(v, "location")?
            .iter()
            .map(|n| {
                u8::try_from(n.as_u64().ok_or("bad location")?).map_err(|_| "bad location".into())
            })
            .collect::<Result<Vec<_>, String>>()?
            .try_into()
            .map_err(|_| "bad location length")?;
        let context = qi::Context {
            chain_id: raw(v, "chain_id")?,
            block_height: number(v, "height")?,
            location,
        };
        let total = qi::check_input_values(&transaction, &view, &context)
            .map(|n| json!(n.to_string()))
            .unwrap_or(Value::Null);
        compare(&v["id"], "input helper result", total, &v["input_total"]);
        compare(
            &v["id"],
            "unique spend set",
            json!(qi::unique_inputs(&transaction).is_ok()),
            &v["unique_inputs"],
        );
        compare(
            &v["id"],
            "Qi intrinsic gas",
            json!(gas::intrinsic_qi_gas(
                &transaction,
                v["scaling_factor"].as_f64().ok_or("bad scaling factor")?
            )?),
            &v["intrinsic_gas"],
        );
    }
    let mut steps = 0;
    for trace in array(vectors, "etx_traces")? {
        let mut state = EtxState::default();
        let mut checkpoint = None;
        for step in array(trace, "steps")? {
            let before = state.clone();
            match step["op"].as_str().ok_or("missing ETX operation")? {
                "initial" => {}
                "auxiliary" => state.set_auxiliary(raw(step, "key")?, raw(step, "value")?)?,
                "push" => {
                    let input = array(step, "transactions")?
                        .iter()
                        .map(|v| Transaction::decode(&decode_hex(v.as_str().ok_or("bad ETX")?)?))
                        .collect::<Result<Vec<_>, String>>()?;
                    state.push(&input)?;
                }
                "pop" => {
                    let actual = state
                        .pop()?
                        .map(|t| json!(encode_hex(&t.encode())))
                        .unwrap_or(Value::Null);
                    compare(&trace["id"], "popped ETX", actual, &step["transaction"]);
                }
                "read" => {
                    let actual = state
                        .read(number(step, "index")?)?
                        .map(|t| json!(encode_hex(&t.encode())))
                        .unwrap_or(Value::Null);
                    compare(&trace["id"], "read ETX", actual, &step["transaction"]);
                    compare(
                        &trace["id"],
                        "read preserves state",
                        json!(state == before),
                        &json!(true),
                    );
                }
                "checkpoint" => checkpoint = Some(state.clone()),
                "revert" => state = checkpoint.as_ref().ok_or("missing checkpoint")?.clone(),
                _ => return Err("unknown ETX operation".into()),
            }
            compare(
                &trace["id"],
                "ETX secure-trie root",
                json!(encode_hex(&state.root()?)),
                &step["root"],
            );
            compare(
                &trace["id"],
                "oldest index",
                json!(state.oldest()),
                &step["oldest"],
            );
            compare(
                &trace["id"],
                "newest index",
                json!(state.newest()),
                &step["newest"],
            );
            compare(
                &trace["id"],
                "queue length",
                json!(state.len()),
                &step["length"],
            );
            steps += 1;
        }
    }
    Ok(
        json!({"passed":failures.is_empty(),"checks":checks,"intrinsic_gas_cases":array(vectors,"intrinsic_gas")?.len(),"qi_input_cases":array(vectors,"qi_inputs")?.len(),"live_etx_traces":array(vectors,"etx_traces")?.len(),"live_etx_steps":steps,"failures":failures,"full_evm_execution":false,"full_qi_state_transition":false,"inbound_etx_eligibility":false,"durable_state":false,"scope":"Bounded intrinsic-gas, Qi input helper/spend uniqueness, and live ETX queue/root semantics only."}),
    )
}
