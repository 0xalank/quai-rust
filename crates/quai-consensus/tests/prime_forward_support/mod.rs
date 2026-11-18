use crypto_bigint::{CheckedAdd, CheckedMul, U512};
use quai_consensus::{
    choice_window::Window,
    history::Record,
    prime_economics::{self, Override},
    prime_forward,
    rules::Uint,
    Error,
};
use quai_primitives::{
    decode_hex, encode_hex, transaction::Transaction, work::transactions_root, Hash,
};
use quai_store::state_db::StateDatabase;
use serde_json::{json, Value};
use std::{
    io,
    path::{Path, PathBuf},
};
fn uint(v: &Value) -> Uint {
    let mut n = Uint::ZERO;
    for c in v.as_str().unwrap().bytes() {
        assert!(c.is_ascii_digit());
        n = Option::<Uint>::from(n.checked_mul(&Uint::from_u64(10))).unwrap();
        n = Option::<Uint>::from(n.checked_add(&Uint::from_u64((c - b'0') as u64))).unwrap();
    }
    n
}
fn wide(v: &Value) -> U512 {
    let mut n = U512::ZERO;
    for c in v.as_str().unwrap().bytes() {
        assert!(c.is_ascii_digit());
        n = Option::<U512>::from(n.checked_mul(&U512::from_u64(10))).unwrap();
        n = Option::<U512>::from(n.checked_add(&U512::from_u64((c - b'0') as u64))).unwrap();
    }
    n
}
fn hash(v: &Value) -> Hash {
    decode_hex(v.as_str().unwrap()).unwrap().try_into().unwrap()
}
fn record(v: &Value) -> Record {
    Record::decode(&decode_hex(v.as_str().unwrap()).unwrap()).unwrap()
}
fn txs(v: &Value) -> Vec<Transaction> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| Transaction::decode(&decode_hex(s.as_str().unwrap()).unwrap()).unwrap())
        .collect()
}
fn authority(block: &Record, row: &Value) -> Override {
    Override {
        block: block.parent(2).unwrap(),
        exchange: uint(&row["stored_exchange"]),
        update_bit: row["update_bit"].as_u64().unwrap() as u8,
    }
}
pub fn verify(v: &Value, dir: &Path) -> Value {
    assert_eq!(v["reference_revision"], quai_primitives::REFERENCE_REVISION);
    assert_eq!(v["forward_steps"], 1024);
    assert_eq!(v["undefined_zero_probe_cases"], 240);
    let mut steps = 0;
    let mut undefined = 0;
    let mut missing = 0;
    let mut bindings = 0;
    let mut outputs = 0;
    let mut reopens = 0;
    let mut persisted = 0;
    let mut aborts = 0;
    let mut max_rows = 0;
    let mut max_writes = 0;
    let mut total_refunds = 0;
    for case in v["vectors"].as_array().unwrap() {
        let seed = &case["seed"];
        let mut grand = record(&seed["grandparent"]);
        let mut parent = record(&seed["parent"]);
        assert_eq!(grand.id(), hash(&seed["grandparent_hash"]));
        assert_eq!(parent.id(), hash(&seed["parent_hash"]));
        let mut parent_input = txs(&seed["inbound"]);
        let mut parent_old = Some(uint(&seed["old_exchange"]));
        let mut parent_override = authority(&parent, seed);
        let mut grand_window = Window::empty_at(grand.id(), grand.number(0).unwrap());
        let parent_result = prime_economics::prepare_component(
            &parent,
            &grand,
            Some(&parent_input),
            &grand_window,
            parent_old,
            Some(parent_override),
        )
        .unwrap();
        let mut parent_window = parent_result.window().clone();
        assert_eq!(
            encode_hex(&parent_window.logical_digest()),
            seed["window_sha256"]
        );
        assert_eq!(
            parent_window.difficulty_sum(),
            wide(&seed["difficulty_sum"])
        );
        let index = case["case"].as_u64().unwrap();
        let durable = [0, 1, 6, 11, 20, 23, 26, 27].contains(&index);
        let mut disk: Option<(StateDatabase, Hash, PathBuf)> = None;
        if durable {
            let path = dir.join(format!("forward-{index}.redb"));
            let db = StateDatabase::open(&path, [117; 32]).unwrap();
            let mut initial = None;
            let first = db
                .commit(
                    None,
                    None,
                    grand.id(),
                    b"supplied controller component seed",
                    true,
                    |ws| {
                        initial = Some(grand_window.persist(ws, None)?);
                        Ok(())
                    },
                )
                .unwrap();
            let saved_grand = initial.unwrap();
            let mut saved = None;
            let next = db
                .commit(
                    Some(first.version.id),
                    Some(first.version.id),
                    parent.id(),
                    b"computed parent choice component",
                    true,
                    |ws| {
                        saved = Some(parent_window.persist(ws, Some(&saved_grand))?);
                        Ok(())
                    },
                )
                .unwrap();
            grand_window = saved_grand;
            parent_window = saved.unwrap();
            disk = Some((db, next.version.id, path));
        }
        for (offset, row) in case["steps"].as_array().unwrap().iter().enumerate() {
            let block = record(&row["block"]);
            assert_eq!(block.id(), hash(&row["block_hash"]));
            assert!(row["parent_verifier_accepted"].as_bool().unwrap());
            let checked_parent = prime_economics::prepare_component(
                &parent,
                &grand,
                Some(&parent_input),
                &grand_window,
                parent_old,
                Some(parent_override),
            )
            .unwrap_or_else(|e| panic!("case {index} step {offset} parent: {e:?}"));
            assert_eq!(
                checked_parent.window().logical_digest(),
                parent_window.logical_digest()
            );
            checked_parent.verify_child(&block).unwrap();
            let input = txs(&row["inbound"]);
            let original = input.clone();
            let old = Some(uint(&row["old_exchange"]));
            let scoped = authority(&block, row);
            let previous_digest = parent_window.logical_digest();
            let result = prime_forward::prepare_component(
                &block,
                &parent,
                Some(&input),
                &parent_window,
                old,
                Some(scoped),
            )
            .unwrap_or_else(|e| panic!("case {index} step {offset} forward: {e:?}"));
            assert_eq!(result.block(), block.id());
            assert_eq!(
                result.forward_rate(),
                uint(&row["forward_rate_reference"]),
                "rate case {index} step {offset}"
            );
            let expected = row["output"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| decode_hex(s.as_str().unwrap()).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                result
                    .output()
                    .iter()
                    .map(Transaction::encode)
                    .collect::<Vec<_>>(),
                expected,
                "output case {index} step {offset}"
            );
            assert_eq!(
                transactions_root(result.output()).unwrap(),
                hash(&row["output_root"])
            );
            assert_eq!(result.refunds(), row["refunds"].as_u64().unwrap() as usize);
            assert_eq!(
                encode_hex(&result.window().logical_digest()),
                row["window_sha256"],
                "window case {index} step {offset}"
            );
            assert_eq!(
                result.window().difficulty_sum(),
                wide(&row["difficulty_sum"])
            );
            assert_eq!(parent_window.logical_digest(), previous_digest);
            assert_eq!(input, original);
            outputs += result.output().len();
            total_refunds += result.refunds();
            max_rows = max_rows.max(result.window().copied_rows());
            if offset == 0 {
                assert!(matches!(
                    prime_forward::prepare_component(
                        &block,
                        &parent,
                        None,
                        &parent_window,
                        old,
                        Some(scoped)
                    ),
                    Err(Error::Unavailable(_))
                ));
                missing += 1;
                assert!(matches!(
                    prime_forward::prepare_component(
                        &block,
                        &parent,
                        Some(&input),
                        &parent_window,
                        None,
                        Some(scoped)
                    ),
                    Err(Error::Unavailable(_))
                ));
                missing += 1;
                assert!(matches!(
                    prime_forward::prepare_component(
                        &block,
                        &parent,
                        Some(&input),
                        &parent_window,
                        old,
                        None
                    ),
                    Err(Error::Unavailable(_))
                ));
                missing += 1;
                let bad = Override {
                    block: [55; 32],
                    ..scoped
                };
                assert!(matches!(
                    prime_forward::prepare_component(
                        &block,
                        &parent,
                        Some(&input),
                        &parent_window,
                        old,
                        Some(bad)
                    ),
                    Err(Error::Invalid(_))
                ));
                bindings += 1;
                let bad = Window::empty_at([55; 32], parent.number(0).unwrap());
                assert!(matches!(
                    prime_forward::prepare_component(
                        &block,
                        &parent,
                        Some(&input),
                        &bad,
                        old,
                        Some(scoped)
                    ),
                    Err(Error::Invalid(_))
                ));
                bindings += 1;
                let bad = Window::empty_at(parent.id(), parent.number(0).unwrap() - 1);
                assert!(matches!(
                    prime_forward::prepare_component(
                        &block,
                        &parent,
                        Some(&input),
                        &bad,
                        old,
                        Some(scoped)
                    ),
                    Err(Error::Invalid(_))
                ));
                bindings += 1;
                assert!(matches!(
                    prime_forward::prepare_component(
                        &block,
                        &block,
                        Some(&input),
                        &parent_window,
                        old,
                        Some(scoped)
                    ),
                    Err(Error::Invalid(_))
                ));
                bindings += 1;
                if !case["undefined_zero_probe"].is_null() {
                    let probe = &case["undefined_zero_probe"];
                    assert_eq!(probe["panic_observed"], true);
                    assert_eq!(probe["batch_committed"], false);
                    let unsafe_input = txs(&probe["inbound"]);
                    assert!(matches!(
                        prime_forward::prepare_component(
                            &block,
                            &parent,
                            Some(&unsafe_input),
                            &parent_window,
                            old,
                            Some(scoped)
                        ),
                        Err(Error::Unsupported(_))
                    ));
                    assert_eq!(parent_window.logical_digest(), previous_digest);
                    undefined += 1;
                }
            }
            let mut next_window = result.window().clone();
            if let Some((db, version, path)) = disk.take() {
                if offset == 0 {
                    let failure = db.commit(
                        Some(version),
                        Some(version),
                        block.id(),
                        b"must abort forward state",
                        true,
                        |ws| {
                            result.persist_window(ws, &parent_window)?;
                            Err(io::Error::other("injected after forward choice writes"))
                        },
                    );
                    assert!(failure.is_err());
                    assert_eq!(db.head().unwrap(), Some(version));
                    assert_eq!(
                        Window::load(&db.snapshot(Some(version)).unwrap())
                            .unwrap()
                            .logical_digest(),
                        previous_digest
                    );
                    aborts += 1;
                }
                let mut saved = None;
                let committed = db
                    .commit(
                        Some(version),
                        Some(version),
                        block.id(),
                        b"conditional native forward component",
                        true,
                        |ws| {
                            saved = Some(result.persist_window(ws, &parent_window)?);
                            Ok(())
                        },
                    )
                    .unwrap();
                let stored = saved.unwrap();
                max_writes = max_writes.max(stored.persisted_row_writes().unwrap());
                assert_eq!(stored.logical_digest(), result.window().logical_digest());
                assert_eq!(db.head().unwrap(), Some(committed.version.id));
                persisted += 1;
                if offset % 2 == 1 {
                    drop(db);
                    let reopened = StateDatabase::open(&path, [117; 32]).unwrap();
                    let loaded =
                        Window::load(&reopened.snapshot(Some(committed.version.id)).unwrap())
                            .unwrap();
                    assert_eq!(loaded.logical_digest(), stored.logical_digest());
                    assert_eq!(loaded.block(), block.id());
                    next_window = loaded;
                    reopens += 1;
                    disk = Some((reopened, committed.version.id, path));
                } else {
                    next_window = stored;
                    disk = Some((db, committed.version.id, path));
                }
            }
            grand_window = parent_window;
            parent_window = next_window;
            grand = parent;
            parent = block;
            parent_input = input;
            parent_old = old;
            parent_override = scoped;
            steps += 1;
        }
        drop(disk);
    }
    assert_eq!(steps, 1024);
    assert_eq!(undefined, 240);
    assert_eq!(missing, 768);
    assert_eq!(bindings, 1024);
    assert_eq!(persisted, 32);
    assert_eq!(reopens, 16);
    assert_eq!(aborts, 8);
    assert!(max_rows <= 64 && max_writes <= 1);
    json!({"passed":true,"histories":256,"linked_forward_steps":steps,"output_sequence_comparisons":steps,"output_root_comparisons":steps,"forward_rate_comparisons":steps,"choice_window_digest_comparisons":steps,"choice_sum_comparisons":steps,"native_header_verifications":steps,"matched_etx_encodings":outputs,"reference_refunds":total_refunds,"native_missing_inputs_rejected":missing,"native_bindings_rejected":bindings,"reference_undefined_zero_cases":undefined,"native_undefined_zero_cases_refused":undefined,"persisted_forward_updates":persisted,"writer_reopens":reopens,"atomic_aborts":aborts,"max_window_rows_copied":max_rows,"max_persisted_row_writes":max_writes,"supplied_seed":true,"canonical_authority":false,"full_consensus":false,"runtime_go_dependency":false})
}
