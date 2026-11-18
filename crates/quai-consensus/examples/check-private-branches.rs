use quai_consensus::{
    private_branch::{PrivateBranch, Restored},
    verifier::Verifier,
};
use quai_execution::historical::History as _;
use quai_primitives::{decode_hex, encode_hex, transaction::Transaction};
use serde_json::{json, Value};
use std::{fs, path::Path};
pub fn bytes(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
pub fn incoming(v: &Value) -> Vec<Transaction> {
    v["inbound"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| Transaction::decode(&bytes(x)).unwrap())
        .collect()
}
fn check(valid: bool, checks: &mut usize, label: &str) {
    *checks += 1;
    assert!(valid, "{label}");
}
fn matches(state: &Restored, row: &Value, checks: &mut usize) {
    for (key, actual) in [
        ("block_hash", json!(encode_hex(&state.anchor.block))),
        (
            "evm_root",
            json!(encode_hex(&state.state.world.root().unwrap())),
        ),
        (
            "etx_root",
            json!(encode_hex(&state.state.world.etxs().root().unwrap())),
        ),
        ("utxo_root", json!(encode_hex(&state.state.utxos.root()))),
        ("utxo_set_size", json!(state.state.utxos.consensus_size())),
        ("quai_state_size", json!(state.state.world.account_count())),
        (
            "receipt_rlp",
            json!(state
                .receipt_rlp
                .iter()
                .map(|r| encode_hex(r))
                .collect::<Vec<_>>()),
        ),
    ] {
        check(actual == row[key], checks, key);
    }
    check(
        state.history.len() == state.anchor.height as usize + 1,
        checks,
        "restored branch ancestry",
    );
    let included = state.past.incoming_at(state.anchor.height).unwrap();
    check(
        included.iter().map(Transaction::encode).collect::<Vec<_>>()
            == incoming(row)
                .iter()
                .map(Transaction::encode)
                .collect::<Vec<_>>(),
        checks,
        "restored historical incoming",
    );
}
pub fn verify(corpus: &Value, directory: &Path) -> Value {
    assert_eq!(
        corpus["reference_revision"],
        quai_primitives::REFERENCE_REVISION
    );
    let rows = corpus["branches"][0]["vectors"].as_array().unwrap();
    let alt = corpus["branches"][1]["vectors"].as_array().unwrap();
    assert_eq!(rows.len(), 16);
    assert_eq!(alt.len(), 16);
    assert_eq!(&rows[..8], &alt[..8]);
    let mut store = PrivateBranch::open(directory).unwrap();
    let mut checks = 0;
    let mut verifier = Verifier::default();
    let mut ids = vec![store.head()];
    for row in rows {
        let before = store.head();
        let candidate = store
            .execute_candidate(
                &mut verifier,
                before,
                &bytes(&row["work_object"]),
                &incoming(row),
            )
            .unwrap();
        check(
            store.head() == before && !candidate.fully_validated,
            &mut checks,
            "stage grants no selected/canonical authority",
        );
        store
            .select_execution_head(before, candidate.checkpoint)
            .unwrap();
        check(
            encode_hex(&candidate.work_hash) == row["work_hash"],
            &mut checks,
            "native work hash",
        );
        ids.push(candidate.checkpoint);
        drop(store);
        store = PrivateBranch::open(directory).unwrap();
        matches(&store.restore(store.head()).unwrap(), row, &mut checks);
    }
    let main_tip = store.head();
    let mut alt_ids = ids[..9].to_vec();
    for row in &alt[8..] {
        let candidate = store
            .execute_candidate(
                &mut verifier,
                *alt_ids.last().unwrap(),
                &bytes(&row["work_object"]),
                &incoming(row),
            )
            .unwrap();
        check(
            encode_hex(&candidate.work_hash) == row["work_hash"],
            &mut checks,
            "alternate native work hash",
        );
        alt_ids.push(candidate.checkpoint);
        check(
            store.head() == main_tip,
            &mut checks,
            "alternate stage cannot change head",
        );
        matches(
            &store.restore(candidate.checkpoint).unwrap(),
            row,
            &mut checks,
        );
    }
    let alternate_tip = *alt_ids.last().unwrap();
    let (undo, forward) = store.reorg_path(alternate_tip).unwrap();
    check(
        undo == ids[9..].iter().rev().copied().collect::<Vec<_>>(),
        &mut checks,
        "reorg undo order",
    );
    check(forward == alt_ids[9..], &mut checks, "reorg forward order");
    check(
        store.select_execution_head(ids[8], alternate_tip).is_err() && store.head() == main_tip,
        &mut checks,
        "stale selection rejected",
    );
    store
        .select_execution_head(main_tip, alternate_tip)
        .unwrap();
    drop(store);
    store = PrivateBranch::open(directory).unwrap();
    matches(&store.restore(store.head()).unwrap(), &alt[15], &mut checks);
    check(
        store
            .restore(store.head())
            .unwrap()
            .history
            .get(&bytes(&rows[8]["block_hash"]).try_into().unwrap())
            .is_err(),
        &mut checks,
        "no old branch history leakage",
    );
    store
        .select_execution_head(alternate_tip, main_tip)
        .unwrap();
    drop(store);
    store = PrivateBranch::open(directory).unwrap();
    matches(
        &store.restore(store.head()).unwrap(),
        &rows[15],
        &mut checks,
    );

    // Malformed input must fail before staging any candidate object.
    let mut wrong = bytes(&rows[15]["work_object"]);
    wrong.truncate(wrong.len() / 2);
    let objects = fs::read_dir(directory.join("objects")).unwrap().count();
    check(
        store
            .execute_candidate(&mut verifier, ids[15], &wrong, &incoming(&rows[15]))
            .is_err()
            && store.head() == main_tip
            && fs::read_dir(directory.join("objects")).unwrap().count() == objects,
        &mut checks,
        "malformed candidate atomicity",
    );
    drop(store);
    let alt_path = directory
        .join("objects")
        .join(encode_hex(&alternate_tip).trim_start_matches("0x"));
    let mut corrupt = fs::read(&alt_path).unwrap();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 1;
    fs::write(alt_path, corrupt).unwrap();
    store = PrivateBranch::open(directory).unwrap();
    let head_bytes = fs::read(directory.join("HEAD")).unwrap();
    check(
        store
            .select_execution_head(main_tip, alternate_tip)
            .is_err()
            && store.head() == main_tip
            && fs::read(directory.join("HEAD")).unwrap() == head_bytes,
        &mut checks,
        "corrupt alternate rejected before publication",
    );
    drop(store);
    let selected_path = directory
        .join("objects")
        .join(encode_hex(&main_tip).trim_start_matches("0x"));
    fs::write(selected_path, b"truncated").unwrap();
    check(
        PrivateBranch::open(directory).is_err(),
        &mut checks,
        "corrupt selected branch fails closed on reopen",
    );
    json!({"passed":true,"unique_native_blocks_executed":24,"oracle_rows":32,"checks":checks,"selected_head_reopens":18,"branch_switches":2,"undo_blocks":8,"forward_blocks":8,"runtime_go_dependency":false,"full_consensus":false,"canonical_sync":false,"scope":"Two private execution-only branches, complete local state/receipts/history restoration and explicit head selection. Native proof kernels; controlled placement/inbound inputs; no PoEM or full header/hierarchy admission."})
}
#[allow(dead_code)]
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(args.len(), 2);
    let corpus: Value = serde_json::from_slice(&fs::read(&args[0]).unwrap()).unwrap();
    println!(
        "{}",
        serde_json::to_string_pretty(&verify(&corpus, Path::new(&args[1]))).unwrap()
    );
}
