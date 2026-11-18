use crate::{
    confirmation::Hierarchy,
    history::Record,
    private_profile::{PrivateChain, ALL_ETX_BYTES},
    runtime_fixture as base,
    verifier::Verifier,
    Error,
};
use quai_primitives::decode_hex;
use serde_json::Value;
#[test]
fn all_etx_contract_is_frozen_distinct_and_keeps_the_actual_go_genesis() {
    let old = PrivateChain::execution_template().unwrap();
    let p = PrivateChain::all_etx_template().unwrap();
    assert_ne!(old.fingerprint, p.fingerprint);
    assert_eq!(old.genesis[2].bytes(), p.genesis[2].bytes());
    assert!(p.activated_hierarchy && p.all_etx_hierarchy);
    assert!(!old.all_etx_hierarchy);
    assert!(
        PrivateChain::hierarchy_for_fingerprint(p.fingerprint)
            .unwrap()
            .all_etx_hierarchy
    );
    let mut changed = ALL_ETX_BYTES.to_vec();
    changed.push(b' ');
    assert!(PrivateChain::decode(&changed).is_err());
}
#[test]
fn actual_runtime_dispatch_checks_all_defined_etx_families_and_bound_views() {
    let v: Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/etx-family.json")).unwrap();
    let now = v["now"].as_u64().unwrap();
    let mut verifier = Verifier::default();
    let mut prepared = 0;
    for branch in v["branches"].as_array().unwrap() {
        let (_, _, index, _) = base::inputs(branch);
        let reader = base::reader(branch, &index);
        let mut old = Hierarchy::from_reader_for_chain(
            reader.clone(),
            PrivateChain::execution_template().unwrap(),
        )
        .unwrap();
        let mut active =
            Hierarchy::from_reader_for_chain(reader, PrivateChain::all_etx_template().unwrap())
                .unwrap();
        for (i, row) in branch["vectors"].as_array().unwrap().iter().enumerate() {
            let block =
                Record::decode(&decode_hex(row["work_object"].as_str().unwrap()).unwrap()).unwrap();
            let views: Vec<_> = row["contexts"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r["context"] != 2)
                .map(|r| {
                    (
                        r["context"].as_u64().unwrap() as u8,
                        decode_hex(r["work_object"].as_str().unwrap()).unwrap(),
                    )
                })
                .collect();
            let before = active.heads();
            if i == 0 {
                assert!(matches!(
                    old.prepare_for_runtime(&mut verifier, &block, &views, now),
                    Err(Error::Unsupported(_))
                ));
                let permit = active
                    .prepare_for_runtime(&mut verifier, &block, &views, now)
                    .unwrap();
                assert!(matches!(old.commit(permit), Err(Error::Invalid(_))));
                assert_eq!(active.heads(), before);
            }
            if !views.is_empty() {
                assert!(active
                    .prepare_for_runtime(&mut verifier, &block, &[], now)
                    .is_err());
                assert_eq!(active.heads(), before);
            }
            let permit = active
                .prepare_for_runtime(&mut verifier, &block, &views, now)
                .unwrap();
            active.commit(permit).unwrap();
            prepared += 1;
        }
    }
    assert_eq!(prepared, 32);
}
