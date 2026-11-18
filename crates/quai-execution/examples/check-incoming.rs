#[allow(dead_code)]
#[path = "../tests/vm_support/mod.rs"]
mod support;
use quai_execution::{
    economic_state::magnitude,
    evm::{self, calls::Environment, word::Word},
    incoming::{self, Context},
    settlement::BlockContext,
    utxo::UtxoState,
};
use quai_primitives::{decode_hex, encode_hex, transaction::Transaction};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
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
    let rows = corpus["vectors"].as_array().unwrap();
    let mut checks = 0;
    let mut failures = vec![];
    for row in rows {
        let mut world = support::parse_world(row).unwrap();
        let mut state = UtxoState::default();
        let tx = Transaction::decode(&decode_hex(row["transaction"].as_str().unwrap()).unwrap())
            .unwrap();
        let location = [
            row["location"][0].as_u64().unwrap() as u8,
            row["location"][1].as_u64().unwrap() as u8,
        ];
        let mut coinbase = [0; 20];
        coinbase[0] = location[0] << 4 | location[1];
        coinbase[18] = 0x11;
        coinbase[19] = 5;
        let mut c = Context {
            block: BlockContext {
                vm: evm::Context {
                    address: [0; 20],
                    caller: [0; 20],
                    origin: [0; 20],
                    coinbase,
                    etx_sender: [0; 20],
                    location,
                    tx_type: 1,
                    value: Word::ZERO,
                    gas_price: Word::ZERO,
                    block_number: Word::from_u64(row["number"].as_u64().unwrap()),
                    timestamp: Word::from_u64(1769112000),
                    difficulty: Word::ONE,
                    base_fee: Word::from_u64(13),
                    chain_id: Word::from_u64(1337),
                    state_size: Word::from_u64(1 << 20),
                    block_gas_limit: 30000000,
                    address_listed: false,
                    warm_slots: BTreeSet::new(),
                    read_only: false,
                    trace: false,
                },
                environment: Environment {
                    prime_number: row["prime_number"].as_u64(),
                    block_hashes: BTreeMap::new(),
                    delete_empty: true,
                    ..Default::default()
                },
                gas_remaining: row["gas_pool"].as_u64().unwrap(),
                cumulative_gas: 0,
                state_used: 0,
            },
            parent_height: row["parent_number"].as_u64().unwrap(),
            total_etx_gas: 0,
            region_remaining: 10000000,
            prime_remaining: 10000000,
        };
        let mut compare = |field: &str, actual: Value| {
            checks += 1;
            if actual != row[field] {
                failures.push(json!({"id":row["id"],"field":field,"actual":actual,"expected":row[field],"reference_error":row["reference_error"]}));
            }
        };
        compare("before_root", json!(encode_hex(&world.root().unwrap())));
        if row["queued"] == true {
            world
                .etxs_mut()
                .push(&vec![
                    tx.clone();
                    row["repetitions"].as_u64().unwrap() as usize
                ])
                .unwrap();
        }
        compare(
            "before_etx_root",
            json!(encode_hex(&world.etxs().root().unwrap())),
        );
        if let Some(observe) = observer.as_deref_mut() {
            observe(row, &mut world, &mut state, 0);
        }
        let before = world.clone();
        let prior = c.clone();
        world.begin_block();
        let result = (|| {
            let mut receipts = vec![];
            let mut added = Word::ZERO;
            let mut outbound = vec![];
            for _ in 0..row["repetitions"].as_u64().unwrap() {
                let a = incoming::apply(&mut world, &mut state, &tx, &mut c)?;
                receipts.extend(a.receipt);
                added = added.wrapping_add(&a.supply_added);
                outbound.extend(a.outbound);
            }
            Ok::<_, quai_execution::settlement::Error>((receipts, added, outbound))
        })();
        compare("accepted", json!(result.is_ok()));
        match result {
            Err(e) => {
                if let Some(observe) = observer.as_deref_mut() {
                    observe(row, &mut world, &mut state, 2);
                }
                if row["accepted"] == true {
                    failures.push(json!({"id":row["id"],"error":e.to_string()}));
                } else {
                    checks += 1;
                    if world.accounts() != before.accounts()
                        || world.etxs() != before.etxs()
                        || !state.is_empty()
                        || c.block.gas_remaining != prior.block.gas_remaining
                        || c.block.cumulative_gas != prior.block.cumulative_gas
                    {
                        failures.push(json!({"id":row["id"],"field":"rejection_atomicity"}));
                    }
                }
            }
            Ok((receipts, supply, outbound)) => {
                world.finish_block().unwrap();
                if let Some(observe) = observer.as_deref_mut() {
                    observe(row, &mut world, &mut state, 1);
                }
                compare("after_root", json!(encode_hex(&world.root().unwrap())));
                compare("etx_root", json!(encode_hex(&world.etxs().root().unwrap())));
                compare("utxo_root", json!(encode_hex(&state.root())));
                compare("utxo_size", json!(state.consensus_size()));
                compare("supply_added", json!(encode_hex(&magnitude(supply))));
                compare("gas_remaining", json!(c.block.gas_remaining));
                compare("gas_used", json!(c.block.cumulative_gas));
                compare("state_used", json!(c.block.state_used));
                compare("etx_gas", json!(c.total_etx_gas));
                compare("region_remaining", json!(c.region_remaining));
                compare("prime_remaining", json!(c.prime_remaining));
                compare(
                    "outbound",
                    json!(outbound
                        .iter()
                        .map(|t| encode_hex(&t.encode()))
                        .collect::<Vec<_>>()),
                );
                compare("receipts",json!(receipts.into_iter().map(|r|json!({"status":r.status,"gas_used":r.gas_used,"cumulative_gas_used":r.cumulative_gas_used,"rlp":encode_hex(&r.typed_rlp().unwrap())})).collect::<Vec<_>>()));
            }
        }
    }
    // Large receipt mismatches remain in the artifact, never copied to console.
    json!({"passed":failures.is_empty(),"cases":rows.len(),"checks":checks,"failures":failures,"reference_revision":quai_primitives::REFERENCE_REVISION,"full_consensus":false,"canonical_sync":false})
}
fn main() {
    let path = std::env::args().nth(1).expect("incoming ETX corpus");
    let corpus: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let result = verify(&corpus);
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    if result["passed"] != true {
        std::process::exit(1);
    }
}
