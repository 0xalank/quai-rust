use crypto_bigint::{CheckedAdd, CheckedMul, U512};
use quai_consensus::{
    choice_window::{self, Choice, Window},
    history::Record,
    rules::Uint,
};
use quai_primitives::{decode_hex, encode_hex, transaction::Transaction, Hash};
use quai_store::state_db::StateDatabase;
use serde_json::{json, Value};
use std::{io, path::Path};
fn hash(v: &Value) -> Hash {
    decode_hex(v.as_str().unwrap()).unwrap().try_into().unwrap()
}
fn uint(v: &Value) -> Uint {
    let mut out = Uint::ZERO;
    for c in v.as_str().unwrap().bytes() {
        assert!(c.is_ascii_digit());
        out = Option::<Uint>::from(out.checked_mul(&Uint::from_u64(10))).unwrap();
        out = Option::<Uint>::from(out.checked_add(&Uint::from_u64((c - b'0') as u64))).unwrap()
    }
    out
}
fn sum(v: &Value) -> U512 {
    let mut out = U512::ZERO;
    for c in v.as_str().unwrap().bytes() {
        assert!(c.is_ascii_digit());
        out = Option::<U512>::from(out.checked_mul(&U512::from_u64(10))).unwrap();
        out = Option::<U512>::from(out.checked_add(&U512::from_u64((c - b'0') as u64))).unwrap()
    }
    out
}
fn derive(row: &Value) -> Choice {
    let parent = Record::decode(&decode_hex(row["parent"].as_str().unwrap()).unwrap()).unwrap();
    let etxs: Vec<_> = row["etxs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| Transaction::decode(&decode_hex(v.as_str().unwrap()).unwrap()).unwrap())
        .collect();
    choice_window::derive(
        &parent,
        uint(&row["exchange"]),
        &etxs,
        uint(&row["actual"]),
        uint(&row["realized"]),
        uint(&row["miner"]),
    )
    .unwrap()
}
pub fn verify(v: &Value, dir: &Path) -> Value {
    assert_eq!(v["reference_revision"], quai_primitives::REFERENCE_REVISION);
    let mut database = StateDatabase::open(&dir.join("choices.redb"), [71; 32]).unwrap();
    let seed = Window::empty_at(hash(&v["seed_key"]), v["seed_number"].as_u64().unwrap());
    let mut initial = None;
    let committed = database
        .commit(
            None,
            None,
            seed.block(),
            b"conditional controller seed",
            true,
            |ws| {
                initial = Some(seed.persist(ws, None)?);
                Ok(())
            },
        )
        .unwrap();
    let mut parent_version = committed.version.id;
    let mut memory = seed;
    let mut stored = initial.unwrap();
    let mut selected = Some(parent_version);
    let mut checkpoint: Option<(Window, Window, Hash)> = None;
    let mut main_tip: Option<(Window, Window, Hash)> = None;
    let mut max_copied_rows = 0usize;
    let mut max_row_writes = 0usize;
    let mut total_row_writes = 0usize;
    let mut checks = 0usize;
    let mut rows_count = 0;
    let mut reopens = 0;
    let mut aborts = 0;
    let mut branch_checks = 0;
    for run in v["runs"].as_array().unwrap() {
        let branch = run["branch"].as_u64().unwrap();
        if branch == 1 {
            let (m, s, id) = checkpoint.as_ref().unwrap();
            memory = m.clone();
            stored = s.clone();
            parent_version = *id;
        }
        for (offset, row) in run["rows"].as_array().unwrap().iter().enumerate() {
            let c = derive(row);
            assert_eq!(c.quai, row["newest"]["quai"].as_u64().unwrap());
            assert_eq!(c.qi, row["newest"]["qi"].as_u64().unwrap());
            assert_eq!(c.difficulty, uint(&row["newest"]["difficulty"]));
            checks += 3;
            let child = memory
                .advance(
                    hash(&row["block_key"]),
                    hash(&row["parent_key"]),
                    row["number"].as_u64().unwrap(),
                    c,
                )
                .unwrap();
            let child_stored = stored
                .advance(child.block(), stored.block(), child.number(), c)
                .unwrap();
            let original = stored.logical_digest();
            if branch == 0 && offset == 1024 {
                let mut aborted = None;
                let failure = database.commit(
                    selected,
                    Some(parent_version),
                    child.block(),
                    b"must abort",
                    true,
                    |ws| {
                        aborted = Some(child_stored.persist(ws, Some(&stored))?);
                        Err(io::Error::other("injected after window writes"))
                    },
                );
                assert!(failure.is_err());
                assert_eq!(database.head().unwrap(), selected);
                assert_eq!(
                    Window::load(&database.snapshot(Some(parent_version)).unwrap())
                        .unwrap()
                        .logical_digest(),
                    original
                );
                let uncommitted = aborted.unwrap();
                let grandchild = uncommitted
                    .advance([31; 32], uncommitted.block(), uncommitted.number() + 1, c)
                    .unwrap();
                assert!(database
                    .commit(
                        selected,
                        Some(parent_version),
                        [31; 32],
                        b"aborted parent",
                        false,
                        |ws| {
                            grandchild.persist(ws, Some(&uncommitted))?;
                            Ok(())
                        }
                    )
                    .is_err());
                assert_eq!(database.head().unwrap(), selected);
                aborts += 2;
            }
            let mut saved = None;
            let commit = database
                .commit(
                    selected,
                    Some(parent_version),
                    child.block(),
                    b"conditional native controller output",
                    branch == 0,
                    |ws| {
                        saved = Some(child_stored.persist(ws, Some(&stored))?);
                        Ok(())
                    },
                )
                .unwrap();
            parent_version = commit.version.id;
            if branch == 0 {
                selected = Some(parent_version)
            }
            let next = saved.unwrap();
            max_copied_rows = max_copied_rows.max(child.copied_rows());
            let writes = next.persisted_row_writes().unwrap();
            max_row_writes = max_row_writes.max(writes);
            total_row_writes += writes;
            assert!(child.copied_rows() <= 64 && writes <= 1);
            assert_eq!(encode_hex(&child.logical_digest()), row["window_sha256"]);
            assert_eq!(child.difficulty_sum(), sum(&row["difficulty_sum"]));
            assert_eq!(
                child
                    .exchange_rate(uint(&row["exchange"]), uint(&row["miner"]))
                    .unwrap(),
                uint(&row["next_exchange"])
            );
            assert_eq!(next.logical_digest(), child.logical_digest());
            assert_eq!(stored.logical_digest(), original);
            checks += 5;
            memory = child;
            stored = next;
            rows_count += 1;
            if branch == 0 && offset == 4199 {
                checkpoint = Some((memory.clone(), stored.clone(), parent_version));
            }
            if offset % 512 == 511 {
                drop(database);
                database = StateDatabase::open(&dir.join("choices.redb"), [71; 32]).unwrap();
                let loaded =
                    Window::load(&database.snapshot(Some(parent_version)).unwrap()).unwrap();
                assert_eq!(loaded.logical_digest(), stored.logical_digest());
                assert_eq!(loaded.difficulty_sum(), stored.difficulty_sum());
                stored = loaded;
                reopens += 1;
                println!(
                    "{}",
                    json!({"kind":"progress","updates":rows_count,"writer_reopens":reopens})
                );
            }
        }
        if branch == 0 {
            main_tip = Some((memory.clone(), stored.clone(), parent_version));
        }
    }
    let (main_memory, main_stored, main_id) = main_tip.unwrap();
    assert_eq!(database.head().unwrap(), Some(main_id));
    let main_snapshot = database.snapshot(Some(main_id)).unwrap();
    let fork_snapshot = database.snapshot(Some(parent_version)).unwrap();
    assert_eq!(
        Window::load(&main_snapshot).unwrap().logical_digest(),
        main_memory.logical_digest()
    );
    assert_eq!(
        Window::load(&fork_snapshot).unwrap().logical_digest(),
        memory.logical_digest()
    );
    assert_ne!(main_memory.logical_digest(), memory.logical_digest());
    branch_checks += 1;
    let wrong = stored
        .advance(
            [39; 32],
            stored.block(),
            stored.number() + 1,
            Choice {
                quai: 0,
                qi: 1,
                difficulty: Uint::ONE,
            },
        )
        .unwrap();
    assert!(database
        .commit(
            selected,
            Some(main_id),
            [39; 32],
            b"wrong branch window",
            false,
            |ws| {
                wrong.persist(ws, Some(&stored))?;
                Ok(())
            }
        )
        .is_err());
    assert_eq!(database.head().unwrap(), Some(main_id));
    branch_checks += 1;
    assert!(main_stored
        .advance(
            [40; 32],
            [0; 32],
            main_stored.number() + 1,
            Choice::default()
        )
        .is_err());
    assert!(main_stored
        .advance(
            [40; 32],
            main_stored.block(),
            main_stored.number() + 2,
            Choice::default()
        )
        .is_err());
    json!({"passed":true,"checks":checks,"go_comparisons":rows_count*6,"max_rows_copied_per_update":max_copied_rows,"max_persisted_row_writes_per_update":max_row_writes,"persisted_row_writes":total_row_writes,"metadata_writes_per_update":1,"updates":rows_count,"go_window_entries_bound_by_matching_digests":rows_count*4000,"writer_reopens":reopens,"aborted_or_unpublished_parent_rejections":aborts,"fork_isolation_checks":branch_checks,"complete_window":4000,"supplied_seed":true,"canonical_authority":false,"full_consensus":false,"canonical_public_sync":false,"timing_measured":false,"heap_or_rss_measured":false})
}
