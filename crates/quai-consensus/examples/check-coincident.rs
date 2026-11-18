//! Conditional activated hierarchy/execution replay; local versions grant no admission.
#[allow(dead_code)]
#[path = "check-activation.rs"]
mod base;
use quai_consensus::{
    confirmation::{Hierarchy, LocalOutputs, Reader},
    history::{History, Record},
    placement::Snapshot as BranchSnapshot,
    verifier::Verifier,
    zone_execution::{self, Profile, State},
    Error,
};
use quai_execution::{
    state_image::Roots,
    state_store::{self, Commitment},
};
use quai_primitives::{
    decode_hex, encode_hex,
    transaction::{keccak, Kind, Transaction},
    Hash,
};
use quai_store::state_db::{Snapshot, StateDatabase};
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
fn unavailable(_: impl std::fmt::Display) -> Error {
    Error::Unavailable("conditional persisted branch")
}
fn cjson(c: &Commitment) -> Value {
    json!({"digest":encode_hex(&c.digest),"evm":encode_hex(&c.roots.evm),"etx":encode_hex(&c.roots.etx),"utxo":encode_hex(&c.roots.utxo),"accounts":c.roots.accounts,"utxo_count":c.roots.utxo_count})
}
fn commitment(v: &Value) -> Commitment {
    Commitment {
        digest: hash(&v["digest"]),
        roots: Roots {
            evm: hash(&v["evm"]),
            etx: hash(&v["etx"]),
            utxo: hash(&v["utxo"]),
            accounts: v["accounts"].as_u64().unwrap(),
            utxo_count: v["utxo_count"].as_u64().unwrap(),
        },
    }
}
fn seed(v: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in [
        "genesis",
        "checkpoint_records",
        "checkpoint_snapshots",
        "historical_slices",
        "initial_world",
        "initial_etxs",
        "initial_utxos",
    ] {
        out.insert(key.into(), v[key].clone());
    }
    Value::Object(out)
}
fn views(row: &Value) -> Vec<(u8, Vec<u8>)> {
    row["contexts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| {
            let c = r["context"].as_u64().unwrap() as u8;
            (c < 2).then(|| (c, bytes(&r["work_object"])))
        })
        .collect()
}
fn wire(t: &[Transaction]) -> Vec<String> {
    t.iter().map(|t| encode_hex(&t.encode())).collect()
}
struct Loaded {
    records: [Option<Arc<Record>>; 3],
    outputs: LocalOutputs,
}
/// This test reader applies the same immutable-version ancestry constraint as
/// native branch storage. The initial checkpoint is an explicitly trusted input.
pub struct Persisted {
    snapshot: Arc<Mutex<Snapshot>>,
    initial: Arc<dyn Reader>,
    heads: [Hash; 3],
    cache: Mutex<BTreeMap<Hash, Arc<Loaded>>>,
}
impl Persisted {
    fn new(snapshot: Snapshot) -> Self {
        let first = snapshot.ancestor(0).unwrap();
        let raw = snapshot.metadata_for(first.id).unwrap();
        let initial: Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(initial["kind"], "supplied-coincident-checkpoint-v1");
        let (_, _, index, _) = base::inputs(&initial["seed"]);
        let initial = base::reader(&initial["seed"], &index);
        let head: Value =
            serde_json::from_slice(&snapshot.metadata_for(snapshot.version().id).unwrap()).unwrap();
        let heads = if snapshot.version().height == 0 {
            initial.heads().unwrap()
        } else {
            let zone = Record::decode(&bytes(&head["zone"])).unwrap();
            LocalOutputs::decode(&bytes(&head["outputs"]), &zone)
                .unwrap()
                .heads()
        };
        Self {
            snapshot: Arc::new(Mutex::new(snapshot)),
            initial,
            heads,
            cache: Mutex::new(BTreeMap::new()),
        }
    }
    fn entry(&self, id: &Hash) -> Result<Option<Arc<Loaded>>, Error> {
        if let Some(e) = self.cache.lock().map_err(unavailable)?.get(id) {
            return Ok(Some(e.clone()));
        }
        let (snapshot_raw, block, parent_block) = {
            let s = self.snapshot.lock().map_err(unavailable)?;
            let Some(v) = s.version_for_block(*id).map_err(unavailable)? else {
                return Ok(None);
            };
            if v.height == 0
                || v.height > s.version().height
                || s.ancestor(v.height).map_err(unavailable)?.id != v.id
            {
                return Ok(None);
            }
            let parent = s.ancestor(v.height - 1).map_err(unavailable)?;
            (
                s.metadata_for(v.id).map_err(unavailable)?,
                v.block,
                parent.block,
            )
        };
        let value: Value = serde_json::from_slice(&snapshot_raw).map_err(unavailable)?;
        if value["kind"] != "conditional-coincident-components-v1" {
            return Err(unavailable("metadata format"));
        }
        let zone = Record::decode(&bytes(&value["zone"]))?;
        if zone.id() != block || zone.parent(2)? != parent_block {
            return Err(unavailable("local parent binding"));
        }
        let outputs = LocalOutputs::decode(&bytes(&value["outputs"]), &zone)?;
        let mut records = [None, None, Some(Arc::new(zone))];
        for (c, raw) in views(&value) {
            let r = Record::decode(&raw)?;
            if r.id() != block || c < outputs.order() {
                return Err(unavailable("local role binding"));
            }
            records[c as usize] = Some(Arc::new(r));
        }
        let loaded = Arc::new(Loaded { records, outputs });
        let mut cache = self.cache.lock().map_err(unavailable)?;
        if cache.len() >= 32 {
            let first = *cache.keys().next().unwrap();
            cache.remove(&first);
        }
        cache.insert(*id, loaded.clone());
        Ok(Some(loaded))
    }
}
impl Reader for Persisted {
    fn heads(&self) -> Result<[Hash; 3], Error> {
        Ok(self.heads)
    }
    fn record(&self, c: usize, id: &Hash) -> Result<Option<Arc<Record>>, Error> {
        if let Some(r) = self.initial.record(c, id)? {
            return Ok(Some(r));
        }
        Ok(self
            .entry(id)?
            .and_then(|e| e.records.get(c).cloned().flatten()))
    }
    fn snapshot(&self, c: usize, id: &Hash) -> Result<Option<Arc<BranchSnapshot>>, Error> {
        if let Some(r) = self.initial.snapshot(c, id)? {
            return Ok(Some(r));
        }
        Ok(self
            .entry(id)?
            .and_then(|e| e.outputs.snapshot(c).cloned().map(Arc::new)))
    }
    fn order(&self, id: &Hash) -> Result<Option<u8>, Error> {
        if let Some(v) = self.initial.order(id)? {
            return Ok(Some(v));
        }
        Ok(self.entry(id)?.map(|e| e.outputs.order()))
    }
    fn region_rollup(&self, id: &Hash) -> Result<Option<Arc<Vec<Transaction>>>, Error> {
        if let Some(v) = self.initial.region_rollup(id)? {
            return Ok(Some(v));
        }
        Ok(self
            .entry(id)?
            .filter(|e| e.outputs.order() <= 1)
            .map(|e| Arc::new(e.outputs.region_rollup().to_vec())))
    }
    fn confirmed(&self, id: &Hash) -> Result<Option<Arc<Vec<Transaction>>>, Error> {
        if let Some(v) = self.initial.confirmed(id)? {
            return Ok(Some(v));
        }
        Ok(self
            .entry(id)?
            .map(|e| Arc::new(e.outputs.confirmed().to_vec())))
    }
}
struct Hist(Arc<Persisted>);
impl quai_consensus::history::Reader for Hist {
    fn read(&self, id: &Hash) -> Result<Option<Arc<Record>>, Error> {
        self.0.record(2, id)
    }
}
fn check(ok: bool, n: &mut usize, why: &str) {
    *n += 1;
    assert!(ok, "{why}")
}
pub fn verify(v: &Value, path: &Path) -> Value {
    assert_eq!(v["reference_revision"], quai_primitives::REFERENCE_REVISION);
    assert_eq!(v["checkpoint_height"], 259200);
    assert_eq!(v["canonical_authority"], false);
    assert_eq!(v["validated_from_genesis"], false);
    let profile = Profile::reviewed_admission().unwrap();
    let mut checks = 0;
    let mut rejected = 0;
    let mut rounds = 0;
    let mut reopens = 0;
    let mut contexts = 0;
    let mut incoming = 0;
    let mut orders = [0; 3];
    let mut reused = 0;
    let mut isolated = 0;
    for sparse in [false, true] {
        let file = path.join(if sparse { "sparse.redb" } else { "memory.redb" });
        let marker = keccak(b"conditional-coincident-only-v1");
        let mut db = StateDatabase::open(&file, marker).unwrap();
        let supplied = seed(&v["branches"][0]);
        let (initial, _, _, _) = base::inputs(&supplied);
        let empty = State::default();
        let mut c = None;
        let cp_block = Record::decode(&bytes(
            supplied["checkpoint_records"]
                .as_array()
                .unwrap()
                .last()
                .unwrap(),
        ))
        .unwrap()
        .id();
        let cp=db.commit_prepared(None,None,cp_block,true,|w|{let commit=state_store::apply_delta(w,&empty.world,&empty.utxos,&initial.world,&initial.utxos)?;let meta=serde_json::to_vec(&json!({"kind":"supplied-coincident-checkpoint-v1","seed":supplied,"state":cjson(&commit)})).unwrap();c=Some(commit);Ok(meta)}).unwrap().version.id;
        let cp_commitment = c.unwrap();
        let mut branch_ends = vec![];
        for branch in v["branches"].as_array().unwrap() {
            let (mut state, mut history, index, past) = base::inputs(branch);
            let mut graph = base::graph(branch, &index);
            let mut parent = cp;
            let mut verifier = Verifier::default();
            if sparse {
                state = base::restore(&db, cp, &cp_commitment);
                rounds += 1;
            }
            for row in branch["vectors"].as_array().unwrap() {
                let block = Record::decode(&bytes(&row["work_object"])).unwrap();
                if sparse {
                    let reader = Arc::new(Persisted::new(db.snapshot(Some(parent)).unwrap()));
                    history = History::with_reader(Arc::new(Hist(reader.clone())));
                    graph = Hierarchy::from_reader(reader).unwrap();
                }
                for wrong in
                    branch["negatives"].as_array().unwrap().iter().filter(|r| {
                        r["parent_height"].as_u64() == Some(block.number(2).unwrap() - 1)
                    })
                {
                    let bad = Record::decode(&bytes(&wrong["work_object"])).unwrap();
                    check(
                        encode_hex(&verifier.verify_header_proof(bad.header()).unwrap().hash())
                            == wrong["work_hash"],
                        &mut checks,
                        "native proof matches Go mutation",
                    );
                    let before = state.clone();
                    let heads = graph.heads();
                    let selected = db.head().unwrap();
                    let prepared = graph.prepare_activated_components(
                        &mut verifier,
                        &bad,
                        &views(wrong),
                        v["now"].as_u64().unwrap(),
                    );
                    let stage = wrong["rejection_stage"].as_str().unwrap();
                    if matches!(stage, "rollup" | "context-body") {
                        check(
                            matches!(prepared, Err(Error::Invalid(_))),
                            &mut checks,
                            "Go-rejected rollup/context witness rejected",
                        );
                    } else {
                        let prepared =
                            prepared.unwrap_or_else(|e| panic!("{} header: {e}", wrong["id"]));
                        let result = zone_execution::execute_prepared_components(
                            &mut verifier,
                            &mut state,
                            &history,
                            &past,
                            &bad,
                            &profile,
                            &prepared,
                        );
                        check(
                            matches!(result, Err(Error::Invalid(_))),
                            &mut checks,
                            "Go-rejected execution/state mutation rejected",
                        );
                    }
                    check(
                        state == before,
                        &mut checks,
                        "rejection keeps account/UTXO/ETX state",
                    );
                    check(
                        graph.heads() == heads,
                        &mut checks,
                        "rejection keeps every context head",
                    );
                    check(
                        db.head().unwrap() == selected,
                        &mut checks,
                        "rejection keeps selected local version",
                    );
                    rejected += 1;
                }
                let prepared = graph
                    .prepare_activated_components(
                        &mut verifier,
                        &block,
                        &views(row),
                        v["now"].as_u64().unwrap(),
                    )
                    .unwrap_or_else(|e| panic!("{} prepare: {e}", row["id"]));
                check(
                    prepared.order() as u64 == row["order"].as_u64().unwrap(),
                    &mut checks,
                    "native order",
                );
                check(
                    wire(prepared.inbound_for_execution())
                        == row["inbound"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|t| t.as_str().unwrap())
                            .collect::<Vec<_>>(),
                    &mut checks,
                    "derived execution inbounds",
                );
                check(
                    wire(prepared.newly_confirmed())
                        == row["confirmed"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|t| t.as_str().unwrap())
                            .collect::<Vec<_>>(),
                    &mut checks,
                    "newly confirmed transactions",
                );
                check(
                    wire(prepared.region_rollup())
                        == row["cross_rollup"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|t| t.as_str().unwrap())
                            .collect::<Vec<_>>(),
                    &mut checks,
                    "cross-prime rollup",
                );
                let encoded = prepared.encode_local_outputs().unwrap();
                let outputs = LocalOutputs::decode(&encoded, &block).unwrap();
                for c in 0..3 {
                    check(
                        outputs.snapshot(c) == prepared.snapshot(c),
                        &mut checks,
                        "persisted component snapshot",
                    );
                }
                check(
                    wire(outputs.confirmed()) == wire(prepared.newly_confirmed()),
                    &mut checks,
                    "persisted confirmations",
                );
                for ctx in row["contexts"].as_array().unwrap() {
                    let role = ctx["context"].as_u64().unwrap() as usize;
                    let snapshot = prepared.snapshot(role).unwrap();
                    let terms = snapshot.termini.as_ref().unwrap();
                    check(
                        json!(terms.dominant.map(|h| encode_hex(&h))) == ctx["termini"]["dom"],
                        &mut checks,
                        "Go dominant termini",
                    );
                    check(
                        json!(terms.subordinate.map(|h| encode_hex(&h))) == ctx["termini"]["sub"],
                        &mut checks,
                        "Go subordinate termini",
                    );
                    check(
                        encode_hex(&prepared.returned_dominant(role).unwrap())
                            == ctx["returned_dominant"],
                        &mut checks,
                        "Go PCRC returned claim",
                    );
                    contexts += 1;
                }
                let before = state.clone();
                let executed = zone_execution::execute_prepared_components(
                    &mut verifier,
                    &mut state,
                    &history,
                    &past,
                    &block,
                    &profile,
                    &prepared,
                )
                .unwrap_or_else(|e| panic!("{} execute: {e}", row["id"]));
                check(
                    !executed.fully_validated,
                    &mut checks,
                    "component does not mint authority",
                );
                for (key, value) in [
                    ("gas_used", json!(executed.gas_used)),
                    ("state_used", json!(executed.state_used)),
                    ("evm_root", json!(encode_hex(&state.world.root().unwrap()))),
                    (
                        "etx_root",
                        json!(encode_hex(&state.world.etxs().root().unwrap())),
                    ),
                    ("utxo_root", json!(encode_hex(&state.utxos.root()))),
                    ("utxo_set_size", json!(state.utxos.consensus_size())),
                    ("quai_state_size", json!(state.world.account_count())),
                    (
                        "receipt_rlp",
                        json!(executed
                            .receipts
                            .iter()
                            .map(|r| encode_hex(&r.typed_rlp().unwrap()))
                            .collect::<Vec<_>>()),
                    ),
                    ("outbound_count", json!(executed.outbound.len())),
                    ("unlock_count", json!(executed.unlocks.len())),
                ] {
                    check(
                        value == row[key],
                        &mut checks,
                        &format!("{} {key}", row["id"]),
                    );
                }
                if block.number(2).unwrap() == 259201 {
                    check(
                        block
                            .object()
                            .transactions()
                            .iter()
                            .map(|t| match t.kind() {
                                Kind::Qi(_) => 2,
                                Kind::Quai(_) => 0,
                                Kind::External(_) => 1,
                            })
                            .collect::<Vec<_>>()
                            == [2, 0, 1],
                        &mut checks,
                        "mixed-ledger first block",
                    );
                }
                let child = if let Some(existing) = db.version_for_block(block.id()).unwrap() {
                    reused += 1;
                    existing
                } else {
                    db.commit_prepared(Some(cp),Some(parent),block.id(),false,|w|{let commit=state_store::apply_delta(w,&before.world,&before.utxos,&state.world,&state.utxos)?;Ok(serde_json::to_vec(&json!({"kind":"conditional-coincident-components-v1","zone":encode_hex(block.bytes()),"contexts":row["contexts"],"outputs":encode_hex(&encoded),"state":cjson(&commit)})).unwrap())}).unwrap().version.id
                };
                let snapshot = db.snapshot(Some(child)).unwrap();
                let metadata: Value =
                    serde_json::from_slice(&snapshot.metadata_for(child).unwrap()).unwrap();
                let c = commitment(&metadata["state"]);
                drop(snapshot);
                let restored = base::restore(&db, child, &c);
                base::same(&state, &restored);
                rounds += 1;
                check(
                    db.head().unwrap() == Some(cp),
                    &mut checks,
                    "conditional versions cannot select canonical history",
                );
                if row["height"].as_u64().unwrap() % 4 == 0 {
                    drop(db);
                    db = StateDatabase::open(&file, marker).unwrap();
                    base::same(&state, &base::restore(&db, child, &c));
                    let recovered = Hierarchy::from_reader(Arc::new(Persisted::new(
                        db.snapshot(Some(child)).unwrap(),
                    )))
                    .unwrap();
                    check(
                        recovered.heads() == outputs.heads(),
                        &mut checks,
                        "all context heads recover from same local version",
                    );
                    reopens += 1;
                }
                if sparse {
                    state = restored;
                }
                orders[prepared.order() as usize] += 1;
                incoming += prepared.inbound_for_execution().len();
                graph.commit(prepared).unwrap();
                history.insert(block).unwrap();
                parent = child;
            }
            branch_ends.push(parent);
        }
        let a = db.snapshot(Some(branch_ends[0])).unwrap().version().block;
        let b = db.snapshot(Some(branch_ends[1])).unwrap().version().block;
        check(a != b, &mut checks, "competing conditional forks differ");
        let reader = Persisted::new(db.snapshot(Some(branch_ends[1])).unwrap());
        check(
            reader.record(2, &a).unwrap().is_none(),
            &mut checks,
            "foreign fork record is absent from pinned branch",
        );
        check(
            reader.snapshot(2, &a).unwrap().is_none(),
            &mut checks,
            "foreign fork snapshot is absent from pinned branch",
        );
        check(
            reader.confirmed(&a).unwrap().is_none(),
            &mut checks,
            "foreign fork confirmations are absent from pinned branch",
        );
        isolated += 1;
    }
    json!({"passed":true,"checks":checks,"branch_runs":32,"distinct_conditional_blocks":24,"backends":2,"valid_work_rejections":246,"rejected_attempts":rejected,"state_roundtrips":rounds,"writer_reopens":reopens,"context_checks":contexts,"execution_inbounds":incoming,"orders":orders,"shared_prefix_versions_reused":reused,"fork_isolation_checks":isolated,"persisted_hierarchy_outputs":true,"lazy_state_and_history_checked":true,"checkpoint_ancestry_verified":false,"canonical_publication":false,"global_full_consensus":false,"canonical_public_sync":false,"runtime_go_dependency":false})
}
fn main() {
    let a = std::env::args().collect::<Vec<_>>();
    let v = serde_json::from_str(&std::fs::read_to_string(&a[1]).unwrap()).unwrap();
    println!("{}", verify(&v, Path::new(&a[2])));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activated_hierarchy_requires_complete_bound_views_and_keeps_runtime_scope() {
        let v: Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/coincident.json"))
                .unwrap();
        let branch = &v["branches"][0];
        let (_, _, index, _) = base::inputs(branch);
        let mut graph = base::graph(branch, &index);
        let mut verifier = Verifier::default();
        let now = v["now"].as_u64().unwrap();
        let rows = branch["vectors"].as_array().unwrap();
        for row in &rows[..2] {
            let zone = Record::decode(&bytes(&row["work_object"])).unwrap();
            let p = graph
                .prepare_activated_components(&mut verifier, &zone, &views(row), now)
                .unwrap();
            graph.commit(p).unwrap();
        }
        let zone = Record::decode(&bytes(&rows[2]["work_object"])).unwrap();
        let heads = graph.heads();
        assert!(matches!(
            graph.prepare_activated_components(&mut verifier, &zone, &[], now),
            Err(Error::Unavailable(_))
        ));
        assert!(matches!(
            graph.prepare_activated_components(&mut verifier, &zone, &views(&rows[7]), now),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            graph.prepare(&mut verifier, &zone, &views(&rows[2]), now),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            graph.prepare_activated_local_components(&mut verifier, &zone, now),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(heads, graph.heads());
    }
}
