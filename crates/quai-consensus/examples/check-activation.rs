//! Native local append/execution from supplied, explicitly unadmitted state.
use quai_consensus::{
    history::{History, Record},
    placement::{CandidateIndex, Snapshot, Termini},
    verifier::Verifier,
    zone_admission,
    zone_execution::{self, Profile, State},
};
use quai_execution::{
    evm::{state::Account, word::from_bytes},
    historical,
    qi::{OutPoint, Utxo},
    state_store::{self, Commitment},
};
use quai_primitives::{
    decode_hex, encode_hex,
    transaction::{keccak, Kind, Transaction},
    Hash,
};
use quai_store::state_db::StateDatabase;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};

fn bytes(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
fn hash(v: &Value) -> Hash {
    bytes(v).try_into().unwrap()
}
#[derive(Default)]
pub struct Past {
    incoming: BTreeMap<u64, Vec<Transaction>>,
    created: BTreeMap<u64, Vec<(OutPoint, u8)>>,
}
impl historical::History for Past {
    fn incoming_at(&self, h: u64) -> Result<Vec<Transaction>, quai_execution::settlement::Error> {
        self.incoming
            .get(&h)
            .cloned()
            .ok_or(quai_execution::settlement::Error::Unavailable(
                "redemption slice".into(),
            ))
    }
    fn created_at(&self, h: u64) -> Result<Vec<(OutPoint, u8)>, quai_execution::settlement::Error> {
        self.created
            .get(&h)
            .cloned()
            .ok_or(quai_execution::settlement::Error::Unavailable(
                "creation slice".into(),
            ))
    }
}
pub fn inputs(v: &Value) -> (State, History, CandidateIndex, Past) {
    let mut state = State::default();
    for row in v["initial_world"].as_array().unwrap() {
        state
            .world
            .insert(
                hash20(&row["address"]),
                Account {
                    nonce: row["nonce"].as_u64().unwrap(),
                    balance: from_bytes(&bytes(&row["balance"])),
                    ..Account::default()
                },
            )
            .unwrap();
    }
    let queue = v["initial_etxs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| Transaction::decode(&bytes(r)).unwrap())
        .collect::<Vec<_>>();
    state.world.etxs_mut().push(&queue).unwrap();
    for row in v["initial_utxos"].as_array().unwrap() {
        state
            .utxos
            .insert(
                OutPoint {
                    hash: hash(&row["hash"]),
                    index: row["index"].as_u64().unwrap() as u16,
                },
                Utxo {
                    address: hash20(&row["address"]).into(),
                    denomination: row["denomination"].as_u64().unwrap() as u8,
                    lock: bytes(&row["lock"]),
                },
            )
            .unwrap();
    }
    let mut history = History::default();
    history
        .insert(Record::decode(&bytes(&v["genesis"])).unwrap())
        .unwrap();
    for raw in v["checkpoint_records"].as_array().unwrap() {
        history
            .insert(Record::decode(&bytes(raw)).unwrap())
            .unwrap();
    }
    let mut index = CandidateIndex::default();
    for row in v["checkpoint_snapshots"].as_array().unwrap() {
        let values = |key: &str| {
            row["termini"][key]
                .as_array()
                .unwrap()
                .iter()
                .map(hash)
                .collect::<Vec<_>>()
                .try_into()
                .unwrap()
        };
        index
            .insert_imported(
                hash(&row["hash"]),
                Snapshot {
                    manifest: Some(
                        row["manifest"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(hash)
                            .collect(),
                    ),
                    interlinks: None,
                    termini: Some(Termini {
                        dominant: values("dom"),
                        subordinate: values("sub"),
                    }),
                },
            )
            .unwrap();
    }
    let mut past = Past::default();
    for row in v["historical_slices"].as_array().unwrap() {
        let r = Record::decode(&bytes(&row["work_object"])).unwrap();
        let height = row["height"].as_u64().unwrap();
        assert_eq!(r.number(2).unwrap(), height);
        assert!(r.object().transactions().is_empty());
        assert!(row["incoming"].as_array().unwrap().is_empty());
        assert!(row["created"].as_array().unwrap().is_empty());
        past.incoming.insert(height, vec![]);
        past.created.insert(height, vec![]);
    }
    (state, history, index, past)
}
fn hash20(v: &Value) -> [u8; 20] {
    bytes(v).try_into().unwrap()
}

struct CheckpointReader {
    records: BTreeMap<Hash, Arc<Record>>,
    snapshots: BTreeMap<Hash, Arc<Snapshot>>,
    genesis: Hash,
    head: Hash,
}
impl quai_consensus::confirmation::Reader for CheckpointReader {
    fn heads(&self) -> Result<[Hash; 3], quai_consensus::Error> {
        Ok([self.genesis, self.genesis, self.head])
    }
    fn record(
        &self,
        context: usize,
        id: &Hash,
    ) -> Result<Option<Arc<Record>>, quai_consensus::Error> {
        Ok(if context == 2 || *id == self.genesis {
            self.records.get(id).cloned()
        } else {
            None
        })
    }
    fn snapshot(
        &self,
        context: usize,
        id: &Hash,
    ) -> Result<Option<Arc<Snapshot>>, quai_consensus::Error> {
        if *id == self.genesis {
            let mut p = zone_admission::profile()?;
            p.location.truncate(context);
            return Ok(Some(Arc::new(
                quai_consensus::placement::bootstrap_candidate(self.records.get(id).unwrap(), &p)?,
            )));
        }
        Ok(if context == 2 {
            self.snapshots.get(id).cloned()
        } else {
            None
        })
    }
    fn order(&self, id: &Hash) -> Result<Option<u8>, quai_consensus::Error> {
        Ok(self
            .records
            .contains_key(id)
            .then_some(if *id == self.genesis { 0 } else { 2 }))
    }
    fn region_rollup(
        &self,
        id: &Hash,
    ) -> Result<Option<Arc<Vec<Transaction>>>, quai_consensus::Error> {
        Ok((*id == self.genesis).then(|| Arc::new(vec![])))
    }
    fn confirmed(&self, id: &Hash) -> Result<Option<Arc<Vec<Transaction>>>, quai_consensus::Error> {
        Ok(self.records.contains_key(id).then(|| Arc::new(vec![])))
    }
}
pub fn reader(v: &Value, index: &CandidateIndex) -> Arc<dyn quai_consensus::confirmation::Reader> {
    let genesis = Record::decode(&bytes(&v["genesis"])).unwrap();
    let gid = genesis.id();
    let mut records = BTreeMap::from([(gid, Arc::new(genesis))]);
    let mut snapshots = BTreeMap::new();
    let mut head = gid;
    for row in v["checkpoint_records"].as_array().unwrap() {
        let r = Record::decode(&bytes(row)).unwrap();
        head = r.id();
        snapshots.insert(head, index.get(&head).unwrap());
        records.insert(head, Arc::new(r));
    }
    Arc::new(CheckpointReader {
        records,
        snapshots,
        genesis: gid,
        head,
    })
}
pub fn graph(v: &Value, index: &CandidateIndex) -> quai_consensus::confirmation::Hierarchy {
    quai_consensus::confirmation::Hierarchy::from_reader(reader(v, index)).unwrap()
}

pub fn same(a: &State, b: &State) {
    let x = a.world.materialize().unwrap();
    let y = b.world.materialize().unwrap();
    assert_eq!(x.accounts().unwrap(), y.accounts().unwrap());
    assert_eq!(x.etxs(), y.etxs());
    assert_eq!(x.lockups().unwrap(), y.lockups().unwrap());
    assert_eq!(
        a.utxos.materialize().unwrap(),
        b.utxos.materialize().unwrap()
    );
    assert_eq!(a.utxos.accumulator(), b.utxos.accumulator());
    assert_eq!(a.utxos.consensus_size(), b.utxos.consensus_size());
}

pub fn restore(db: &StateDatabase, head: Hash, c: &Commitment) -> State {
    let (a, u) =
        state_store::restore_sparse(Arc::new(Mutex::new(db.snapshot(Some(head)).unwrap())), c)
            .unwrap();
    assert!(a.is_lazy() && a.etxs().is_lazy() && a.lazy_lockups() && u.is_lazy());
    assert_eq!(a.loaded_accounts(), 0);
    assert_eq!(a.etxs().loaded_entries(), 0);
    assert_eq!(a.loaded_lockups(), 0);
    assert_eq!(u.loaded_entries(), 0);
    State { world: a, utxos: u }
}

pub fn verify(v: &Value, path: &Path) -> Value {
    assert_eq!(v["reference_revision"], quai_primitives::REFERENCE_REVISION);
    assert_eq!(v["checkpoint_height"], 259200);
    assert_eq!(v["validated_from_genesis"], false);
    assert_eq!(v["canonical_authority"], false);
    let profile = Profile::reviewed_admission().unwrap();
    let placement = zone_admission::profile().unwrap();
    let mut checks = 0;
    let mut rejected = 0;
    let mut roundtrips = 0;
    let mut reopens = 0;
    for sparse in [false, true] {
        let (mut state, mut history, mut index, past) = inputs(v);
        let mut verifier = Verifier::default();
        let mut hierarchy = graph(v, &index);
        let first = Record::decode(&bytes(&v["vectors"][0]["work_object"])).unwrap();
        assert!(matches!(
            hierarchy.prepare(&mut verifier, &first, &[], v["now"].as_u64().unwrap()),
            Err(quai_consensus::Error::Unsupported(_))
        ));
        checks += 1;

        let db_path = path.join(if sparse { "stored.redb" } else { "memory.redb" });
        let marker = keccak(b"explicit-unadmitted-activation-components");
        let mut db = StateDatabase::open(&db_path, marker).unwrap();
        let empty = State::default();
        let mut commitment = None;
        let initial = db
            .commit_prepared(None, None, keccak(b"supplied-checkpoint"), true, |w| {
                let c = state_store::apply_delta(
                    w,
                    &empty.world,
                    &empty.utxos,
                    &state.world,
                    &state.utxos,
                )?;
                let meta = c.digest.to_vec();
                commitment = Some(c);
                Ok(meta)
            })
            .unwrap();
        let mut head = initial.version.id;
        let mut commitment = commitment.unwrap();
        if sparse {
            let recovered = restore(&db, head, &commitment);
            same(&state, &recovered);
            state = recovered;
            roundtrips += 1;
        }
        for row in v["vectors"].as_array().unwrap() {
            let block = Record::decode(&bytes(&row["work_object"])).unwrap();
            for wrong in v["negatives"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|n| n["parent_height"].as_u64() == Some(block.number(2).unwrap() - 1))
            {
                let bad = Record::decode(&bytes(&wrong["work_object"])).unwrap();
                assert_eq!(wrong["header_body_accepted"], true);
                assert_eq!(wrong["accepted"], false);
                let proof = verifier.verify_header_proof(bad.header()).unwrap();
                assert_eq!(encode_hex(&proof.hash()), wrong["work_hash"]);
                let header = zone_admission::inspect_indexed_local(
                    &mut verifier,
                    &history,
                    &index,
                    &bad,
                    &placement,
                    v["now"].as_u64().unwrap(),
                )
                .unwrap();
                assert!(
                    header.header_policy_checked && header.body_checked && header.ancestry_checked
                );
                assert!(!header.fully_validated && !header.history_verified);
                if wrong["rejection_stage"] == "rollup" {
                    let before = state.clone();
                    let selected = db.head().unwrap();
                    assert!(matches!(
                        hierarchy.prepare_activated_local_components(
                            &mut verifier,
                            &bad,
                            v["now"].as_u64().unwrap()
                        ),
                        Err(quai_consensus::Error::Invalid(_))
                    ));
                    assert_eq!(state, before);
                    assert_eq!(db.head().unwrap(), selected);
                    checks += 6;
                    rejected += 1;
                    continue;
                }
                let prepared = hierarchy
                    .prepare_activated_local_components(
                        &mut verifier,
                        &bad,
                        v["now"].as_u64().unwrap(),
                    )
                    .unwrap();
                assert!(
                    prepared.newly_confirmed().is_empty()
                        && prepared.inbound_for_execution().is_empty()
                );
                checks += 1;

                let before = state.clone();
                let selected = db.head().unwrap();
                let err = zone_execution::inspect_local_components(
                    &mut verifier,
                    &mut state,
                    &history,
                    &past,
                    &index,
                    &bad,
                    &profile,
                    &placement,
                    &[],
                    v["now"].as_u64().unwrap(),
                );
                assert!(
                    matches!(err, Err(quai_consensus::Error::Invalid(_))),
                    "{}: {:?}",
                    wrong["id"],
                    err.err()
                );
                assert_eq!(state, before, "{}", wrong["id"]);
                assert_eq!(db.head().unwrap(), selected);
                let err = zone_execution::execute_prepared_components(
                    &mut verifier,
                    &mut state,
                    &history,
                    &past,
                    &bad,
                    &profile,
                    &prepared,
                );
                assert!(matches!(err, Err(quai_consensus::Error::Invalid(_))));
                assert_eq!(state, before);
                checks += 2;
                checks += 6;
                rejected += 1;
            }
            let prepared = hierarchy
                .prepare_activated_local_components(
                    &mut verifier,
                    &block,
                    v["now"].as_u64().unwrap(),
                )
                .unwrap();
            let before = state.clone();
            let result = zone_execution::inspect_local_components(
                &mut verifier,
                &mut state,
                &history,
                &past,
                &index,
                &block,
                &profile,
                &placement,
                &[],
                v["now"].as_u64().unwrap(),
            )
            .unwrap_or_else(|e| panic!("{}: {e}", row["id"]));
            assert!(
                result.append.header_policy_checked
                    && result.append.body_checked
                    && result.append.ancestry_checked
            );
            assert!(
                !result.append.fully_validated
                    && !result.append.history_verified
                    && !result.execution.fully_validated
            );
            assert_eq!(prepared.snapshot(2).unwrap(), &result.append.next);
            assert_eq!(
                prepared
                    .region_rollup()
                    .iter()
                    .map(|t| encode_hex(&t.encode()))
                    .collect::<Vec<_>>(),
                row["cross_rollup"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| t.as_str().unwrap())
                    .collect::<Vec<_>>()
            );
            assert!(
                prepared.newly_confirmed().is_empty()
                    && prepared.inbound_for_execution().is_empty()
            );
            checks += 3;
            let mut bound = before.clone();
            let executed = zone_execution::execute_prepared_components(
                &mut verifier,
                &mut bound,
                &history,
                &past,
                &block,
                &profile,
                &prepared,
            )
            .unwrap();
            same(&state, &bound);
            assert_eq!(
                executed
                    .receipts
                    .iter()
                    .map(|r| r.typed_rlp().unwrap())
                    .collect::<Vec<_>>(),
                result
                    .execution
                    .receipts
                    .iter()
                    .map(|r| r.typed_rlp().unwrap())
                    .collect::<Vec<_>>()
            );
            checks += 2;
            state = bound;
            checks += 2;
            for (key, actual) in [
                ("gas_used", json!(result.execution.gas_used)),
                ("state_used", json!(result.execution.state_used)),
                ("quai_state_size", json!(state.world.account_count())),
                ("utxo_set_size", json!(state.utxos.consensus_size())),
                ("evm_root", json!(encode_hex(&state.world.root().unwrap()))),
                (
                    "etx_root",
                    json!(encode_hex(&state.world.etxs().root().unwrap())),
                ),
                ("utxo_root", json!(encode_hex(&state.utxos.root()))),
                (
                    "receipt_rlp",
                    json!(result
                        .execution
                        .receipts
                        .iter()
                        .map(|r| encode_hex(&r.typed_rlp().unwrap()))
                        .collect::<Vec<_>>()),
                ),
                ("outbound_count", json!(result.execution.outbound.len())),
                ("unlock_count", json!(result.execution.unlocks.len())),
            ] {
                assert_eq!(actual, row[key], "{} {key}", row["id"]);
                checks += 1;
            }
            assert_eq!(
                result.append.next.manifest.as_ref().unwrap(),
                &row["manifest"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(hash)
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                encode_hex(&result.append.previous_dominant),
                row["returned_dominant"]
            );
            let terms = result.append.next.termini.as_ref().unwrap();
            assert_eq!(
                json!(terms.dominant.map(|h| encode_hex(&h))),
                row["termini"]["dom"]
            );
            assert_eq!(
                json!(terms.subordinate.map(|h| encode_hex(&h))),
                row["termini"]["sub"]
            );
            checks += 4;
            let proof = verifier.verify_header_proof(block.header()).unwrap();
            assert_eq!(encode_hex(&proof.hash()), row["work_hash"]);
            let types = block
                .object()
                .transactions()
                .iter()
                .map(|t| match t.kind() {
                    Kind::Quai(_) => 0,
                    Kind::External(_) => 1,
                    Kind::Qi(_) => 2,
                })
                .collect::<Vec<_>>();
            if block.number(2).unwrap() == 259201 {
                assert_eq!(types, vec![2, 0, 1]);
            }
            checks += 2;
            let mut next = None;
            let done = db
                .commit_prepared(Some(head), Some(head), block.id(), true, |w| {
                    let c = state_store::apply_delta(
                        w,
                        &before.world,
                        &before.utxos,
                        &state.world,
                        &state.utxos,
                    )?;
                    let meta = c.digest.to_vec();
                    next = Some(c);
                    Ok(meta)
                })
                .unwrap();
            head = done.version.id;
            commitment = next.unwrap();
            let recovered = restore(&db, head, &commitment);
            same(&state, &recovered);
            roundtrips += 1;
            checks += 1;
            drop(db);
            db = StateDatabase::open(&db_path, marker).unwrap();
            assert_eq!(db.head().unwrap(), Some(head));
            let reopened = restore(&db, head, &commitment);
            same(&state, &reopened);
            reopens += 1;
            checks += 1;
            if sparse {
                state = reopened;
            }
            index
                .insert_imported(block.id(), result.append.next)
                .unwrap();
            hierarchy.commit(prepared).unwrap();
            history.insert(block).unwrap();
        }
    }
    json!({"passed":true,"checks":checks,"valid_blocks":4,"valid_work_rejections":33,"backend_runs":2,"rejected_attempts":rejected,"state_roundtrips":roundtrips,"writer_reopens":reopens,"checkpoint_height":259200,"mixed_signed_qi_quai_standard_etx":true,"native_header_body_manifest_pcrc_and_execution":true,"prepared_hierarchy_drives_execution":true,"activated_local_hierarchy_rollups":true,"lazy_state_checked":true,"checkpoint_ancestry_verified":false,"canonical_publication":false,"global_full_consensus":false,"canonical_public_sync":false,"runtime_go_dependency":false})
}
fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let v = serde_json::from_str(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
    println!("{}", verify(&v, Path::new(&args[2])));
}
