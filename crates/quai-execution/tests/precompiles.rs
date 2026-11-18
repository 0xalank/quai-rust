//! Precompile addresses, the access list of a creation, address-scope errors
//! in nested frames, lockup-contract calls and precompile inputs against
//! go-quai v0.57.0 (fixtures/protocol/precompiles.json,
//! scripts/gen_precompile_vectors.py, reference/precompiles).
#[allow(dead_code)]
#[path = "../examples/check-settlement.rs"]
mod settlement;
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
