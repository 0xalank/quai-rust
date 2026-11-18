use quai_consensus::{
    confirmation::{self, Hierarchy},
    history::Record,
    private_branch::PrivateBranch,
    verifier::Verifier,
};
use quai_primitives::{decode_hex, encode_hex, transaction::Transaction};
use serde_json::{json, Value};
use std::{fs, path::Path};
pub fn bytes(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
pub fn views(v: &Value) -> Vec<(u8, Vec<u8>)> {
    v["contexts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| {
            let c = x["context"].as_u64().unwrap() as u8;
            (c < 2).then(|| (c, bytes(&x["work_object"])))
        })
        .collect()
}
fn txs(v: &[Transaction]) -> Value {
    json!(v
        .iter()
        .map(|t| encode_hex(&t.encode()))
        .collect::<Vec<_>>())
}
fn check(ok: bool, n: &mut usize, label: &str) {
    *n += 1;
    assert!(ok, "{label}");
}
pub fn verify(c: &Value, directory: &Path) -> Value {
    assert_eq!(c["reference_revision"], quai_primitives::REFERENCE_REVISION);
    let now = c["now"].as_u64().unwrap();
    let mut store = PrivateBranch::open_hierarchy(directory).unwrap();
    let mut v = Verifier::default();
    let mut graph = Hierarchy::new().unwrap();
    let mut checks = 0;
    let mut main = vec![store.head()];
    let mut alt = vec![];
    let mut contexts = 0;
    let mut received = 0;
    let mut confirms = 0;
    let mut rejected = 0;
    for (which, branch) in c["branches"].as_array().unwrap().iter().enumerate() {
        let rows = branch["vectors"].as_array().unwrap();
        if which == 1 {
            alt = main[..9].to_vec();
            graph = store
                .restore(main[8])
                .unwrap()
                .rebuild_confirmation_components(&mut v, now)
                .unwrap();
        }
        for row in rows.iter().skip(if which == 0 { 0 } else { 8 }) {
            let parent = if which == 0 {
                *main.last().unwrap()
            } else {
                *alt.last().unwrap()
            };
            let raw = bytes(&row["work_object"]);
            let block = Record::decode(&raw).unwrap();
            let witnesses = views(row);
            let old = store.head();
            if row["id"] == "hierarchy-10" && which == 0 {
                let before = fs::read(directory.join("HEAD")).unwrap();
                let count = fs::read_dir(directory.join("objects")).unwrap().count();
                check(
                    store
                        .execute_hierarchy_candidate(&mut v, parent, &raw, vec![], now)
                        .is_err(),
                    &mut checks,
                    "missing prime/region witnesses",
                );
                rejected += 1;
                let other = views(&c["branches"][1]["vectors"][9]);
                check(
                    store
                        .execute_hierarchy_candidate(&mut v, parent, &raw, other, now)
                        .is_err(),
                    &mut checks,
                    "cross-branch view substitution",
                );
                rejected += 1;
                for bad in c["negatives"].as_array().unwrap() {
                    let b = Record::decode(&bytes(&bad["work_object"])).unwrap();
                    check(
                        encode_hex(&v.verify_header_proof(b.header()).unwrap().hash())
                            == bad["work_hash"],
                        &mut checks,
                        "forged rollup has valid native work",
                    );
                    check(
                        store
                            .execute_hierarchy_candidate(&mut v, parent, b.bytes(), views(bad), now)
                            .is_err(),
                        &mut checks,
                        "forged committed rollup rejected",
                    );
                    rejected += 1;
                }
                check(
                    store.head() == old
                        && fs::read(directory.join("HEAD")).unwrap() == before
                        && fs::read_dir(directory.join("objects")).unwrap().count() == count,
                    &mut checks,
                    "invalid confirmation atomicity",
                );
            }
            let prepared = graph.prepare(&mut v, &block, &witnesses, now).unwrap();
            check(
                prepared.order() == row["order"].as_u64().unwrap() as u8,
                &mut checks,
                "native order",
            );
            check(
                txs(prepared.inbound_for_execution()) == row["inbound"],
                &mut checks,
                "derived execution inbounds",
            );
            check(
                txs(prepared.newly_confirmed()) == row["confirmed"],
                &mut checks,
                "Go newly confirmed ETX order",
            );
            check(
                txs(prepared.region_rollup()) == row["cross_rollup"],
                &mut checks,
                "Go cross-prime rollup",
            );
            for expected in row["contexts"].as_array().unwrap() {
                let context = expected["context"].as_u64().unwrap() as usize;
                let t = prepared
                    .snapshot(context)
                    .unwrap()
                    .termini
                    .as_ref()
                    .unwrap();
                check(
                    json!(t.dominant.iter().map(|h| encode_hex(h)).collect::<Vec<_>>())
                        == expected["termini"]["dom"]
                        && json!(t
                            .subordinate
                            .iter()
                            .map(|h| encode_hex(h))
                            .collect::<Vec<_>>())
                            == expected["termini"]["sub"],
                    &mut checks,
                    "Go context termini",
                );
                check(
                    encode_hex(&prepared.returned_dominant(context).unwrap())
                        == expected["returned_dominant"],
                    &mut checks,
                    "Go returned dominant reference",
                );
            }
            let candidate = store
                .execute_hierarchy_candidate(&mut v, parent, &raw, witnesses, now)
                .unwrap();
            check(
                !candidate.fully_validated
                    && candidate.header_components_checked
                    && candidate.inbound_derived
                    && store.head() == old,
                &mut checks,
                "component/candidate authority boundary",
            );
            check(
                encode_hex(&candidate.work_hash) == row["work_hash"],
                &mut checks,
                "native work hash",
            );
            check(
                candidate.execution_inbound_etxs == prepared.inbound_for_execution().len()
                    && candidate.confirmed_etxs == prepared.newly_confirmed().len()
                    && candidate.hierarchy_contexts == prepared.context_components(),
                &mut checks,
                "sealed confirmation counters",
            );
            contexts += candidate.hierarchy_contexts;
            received += candidate.execution_inbound_etxs;
            confirms += candidate.confirmed_etxs;
            graph.commit(prepared).unwrap();
            if which == 0 {
                main.push(candidate.checkpoint);
                store
                    .select_execution_head(old, candidate.checkpoint)
                    .unwrap();
            } else {
                alt.push(candidate.checkpoint);
            }
            let state = store.restore(candidate.checkpoint).unwrap();
            for (field, value) in [
                (
                    "evm_root",
                    json!(encode_hex(&state.state.world.root().unwrap())),
                ),
                (
                    "etx_root",
                    json!(encode_hex(&state.state.world.etxs().root().unwrap())),
                ),
                ("utxo_root", json!(encode_hex(&state.state.utxos.root()))),
                ("quai_state_size", json!(state.state.world.account_count())),
                ("utxo_set_size", json!(state.state.utxos.consensus_size())),
                (
                    "receipt_rlp",
                    json!(state
                        .receipt_rlp
                        .iter()
                        .map(|r| encode_hex(r))
                        .collect::<Vec<_>>()),
                ),
            ] {
                check(value == row[field], &mut checks, field);
            }
            if row["id"] == "hierarchy-11" {
                drop(store);
                store = PrivateBranch::open_hierarchy(directory).unwrap();
                let rebuilt = store
                    .restore(candidate.checkpoint)
                    .unwrap()
                    .rebuild_confirmation_components(&mut v, now)
                    .unwrap();
                check(
                    rebuilt.heads() == graph.heads(),
                    &mut checks,
                    "persisted confirmation witnesses/recovery",
                );
            }
        }
    }
    let old = store.head();
    let path = store.reorg_path(*alt.last().unwrap()).unwrap();
    check(
        path.0.len() == 8 && path.1.len() == 8,
        &mut checks,
        "hierarchy branch switch plan",
    );
    store
        .select_execution_head(old, *alt.last().unwrap())
        .unwrap();
    drop(store);
    store = PrivateBranch::open_hierarchy(directory).unwrap();
    check(
        store
            .restore(store.head())
            .unwrap()
            .rebuild_confirmation_components(&mut v, now)
            .unwrap()
            .heads()
            == graph.heads(),
        &mut checks,
        "hierarchy branch witness isolation",
    );
    let first = &c["branches"][0]["vectors"][0];
    check(
        store
            .execute_candidate(&mut v, main[0], &bytes(&first["work_object"]), &[])
            .is_err()
            && store
                .execute_admission_candidate(&mut v, main[0], &bytes(&first["work_object"]), now)
                .is_err(),
        &mut checks,
        "weaker entrypoints refused",
    );
    let altrows = c["branches"][1]["vectors"].as_array().unwrap();
    let warm = store
        .execute_hierarchy_candidate(
            &mut v,
            alt[14],
            &bytes(&altrows[14]["work_object"]),
            views(&altrows[14]),
            now,
        )
        .unwrap();
    check(
        warm.checkpoint == alt[15],
        &mut checks,
        "deterministic persisted hierarchy candidate",
    );
    let head_before = fs::read(directory.join("HEAD")).unwrap();
    let path = directory
        .join("objects")
        .join(encode_hex(&warm.checkpoint).trim_start_matches("0x"));
    let saved = fs::read(&path).unwrap();
    fs::write(&path, b"corrupt after graph preparation").unwrap();
    check(
        store
            .execute_hierarchy_candidate(
                &mut v,
                warm.checkpoint,
                &bytes(&altrows[15]["work_object"]),
                views(&altrows[15]),
                now,
            )
            .is_err()
            && fs::read(directory.join("HEAD")).unwrap() == head_before,
        &mut checks,
        "fresh parent integrity before graph reuse",
    );
    fs::write(path, saved).unwrap();
    drop(store);
    check(
        PrivateBranch::open_admission(directory).is_err(),
        &mut checks,
        "hierarchy profile downgrade refused",
    );
    let input = c["filters"]["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| Transaction::decode(&bytes(x)).unwrap())
        .collect::<Vec<_>>();
    for row in c["filters"]["rows"].as_array().unwrap() {
        let loc = [
            row["location"][0].as_u64().unwrap() as u8,
            row["location"][1].as_u64().unwrap() as u8,
        ];
        check(
            txs(&confirmation::filter_to_sub(
                &input,
                loc,
                row["context"].as_u64().unwrap() as usize,
                row["order"].as_u64().unwrap() as u8,
            )
            .unwrap())
                == row["expected"],
            &mut checks,
            "actual Go ETX filtering",
        );
    }
    for row in c["filters"]["cross"].as_array().unwrap() {
        check(
            txs(&confirmation::cross_prime(&input, row["region"].as_u64().unwrap() as u8).unwrap())
                == row["expected"],
            &mut checks,
            "actual Go cross-prime filtering",
        );
    }
    json!({"passed":true,"checks":checks,"unique_native_blocks":24,"native_context_components":contexts,"confirmed_etxs":confirms,"executed_inbound_etxs":received,"invalid_confirmation_inputs_rejected":rejected,"filter_cases":29,"full_consensus":false,"canonical_sync":false,"runtime_go_dependency":false,"scope":"Native mixed prime/region/zone components, committed ETX rollups and derived incoming coinbase execution on two early private branches. Full fork/workshare/mixed transaction and canonical publication/sync gates remain open."})
}
#[allow(dead_code)]
fn main() {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    let c = serde_json::from_slice(&fs::read(&a[0]).unwrap()).unwrap();
    println!(
        "{}",
        serde_json::to_string_pretty(&verify(&c, Path::new(&a[1]))).unwrap()
    );
}
