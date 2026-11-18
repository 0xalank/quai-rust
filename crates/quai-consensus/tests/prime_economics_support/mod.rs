use crypto_bigint::{CheckedAdd, CheckedMul};
use quai_consensus::{
    choice_window::Window,
    history::Record,
    prime_economics::{self, Override},
    rules::Uint,
    Error,
};
use quai_primitives::{decode_hex, transaction::Transaction};
use serde_json::{json, Value};
fn uint(v: &Value) -> Uint {
    let mut n = Uint::ZERO;
    for c in v.as_str().unwrap().bytes() {
        assert!(c.is_ascii_digit());
        n = Option::<Uint>::from(n.checked_mul(&Uint::from_u64(10))).unwrap();
        n = Option::<Uint>::from(n.checked_add(&Uint::from_u64((c - b'0') as u64))).unwrap();
    }
    n
}
fn record(v: &Value) -> Record {
    Record::decode(&decode_hex(v.as_str().unwrap()).unwrap()).unwrap()
}
pub fn verify(value: &Value) -> Value {
    assert_eq!(
        value["reference_revision"],
        quai_primitives::REFERENCE_REVISION
    );
    assert_eq!(value["go_field_mutations_rejected"], 768);
    assert_eq!(value["go_missing_history_rejected"], 512);
    let mut checks = 0;
    let mut compared = 0;
    let mut missing = 0;
    let mut binding = 0;
    let mut copied = 0;
    for row in value["vectors"].as_array().unwrap() {
        let parent = record(&row["parent"]);
        let grand = record(&row["grandparent"]);
        let child = record(&row["child"]);
        assert_eq!(
            parent.id().as_slice(),
            decode_hex(row["parent_hash"].as_str().unwrap()).unwrap()
        );
        assert_eq!(
            grand.id().as_slice(),
            decode_hex(row["grandparent_hash"].as_str().unwrap()).unwrap()
        );
        assert_eq!(
            child.id().as_slice(),
            decode_hex(row["child_hash"].as_str().unwrap()).unwrap()
        );
        checks += 3;
        assert!(row["unmodified_verifier_accepted"].as_bool().unwrap());
        let etxs = row["inbound"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| Transaction::decode(&decode_hex(v.as_str().unwrap()).unwrap()).unwrap())
            .collect::<Vec<_>>();
        let original = etxs.clone();
        let prior = Window::empty_at(grand.id(), grand.number(0).unwrap());
        let digest = prior.logical_digest();
        let old = Some(uint(&row["old_exchange"]));
        let authority = Override {
            block: parent.parent(2).unwrap(),
            exchange: uint(&row["stored_exchange"]),
            update_bit: row["update_bit"].as_u64().unwrap() as u8,
        };
        let result = prime_economics::prepare_component(
            &parent,
            &grand,
            Some(&etxs),
            &prior,
            old,
            Some(authority),
        )
        .unwrap_or_else(|e| panic!("case {}: {e:?}", row["case"]));
        let fields = result.fields();
        assert_eq!(
            fields.conversion_flow,
            uint(&row["flow"]),
            "flow case {}",
            row["case"]
        );
        assert_eq!(
            fields.exchange_rate,
            uint(&row["exchange"]),
            "exchange case {}",
            row["case"]
        );
        assert_eq!(
            fields.discount,
            uint(&row["discount"]),
            "discount case {}",
            row["case"]
        );
        assert_eq!(
            fields.miner_difficulty,
            uint(&row["miner"]),
            "miner case {}",
            row["case"]
        );
        compared += 4;
        checks += 4;
        result
            .verify_child(&child)
            .unwrap_or_else(|e| panic!("child case {}: {e:?}", row["case"]));
        checks += 1;
        assert!(result.verify_child(&parent).is_err());
        binding += 1;
        checks += 1;
        assert_eq!(result.window().block(), parent.id());
        assert_eq!(result.window().number(), parent.number(0).unwrap());
        checks += 2;
        assert_eq!(prior.logical_digest(), digest);
        assert_eq!(etxs, original);
        checks += 2;
        copied = copied.max(result.window().copied_rows());
        assert!(matches!(
            prime_economics::prepare_component(&parent, &grand, None, &prior, old, Some(authority)),
            Err(Error::Unavailable(_))
        ));
        missing += 1;
        checks += 1;
        assert!(matches!(
            prime_economics::prepare_component(
                &parent,
                &grand,
                Some(&etxs),
                &prior,
                None,
                Some(authority)
            ),
            Err(Error::Unavailable(_))
        ));
        missing += 1;
        checks += 1;
        assert!(matches!(
            prime_economics::prepare_component(&parent, &grand, Some(&etxs), &prior, old, None),
            Err(Error::Unavailable(_))
        ));
        missing += 1;
        checks += 1;
        let wrong = Override {
            block: [55; 32],
            ..authority
        };
        assert!(matches!(
            prime_economics::prepare_component(
                &parent,
                &grand,
                Some(&etxs),
                &prior,
                old,
                Some(wrong)
            ),
            Err(Error::Invalid(_))
        ));
        binding += 1;
        checks += 1;
        let wrong = Window::empty_at([55; 32], grand.number(0).unwrap());
        assert!(matches!(
            prime_economics::prepare_component(
                &parent,
                &grand,
                Some(&etxs),
                &wrong,
                old,
                Some(authority)
            ),
            Err(Error::Invalid(_))
        ));
        binding += 1;
        checks += 1;
        let wrong = Window::empty_at(grand.id(), grand.number(0).unwrap() - 1);
        assert!(matches!(
            prime_economics::prepare_component(
                &parent,
                &grand,
                Some(&etxs),
                &wrong,
                old,
                Some(authority)
            ),
            Err(Error::Invalid(_))
        ));
        binding += 1;
        checks += 1;
        assert!(matches!(
            prime_economics::prepare_component(
                &parent,
                &parent,
                Some(&etxs),
                &prior,
                old,
                Some(authority)
            ),
            Err(Error::Invalid(_))
        ));
        binding += 1;
        checks += 1;
    }
    assert_eq!(compared, 1024);
    assert_eq!(missing, 768);
    assert_eq!(binding, 1280);
    assert!(copied <= 64);
    json!({"passed":true,"cases":256,"go_field_comparisons":compared,"native_checks":checks,"native_missing_inputs_rejected":missing,"native_bindings_rejected":binding,"go_field_mutations_rejected":768,"go_missing_history_rejected":512,"max_window_rows_copied":copied,"supplied_history":true,"canonical_authority":false,"full_consensus":false,"runtime_go_dependency":false})
}
