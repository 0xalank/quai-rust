#[allow(dead_code)]
#[path = "../tests/vm_support/mod.rs"]
mod support;
use quai_execution::{
    evm::{calls::Environment, word::bytes},
    settlement::{self, BlockContext},
};
use quai_primitives::{decode_hex, encode_hex, transaction::Transaction};
use serde_json::{json, Value};
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
        let mut world = support::parse_world(row).unwrap();
        let mut storage_utxos = quai_execution::utxo::UtxoState::default();
        if let Some(observe) = observer.as_deref_mut() {
            observe(row, &mut world, &mut storage_utxos, 0);
        }
        let context = support::parse_context(row).unwrap();
        let tx = Transaction::decode(&decode_hex(row["transaction"].as_str().unwrap()).unwrap())
            .unwrap();
        let mut block = BlockContext {
            vm: context,
            environment: Environment {
                delete_empty: row["finalize_empty"].as_bool().unwrap_or(false),
                prime_number: row["context"]["prime_number"].as_u64(),
                ..Default::default()
            },
            gas_remaining: row["initial_gas_pool"].as_u64().unwrap(),
            cumulative_gas: row["initial_cumulative_gas"].as_u64().unwrap(),
            state_used: row["initial_state_used"].as_u64().unwrap(),
        };
        let mut cmp = |name: &str, actual: Value| {
            checks += 1;
            if actual != row[name] {
                failures.push(
                    json!({"id":row["id"],"field":name,"expected":row[name],"actual":actual}),
                );
            }
        };
        cmp("before_root", json!(encode_hex(&world.root().unwrap())));
        match settlement::apply_quai(&mut world, &tx, &mut block) {
            Err(settlement::Error::Invalid(name)) => {
                cmp("consensus_error", json!(name));
            }
            Err(settlement::Error::Unavailable(e)) => panic!("fixture state unavailable: {e}"),
            Err(settlement::Error::Unsupported(e)) => {
                cmp("unsupported", json!(e.to_string()));
            }
            Ok(out) => {
                cmp("consensus_error", Value::Null);
                cmp("tx_hash", json!(encode_hex(&out.id)));
                cmp("gas_used", json!(out.gas_used));
                cmp("state_used", json!(out.state_used));
                cmp("fees", json!(encode_hex(&bytes(out.fee))));
                cmp("output", json!(encode_hex(&out.output)));
                cmp("error", json!(out.error.map(|e| e.class())));
                cmp("reverted", json!(out.reverted));
                cmp("cumulative_gas", json!(block.cumulative_gas));
                cmp("cumulative_state", json!(block.state_used));
                cmp(
                    "contract_address",
                    json!(out.contract_address.map(|a| encode_hex(&a))),
                );
                if row["receipt_rlp"].is_string() {
                    cmp(
                        "receipt_rlp",
                        json!(encode_hex(&out.receipt.typed_rlp().unwrap())),
                    );
                }
                cmp("logs",json!(out.receipt.logs.iter().map(|l|json!({"address":encode_hex(&l.address),"topics":l.topics.iter().map(|h|encode_hex(h)).collect::<Vec<_>>(),"data":encode_hex(&l.data)})).collect::<Vec<_>>()));
            }
        }
        if let Some(observe) = observer.as_deref_mut() {
            observe(row, &mut world, &mut storage_utxos, 1);
        }
        cmp("after_root", json!(encode_hex(&world.root().unwrap())));
        cmp("gas_pool", json!(block.gas_remaining));
    }
    let value = json!({"passed":failures.is_empty(),"checks":checks,"cases":cases.len(),"failures":failures,"reference_revision":quai_primitives::REFERENCE_REVISION,"full_block_replay":false});
    value
}
fn main() {
    let path = std::env::args().nth(1).expect("settlement corpus");
    let corpus: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let result = verify(&corpus);
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    if result["passed"] != true {
        std::process::exit(1);
    }
}
