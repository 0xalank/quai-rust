use quai_consensus::{
    history::Record, private_branch::PrivateBranch, private_profile::PrivateChain,
    verifier::Verifier, zone_admission,
};
use quai_primitives::{decode_hex, encode_hex};
use serde_json::{json, Value};
use std::{fs, path::Path};
fn raw(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
fn check(ok: bool, n: &mut usize, label: &str) {
    *n += 1;
    assert!(ok, "{label}");
}
pub fn verify(c: &Value, directory: &Path) -> Value {
    assert_eq!(c["reference_revision"], quai_primitives::REFERENCE_REVISION);
    let mut store = PrivateBranch::open_admission(directory).unwrap();
    let mut v = Verifier::default();
    let mut checks = 0;
    let mut main = vec![store.head()];
    let mut alt = vec![];
    let now = c["now"].as_u64().unwrap();
    for (which, b) in c["branches"].as_array().unwrap().iter().enumerate() {
        let rows = b["vectors"].as_array().unwrap();
        if which == 1 {
            alt = main[..9].to_vec();
        }
        for row in rows.iter().skip(if which == 0 { 0 } else { 8 }) {
            let parent = if which == 0 {
                *main.last().unwrap()
            } else {
                *alt.last().unwrap()
            };
            let old = store.head();
            let candidate = store
                .execute_admission_candidate(&mut v, parent, &raw(&row["work_object"]), now)
                .unwrap();
            check(
                candidate.header_components_checked
                    && candidate.inbound_derived
                    && !candidate.fully_validated
                    && store.head() == old,
                &mut checks,
                "component boundary/atomic stage",
            );
            check(
                encode_hex(&candidate.work_hash) == row["work_hash"],
                &mut checks,
                "native work hash",
            );
            if which == 0 {
                store
                    .select_execution_head(old, candidate.checkpoint)
                    .unwrap();
                main.push(candidate.checkpoint);
                drop(store);
                store = PrivateBranch::open_admission(directory).unwrap();
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
            let record = Record::decode(&raw(&row["work_object"])).unwrap();
            let p = zone_admission::profile().unwrap();
            let index = zone_admission::index(&state.history, &record, &p).unwrap();
            let snapshot = index.get(&record.id()).unwrap();
            check(
                json!(snapshot
                    .manifest
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|h| encode_hex(h))
                    .collect::<Vec<_>>())
                    == row["manifest"],
                &mut checks,
                "Go manifest",
            );
            let t = snapshot.termini.as_ref().unwrap();
            check(
                json!(t.dominant.iter().map(|h| encode_hex(h)).collect::<Vec<_>>())
                    == row["termini"]["dom"]
                    && json!(t
                        .subordinate
                        .iter()
                        .map(|h| encode_hex(h))
                        .collect::<Vec<_>>())
                        == row["termini"]["sub"],
                &mut checks,
                "Go PCRC termini",
            );
        }
    }
    let old = store.head();
    let before = fs::read(directory.join("HEAD")).unwrap();
    let objects = fs::read_dir(directory.join("objects")).unwrap().count();
    let mut rejected = 0;
    for row in c["negatives"].as_array().unwrap() {
        let b = Record::decode(&raw(&row["work_object"])).unwrap();
        let proof = v.verify_header_proof(b.header()).unwrap();
        check(
            encode_hex(&proof.hash()) == row["work_hash"],
            &mut checks,
            "negative has real valid native proof",
        );
        let result = store.execute_admission_candidate(&mut v, main[8], b.bytes(), now);
        if result.is_ok() {
            panic!("negative accepted: {}", row["id"]);
        }
        check(
            store.head() == old
                && fs::read(directory.join("HEAD")).unwrap() == before
                && fs::read_dir(directory.join("objects")).unwrap().count() == objects,
            &mut checks,
            "negative rejection without staging/publication",
        );
        rejected += 1;
    }
    let first = &c["branches"][0]["vectors"][0];
    check(
        store
            .execute_admission_candidate(&mut v, main[0], &raw(&first["work_object"]), 0)
            .is_err(),
        &mut checks,
        "clock policy reevaluated even with verified proof",
    );
    check(
        store
            .execute_candidate(&mut v, main[0], &raw(&first["work_object"]), &[])
            .is_err(),
        &mut checks,
        "execution-only bypass refused in admission store",
    );
    let paths = store.reorg_path(*alt.last().unwrap()).unwrap();
    check(
        paths.0.len() == 8 && paths.1.len() == 8,
        &mut checks,
        "strict branch reorg plan",
    );
    store
        .select_execution_head(old, *alt.last().unwrap())
        .unwrap();
    drop(store);
    store = PrivateBranch::open_admission(directory).unwrap();
    check(
        store.restore(store.head()).unwrap().anchor.height == 16,
        &mut checks,
        "strict branch reopen",
    );
    drop(store);
    check(
        PrivateBranch::open(directory).is_err(),
        &mut checks,
        "profile downgrade refused",
    );
    check(
        PrivateChain::reviewed_admission().unwrap().fingerprint
            != PrivateChain::reviewed().unwrap().fingerprint,
        &mut checks,
        "separate frozen profile",
    );
    json!({"passed":true,"checks":checks,"unique_native_blocks":24,"oracle_rows":32,"valid_work_forgeries_rejected":rejected,"branch_switches":1,"runtime_go_dependency":false,"full_consensus":false,"canonical_sync":false,"scope":"Native local-zone header/body/manifest/PCRC components and atomic execution/commit, with empty inbounds derived from absence of dominant confirmation. Higher-order/fork/workshare admission and PoEM canonical sync remain unsupported."})
}
#[allow(dead_code)]
fn main() {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(a.len(), 2);
    let c: Value = serde_json::from_slice(&fs::read(&a[0]).unwrap()).unwrap();
    println!(
        "{}",
        serde_json::to_string_pretty(&verify(&c, Path::new(&a[1]))).unwrap()
    );
}
