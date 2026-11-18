mod prime_economics_support;
#[test]
fn composed_parent_conversion_and_controller_fields_match_unmodified_go_verifier() {
    let corpus = serde_json::from_str(include_str!(
        "../../../fixtures/protocol/prime-economics.json"
    ))
    .unwrap();
    println!("{}", prime_economics_support::verify(&corpus));
}

#[test]
fn parent_controller_scope_is_explicit_and_inbound_lists_are_unbounded_as_in_go() {
    use quai_consensus::{choice_window::Window, history::Record, prime_economics, Error};
    use quai_primitives::{
        decode_hex,
        transaction::{Kind, Transaction},
    };
    let v: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/protocol/prime-economics.json"
    ))
    .unwrap();
    let row = &v["vectors"][2];
    let parent = Record::decode(&decode_hex(row["parent"].as_str().unwrap()).unwrap()).unwrap();
    let tx =
        Transaction::decode(&decode_hex(row["inbound"][0].as_str().unwrap()).unwrap()).unwrap();
    // core/slice.go Append prices any number of inbound ETXs; the former
    // 8,192-ETX and 32 MiB caps were Rust-only.
    let many = prime_economics::conversions(&parent, &vec![tx.clone(); 8193], None).unwrap();
    assert_eq!(many.etxs.len(), 8193);
    let Kind::External(mut large) = tx.kind().clone() else {
        unreachable!()
    };
    large.data = vec![7; 128 * 1024];
    let large = Transaction::external(large).unwrap();
    assert_eq!(
        prime_economics::conversions(&parent, &vec![large; 257], None)
            .unwrap()
            .etxs
            .len(),
        257
    );
    let canonical: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json")).unwrap();
    let genesis =
        Record::decode(&decode_hex(canonical["genesis"].as_str().unwrap()).unwrap()).unwrap();
    let prior = Window::empty_at(genesis.id(), 0);
    assert!(matches!(
        prime_economics::prepare_component(&genesis, &genesis, None, &prior, None, None),
        Err(Error::Unsupported(_))
    ));
}
