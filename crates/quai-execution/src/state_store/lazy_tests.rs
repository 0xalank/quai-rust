//! Real local state versions plus explicit unavailable-state fault readers.
use super::*;
use crate::evm::{
    calls,
    journal::Journal,
    state::{NodeReader, StorageBase},
    word::{bytes, Word},
    CapabilityError,
};
use crate::state_error::ReadError;
use quai_primitives::{
    mpt::ReadNodes,
    transaction::{AccessTuple, Address},
};
#[allow(dead_code)]
#[path = "../../tests/vm_support/mod.rs"]
mod support;
fn dir(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "quai-lazy-world-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    path
}
fn persist(
    db: &quai_store::state_db::StateDatabase,
    parent: Option<Hash>,
    serial: u8,
    before: &World,
    after: &World,
) -> (Hash, Commitment) {
    let mut c = None;
    let committed = db
        .commit_prepared(db.head().unwrap(), parent, [serial; 32], true, |w| {
            let value = apply_delta(
                w,
                before,
                &UtxoState::default(),
                after,
                &UtxoState::default(),
            )?;
            let raw = value.digest.to_vec();
            c = Some(value);
            Ok(raw)
        })
        .unwrap();
    (committed.version.id, c.unwrap())
}
fn slot(n: u64) -> Hash {
    bytes(Word::from_u64(n))
}
fn address(n: u64) -> Address {
    let mut out = [0; 20];
    out[12..].copy_from_slice(&n.to_be_bytes());
    out
}
#[test]
fn lazy_accounts_and_slots_roundtrip_touched_writes_and_pinned_reopen() {
    let directory = dir("roundtrip");
    let path = directory.join("state.redb");
    let db = quai_store::state_db::StateDatabase::open(&path, [31; 32]).unwrap();
    let mut memory = World::default();
    for n in 1..=1000 {
        let mut account = Account {
            nonce: 1,
            balance: Word::from_u64(100),
            storage_size: Word::from_u64(2),
            ..Default::default()
        };
        account.put(slot(0), slot(n)).unwrap();
        account.put(slot(1), slot(n + 1)).unwrap();
        memory.insert(address(n), account).unwrap();
    }
    let (parent, c) = persist(&db, None, 0, &World::default(), &memory);
    let (old, _) =
        restore_sparse(Arc::new(Mutex::new(db.snapshot(Some(parent)).unwrap())), &c).unwrap();
    assert!(old.is_lazy());
    assert_eq!(old.loaded_accounts(), 0);
    assert_eq!(old.account_count(), 1000);
    assert_eq!(old.root().unwrap(), memory.root().unwrap());
    let mut journal = Journal::new(old.clone()).unwrap();
    let revision = journal.snapshot().unwrap();
    journal
        .set_storage(address(1), slot(0), slot(7000))
        .unwrap();
    journal.set_storage(address(1), slot(3), slot(2)).unwrap();
    journal
        .transfer(address(2), address(3), Word::from_u64(7))
        .unwrap();
    journal.finalize_sizes().unwrap();
    journal.commit(revision).unwrap();
    let next = journal.into_world().unwrap();
    assert!(old.loaded_accounts() <= 3, "enumerated untouched accounts");
    let mut expected = Journal::new(memory.clone()).unwrap();
    let point = expected.snapshot().unwrap();
    expected
        .set_storage(address(1), slot(0), slot(7000))
        .unwrap();
    expected.set_storage(address(1), slot(3), slot(2)).unwrap();
    expected
        .transfer(address(2), address(3), Word::from_u64(7))
        .unwrap();
    expected.finalize_sizes().unwrap();
    expected.commit(point).unwrap();
    let expected = expected.into_world().unwrap();
    let (child, c2) = persist(&db, Some(parent), 1, &old, &next);
    assert!(old.loaded_accounts() <= 3);
    drop(db);
    assert_eq!(
        old.account(&address(1))
            .unwrap()
            .unwrap()
            .get(slot(0))
            .unwrap(),
        slot(1)
    );
    let db = quai_store::state_db::StateDatabase::open(&path, [31; 32]).unwrap();
    let (recovered, _) =
        restore_sparse(Arc::new(Mutex::new(db.snapshot(Some(child)).unwrap())), &c2).unwrap();
    assert_eq!(recovered.loaded_accounts(), 0);
    assert_eq!(recovered.root().unwrap(), expected.root().unwrap());
    assert_eq!(
        recovered.materialize().unwrap(),
        expected.materialize().unwrap()
    );
    assert_eq!(
        recovered
            .account(&address(1))
            .unwrap()
            .unwrap()
            .storage_len(),
        3
    );
    drop(recovered);
    drop(next);
    drop(old);
    drop(db);
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
fn stored_account_growth_and_deletion_do_not_require_total_slot_materialization() {
    let directory = dir("large-slot-set");
    let db =
        quai_store::state_db::StateDatabase::open(&directory.join("state.redb"), [32; 32]).unwrap();
    let mut memory = World::default();
    let mut account = Account {
        nonce: 1,
        storage_size: Word::from_u64(4096),
        ..Default::default()
    };
    for n in 0..4096 {
        account.put(slot(n), slot(n + 1)).unwrap();
    }
    memory.insert(address(1), account).unwrap();
    let (parent, c) = persist(&db, None, 0, &World::default(), &memory);
    let (old, _) =
        restore_sparse(Arc::new(Mutex::new(db.snapshot(Some(parent)).unwrap())), &c).unwrap();
    assert_eq!(old.loaded_accounts(), 0);
    let mut next = old.clone();
    let account = next.account_mut(&address(1)).unwrap().unwrap();
    assert!(account.storage.is_empty());
    account.put(slot(4096), slot(5000)).unwrap();
    account.storage_size = Word::from_u64(4097);
    assert_eq!(account.storage_len(), 4097);
    assert_eq!(account.storage.len(), 1);
    let (child, c2) = persist(&db, Some(parent), 1, &old, &next);
    let (large, _) =
        restore_sparse(Arc::new(Mutex::new(db.snapshot(Some(child)).unwrap())), &c2).unwrap();
    let account = large.account(&address(1)).unwrap().unwrap();
    assert_eq!(account.storage_len(), 4097);
    assert!(account.storage.is_empty());
    assert_eq!(account.get(slot(4096)).unwrap(), slot(5000));
    assert!(
        account.materialize_storage().is_err(),
        "bounded legacy export unexpectedly accepted a larger total base"
    );
    // Independent consensus trie construction has no fixture snapshot limit.
    let pairs = (0..4097)
        .map(|n| {
            (
                slot(n).to_vec(),
                crate::evm::state::slot_rlp(&if n == 4096 { slot(5000) } else { slot(n + 1) }),
            )
        })
        .collect::<Vec<_>>();
    let storage = quai_primitives::trie::secure_root(&pairs).unwrap();
    let rlp = quai_primitives::trie::list([
        crate::evm::state::rlp_bytes(&[1]),
        crate::evm::state::rlp_bytes(&[]),
        crate::evm::state::rlp_bytes(&storage),
        crate::evm::state::rlp_bytes(&keccak(&[])),
        crate::evm::state::rlp_bytes(&[0x10, 1]),
    ]);
    let expected = quai_primitives::trie::secure_root(&[(address(1).to_vec(), rlp)]).unwrap();
    assert_eq!(large.root().unwrap(), expected);
    let mut deleted = large.clone();
    deleted.remove(&address(1)).unwrap();
    let (empty, c3) = persist(&db, Some(child), 2, &large, &deleted);
    let (recovered, _) =
        restore_sparse(Arc::new(Mutex::new(db.snapshot(Some(empty)).unwrap())), &c3).unwrap();
    assert_eq!(recovered.account_count(), 0);
    assert_eq!(recovered.root().unwrap(), mpt::empty_root());
    assert!(recovered.account(&address(1)).unwrap().is_none());
    assert_eq!(
        old.account(&address(1))
            .unwrap()
            .unwrap()
            .get(slot(4095))
            .unwrap(),
        slot(4096)
    );
    drop(account);
    drop(recovered);
    drop(deleted);
    drop(large);
    drop(next);
    drop(old);
    drop(db);
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
fn malformed_lazy_account_and_code_are_unavailable_instead_of_missing() {
    let directory = dir("corrupt-account");
    let db =
        quai_store::state_db::StateDatabase::open(&directory.join("state.redb"), [33; 32]).unwrap();
    let mut world = World::default();
    world
        .insert(
            address(1),
            Account {
                nonce: 1,
                code: vec![0],
                ..Default::default()
            },
        )
        .unwrap();
    world
        .insert(
            address(2),
            Account {
                nonce: 1,
                ..Default::default()
            },
        )
        .unwrap();
    let (parent, c) = persist(&db, None, 0, &World::default(), &world);
    for (number, tree, key, value) in [
        (1, b"account-data".as_slice(), address(1).to_vec(), vec![1]),
        (2, b"account-data".as_slice(), address(1).to_vec(), vec![]),
        (3, b"code".as_slice(), keccak(&[0]).to_vec(), vec![]),
        (4, b"account-data".as_slice(), address(1).to_vec(), vec![]),
    ] {
        let version = db
            .commit(
                Some(parent),
                Some(parent),
                [number; 32],
                b"synthetic-corrupt-unadmitted-local-version",
                false,
                |w| {
                    w.put(tree, &key, &value)?;
                    if number == 4 {
                        w.put(b"account-data", &address(2), &[])?;
                    }
                    Ok(())
                },
            )
            .unwrap()
            .version
            .id;
        let snapshot = db.snapshot(Some(version)).unwrap();
        let mut expected = c.clone();
        expected.digest = snapshot_digest(&snapshot).unwrap();
        let restored = restore_sparse(Arc::new(Mutex::new(snapshot)), &expected);
        if number == 4 {
            assert!(
                restored.is_err(),
                "empty index with nonempty accounts must fail at restoration"
            );
            assert_eq!(db.head().unwrap(), Some(parent));
            continue;
        }
        let (sparse, _) = restored.unwrap();
        assert_eq!(sparse.loaded_accounts(), 0);
        assert!(matches!(
            sparse.account(&address(1)),
            Err(ReadError::Unavailable(_))
        ));
        assert!(sparse.account(&address(3)).unwrap().is_none());
        assert_eq!(db.head().unwrap(), Some(parent));
    }
    drop(db);
    std::fs::remove_dir_all(directory).unwrap();
}
#[derive(Debug)]
struct UnavailableSlots;
impl ReadNodes for UnavailableSlots {
    fn get_node(&self, _: &Hash) -> Result<Option<Vec<u8>>, mpt::Error> {
        Err(mpt::Error::Backend(
            "injected local storage read failure".into(),
        ))
    }
}
impl NodeReader for UnavailableSlots {
    fn identity(&self) -> Hash {
        [99; 32]
    }
    fn slots(&self, _: Hash, _: usize, _: usize) -> Result<BTreeMap<Hash, Hash>, ReadError> {
        panic!("native execution attempted a complete storage export")
    }
}
#[test]
fn nested_call_unavailable_storage_aborts_outer_writes_instead_of_becoming_revert() {
    let corpus: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../../fixtures/protocol/serial-vm.json"
    ))
    .unwrap();
    let mut context = support::parse_context(&corpus["vectors"][0]).unwrap();
    let parent = address(0x1101);
    let child = address(0x1102);
    context.address = parent;
    context.caller = address(10);
    context.origin = context.caller;
    context.location = [0, 0];
    context.value = Word::ZERO;
    context.read_only = false;
    context.state_size = Word::ZERO;
    context.warm_slots = Default::default();
    let mut code = vec![
        0x60, 1, 0x60, 0, 0x55, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x60, 0, 0x73,
    ];
    code.extend(child);
    code.extend([0x62, 0x0f, 0x42, 0x40, 0xf1, 0x00]);
    let mut world = World::default();
    world
        .insert(
            context.caller,
            Account {
                nonce: 1,
                balance: Word::from_u64(100),
                ..Default::default()
            },
        )
        .unwrap();
    world
        .insert(
            parent,
            Account {
                nonce: 1,
                code,
                ..Default::default()
            },
        )
        .unwrap();
    world
        .insert(
            child,
            Account {
                nonce: 1,
                code: vec![0x60, 0, 0x54, 0],
                backing: Some(StorageBase {
                    reader: Arc::new(UnavailableSlots),
                    root: [88; 32],
                    count: 1,
                }),
                ..Default::default()
            },
        )
        .unwrap();
    let before = world.clone();
    let access = [
        AccessTuple {
            address: parent,
            keys: vec![slot(0)],
        },
        AccessTuple {
            address: child,
            keys: vec![slot(0)],
        },
    ];
    let error = calls::execute_call(&mut world, &context, &access, &[], 3_000_000).unwrap_err();
    assert!(
        matches!(error, CapabilityError::StateUnavailable(_)),
        "{error:?}"
    );
    assert_eq!(world, before);
    assert_eq!(
        world
            .account(&parent)
            .unwrap()
            .unwrap()
            .get(slot(0))
            .unwrap(),
        [0; 32]
    );
}
#[test]
fn signed_quai_lazy_backend_failure_preserves_state_gas_and_selected_head() {
    let corpus: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../../fixtures/protocol/settlement.json"
    ))
    .unwrap();
    let row = corpus["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["consensus_error"].is_null() && !row["transaction"].is_null())
        .unwrap();
    let memory = support::parse_world(row).unwrap();
    let context = support::parse_context(row).unwrap();
    let tx = quai_primitives::transaction::Transaction::decode(
        &quai_primitives::decode_hex(row["transaction"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let quai_primitives::transaction::Kind::Quai(t) = tx.kind() else {
        panic!("signed Quai fixture")
    };
    let target = t.to.unwrap();
    assert!(memory.account(&target).unwrap().is_some());
    let directory = dir("signed-failure");
    let db =
        quai_store::state_db::StateDatabase::open(&directory.join("state.redb"), [34; 32]).unwrap();
    let (parent, c) = persist(&db, None, 0, &World::default(), &memory);
    let version = db
        .commit(
            Some(parent),
            Some(parent),
            [1; 32],
            b"synthetic-unadmitted-metadata-damage",
            false,
            |w| w.put(b"account-data", &target, &[1]),
        )
        .unwrap()
        .version
        .id;
    let snapshot = db.snapshot(Some(version)).unwrap();
    let mut expected = c;
    expected.digest = snapshot_digest(&snapshot).unwrap();
    let (mut world, _) = restore_sparse(Arc::new(Mutex::new(snapshot)), &expected).unwrap();
    let before = world.clone();
    let mut block = crate::settlement::BlockContext {
        vm: context,
        environment: calls::Environment {
            delete_empty: row["finalize_empty"].as_bool().unwrap_or(false),
            prime_number: row["context"]["prime_number"].as_u64(),
            ..Default::default()
        },
        gas_remaining: row["initial_gas_pool"].as_u64().unwrap(),
        cumulative_gas: row["initial_cumulative_gas"].as_u64().unwrap(),
        state_used: row["initial_state_used"].as_u64().unwrap(),
    };
    let gas = (block.gas_remaining, block.cumulative_gas, block.state_used);
    let result = crate::settlement::apply_quai(&mut world, &tx, &mut block);
    assert!(matches!(
        result,
        Err(crate::settlement::Error::Unavailable(_))
    ));
    assert_eq!(world, before);
    assert_eq!(
        (block.gas_remaining, block.cumulative_gas, block.state_used),
        gas
    );
    assert_eq!(db.head().unwrap(), Some(parent));
    drop(world);
    drop(before);
    drop(db);
    std::fs::remove_dir_all(directory).unwrap();
}

#[derive(Debug)]
struct FailingAccounts {
    known: BTreeMap<Address, Arc<Account>>,
}
impl ReadNodes for FailingAccounts {
    fn get_node(&self, _: &Hash) -> Result<Option<Vec<u8>>, mpt::Error> {
        Err(mpt::Error::Backend(
            "injected account trie read failure".into(),
        ))
    }
}
impl crate::evm::state::AccountReader for FailingAccounts {
    fn identity(&self) -> Hash {
        [98; 32]
    }
    fn account(&self, address: &Address) -> Result<Option<Arc<Account>>, ReadError> {
        self.known.get(address).cloned().map(Some).ok_or_else(|| {
            ReadError::Unavailable("injected uncached account lookup failure".into())
        })
    }
    fn accounts(&self, _: usize, _: usize) -> Result<BTreeMap<Address, Arc<Account>>, ReadError> {
        panic!("execution attempted a complete account export")
    }
    fn loaded(&self) -> usize {
        0
    }
}
#[test]
fn nested_creation_unavailable_account_aborts_outer_storage_and_nonce_writes() {
    let corpus: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../../fixtures/protocol/serial-vm.json"
    ))
    .unwrap();
    let mut context = support::parse_context(&corpus["vectors"][0]).unwrap();
    let parent = address(0x1101);
    context.address = parent;
    context.caller = address(10);
    context.origin = context.caller;
    context.location = [0, 0];
    context.value = Word::ZERO;
    context.read_only = false;
    context.state_size = Word::ZERO;
    context.warm_slots = Default::default();
    let code = vec![0x60, 1, 0x60, 0, 0x55, 0x60, 0, 0x60, 0, 0x60, 0, 0xf0, 0];
    let mut memory = World::default();
    memory
        .insert(
            context.caller,
            Account {
                nonce: 1,
                balance: Word::from_u64(100),
                ..Default::default()
            },
        )
        .unwrap();
    memory
        .insert(
            parent,
            Account {
                nonce: 1,
                code,
                ..Default::default()
            },
        )
        .unwrap();
    let root = memory.root().unwrap();
    let known = memory.accounts().unwrap().into_owned();
    let mut world = World::from_reader(Arc::new(FailingAccounts { known }), root, 2);
    let before = world.clone();
    let (created, _) = calls::creation_address(
        parent,
        1,
        &[],
        None,
        crate::evm::word::uint(context.block_number).unwrap(),
        [0, 0],
        3_000_000,
    )
    .unwrap();
    let access = [
        AccessTuple {
            address: parent,
            keys: vec![slot(0)],
        },
        AccessTuple {
            address: created,
            keys: vec![],
        },
    ];
    let error = calls::execute_call(&mut world, &context, &access, &[], 3_000_000).unwrap_err();
    assert!(
        matches!(error, CapabilityError::StateUnavailable(_)),
        "{error:?}"
    );
    assert_eq!(world, before);
    let account = world.account(&parent).unwrap().unwrap();
    assert_eq!(account.nonce, 1);
    assert_eq!(account.get(slot(0)).unwrap(), [0; 32]);
}
#[test]
fn stored_account_membership_crosses_the_old_total_account_limit() {
    let directory = dir("account-count");
    let db =
        quai_store::state_db::StateDatabase::open(&directory.join("state.redb"), [35; 32]).unwrap();
    let mut memory = World::default();
    let account = Account {
        nonce: 1,
        ..Default::default()
    };
    for n in 1..=65_536 {
        memory.insert(address(n), account.clone()).unwrap();
    }
    let (parent, c) = persist(&db, None, 0, &World::default(), &memory);
    let (old, _) =
        restore_sparse(Arc::new(Mutex::new(db.snapshot(Some(parent)).unwrap())), &c).unwrap();
    assert_eq!(old.loaded_accounts(), 0);
    let mut next = old.clone();
    next.insert(address(65_537), account).unwrap();
    assert_eq!(next.account_count(), 65_537);
    let (child, c2) = persist(&db, Some(parent), 1, &old, &next);
    let (restored, _) =
        restore_sparse(Arc::new(Mutex::new(db.snapshot(Some(child)).unwrap())), &c2).unwrap();
    assert_eq!(restored.account_count(), 65_537);
    assert_eq!(restored.loaded_accounts(), 0);
    assert_eq!(
        restored.account(&address(65_537)).unwrap().unwrap().nonce,
        1
    );
    assert_eq!(restored.account(&address(1)).unwrap().unwrap().nonce, 1);
    assert!(
        restored.accounts().is_err(),
        "bounded diagnostic export unexpectedly accepted a larger total base"
    );
    assert!(old.account(&address(65_537)).unwrap().is_none());
    assert_eq!(db.head().unwrap(), Some(child));
    drop(restored);
    drop(next);
    drop(old);
    drop(db);
    std::fs::remove_dir_all(directory).unwrap();
}

#[derive(Debug)]
struct ExportSlots {
    calls: std::sync::atomic::AtomicUsize,
    values: BTreeMap<Hash, Hash>,
}
impl ReadNodes for ExportSlots {
    fn get_node(&self, _: &Hash) -> Result<Option<Vec<u8>>, mpt::Error> {
        panic!("diagnostic test queried an unexpected trie node")
    }
}
impl NodeReader for ExportSlots {
    fn identity(&self) -> Hash {
        [97; 32]
    }
    fn slots(&self, _: Hash, _: usize, _: usize) -> Result<BTreeMap<Hash, Hash>, ReadError> {
        let count = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        assert!(
            count <= 16,
            "export read slots beyond its aggregate resource limit"
        );
        Ok(self.values.clone())
    }
}
#[derive(Debug)]
struct ExportAccounts {
    rows: BTreeMap<Address, Arc<Account>>,
}
impl ReadNodes for ExportAccounts {
    fn get_node(&self, _: &Hash) -> Result<Option<Vec<u8>>, mpt::Error> {
        panic!("diagnostic test queried account trie")
    }
}
impl crate::evm::state::AccountReader for ExportAccounts {
    fn identity(&self) -> Hash {
        [96; 32]
    }
    fn account(&self, address: &Address) -> Result<Option<Arc<Account>>, ReadError> {
        Ok(self.rows.get(address).cloned())
    }
    fn accounts(&self, _: usize, _: usize) -> Result<BTreeMap<Address, Arc<Account>>, ReadError> {
        Ok(self.rows.clone())
    }
    fn loaded(&self) -> usize {
        0
    }
}
#[test]
fn diagnostic_account_export_stops_before_exceeding_aggregate_slot_budget() {
    let values = (0..4096)
        .map(|n| (slot(n), slot(n + 1)))
        .collect::<BTreeMap<_, _>>();
    let node = Arc::new(ExportSlots {
        calls: std::sync::atomic::AtomicUsize::new(0),
        values,
    });
    let mut rows = BTreeMap::new();
    for n in 1..=18 {
        rows.insert(
            address(n),
            Arc::new(Account {
                nonce: 1,
                storage_size: Word::from_u64(4096),
                backing: Some(StorageBase {
                    reader: node.clone(),
                    root: [95; 32],
                    count: 4096,
                }),
                ..Default::default()
            }),
        );
    }
    let world = World::from_reader(Arc::new(ExportAccounts { rows }), [94; 32], 18);
    assert!(matches!(world.materialize(), Err(ReadError::Invalid(_))));
    assert_eq!(node.calls.load(std::sync::atomic::Ordering::Relaxed), 16);
}
