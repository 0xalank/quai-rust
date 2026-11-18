use quai_execution::evm::{
    journal::Journal,
    state::{Account, World},
    word::Word,
};
#[test]
fn child_commit_stays_revertible_until_parent_commit() {
    let a = [0; 20];
    let b = [1; 20];
    let mut world = World::default();
    world
        .insert(
            a,
            Account {
                balance: Word::from_u64(100),
                ..Default::default()
            },
        )
        .unwrap();
    let original = world.clone();
    let mut s = Journal::new(world).unwrap();
    let outer = s.snapshot().unwrap();
    s.set_nonce(a, 7).unwrap();
    let child = s.snapshot().unwrap();
    assert!(s.transfer(a, b, Word::from_u64(30)).unwrap());
    s.set_storage(a, [0; 32], [3; 32]).unwrap();
    s.set_transient(a, [1; 32], [7; 32]).unwrap();
    s.commit(child).unwrap();
    assert_eq!(
        s.world().account(&b).unwrap().unwrap().balance,
        Word::from_u64(30)
    );
    s.revert(outer).unwrap();
    assert_eq!(s.world(), &original);
    assert_eq!(s.transient(a, [1; 32]), [0; 32]);
    assert!(s.revert(child).is_err());
}
#[test]
fn original_storage_survives_nested_revert() {
    let a = [0; 20];
    let k = [2; 32];
    let mut world = World::default();
    let mut account = Account::default();
    account.put(k, [3; 32]).unwrap();
    world.insert(a, account).unwrap();
    let mut s = Journal::new(world).unwrap();
    let tx = s.snapshot().unwrap();
    s.set_storage(a, k, [4; 32]).unwrap();
    let inner = s.snapshot().unwrap();
    s.set_storage(a, k, [5; 32]).unwrap();
    s.revert(inner).unwrap();
    assert_eq!(s.original(a, k).unwrap(), [3; 32]);
    assert_eq!(
        s.world().account(&a).unwrap().unwrap().get(k).unwrap(),
        [4; 32]
    );
    s.revert(tx).unwrap();
    assert_eq!(
        s.world().account(&a).unwrap().unwrap().get(k).unwrap(),
        [3; 32]
    );
}
#[test]
fn revisions_reject_foreign_and_stale_tokens_without_mutation() {
    let mut a = Journal::new(World::default()).unwrap();
    let token = a.snapshot().unwrap();
    let mut b = Journal::new(World::default()).unwrap();
    let own = b.snapshot().unwrap();
    assert!(b.commit(token).is_err());
    assert!(b.revert(token).is_err());
    assert_eq!(b.world(), &World::default());
    b.commit(own).unwrap();
    assert!(b.revert(own).is_err());
    a.revert(token).unwrap();
}
