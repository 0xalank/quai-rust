#[allow(dead_code)]
#[path = "../examples/check-canonical.rs"]
mod checker;
use quai_consensus::private_canonical::PrivateCanonical;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
fn temporary() -> PathBuf {
    std::env::temp_dir().join(format!(
        "quai-genesis-runtime-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
fn files(path: &Path) -> BTreeMap<PathBuf, [u8; 32]> {
    fn walk(root: &Path, p: &Path, out: &mut BTreeMap<PathBuf, [u8; 32]>) {
        for e in std::fs::read_dir(p).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out)
            } else {
                out.insert(
                    p.strip_prefix(root).unwrap().to_owned(),
                    quai_primitives::transaction::keccak(&std::fs::read(p).unwrap()),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(path, path, &mut out);
    out
}
#[test]
fn genesis_execution_profile_retains_go_canonical_prefix_fork_and_recovery() {
    let v: Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json")).unwrap();
    let dir = temporary();
    let report = checker::verify_execution_profile(&v, &dir);
    assert_eq!(report["passed"], true);
    assert_eq!(report["checks"], 488);
    let chain = PrivateCanonical::open_execution_v2(&dir, v["now"].as_u64().unwrap()).unwrap();
    assert_eq!(chain.profile_name(), "private-execution-v2");
    assert!(chain.activated_execution());
    drop(chain);
    let before = files(&dir);
    assert!(PrivateCanonical::open(&dir, v["now"].as_u64().unwrap()).is_err());
    assert_eq!(before, files(&dir));
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn genesis_execution_profile_cannot_import_a_supplied_activation_checkpoint() {
    let v: Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/coincident.json")).unwrap();
    let now = v["now"].as_u64().unwrap();
    let dir = temporary();
    let mut chain = PrivateCanonical::open_execution_v2(&dir, now).unwrap();
    let head = chain.view().unwrap();
    let cp = chain.durable_checkpoint().unwrap();
    let count = chain.admitted_count();
    assert!(chain
        .import(checker::bundle(&v["branches"][0]["vectors"][0]), now)
        .is_err());
    assert_eq!(chain.view().unwrap().block(), head.block());
    assert_eq!(chain.durable_checkpoint().unwrap(), cp);
    assert_eq!(chain.admitted_count(), count);
    assert_eq!(head.height(), 0);
    drop(chain);
    let before = files(&dir);
    assert!(PrivateCanonical::open(&dir, now).is_err());
    assert_eq!(before, files(&dir));
    let reopened = PrivateCanonical::open_execution_v2(&dir, now).unwrap();
    assert_eq!(reopened.view().unwrap().height(), 0);
    drop(reopened);
    std::fs::remove_dir_all(dir).unwrap();
}
