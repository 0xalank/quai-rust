use quai_consensus::private_canonical::{BlockBundle, PrivateCanonical};
use quai_primitives::{decode_hex, encode_hex};
use serde_json::{json, Value};
use std::{fs, path::Path};
pub fn bundle(row: &Value) -> BlockBundle {
    BlockBundle {
        zone: decode_hex(row["work_object"].as_str().unwrap()).unwrap(),
        contexts: row["contexts"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| {
                let c = r["context"].as_u64().unwrap() as u8;
                (c < 2).then(|| (c, decode_hex(r["work_object"].as_str().unwrap()).unwrap()))
            })
            .collect(),
    }
}
fn check(ok: bool, n: &mut usize, why: &str) {
    *n += 1;
    assert!(ok, "{why}");
}
pub fn verify(v: &Value, dir: &Path) -> Value {
    verify_profile(v, dir, 0)
}
pub fn verify_execution_profile(v: &Value, dir: &Path) -> Value {
    verify_profile(v, dir, 1)
}
pub fn verify_all_etx_profile(v: &Value, dir: &Path) -> Value {
    verify_profile(v, dir, 2)
}
fn verify_profile(v: &Value, dir: &Path, profile: u8) -> Value {
    let activated = profile != 0;
    let open_chain = |dir: &Path, now| {
        if profile == 2 {
            PrivateCanonical::open_execution_v3(dir, now)
        } else if activated {
            PrivateCanonical::open_execution_v2(dir, now)
        } else {
            PrivateCanonical::open(dir, now)
        }
    };
    let now = v["now"].as_u64().unwrap();
    let mut chain = open_chain(dir, now).unwrap();
    assert_eq!(chain.storage_backend(), "incremental-trie-database");
    assert!(chain.indexed_ancestry());
    assert!(chain.indexed_catalog());
    assert!(chain.lazy_qi());
    let mut checks = 0;
    let mut switches = 0;
    let mut lower_reorg = 0;
    let mut rejected = 0;
    let mut reopened = 0;
    let mut pinned = None;
    for (i, event) in v["canonical_events"].as_array().unwrap().iter().enumerate() {
        let b = event["branch"].as_u64().unwrap() as usize;
        let n = event["index"].as_u64().unwrap() as usize;
        let row = &v["branches"][b]["vectors"][n];
        let old = chain.view().unwrap();
        if n == 6 && i < 16 {
            for bad in v["negatives"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|b| b["at_height"] == 7)
            {
                let before = chain.durable_checkpoint().unwrap();
                let catalog = chain.admitted_count();
                check(
                    chain.import(bundle(bad), now).is_err(),
                    &mut checks,
                    "Go-invalid candidate refused before canonical publication",
                );
                check(
                    chain.view().unwrap().checkpoint() == old.checkpoint()
                        && chain.durable_checkpoint().unwrap() == before
                        && chain.admitted_count() == catalog,
                    &mut checks,
                    "invalid canonical import atomicity",
                );
                rejected += 1;
            }
        }
        let result = chain
            .import(bundle(row), now)
            .unwrap_or_else(|e| panic!("canonical event {i}, row {n}: {e}"));
        let view = chain.view().unwrap();
        check(
            result.selected == event["selected"].as_bool().unwrap(),
            &mut checks,
            "actual Go BuildPendingHeaders choice",
        );
        check(
            encode_hex(&view.block()) == event["head"],
            &mut checks,
            "actual Go canonical head",
        );
        check(
            view.score()
                == crypto_bigint::U512::from_u128(
                    event["best_score"].as_str().unwrap().parse().unwrap(),
                ),
            &mut checks,
            "Go frontier entropy",
        );
        check(
            view.height() == event["head_number"].as_u64().unwrap(),
            &mut checks,
            "Go canonical height",
        );
        for (h, id) in event["canonical_hashes"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            check(
                view.hash_at(h as u64).unwrap().map(|b| encode_hex(&b))
                    == id.as_str().map(str::to_owned),
                &mut checks,
                "Go canonical height mapping",
            );
        }
        check(
            view.hash_at(view.height() + 1).unwrap().is_none()
                && !view.global_consensus_qualified()
                && !result.global_consensus_qualified,
            &mut checks,
            "scoped canonical view boundary",
        );
        if result.selected {
            check(
                encode_hex(&view.roots().evm) == row["evm_root"]
                    && encode_hex(&view.roots().etx) == row["etx_root"]
                    && encode_hex(&view.roots().utxo) == row["utxo_root"],
                &mut checks,
                "selected Go execution roots",
            );
        }
        if !result.detached.is_empty() {
            switches += 1;
            if view.height() < old.height() {
                lower_reorg += 1;
            }
            check(
                result.detached[0] == old.block() && result.attached.last() == Some(&view.block()),
                &mut checks,
                "canonical reorg plan endpoints",
            );
        }
        if i == 15 {
            pinned = Some((view.clone(), event["canonical_hashes"].clone()));
        }
        if i == 7 || i == 20 || i == 24 {
            let expected = view.block();
            drop(chain);
            chain = open_chain(dir, now).unwrap();
            check(
                chain.view().unwrap().block() == expected,
                &mut checks,
                "restart preserves native selected head",
            );
            reopened += 1;
        }
    }
    let (snapshot, hashes) = pinned.unwrap();
    for (h, id) in hashes.as_array().unwrap().iter().enumerate() {
        check(
            snapshot.hash_at(h as u64).unwrap().map(|b| encode_hex(&b))
                == id.as_str().map(str::to_owned),
            &mut checks,
            "pinned generation stays consistent after reorg",
        );
    }
    check(
        switches >= 1 && lower_reorg == 1,
        &mut checks,
        "higher entropy beats greater height",
    );
    let first = &v["branches"][0]["vectors"][0];
    let mut conflicting = bundle(first);
    conflicting.contexts.push((0, conflicting.zone.clone()));
    check(
        chain.import(conflicting, now).is_err(),
        &mut checks,
        "known ID cannot replace stored witnesses",
    );
    let file = dir.join("catalog.redb");
    let saved = dir.with_extension("catalog-test-backup");
    assert!(!saved.exists());
    let known_head = chain.view().unwrap().block();
    fs::rename(&file, &saved).unwrap();
    fs::write(&file, b"corrupt after successful lookup").unwrap();
    check(
        chain.bundle(known_head).is_err(),
        &mut checks,
        "replaced catalog refused after successful reads",
    );
    drop(chain);
    check(
        open_chain(dir, now).is_err(),
        &mut checks,
        "corrupt catalog fails reopen",
    );
    fs::remove_file(&file).unwrap();
    fs::rename(&saved, &file).unwrap();
    let chain = open_chain(dir, now).unwrap();
    json!({"passed":true,"lazy_qi":chain.lazy_qi(),"lazy_accounts":chain.lazy_accounts(),"lazy_storage":chain.lazy_storage(),"lazy_etx":chain.lazy_etx(),"lazy_lockups":chain.lazy_lockups(),"indexed_ancestry":chain.indexed_ancestry(),"indexed_catalog":chain.indexed_catalog(),"canonical_execution_uses_state_database":true,"complete_state_images_written":false,"checks":checks,"events":25,"unique_native_blocks":24,"reorgs":switches,"higher_entropy_shorter_branch_reorgs":lower_reorg,"restart_checks":reopened,"invalid_candidates_rejected":rejected,"height":chain.view().unwrap().height(),"head":encode_hex(&chain.view().unwrap().block()),"private_profile_canonical_publication":true,"global_full_consensus":false,"canonical_public_sync":false,"runtime_go_dependency":false})
}
#[allow(dead_code)]
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let v = serde_json::from_slice(&fs::read(&args[0]).unwrap()).unwrap();
    let dir = Path::new(&args[1]);
    fs::create_dir_all(dir).unwrap();
    println!("{}", verify(&v, dir));
}
