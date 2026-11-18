#[allow(dead_code)]
#[path = "../eligibility_support/mod.rs"]
pub mod eligibility;
use crypto_bigint::{Encoding, U512};
use quai_consensus::{
    candidate_state::CandidateState,
    coordinator::{Candidate, ReapplicationSchedule, RecentCandidates, SearchReport},
    eligibility::CoordinatorView,
    frontier::{Frontier, PendingFrontiers, Tip},
    initialization,
    rules::Uint,
    scoring,
    verifier::Verifier,
};
use quai_primitives::{encode_hex, Hash};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
pub fn uint(v: &Value) -> Result<Uint, String> {
    let n = eligibility::frontier::genesis::wide(v)?;
    if n.bits_vartime() > 256 {
        return Err("uint width".into());
    }
    Ok(n.resize())
}
pub fn candidate(v: &Value) -> Result<Candidate, String> {
    let a = v["numbers"].as_array().ok_or("numbers")?;
    Ok(Candidate {
        location: eligibility::location(&v["location"]),
        tip: Tip {
            id: eligibility::hash(&v["hash"])?,
            numbers: [
                a[0].as_u64().unwrap(),
                a[1].as_u64().unwrap(),
                a[2].as_u64().unwrap(),
            ],
            entropy: uint(&v["entropy"])?,
        },
    })
}
fn candidates(v: &Value) -> Result<Vec<Candidate>, String> {
    v.as_array().ok_or("nodes")?.iter().map(candidate).collect()
}
fn wide(v: &Value) -> Result<U512, String> {
    eligibility::frontier::genesis::wide(v)
}
fn key(l: &[u8], legacy: bool) -> Vec<u8> {
    let mut k = l.to_vec();
    if legacy && !k.is_empty() && k[0] >= 3 {
        k[0] = 3
    }
    k
}
pub fn digest(f: &Frontier) -> String {
    let mut h = Sha256::new();
    for (l, t) in f.tips() {
        h.update([l.len() as u8]);
        h.update(l);
        h.update(t.id);
        for n in t.numbers {
            h.update(n.to_be_bytes())
        }
        h.update(t.entropy.to_be_bytes())
    }
    let b = h.finalize();
    encode_hex(&b)[2..].to_string()
}
fn key_state(nodes: &[Candidate]) -> Value {
    let mut h = Sha256::new();
    for n in nodes {
        h.update(n.tip.id)
    }
    json!({"sha256":encode_hex(&h.finalize())[2..],"length":nodes.len(),"oldest":nodes.first().map(|n|encode_hex(&n.tip.id)),"newest":nodes.last().map(|n|encode_hex(&n.tip.id))})
}
#[derive(Default)]
struct Checks {
    checks: usize,
    mismatches: usize,
    failures: Vec<Value>,
}
impl Checks {
    fn check(&mut self, id: impl Into<Value>, field: &str, ok: bool) {
        self.checks += 1;
        if !ok {
            self.mismatches += 1;
            if self.failures.len() < 30 {
                self.failures.push(json!({"id":id.into(),"field":field}))
            }
        }
    }
    fn finish(self) -> Value {
        json!({"passed":self.mismatches==0,"checks":self.checks,"mismatches":self.mismatches,"failures":self.failures})
    }
}
pub struct Dataset {
    pub view: CoordinatorView,
    pub orders: BTreeMap<Hash, u8>,
    pub entropy: BTreeMap<(Vec<u8>, Hash), Uint>,
    pub genesis: Hash,
    pub expansion: u8,
    pub legacy: bool,
}
impl Dataset {
    pub fn decode(v: &Value) -> Result<Self, String> {
        let legacy = v["legacy"].as_bool().unwrap();
        let mut view = CoordinatorView::new(legacy);
        let gen = eligibility::hash(&v["genesis"])?;
        let mut orders = BTreeMap::new();
        let mut entropy = BTreeMap::new();
        for e in v["entries"].as_array().unwrap() {
            let loc = eligibility::location(&e["location"]);
            view.register(eligibility::profile(loc.clone(), gen))
                .map_err(|e| e.to_string())?;
            let r = eligibility::record(&e["block"])?;
            orders.insert(r.id(), e["order"].as_u64().unwrap() as u8);
            entropy.insert((loc.clone(), r.id()), uint(&e["entropy"])?);
            view.import_unverified(&loc, r, Some(eligibility::term(&e["termini"])?))
                .map_err(|e| e.to_string())?;
        }
        Ok(Self {
            view,
            orders,
            entropy,
            genesis: gen,
            expansion: v["expansion"].as_u64().unwrap() as u8,
            legacy,
        })
    }
    fn recent(&self, seeds: &Value) -> Result<RecentCandidates, String> {
        let mut r =
            RecentCandidates::new(self.expansion, self.legacy, 1000).map_err(|e| e.to_string())?;
        for n in candidates(seeds)? {
            r.import_unverified(&n.location, n.tip)
                .map_err(|e| e.to_string())?;
        }
        Ok(r)
    }
}
fn pending_checks(
    t: &mut Checks,
    id: Value,
    p: &PendingFrontiers,
    expected: &Value,
) -> Result<(), String> {
    let order = expected["order"]
        .as_array()
        .unwrap()
        .iter()
        .map(wide)
        .collect::<Result<Vec<_>, _>>()?;
    t.check(
        id.clone(),
        "pending processing order",
        p.processing_order() == order,
    );
    t.check(
        id.clone(),
        "pending entropy high water mark",
        p.best_ever() == wide(&expected["best_ever"])?,
    );
    t.check(
        id.clone(),
        "pending generation selection availability",
        p.peek(&p.best_ever()).is_some() == expected["generation_ready"].as_bool().unwrap(),
    );
    t.check(
        id.clone(),
        "pending retained count",
        p.len() == expected["sets"].as_array().unwrap().len(),
    );
    for row in expected["sets"].as_array().unwrap() {
        let f = p.peek(&wide(&row["key"])?);
        t.check(
            id.clone(),
            "pending exact frontier digest",
            f.is_some_and(|f| digest(f) == row["digest"].as_str().unwrap()),
        );
    }
    Ok(())
}
fn search_checks(
    t: &mut Checks,
    id: Value,
    r: &SearchReport,
    expected: &Value,
) -> Result<(), String> {
    t.check(
        id.clone(),
        "map search outcome",
        r.frontier.is_some() == expected["accepted"].as_bool().unwrap(),
    );
    let attempts = expected["attempts"].as_array().unwrap();
    t.check(
        id.clone(),
        "search attempt count",
        r.attempts.len() == attempts.len(),
    );
    for (a, b) in r.attempts.iter().zip(attempts) {
        t.check(
            id.clone(),
            "exact leader ordering",
            a.leaders == candidates(&b["leaders"])?,
        );
        t.check(
            id.clone(),
            "trace attempts",
            a.traces.len() == b["traces"].as_array().unwrap().len(),
        );
        for (x, y) in a.traces.iter().zip(b["traces"].as_array().unwrap()) {
            t.check(
                id.clone(),
                "trace leader binding",
                x.id == eligibility::hash(&y["hash"])?
                    && x.location == eligibility::location(&y["location"]),
            );
            t.check(
                id.clone(),
                "first flag after failed trace",
                x.first == y["first"].as_bool().unwrap(),
            );
            t.check(
                id.clone(),
                "trace outcome",
                x.accepted == y["accepted"].as_bool().unwrap(),
            );
        }
        t.check(
            id.clone(),
            "completed search constraints",
            a.constraints == eligibility::constraints(&b["constraints"])?,
        );
        let rejected = if b["rejected"].is_null() {
            None
        } else {
            Some(eligibility::hash(&b["rejected"])?)
        };
        t.check(
            id.clone(),
            "first rejected reference",
            a.rejected == rejected,
        );
    }
    t.check(
        id.clone(),
        "search remains unverified",
        !r.fully_validated
            && !r.history_verified
            && r.eligibility
                .as_ref()
                .is_none_or(|e| !e.fully_validated && !e.history_verified),
    );
    let mut p = PendingFrontiers::new(2000).unwrap();
    if let Some(f) = &r.frontier {
        p.offer(f.clone());
    }
    pending_checks(t, id, &p, &expected["pending"])
}
fn recent_checks(
    t: &mut Checks,
    id: Value,
    r: &RecentCandidates,
    expected: &Value,
) -> Result<(), String> {
    for row in expected.as_array().unwrap() {
        let got = r
            .at(&eligibility::location(&row["location"]))
            .map_err(|e| e.to_string())?;
        t.check(
            id.clone(),
            "recent cache after seeding",
            got == candidates(&row["nodes"])?,
        );
    }
    Ok(())
}
pub fn synthetic_frontier(exp: u8, seed: u64) -> Frontier {
    let (r, _) = initialization::hierarchy_size(exp);
    let mut locations = vec![vec![]];
    for i in 0..r as u8 {
        locations.push(vec![i]);
        for l in initialization::active_slices(exp).unwrap() {
            if l[0] == i {
                locations.push(l)
            }
        }
    }
    let mut aliases = BTreeMap::new();
    for (i, l) in locations.iter().enumerate() {
        let v = seed * 1000 + i as u64 + 1;
        let mut id = [0; 32];
        id[24..].copy_from_slice(&v.to_be_bytes());
        let entropy = if seed % 2 == 1 {
            Uint::MAX.wrapping_sub(&Uint::from_u64(i as u64))
        } else {
            Uint::from_u64(v)
        };
        aliases.insert(
            key(l, true),
            Tip {
                id,
                numbers: [i as u64, i as u64 + 1, i as u64 + 2],
                entropy,
            },
        );
    }
    Frontier::from_legacy_imported(
        exp,
        locations
            .into_iter()
            .map(|l| {
                let t = aliases[&key(&l, true)].clone();
                (l, t)
            })
            .collect(),
    )
    .unwrap()
}
pub fn policy(v: &Value) -> Result<Value, String> {
    let p = &v["policy"];
    let mut t = Checks::default();
    for (i, row) in p["sorts"].as_array().unwrap().iter().enumerate() {
        let mut r = RecentCandidates::new(0, false, 1000).unwrap();
        for n in candidates(&row["input"])? {
            r.import_unverified(&n.location, n.tip)
                .map_err(|e| e.to_string())?;
        }
        let a = r
            .candidates(&[0, 0], &BTreeSet::new())
            .map_err(|e| e.to_string())?;
        let b = candidates(&row["output"])?;
        t.check(json!(i), "sort result length", a.len() == b.len());
        for (a, b) in a.iter().zip(&b) {
            t.check(json!(i), "exact Go unstable tie ordering", a == b);
        }
    }
    for (i, row) in p["leader_boundaries"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let mut recent =
            RecentCandidates::new(row["expansion"].as_u64().unwrap() as u8, false, 1000).unwrap();
        for n in candidates(&row["seeds"])? {
            recent
                .import_unverified(&n.location, n.tip)
                .map_err(|e| e.to_string())?;
        }
        let bad = row["bad"]
            .as_array()
            .unwrap()
            .iter()
            .map(eligibility::hash)
            .collect::<Result<BTreeSet<_>, _>>()?;
        t.check(
            json!(i),
            "source empty/panic leader boundary recorded",
            row["source_panic"] == true || row["source_empty"] == true,
        );
        t.check(
            json!(i),
            "all-excluded leaders fail closed",
            matches!(
                recent.leaders(&bad),
                Err(quai_consensus::Error::Unavailable(_))
            ),
        );
    }
    let datasets = p["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(Dataset::decode)
        .collect::<Result<Vec<_>, _>>()?;
    let mut accepted = 0;
    let mut retries = 0;
    for (i, row) in p["searches"].as_array().unwrap().iter().enumerate() {
        let d = &datasets[row["dataset"].as_u64().unwrap() as usize];
        let mut recent = d.recent(&row["seeds"])?;
        let r = recent
            .search_projection(&d.view, d.genesis, &d.orders, &d.entropy)
            .map_err(|e| format!("search {i}: {e}"))?;
        if r.frontier.is_some() {
            accepted += 1
        }
        retries += r.attempts.len() - 1;
        search_checks(&mut t, json!(i), &r, &row["result"])?;
        recent_checks(&mut t, json!(i), &recent, &row["recent_after"])?;
    }
    let mut events = 0;
    for (i, row) in p["events"].as_array().unwrap().iter().enumerate() {
        let mut recent =
            RecentCandidates::new(1, false, row["capacity"].as_u64().unwrap() as usize).unwrap();
        for n in candidates(&row["seed"])? {
            recent
                .import_unverified(&n.location, n.tip)
                .map_err(|e| e.to_string())?;
        }
        for (j, step) in row["steps"].as_array().unwrap().iter().enumerate() {
            let n = candidate(&step["node"])?;
            let id = json!([i, j]);
            t.check(
                id.clone(),
                "recent cache before event",
                key_state(&recent.at(&n.location).unwrap()) == step["keys_before"],
            );
            let result = recent
                .observe_projection(&n.location, n.tip)
                .map_err(|e| e.to_string())?;
            if step["accepted"].is_boolean() {
                t.check(
                    id.clone(),
                    "oldest-entropy event admission",
                    result.accepted == step["accepted"].as_bool().unwrap(),
                );
            }
            t.check(
                id,
                "recent cache FIFO/duplicate movement",
                key_state(&recent.at(&n.location).unwrap()) == step["keys_after"],
            );
            events += 1;
        }
    }
    let mut replays = 0;
    for (i, row) in p["reapplications"].as_array().unwrap().iter().enumerate() {
        let exp = row["expansion"].as_u64().unwrap() as u8;
        let mut pending =
            PendingFrontiers::new(row["capacity"].as_u64().unwrap() as usize).unwrap();
        for seed in row["initial_seeds"].as_array().unwrap() {
            pending.offer(synthetic_frontier(exp, seed.as_u64().unwrap()));
        }
        pending_checks(&mut t, json!(i), &pending, &row["before"])?;
        let b = eligibility::record(&row["block"])?;
        for (j, step) in row["steps"].as_array().unwrap().iter().enumerate() {
            let r = pending
                .reapply_projection(
                    &b,
                    row["order"].as_u64().unwrap() as u8,
                    uint(&step["entropy"])?,
                )
                .map_err(|e| format!("replay {i}/{j}: {e}"))?;
            t.check(
                json!([i, j]),
                "21-visit source bound",
                r.visited == step["visited"].as_u64().unwrap() as usize,
            );
            t.check(
                json!([i, j]),
                "extendable source count",
                r.extended == step["extended"].as_u64().unwrap() as usize,
            );
            t.check(
                json!([i, j]),
                "reapplication remains unverified",
                !r.fully_validated && !r.history_verified,
            );
            pending_checks(&mut t, json!([i, j]), &pending, &step["after"])?;
            replays += 1;
        }
    }
    let mut schedule = ReapplicationSchedule::new(0).unwrap();
    let mut clock = 0;
    for (j, sleep) in p["sleeps"].as_array().unwrap().iter().enumerate() {
        t.check(
            json!(j),
            "virtual source retry due",
            schedule.due() == Some(clock) && schedule.take_due(clock),
        );
        clock += sleep.as_u64().unwrap();
    }
    t.check(
        json!("schedule"),
        "virtual source retry completion",
        schedule.due().is_none() && schedule.finished_at() == clock,
    );
    let mut r = t.finish();
    r["sort_cases"] = json!(p["sorts"].as_array().unwrap().len());
    r["search_cases"] = json!(p["searches"].as_array().unwrap().len());
    r["accepted_searches"] = json!(accepted);
    r["search_retries"] = json!(retries);
    r["recent_events"] = json!(events);
    r["reapplication_passes"] = json!(replays);
    Ok(r)
}
pub fn native(v: &Value) -> Result<Value, String> {
    let n = &v["native"];
    let input = &n["input"];
    let wrap = json!({"native":input});
    let assembly = eligibility::native(&wrap)?;
    if assembly["passed"] != true {
        return Err("native assembly qualification failed".into());
    }
    let graph = eligibility::native_view(&wrap)?;
    let gen = eligibility::hash(&input["genesis"])?;
    let mut verifier = Verifier::default();
    let mut t = Checks::default();
    let mut accepted = 0;
    for (i, row) in n["searches"].as_array().unwrap().iter().enumerate() {
        let mut recent = RecentCandidates::new(0, false, 1000).unwrap();
        for seed in candidates(&row["seeds"])? {
            let got = graph
                .tip_native(&mut verifier, &seed.location, seed.tip.id)
                .map_err(|e| e.to_string())?;
            t.check(
                json!(i),
                "native candidate entropy and numbers",
                got == seed.tip,
            );
            recent
                .import_unverified(&seed.location, got)
                .map_err(|e| e.to_string())?;
        }
        let r = recent
            .search_native(&graph, &mut verifier, gen)
            .map_err(|e| format!("native search {i}: {e}"))?;
        accepted += usize::from(r.frontier.is_some());
        search_checks(&mut t, json!(i), &r, &row["result"])?;
        recent_checks(&mut t, json!(i), &recent, &row["recent_after"])?;
    }
    let seed = candidate(&n["event_seed"])?;
    let mut recent = RecentCandidates::new(0, false, 1000).unwrap();
    recent
        .import_unverified(&seed.location, seed.tip)
        .map_err(|e| e.to_string())?;
    for (i, step) in n["events"].as_array().unwrap().iter().enumerate() {
        let node = candidate(&step["node"])?;
        let actual = graph
            .tip_native(&mut verifier, &node.location, node.tip.id)
            .map_err(|e| e.to_string())?;
        t.check(json!(i), "native event entropy binding", actual == node.tip);
        let a = recent
            .observe_native(&graph, &mut verifier, &node.location, node.tip.id)
            .map_err(|e| e.to_string())?;
        t.check(
            json!(i),
            "native event admission",
            a.accepted == step["accepted"].as_bool().unwrap(),
        );
        t.check(
            json!(i),
            "native event FIFO and duplicate movement",
            key_state(&recent.at(&node.location).unwrap()) == step["after"],
        );
    }
    let profile = eligibility::profile(vec![], gen);
    let mut state = CandidateState::new(profile.clone()).map_err(|e| e.to_string())?;
    state
        .import_unverified_ancestry(
            eligibility::record(&input["root"])?,
            eligibility::frontier::genesis::placement::snapshot(&input["seeds"][0]["snapshot"])?,
        )
        .map_err(|e| e.to_string())?;
    let mut scores = vec![];
    let mut records = vec![];
    for step in input["steps"].as_array().unwrap() {
        let b = eligibility::record(&step["block"])?;
        let s = scoring::score_candidate_components(
            &mut verifier,
            state.history(),
            state.index(),
            &b,
            &profile,
            input["now"].as_u64().unwrap(),
        )
        .map_err(|e| e.to_string())?;
        state
            .import_unverified_ancestry(b.clone(), s.components().unwrap().next.clone())
            .map_err(|e| e.to_string())?;
        records.push(b);
        scores.push(s);
    }
    let mut replays = 0;
    for (i, row) in n["reapplications"].as_array().unwrap().iter().enumerate() {
        let mut pending =
            PendingFrontiers::new(row["capacity"].as_u64().unwrap() as usize).unwrap();
        pending.offer(eligibility::selected_frontier(&wrap, [gen; 3])?);
        pending_checks(&mut t, json!(i), &pending, &row["before"])?;
        for (j, step) in row["steps"].as_array().unwrap().iter().enumerate() {
            let idx = step["index"].as_u64().unwrap() as usize;
            let r = pending
                .reapply_native(&records[idx], &scores[idx], &graph)
                .map_err(|e| format!("native reapply {i}/{j}: {e}"))?;
            t.check(
                json!([i, j]),
                "native reapplication visit count",
                r.visited == step["visited"].as_u64().unwrap() as usize,
            );
            t.check(
                json!([i, j]),
                "native reapplication extension count",
                r.extended == step["extended"].as_u64().unwrap() as usize,
            );
            t.check(
                json!([i, j]),
                "native reapplication authority",
                !r.fully_validated && !r.history_verified,
            );
            pending_checks(&mut t, json!([i, j]), &pending, &step["after"])?;
            replays += 1;
        }
    }
    let mut result = t.finish();
    result["assembly_comparisons"] = assembly["checks"].clone();
    result["assembly_passed"] = assembly["passed"].clone();
    result["native_blocks"] = assembly["native_blocks"].clone();
    result["context_component_bundles"] = assembly["context_component_bundles"].clone();
    result["search_cases"] = json!(n["searches"].as_array().unwrap().len());
    result["accepted_searches"] = json!(accepted);
    result["reapplication_passes"] = json!(replays);
    result["native_recent_events"] = json!(n["events"].as_array().unwrap().len());
    result["full_coordinator_qualified"] = json!(false);
    Ok(result)
}
