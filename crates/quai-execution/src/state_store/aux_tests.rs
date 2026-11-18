//! Lazy auxiliary state uses real local versions; fault readers carry no admission authority.
use super::*;
use crate::{
    evm::{calls, word::Word, CapabilityError},
    state_error::ReadError,
};
use quai_primitives::{
    mpt::ReadNodes,
    transaction::{AccessTuple, Address, External},
};
#[allow(dead_code)]
#[path = "../../tests/vm_support/mod.rs"]
mod support;
fn dir(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "quai-lazy-aux-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    path
}
fn address(n: u64) -> Address {
    let mut a = [0; 20];
    a[12..].copy_from_slice(&n.to_be_bytes());
    a
}
fn tx(n: u64) -> Transaction {
    Transaction::external(External {
        originating_hash: keccak(&n.to_be_bytes()),
        index: (n % 65536) as u16,
        gas: 21000,
        to: address(0x1101),
        value: vec![1],
        data: vec![],
        access_list: vec![],
        sender: address(0x1102),
        etx_type: 0,
    })
    .unwrap()
}
fn key(n: u32) -> Key {
    Key {
        owner: address(0x1101),
        beneficiary: address(n as u64),
        lock: 1,
        epoch: n,
    }
}
fn value(n: u64) -> Lockup {
    Lockup {
        balance: Word::from_u64(n),
        unlock_height: 50000,
        elements: 1,
        delegate: address(0x1103),
    }
}
fn persist(
    db: &quai_store::state_db::StateDatabase,
    parent: Option<Hash>,
    serial: u8,
    before: &World,
    after: &World,
) -> (Hash, Commitment) {
    let mut c = None;
    let v = db
        .commit_prepared(db.head().unwrap(), parent, [serial; 32], true, |w| {
            let next = apply_delta(
                w,
                before,
                &UtxoState::default(),
                after,
                &UtxoState::default(),
            )?;
            let raw = next.digest.to_vec();
            c = Some(next);
            Ok(raw)
        })
        .unwrap()
        .version
        .id;
    (v, c.unwrap())
}
/// go-quai bounds neither the ETX set nor the lockup records; a lazy overlay
/// that lives across many blocks before a commit must accept more than the
/// former 65,536-change and 32 MiB caps and persist exactly.
#[test]
fn lazy_etx_and_lockup_overlays_have_no_change_or_payload_cap() {
    let directory = dir("uncapped");
    let db =
        quai_store::state_db::StateDatabase::open(&directory.join("state.redb"), [52; 32]).unwrap();
    let mut memory = World::default();
    memory.etxs_mut().push(&[tx(0)]).unwrap();
    memory.set_lockup(key(1), Some(value(1))).unwrap();
    let (parent, c) = persist(&db, None, 0, &World::default(), &memory);
    let (old, _) =
        restore_sparse(Arc::new(Mutex::new(db.snapshot(Some(parent)).unwrap())), &c).unwrap();
    let mut next = old.clone();
    let n = 70_000u64;
    let batch = (1..=n).map(tx).collect::<Vec<_>>();
    for chunk in batch.chunks(10_000) {
        next.etxs_mut().push(chunk).unwrap();
    }
    assert_eq!(next.etxs_mut().pop().unwrap().unwrap(), tx(0));
    for k in 2..=n as u32 {
        next.set_lockup(key(k), Some(value(k as u64))).unwrap();
    }
    next.set_lockup(key(1), None).unwrap();
    assert_eq!(next.etxs().len(), n as usize);
    let etx_root = next.etxs().root().unwrap();
    let (child, c2) = persist(&db, Some(parent), 1, &old, &next);
    let (restored, _) =
        restore_sparse(Arc::new(Mutex::new(db.snapshot(Some(child)).unwrap())), &c2).unwrap();
    assert_eq!(restored.etxs().root().unwrap(), etx_root);
    assert_eq!(restored.etxs().len(), n as usize);
    assert_eq!(restored.etxs().read(n).unwrap().unwrap().as_ref(), &tx(n));
    assert_eq!(restored.lockup(&key(n as u32)).unwrap(), Some(value(n)));
    assert!(restored.lockup(&key(1)).unwrap().is_none());
    // The in-memory image keeps its bounds.
    let mut image = World::default();
    assert!(image.etxs_mut().push(&batch[..20_000]).is_err());
    drop(restored);
    drop(db);
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
fn lazy_etx_and_lockup_roundtrip_reads_only_touched_records_and_keeps_pinned_parents() {
    let directory = dir("roundtrip");
    let file = directory.join("state.redb");
    let db = quai_store::state_db::StateDatabase::open(&file, [51; 32]).unwrap();
    let mut memory = World::default();
    let transactions = (0..5000).map(tx).collect::<Vec<_>>();
    memory.etxs_mut().push(&transactions).unwrap();
    for n in 1..=5000 {
        memory.set_lockup(key(n), Some(value(n as u64))).unwrap();
    }
    let aux = keccak(b"aux-one").to_vec();
    memory
        .etxs_mut()
        .set_auxiliary(aux.clone(), vec![1])
        .unwrap();
    let (parent, c) = persist(&db, None, 0, &World::default(), &memory);
    let (old, _) =
        restore_sparse(Arc::new(Mutex::new(db.snapshot(Some(parent)).unwrap())), &c).unwrap();
    assert!(old.etxs().is_lazy() && old.lazy_lockups());
    assert_eq!(old.etxs().loaded_entries(), 0);
    assert_eq!(old.loaded_lockups(), 0);
    let mut next = old.clone();
    for n in 0..2 {
        assert_eq!(next.etxs_mut().pop().unwrap().unwrap(), transactions[n]);
    }
    next.etxs_mut().push(&[tx(5000)]).unwrap();
    next.etxs_mut().set_auxiliary(aux.clone(), vec![2]).unwrap();
    next.set_lockup(key(1), None).unwrap();
    next.set_lockup(key(2), Some(value(777))).unwrap();
    next.set_lockup(key(6000), Some(value(6000))).unwrap();
    let (child, c2) = persist(&db, Some(parent), 1, &old, &next);
    assert!(old.etxs().loaded_entries() <= 2);
    assert!(old.loaded_lockups() <= 2);
    let mut expected = memory.clone();
    expected.etxs_mut().pop().unwrap();
    expected.etxs_mut().pop().unwrap();
    expected.etxs_mut().push(&[tx(5000)]).unwrap();
    expected.etxs_mut().set_auxiliary(aux, vec![2]).unwrap();
    expected.set_lockup(key(1), None).unwrap();
    expected.set_lockup(key(2), Some(value(777))).unwrap();
    expected.set_lockup(key(6000), Some(value(6000))).unwrap();
    drop(db);
    assert_eq!(
        old.etxs().read(0).unwrap().unwrap().as_ref(),
        &transactions[0]
    );
    assert_eq!(old.lockup(&key(1)).unwrap(), Some(value(1)));
    let db = quai_store::state_db::StateDatabase::open(&file, [51; 32]).unwrap();
    let (recovered, _) =
        restore_sparse(Arc::new(Mutex::new(db.snapshot(Some(child)).unwrap())), &c2).unwrap();
    assert_eq!(recovered.etxs().loaded_entries(), 0);
    assert_eq!(recovered.loaded_lockups(), 0);
    assert_eq!(
        recovered.materialize().unwrap(),
        expected.materialize().unwrap()
    );
    assert_eq!(recovered.etxs().len(), 4999);
    drop(recovered);
    drop(next);
    drop(old);
    drop(db);
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
fn lazy_etx_queue_exceeds_the_old_total_queue_limit_without_exporting_it() {
    let directory = dir("large-queue");
    let db =
        quai_store::state_db::StateDatabase::open(&directory.join("state.redb"), [52; 32]).unwrap();
    let mut memory = World::default();
    let transactions = (0..16384).map(tx).collect::<Vec<_>>();
    memory.etxs_mut().push(&transactions).unwrap();
    let (parent, c) = persist(&db, None, 0, &World::default(), &memory);
    let (old, _) =
        restore_sparse(Arc::new(Mutex::new(db.snapshot(Some(parent)).unwrap())), &c).unwrap();
    assert_eq!(old.etxs().loaded_entries(), 0);
    let mut next = old.clone();
    next.etxs_mut().push(&[tx(16384)]).unwrap();
    let (child, c2) = persist(&db, Some(parent), 1, &old, &next);
    assert_eq!(old.etxs().loaded_entries(), 0);
    let (recovered, _) =
        restore_sparse(Arc::new(Mutex::new(db.snapshot(Some(child)).unwrap())), &c2).unwrap();
    assert_eq!(recovered.etxs().len(), 16385);
    assert_eq!(recovered.etxs().loaded_entries(), 0);
    assert_eq!(
        recovered.etxs().read(16384).unwrap().unwrap().as_ref(),
        &tx(16384)
    );
    assert!(recovered.etxs().entries().is_err());
    let mut consumed = recovered.clone();
    assert_eq!(consumed.etxs_mut().pop().unwrap().unwrap(), transactions[0]);
    persist(&db, Some(child), 2, &recovered, &consumed);
    assert_eq!(old.etxs().read(16384).unwrap(), None);
    drop(consumed);
    drop(recovered);
    drop(next);
    drop(old);
    drop(db);
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
fn unavailable_queue_lockup_and_metadata_records_never_turn_into_absence() {
    let directory = dir("damaged");
    let db =
        quai_store::state_db::StateDatabase::open(&directory.join("state.redb"), [53; 32]).unwrap();
    let mut world = World::default();
    world.etxs_mut().push(&[tx(0), tx(1), tx(2)]).unwrap();
    world.set_lockup(key(1), Some(value(1))).unwrap();
    let aux = keccak(b"economic-field").to_vec();
    world
        .etxs_mut()
        .set_auxiliary(aux.clone(), vec![1])
        .unwrap();
    let (parent, c) = persist(&db, None, 0, &World::default(), &world);
    for (number, tree, key, raw) in [
        (
            1,
            b"etx-data".as_slice(),
            1u64.to_be_bytes().to_vec(),
            vec![],
        ),
        (
            2,
            b"etx-data".as_slice(),
            1u64.to_be_bytes().to_vec(),
            vec![1],
        ),
        (3, b"lockups".as_slice(), key(1).raw().to_vec(), vec![1]),
        (4, b"etx-aux".as_slice(), aux.clone(), vec![]),
    ] {
        let version = db
            .commit(
                Some(parent),
                Some(parent),
                [number; 32],
                b"synthetic-corrupt-unadmitted-aux-version",
                false,
                |w| w.put(tree, &key, &raw),
            )
            .unwrap()
            .version
            .id;
        let snapshot = db.snapshot(Some(version)).unwrap();
        let mut expected = c.clone();
        expected.digest = snapshot_digest(&snapshot).unwrap();
        let (mut sparse, _) = restore_sparse(Arc::new(Mutex::new(snapshot)), &expected).unwrap();
        assert_eq!(sparse.etxs().loaded_entries(), 0);
        assert_eq!(sparse.loaded_lockups(), 0);
        if number <= 2 {
            assert_eq!(sparse.etxs_mut().pop().unwrap().unwrap(), tx(0));
            let before = sparse.clone();
            assert!(matches!(
                sparse.etxs_mut().pop(),
                Err(ReadError::Unavailable(_))
            ));
            assert_eq!(sparse, before);
        } else if number == 3 {
            assert!(matches!(
                sparse.lockup(&super::aux_tests::key(1)),
                Err(ReadError::Unavailable(_))
            ));
            assert!(sparse.lockup(&super::aux_tests::key(99)).unwrap().is_none());
        } else {
            assert!(matches!(
                sparse.etxs().auxiliary_value(&aux),
                Err(ReadError::Unavailable(_))
            ));
        }
        assert_eq!(db.head().unwrap(), Some(parent));
    }
    drop(db);
    std::fs::remove_dir_all(directory).unwrap();
}
#[derive(Debug)]
struct Unavailable;
impl ReadNodes for Unavailable {
    fn get_node(&self, _: &Hash) -> Result<Option<Vec<u8>>, mpt::Error> {
        Err(mpt::Error::Backend(
            "injected local auxiliary read failure".into(),
        ))
    }
}
impl crate::lockup_state::Reader for Unavailable {
    fn identity(&self) -> Hash {
        [81; 32]
    }
    fn get(&self, _: &Key) -> Result<Option<Lockup>, ReadError> {
        Err(ReadError::Unavailable(
            "injected lockup read failure".into(),
        ))
    }
    fn scan(&self, _: usize, _: usize) -> Result<BTreeMap<Key, Lockup>, ReadError> {
        panic!("runtime attempted a lockup export")
    }
    fn loaded(&self) -> usize {
        0
    }
}
impl crate::etx::Reader for Unavailable {
    fn identity(&self) -> Hash {
        [82; 32]
    }
    fn entry(&self, _: u64) -> Result<Option<Arc<crate::etx::Entry>>, ReadError> {
        Err(ReadError::Unavailable("injected ETX read failure".into()))
    }
    fn auxiliary(&self, _: &[u8]) -> Result<Option<Vec<u8>>, ReadError> {
        Err(ReadError::Unavailable(
            "injected economic metadata read failure".into(),
        ))
    }
    fn auxiliary_image(&self, _: usize, _: usize) -> Result<BTreeMap<Vec<u8>, Vec<u8>>, ReadError> {
        panic!("runtime attempted auxiliary metadata export")
    }
    fn loaded(&self) -> usize {
        0
    }
}
fn context() -> crate::evm::Context {
    let corpus: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../../fixtures/protocol/serial-vm.json"
    ))
    .unwrap();
    let mut c = support::parse_context(&corpus["vectors"][0]).unwrap();
    c.location = [0, 0];
    c.address = address(0x1101);
    c.caller = address(10);
    c.origin = c.caller;
    c.value = Word::ZERO;
    c.read_only = false;
    c.state_size = Word::ZERO;
    c.warm_slots = Default::default();
    c
}
#[test]
fn nested_lockup_backend_failure_aborts_outer_storage_changes() {
    let context = context();
    let target = crate::lockup::contract([0, 0]);
    let mut code = vec![
        0x60, 1, 0x60, 0, 0x55, 0x60, 0, 0x60, 0, 0x60, 21, 0x60, 0, 0x60, 0, 0x73,
    ];
    code.extend(target);
    code.extend([0x62, 0x0f, 0x42, 0x40, 0xf1, 0]);
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
            context.address,
            Account {
                nonce: 1,
                code,
                ..Default::default()
            },
        )
        .unwrap();
    world.set_lockup_reader([80; 32], Arc::new(Unavailable));
    let before = world.clone();
    let access = [
        AccessTuple {
            address: context.address,
            keys: vec![[0; 32]],
        },
        AccessTuple {
            address: target,
            keys: vec![],
        },
    ];
    let env = calls::Environment {
        prime_number: Some(2444574),
        ..Default::default()
    };
    let result = calls::execute_call_with_env(&mut world, &context, &access, &[], 3_000_000, &env);
    assert!(matches!(result, Err(CapabilityError::StateUnavailable(_))));
    assert_eq!(world, before);
    assert_eq!(
        world
            .account(&context.address)
            .unwrap()
            .unwrap()
            .get([0; 32])
            .unwrap(),
        [0; 32]
    );
}
#[test]
fn incoming_queue_backend_failure_preserves_state_and_all_gas_counters() {
    let mut world = World::default();
    *world.etxs_mut() =
        crate::etx::EtxState::from_reader(0, 1, [79; 32], Arc::new(Unavailable)).unwrap();
    let before = world.clone();
    let mut state = UtxoState::default();
    let saved = state.clone();
    let mut c = crate::incoming::Context {
        block: crate::settlement::BlockContext {
            vm: context(),
            environment: calls::Environment {
                prime_number: Some(2444574),
                ..Default::default()
            },
            gas_remaining: 10_000_000,
            cumulative_gas: 7,
            state_used: 9,
        },
        parent_height: 0,
        total_etx_gas: 11,
        region_remaining: 1_000_000,
        prime_remaining: 1_000_000,
    };
    let gas = (
        c.block.gas_remaining,
        c.block.cumulative_gas,
        c.block.state_used,
        c.total_etx_gas,
        c.region_remaining,
        c.prime_remaining,
    );
    let result = crate::incoming::apply(&mut world, &mut state, &tx(0), &mut c);
    assert!(matches!(
        result,
        Err(crate::settlement::Error::Unavailable(_))
    ));
    assert_eq!(world, before);
    assert_eq!(state, saved);
    assert_eq!(
        (
            c.block.gas_remaining,
            c.block.cumulative_gas,
            c.block.state_used,
            c.total_etx_gas,
            c.region_remaining,
            c.prime_remaining
        ),
        gas
    );
}
#[test]
fn authority_metadata_failure_is_unavailable_instead_of_a_false_command_result() {
    let mut world = World::default();
    *world.etxs_mut() =
        crate::etx::EtxState::from_reader(0, 0, mpt::empty_root(), Arc::new(Unavailable)).unwrap();
    let before = world.clone();
    let result = crate::economic_state::authority(&mut world, b"freeze", 0, false);
    assert!(matches!(
        result,
        Err(crate::settlement::Error::Unavailable(_))
    ));
    assert_eq!(world, before);
}
