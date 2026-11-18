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
        "quai-all-etx-runtime-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
fn files(path: &Path) -> BTreeMap<PathBuf, [u8; 32]> {
    fn walk(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, [u8; 32]>) {
        for e in std::fs::read_dir(path).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out)
            } else {
                out.insert(
                    p.strip_prefix(root).unwrap().into(),
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
fn all_etx_native_runtime_retains_real_genesis_prefix_forks_and_recovery() {
    let v: Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json")).unwrap();
    let dir = temporary();
    let report = checker::verify_all_etx_profile(&v, &dir);
    assert_eq!(report["passed"], true);
    assert_eq!(report["checks"], 488);
    let c = PrivateCanonical::open_execution_v3(&dir, v["now"].as_u64().unwrap()).unwrap();
    assert!(c.all_etx_execution());
    assert!(c.activated_execution());
    assert_eq!(c.profile_name(), "private-execution-v3");
    drop(c);
    let before = files(&dir);
    assert!(PrivateCanonical::open_execution_v2(&dir, v["now"].as_u64().unwrap()).is_err());
    assert!(PrivateCanonical::open(&dir, v["now"].as_u64().unwrap()).is_err());
    assert_eq!(before, files(&dir));
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn all_etx_native_runtime_refuses_supplied_checkpoint_and_cross_profile_state() {
    let v: Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/etx-family.json")).unwrap();
    let now = v["now"].as_u64().unwrap();
    let dir = temporary();
    let mut chain = PrivateCanonical::open_execution_v3(&dir, now).unwrap();
    let before = chain.view().unwrap();
    let cp = chain.durable_checkpoint().unwrap();
    let count = chain.admitted_count();
    assert!(chain
        .import(checker::bundle(&v["branches"][0]["vectors"][0]), now)
        .is_err());
    assert_eq!(chain.view().unwrap().block(), before.block());
    assert_eq!(chain.view().unwrap().height(), 0);
    assert_eq!(chain.durable_checkpoint().unwrap(), cp);
    assert_eq!(chain.admitted_count(), count);
    drop(before);
    drop(chain);
    let saved = files(&dir);
    assert!(PrivateCanonical::open_execution_v2(&dir, now).is_err());
    assert_eq!(saved, files(&dir));
    let chain = PrivateCanonical::open_execution_v3(&dir, now).unwrap();
    assert_eq!(chain.view().unwrap().height(), 0);
    drop(chain);
    std::fs::remove_dir_all(dir).unwrap();
}
