//! Reuse the frozen Go transition checkers with persisted before/after state.
#[allow(dead_code)]
#[path = "check-incoming.rs"]
mod incoming;
#[allow(dead_code)]
#[path = "check-qi-settlement.rs"]
mod qi;
#[allow(dead_code)]
#[path = "check-settlement.rs"]
mod quai;
use quai_execution::{
    evm::state::World,
    state_store::{self, Commitment},
    utxo::UtxoState,
};
use quai_primitives::{transaction::keccak, Hash};
use quai_store::state_db::StateDatabase;
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
struct Lab {
    database: StateDatabase,
    genesis: Hash,
    active: Option<(Hash, World, UtxoState)>,
    serial: u64,
    roundtrips: usize,
    rejected: usize,
    versions: usize,
    inserted_nodes: usize,
    inserted_bytes: usize,
}
fn same(a: &World, u: &UtxoState, b: &World, v: &UtxoState) {
    let a_export = a.materialize().unwrap();
    let b_export = b.materialize().unwrap();
    assert_eq!(a_export.accounts().unwrap(), b_export.accounts().unwrap());
    assert_eq!(a.lockups(), b.lockups());
    assert_eq!(a_export.etxs(), b_export.etxs());
    assert_eq!(u.materialize().unwrap(), v.materialize().unwrap());
    assert_eq!(u.consensus_size(), v.consensus_size());
    assert_eq!(u.accumulator(), v.accumulator());
    assert_eq!(u.root(), v.root());
}
impl Lab {
    fn new(path: &Path) -> Self {
        let database =
            StateDatabase::open(path, keccak(b"persisted-component-transitions-v1")).unwrap();
        let empty = World::default();
        let no_utxo = UtxoState::default();
        let genesis = database
            .commit_prepared(None, None, keccak(b"genesis"), true, |w| {
                let state = state_store::apply_delta(w, &empty, &no_utxo, &empty, &no_utxo)?;
                Ok(state.digest.to_vec())
            })
            .unwrap()
            .version
            .id;
        Self {
            database,
            genesis,
            active: None,
            serial: 0,
            roundtrips: 0,
            rejected: 0,
            versions: 1,
            inserted_nodes: 0,
            inserted_bytes: 0,
        }
    }
    fn roundtrip(
        &mut self,
        parent: Hash,
        before: &World,
        before_u: &UtxoState,
        world: &mut World,
        u: &mut UtxoState,
    ) -> Hash {
        self.serial += 1;
        let block = keccak(&self.serial.to_be_bytes());
        let mut commitment = None;
        let commit = self
            .database
            .commit_prepared(Some(self.genesis), Some(parent), block, false, |w| {
                let c = state_store::apply_delta(w, before, before_u, world, u)?;
                let metadata = c.digest.to_vec();
                commitment = Some(c);
                Ok(metadata)
            })
            .unwrap();
        let c: Commitment = commitment.unwrap();
        let restored = state_store::restore_sparse(
            Arc::new(Mutex::new(
                self.database.snapshot(Some(commit.version.id)).unwrap(),
            )),
            &c,
        )
        .unwrap();
        assert!(restored.0.etxs().is_lazy());
        assert_eq!(restored.0.etxs().loaded_entries(), 0);
        assert!(restored.0.lazy_lockups());
        assert_eq!(restored.0.loaded_lockups(), 0);
        assert!(restored.0.is_lazy());
        assert_eq!(restored.0.loaded_accounts(), 0);
        assert!(restored.1.is_lazy());
        assert_eq!(restored.1.loaded_entries(), 0);
        same(world, u, &restored.0, &restored.1);
        *world = restored.0;
        *u = restored.1;
        assert_eq!(self.database.head().unwrap(), Some(self.genesis));
        self.roundtrips += 1;
        self.versions += 1;
        self.inserted_nodes += commit.new_nodes.nodes;
        self.inserted_bytes += commit.new_nodes.bytes;
        commit.version.id
    }
    fn observe(&mut self, _row: &Value, world: &mut World, u: &mut UtxoState, phase: u8) {
        match phase {
            0 => {
                assert!(self.active.is_none());
                let version = self.roundtrip(
                    self.genesis,
                    &World::default(),
                    &UtxoState::default(),
                    world,
                    u,
                );
                self.active = Some((version, world.clone(), u.clone()));
            }
            1 => {
                let (parent, before, before_u) = self.active.take().unwrap();
                self.roundtrip(parent, &before, &before_u, world, u);
            }
            2 => {
                let (_, before, before_u) = self.active.take().unwrap();
                same(world, u, &before, &before_u);
                assert_eq!(self.database.head().unwrap(), Some(self.genesis));
                self.rejected += 1;
            }
            _ => panic!("unknown observer phase"),
        }
    }
}
pub fn verify(directory: &Path) -> Value {
    let mut lab = Lab::new(&directory.join("components.redb"));
    let mut components = serde_json::Map::new();
    let settlement: Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/settlement.json")).unwrap();
    let q = quai::verify_observed(
        &settlement,
        Some(&mut |row, w, u, phase| lab.observe(row, w, u, phase)),
    );
    assert!(q["passed"].as_bool().unwrap(), "{q}");
    components.insert("signed-quai".into(), q);
    let corpus: Value = serde_json::from_str(include_str!(
        "../../../fixtures/protocol/qi-settlement.json"
    ))
    .unwrap();
    let q = qi::verify_observed(
        &corpus,
        Some(&mut |row, w, u, phase| lab.observe(row, w, u, phase)),
    );
    assert!(q["passed"].as_bool().unwrap(), "{q}");
    components.insert("qi".into(), q);
    let corpus: Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/incoming.json")).unwrap();
    let q = incoming::verify_observed(
        &corpus,
        Some(&mut |row, w, u, phase| lab.observe(row, w, u, phase)),
    );
    assert!(q["passed"].as_bool().unwrap(), "{q}");
    components.insert("incoming-etx".into(), q);
    assert!(lab.active.is_none());
    let checks = components
        .values()
        .map(|q| q["checks"].as_u64().unwrap())
        .sum::<u64>();
    let cases = components
        .values()
        .map(|q| q["cases"].as_u64().unwrap())
        .sum::<u64>();
    json!({"passed":true,"transition_inputs_use_lazy_qi_state":true,"transition_inputs_use_lazy_accounts_and_slots":true,"transition_inputs_use_lazy_etx_and_lockups":true,"cases":cases,"go_transition_comparisons":checks,"state_roundtrips":lab.roundtrips,"rejected_incoming_transitions":lab.rejected,"state_versions":lab.versions,"new_unique_nodes":lab.inserted_nodes,"new_encoded_node_bytes":lab.inserted_bytes,"components":components,"runtime_go_dependency":false,"full_block_consensus":false,"canonical_public_sync":false,"scope":"Frozen component transitions execute from a local persisted initial state and successful completed states roundtrip through the same keyed database used by private canonical admission. Includes rejected-transition atomicity; these versions are component cases, not admitted blocks or full protocol replay."})
}
#[allow(dead_code)]
fn main() {
    let path = std::env::args().nth(1).expect("fresh directory");
    let path = Path::new(&path);
    std::fs::create_dir_all(path).unwrap();
    println!("{}", verify(path));
}
