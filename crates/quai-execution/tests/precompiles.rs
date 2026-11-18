//! Precompile addresses, the access list of a creation, address-scope errors
//! in nested frames, lockup-contract calls and precompile inputs against
//! go-quai v0.57.0 (fixtures/protocol/precompiles.json,
//! scripts/gen_precompile_vectors.py, reference/precompiles).
#[allow(dead_code)]
#[path = "../examples/check-settlement.rs"]
mod settlement;
use quai_execution::evm::{message, precompile, Fault};
use quai_primitives::{decode_hex, encode_hex};
use serde_json::Value;

fn corpus() -> Value {
    serde_json::from_slice(include_bytes!(
        "../../../fixtures/protocol/precompiles.json"
    ))
    .unwrap()
}

/// The vectors of `group` through `settlement::apply_quai`: gas used, state
/// gas, fees, output, error class, status, receipt (Keccak-256 of its RLP),
/// logs, contract address, post-state root and block gas pool.
fn check_group(group: &str, cases: usize) {
    let mut corpus = corpus();
    let vectors: Vec<Value> = corpus["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["group"] == group)
        .cloned()
        .collect();
    assert_eq!(vectors.len(), cases, "{group}");
    corpus["vectors"] = Value::Array(vectors);
    let result = settlement::verify(&corpus);
    let failures = result["failures"].as_array().unwrap();
    let shown: Vec<String> = failures
        .iter()
        .take(40)
        .map(|f| {
            format!(
                "{} {} expected {} actual {}",
                f["id"], f["field"], f["expected"], f["actual"]
            )
        })
        .collect();
    assert_eq!(
        result["passed"],
        true,
        "{group}: {} mismatches\n{}",
        failures.len(),
        shown.join("\n")
    );
    println!(
        "PRECOMPILE_TRANSACTIONS group={group} cases={} checks={}",
        result["cases"], result["checks"]
    );
}

/// A creation transaction whose init code reaches a precompile that the
/// access list does not name; also a nested CREATE, a CREATE from a called
/// contract and a creation before the forks.
#[test]
fn creation_access_list_matches_go() {
    check_group("creation", 98);
}

/// A transaction to `XX 00…00 0N` for six first bytes XX in three zones, with
/// and without value, the other addresses of that form (zero address,
/// lockup, 0x0b), CALL, CALLCODE, DELEGATECALL and STATICCALL on the forms,
/// listed and not listed, and BALANCE, EXTCODESIZE, EXTCODECOPY and
/// EXTCODEHASH of them.
#[test]
fn precompile_address_of_every_zone_matches_go() {
    check_group("translation", 531);
}

/// A contract with a balance of 0 or 1 sends 1 with CALL to the lockup
/// contract and with CALLCODE to another zone's address, a local address,
/// the zone's identity precompile and another zone's form of it.
#[test]
fn value_check_order_matches_go() {
    check_group("balance", 18);
}

/// A frame that fails with common.ErrExternalAddress makes the CALL,
/// CALLCODE, DELEGATECALL or STATICCALL that called it fail too: BALANCE,
/// EXTCODECOPY and EXTCODEHASH of another zone's address, a call of the
/// last three kinds to it, three frames deep, CALLCODE with value; a Qi
/// address, a CREATE and SELFDESTRUCT stop it.
#[test]
fn external_address_errors_reach_the_caller_as_in_go() {
    check_group("scope", 180);
}

/// The lockup contract from a contract and from the transaction: every
/// function with an address of this zone, this zone's Qi ledger and another
/// zone of each ledger, and inputs of other lengths.
#[test]
fn lockup_contract_address_errors_match_go() {
    check_group("lockup", 88);
}

/// `vm.RunPrecompiledContract` on the runs of the precompiles `ids`: required
/// gas, output and gas left, out of gas versus a precompile error, and the
/// error text that the RPC reports for a failed top-level call.
fn check_runs(ids: &[u8]) -> usize {
    let corpus = corpus();
    let mut failures = vec![];
    let mut checked = 0;
    for r in corpus["runs"].as_array().unwrap() {
        let n = r["precompile"].as_u64().unwrap() as u8;
        if !ids.contains(&n) {
            continue;
        }
        checked += 1;
        let id = r["id"].as_str().unwrap();
        let input = decode_hex(r["input"].as_str().unwrap()).unwrap();
        let required = precompile::required_gas(n, &input);
        if required != r["required_gas"].as_u64().unwrap() {
            failures.push(format!(
                "{id} required gas {required} go {}",
                r["required_gas"]
            ));
        }
        let got = precompile::run(n, &input, r["gas"].as_u64().unwrap()).unwrap();
        match (&got, r["error"].as_str()) {
            (Ok((output, left)), None) => {
                if encode_hex(output) != r["output"].as_str().unwrap()
                    || *left != r["gas_left"].as_u64().unwrap()
                {
                    failures.push(format!(
                        "{id} output {} left {left} go {} {}",
                        encode_hex(output),
                        r["output"],
                        r["gas_left"]
                    ));
                }
            }
            (Err(Fault::OutOfGas), Some(_)) if r["out_of_gas"] == true => {}
            (Err(Fault::Precompile), Some(text)) if r["out_of_gas"] == false => {
                let rpc = message::precompile_error(n, &input);
                if rpc.as_deref() != Some(text) {
                    failures.push(format!("{id} RPC error {rpc:?} go {text}"));
                }
            }
            (got, go) => failures.push(format!("{id} rust {got:?} go {go:?}")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {checked} runs differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
    println!("PRECOMPILE_RUNS ids={ids:?} cases={checked}");
    checked
}

/// bn256 pairing (0x08): G2 points on the twist curve outside the order-r
/// subgroup, at infinity, off the curve, with coordinates at and above the
/// field modulus and with swapped halves; G1 edge points; empty input,
/// lengths that are not a multiple of 192 and gas edges.
#[test]
fn bn256_pairing_inputs_match_go() {
    assert_eq!(check_runs(&[8]), 86);
}

/// The pairing inputs named `outside-N` hold points on the twist curve
/// y² = x³ + 3/(9+i) that are not in the order-r subgroup: substrate-bn
/// rejects them for the subgroup, not for the curve equation, and Go
/// rejects them as "bn256: malformed point".
#[test]
fn outside_points_are_on_the_twist_but_not_in_the_subgroup() {
    use substrate_bn::{AffineG2, Fq, Fq2, GroupError};
    let corpus = corpus();
    let mut checked = 0;
    for r in corpus["runs"].as_array().unwrap() {
        let id = r["id"].as_str().unwrap();
        if !id.starts_with("pairing-g-outside-") {
            continue;
        }
        let input = decode_hex(r["input"].as_str().unwrap()).unwrap();
        let q = |o: usize| Fq::from_slice(&input[64 + o..96 + o]).unwrap();
        let point = AffineG2::new(Fq2::new(q(32), q(0)), Fq2::new(q(96), q(64)));
        assert!(matches!(point, Err(GroupError::NotInSubgroup)), "{id}");
        assert_eq!(r["error"], "bn256: malformed point", "{id}");
        checked += 1;
    }
    assert_eq!(checked, 4);
}
