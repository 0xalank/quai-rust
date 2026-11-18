#[path = "../examples/check-private-branches.rs"]
mod check;
use quai_consensus::{private_branch::PrivateBranch, verifier::Verifier};
use quai_primitives::{decode_hex, encode_hex};
use std::{
    fs,
    path::{Path, PathBuf},
};
fn corpus() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../fixtures/protocol/private-branches.json"
    ))
    .unwrap()
}
fn directory(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "quai-complete-branch-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&d).unwrap();
    d
}
#[test]
fn durable_execution_branches_and_receipts_match_actual_go_and_recover() {
    let d = directory("fork");
    let report = check::verify(&corpus(), &d);
    assert_eq!(report["passed"], true);
    assert_eq!(report["unique_native_blocks_executed"], 24);
    println!("PRIVATE_BRANCH_CHECKS {}", report);
    fs::remove_dir_all(d).unwrap();
}
#[test]
fn complete_image_recovery_child() {
    let Ok(d) = std::env::var("QUAI_COMPLETE_BRANCH_CHILD") else {
        return;
    };
    let mut store = PrivateBranch::open(Path::new(&d)).unwrap();
    if std::env::var("QUAI_COMPLETE_BRANCH_PHASE").unwrap() == "selected" {
        let next = decode_hex(&std::env::var("QUAI_COMPLETE_BRANCH_NEXT").unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        store.select_execution_head(store.head(), next).unwrap();
    }
    std::process::exit(72);
}
#[test]
fn process_exit_recovers_the_complete_last_selected_image_and_historical_context() {
    let c = corpus();
    let rows = c["branches"][0]["vectors"].as_array().unwrap();
    let mut verifier = Verifier::default();
    for phase in ["staged", "selected"] {
        let d = directory(phase);
        let mut s = PrivateBranch::open(&d).unwrap();
        let first = s
            .execute_candidate(
                &mut verifier,
                s.head(),
                &check::bytes(&rows[0]["work_object"]),
                &check::incoming(&rows[0]),
            )
            .unwrap();
        s.select_execution_head(s.head(), first.checkpoint).unwrap();
        let next = s
            .execute_candidate(
                &mut verifier,
                s.head(),
                &check::bytes(&rows[1]["work_object"]),
                &check::incoming(&rows[1]),
            )
            .unwrap();
        drop(s);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "complete_image_recovery_child"])
            .env("QUAI_COMPLETE_BRANCH_CHILD", &d)
            .env("QUAI_COMPLETE_BRANCH_PHASE", phase)
            .env("QUAI_COMPLETE_BRANCH_NEXT", encode_hex(&next.checkpoint))
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(72));
        let s = PrivateBranch::open(&d).unwrap();
        let restored = s.restore(s.head()).unwrap();
        let index = usize::from(phase == "selected");
        assert_eq!(
            encode_hex(&restored.anchor.block),
            rows[index]["block_hash"]
        );
        assert_eq!(
            encode_hex(&restored.state.world.root().unwrap()),
            rows[index]["evm_root"]
        );
        assert_eq!(restored.history.len(), index + 2);
        assert_eq!(
            restored
                .receipt_rlp
                .iter()
                .map(|r| encode_hex(r))
                .collect::<Vec<_>>(),
            rows[index]["receipt_rlp"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        );
        drop(s);
        fs::remove_dir_all(d).unwrap();
    }
}
