use quai_execution::evm::{
    self,
    state::{Account, World},
    word::{self, Word},
    Context,
};
use quai_primitives::{decode_hex, encode_hex};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
fn raw(v: &Value, key: &str) -> Result<Vec<u8>, String> {
    decode_hex(v[key].as_str().ok_or_else(|| format!("missing {key}"))?)
}
fn address(v: &Value, key: &str) -> Result<[u8; 20], String> {
    raw(v, key)?
        .try_into()
        .map_err(|_| format!("bad address {key}"))
}
fn hash(v: &Value, key: &str) -> Result<[u8; 32], String> {
    raw(v, key)?
        .try_into()
        .map_err(|_| format!("bad hash {key}"))
}
fn word(v: &Value, key: &str) -> Result<Word, String> {
    let b = raw(v, key)?;
    if b.len() > 32 {
        return Err("word exceeds 256-bit scope".into());
    }
    Ok(word::from_bytes(&b))
}
fn number(v: &Value, key: &str) -> Result<u64, String> {
    v[key].as_u64().ok_or_else(|| format!("bad {key}"))
}
fn boolean(v: &Value, key: &str) -> Result<bool, String> {
    v[key].as_bool().ok_or_else(|| format!("bad {key}"))
}
fn array<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>, String> {
    v[key].as_array().ok_or_else(|| format!("missing {key}"))
}
pub fn parse_world(v: &Value) -> Result<World, String> {
    let mut world = World::default();
    for a in array(v, "world")? {
        let mut storage = BTreeMap::new();
        for s in array(a, "storage")? {
            storage.insert(hash(s, "key")?, hash(s, "value")?);
        }
        world.insert(
            address(a, "address")?,
            Account {
                nonce: number(a, "nonce")?,
                balance: word(a, "balance")?,
                code: raw(a, "code")?,
                storage,
                storage_size: word(a, "storage_size")?,
                backing: None,
            },
        )?;
    }
    Ok(world)
}
pub fn parse_context(v: &Value) -> Result<Context, String> {
    let c = &v["context"];
    let location = array(c, "location")?
        .iter()
        .map(|v| u8::try_from(v.as_u64().ok_or("bad location")?).map_err(|_| "bad location".into()))
        .collect::<Result<Vec<_>, String>>()?
        .try_into()
        .map_err(|_| "bad location length")?;
    let warm_slots = array(c, "warm_slots")?
        .iter()
        .map(|v| {
            decode_hex(v.as_str().ok_or("bad warm slot")?)?
                .try_into()
                .map_err(|_| "bad warm slot length".into())
        })
        .collect::<Result<BTreeSet<_>, String>>()?;
    Ok(Context {
        address: address(c, "address")?,
        caller: address(c, "caller")?,
        origin: address(c, "origin")?,
        coinbase: address(c, "coinbase")?,
        etx_sender: address(c, "etx_sender")?,
        location,
        tx_type: u8::try_from(number(c, "tx_type")?).map_err(|_| "bad transaction type")?,
        value: word(c, "value")?,
        gas_price: word(c, "gas_price")?,
        block_number: word(c, "block_number")?,
        timestamp: word(c, "timestamp")?,
        difficulty: word(c, "difficulty")?,
        base_fee: word(c, "base_fee")?,
        chain_id: word(c, "chain_id")?,
        state_size: word(c, "state_size")?,
        block_gas_limit: number(c, "block_gas_limit")?,
        address_listed: boolean(c, "address_listed")?,
        warm_slots,
        read_only: boolean(c, "read_only")?,
        trace: true,
    })
}
pub fn verify(corpus: &Value) -> Result<Value, String> {
    if corpus["schema"] != 1
        || corpus["kind"] != "serial-vm"
        || corpus["reference_revision"] != quai_primitives::REFERENCE_REVISION
        || corpus["new_ops_height"] != quai_primitives::spec::FORKS.max_code_size
    {
        return Err("unrecognized VM corpus/reference/fork".into());
    }
    let mut checks = 0usize;
    let mut failures = Vec::new();
    let mut mismatches = 0usize;
    let mut compare = |id: &Value, field: &str, actual: Value, expected: &Value| {
        checks += 1;
        if &actual != expected {
            mismatches += 1;
            if failures.len() < 200 {
                failures.push(json!({"id":id,"field":field,"actual":actual,"expected":expected}));
            }
        }
    };
    let go_opcodes = array(corpus, "go_instruction_set")?
        .iter()
        .map(|v| v.as_u64().ok_or("bad Go opcode"))
        .collect::<Result<BTreeSet<_>, _>>()?;
    for op in evm::supported_opcodes() {
        compare(
            &json!(op),
            "supported opcode defined by reference",
            json!(go_opcodes.contains(&(op as u64))),
            &json!(true),
        );
    }
    for v in array(corpus, "state_gas")? {
        compare(
            &json!("gas-scaling"),
            "state scaling",
            json!(evm::state_gas::scaled(
                word(v, "state")?,
                word(v, "contract")?,
                number(v, "base")?
            )),
            &v["gas"],
        );
    }
    let mut observed = BTreeSet::new();
    let mut success = 0usize;
    let mut reverted = 0usize;
    let mut fault = 0usize;
    let mut steps = 0usize;
    for v in array(corpus, "vectors")? {
        for op in array(v, "trace_opcode_values")? {
            observed.insert(op.as_u64().ok_or("bad observed opcode")?);
        }
        let mut world = parse_world(v)?;
        let before = world.clone();
        let context = parse_context(v)?;
        compare(
            &v["id"],
            "before root",
            json!(encode_hex(&world.root()?)),
            &v["before_root"],
        );
        let result =
            match evm::execute_frame(&mut world, &context, &raw(v, "input")?, number(v, "gas")?) {
                Ok(r) => r,
                Err(e) => {
                    compare(
                        &v["id"],
                        "capability error",
                        json!(e.to_string()),
                        &json!("supported"),
                    );
                    continue;
                }
            };
        compare(
            &v["id"],
            "output",
            json!(encode_hex(&result.output)),
            &v["output"],
        );
        compare(
            &v["id"],
            "error",
            result
                .error
                .map(|e| json!(e.class()))
                .unwrap_or(Value::Null),
            &v["error"],
        );
        compare(&v["id"], "reverted", json!(result.reverted), &v["reverted"]);
        compare(
            &v["id"],
            "gas remaining",
            json!(result.gas_remaining),
            &v["gas_remaining"],
        );
        compare(
            &v["id"],
            "state gas",
            json!(result.state_gas),
            &v["state_gas"],
        );
        compare(&v["id"], "refund", json!(result.refund), &v["refund"]);
        compare(
            &v["id"],
            "trace hash",
            json!(encode_hex(&result.trace_hash)),
            &v["trace_hash"],
        );
        compare(
            &v["id"],
            "trace steps",
            json!(result.trace_steps),
            &v["trace_steps"],
        );
        compare(
            &v["id"],
            "after root",
            json!(encode_hex(&world.root()?)),
            &v["after_root"],
        );
        compare(
            &v["id"],
            "storage size",
            json!(encode_hex(&word::bytes(
                world
                    .account(&context.address)
                    .unwrap()
                    .unwrap()
                    .storage_size
            ))),
            &v["storage_size"],
        );
        for slot in array(v, "storage")? {
            compare(
                &v["id"],
                "storage slot",
                json!(encode_hex(
                    &world
                        .account(&context.address)
                        .unwrap()
                        .unwrap()
                        .get(hash(slot, "key")?)?
                )),
                &slot["value"],
            );
        }
        let logs=result.logs.iter().map(|l|json!({"address":encode_hex(&l.address),"topics":l.topics.iter().map(|h|encode_hex(h)).collect::<Vec<_>>(),"data":encode_hex(&l.data)})).collect::<Vec<_>>();
        compare(&v["id"], "logs", json!(logs), &v["logs"]);
        if result.reverted || result.error.is_some() {
            compare(
                &v["id"],
                "atomic rollback",
                json!(world == before),
                &json!(true),
            );
        }
        if result.reverted {
            reverted += 1;
        } else if result.error.is_some() {
            fault += 1;
        } else {
            success += 1;
        }
        steps += result.trace_steps;
    }
    Ok(
        json!({"passed":mismatches==0,"checks":checks,"mismatches":mismatches,"failures":failures,"vm_cases":array(corpus,"vectors")?.len(),"state_gas_cases":array(corpus,"state_gas")?.len(),"successes":success,"reverts":reverted,"faults":fault,"traced_instructions":steps,"supported_opcode_values":evm::supported_opcodes().len(),"reference_opcode_values":go_opcodes.len(),"supported_opcode_values_observed":evm::supported_opcodes().iter().filter(|op|observed.contains(&(**op as u64))).count(),"supported_opcode_values_unobserved":evm::supported_opcodes().into_iter().filter(|op|!observed.contains(&(*op as u64))).collect::<Vec<_>>(),"full_transaction_execution":false,"full_evm":false,"full_block_validation":false,"scope":"Bounded isolated frames, account/storage roots, gas/refund/logs and trace digests; calls/creation/precompiles/value transfer/fees and full transaction/block authority absent."}),
    )
}
