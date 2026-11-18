#[allow(dead_code)]
#[path = "../genesis_support/mod.rs"]
pub mod genesis;

use quai_consensus::{
    candidate_state::CandidateState,
    frontier::{Frontier, PendingFrontiers, Tip, MAX_PENDING},
    rules::{Entropy, Uint},
    scoring::{self, CandidateScore},
    verifier::Verifier,
};
use quai_primitives::encode_hex;
use serde_json::{json, Value};
fn uint(v: &Value) -> Result<Uint, String> {
    let n = genesis::wide(v)?;
    if n.bits_vartime() > 256 {
        return Err("uint width".into());
    };
    Ok(n.resize())
}
pub fn frontier(exp: u8, v: &Value) -> Result<Frontier, String> {
    let mut entries = vec![];
    for row in v.as_array().ok_or("tips")? {
        let loc = row["location"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect();
        let ns = row["numbers"].as_array().unwrap();
        entries.push((
            loc,
            Tip {
                id: genesis::hash(&row["hash"])?,
                numbers: [
                    ns[0].as_u64().unwrap(),
                    ns[1].as_u64().unwrap(),
                    ns[2].as_u64().unwrap(),
                ],
                entropy: uint(&row["entropy"])?,
            },
        ));
    }
    Frontier::from_legacy_imported(exp, entries).map_err(|e| e.to_string())
}
fn tips_match(f: &Frontier, v: &Value) -> Result<bool, String> {
    Ok(*f == frontier(f.expansion(), v)?)
}
fn metrics_match(e: &Entropy, v: &Value) -> Result<bool, String> {
    Ok(v.as_object().ok_or("metrics")?.len() == 7
        && json!(e.order) == v["order"]
        && json!(e.rank) == v["rank"]
        && e.intrinsic.resize() == genesis::wide(&v["intrinsic"])?
        && e.total.resize() == genesis::wide(&v["total"])?
        && e.delta.resize() == genesis::wide(&v["delta"])?
        && e.uncled_delta.resize() == genesis::wide(&v["uncled_delta"])?
        && e.workshares.resize() == genesis::wide(&v["workshares"])?)
}
#[derive(Default)]
struct Checks {
    checks: usize,
    mismatches: usize,
    failures: Vec<Value>,
}
impl Checks {
    fn check(&mut self, id: Value, field: &str, yes: bool) {
        self.checks += 1;
        if !yes {
            self.mismatches += 1;
            if self.failures.len() < 30 {
                self.failures.push(json!({"id":id,"field":field}))
            }
        }
    }
    fn finish(self) -> Value {
        json!({"checks":self.checks,"passed":self.mismatches==0,"mismatches":self.mismatches,"failures":self.failures})
    }
}
pub fn policy(v: &Value) -> Result<Value, String> {
    let mut t = Checks::default();
    for (i, row) in v["scores"].as_array().unwrap().iter().enumerate() {
        let r = genesis::placement::history::record(&row["block"])?;
        let e = scoring::project_entropy(
            &r,
            genesis::hash(&row["pow_hash"])?,
            row["context"].as_u64().unwrap() as usize,
        )
        .map_err(|e| e.to_string())?;
        t.check(
            json!(i),
            "PoEM metrics",
            metrics_match(&e, &row["metrics"])?,
        );
    }
    for (i, row) in v["coordinator"]["updates"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let f = frontier(row["expansion"].as_u64().unwrap() as u8, &row["before"])?;
        let r = genesis::placement::history::record(&row["block"])?;
        let o = row["order"].as_u64().unwrap() as u8;
        let ok = f.extendable(&r, o).map_err(|e| e.to_string())?;
        t.check(
            json!(i),
            "NodeSet extendable",
            json!(ok) == row["extendable"],
        );
        t.check(
            json!(i),
            "NodeSet total",
            f.total() == genesis::wide(&row["before_total"])?,
        );
        if ok {
            let next = f
                .updated_projection(&r, o, uint(&row["score"])?)
                .map_err(|e| e.to_string())?;
            t.check(
                json!(i),
                "NodeSet update",
                tips_match(&next, &row["after"])?,
            );
            t.check(
                json!(i),
                "updated total",
                next.total() == genesis::wide(&row["after_total"])?,
            );
        } else {
            t.check(
                json!(i),
                "mismatch rejection",
                f.updated_projection(&r, o, Uint::ZERO).is_err(),
            );
        }
        t.check(
            json!(i),
            "original frontier retained",
            tips_match(&f, &row["original_after"])?,
        );
    }
    let mut pending = PendingFrontiers::new(MAX_PENDING).unwrap();
    for (i, row) in v["coordinator"]["offers"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let f = frontier(0, &row["tips"])?;
        let offer = pending.offer(f);
        let key = genesis::wide(&row["key"])?;
        t.check(
            json!(i),
            "entropy-key insertion",
            json!(offer.inserted) == row["inserted"],
        );
        t.check(
            json!(i),
            "pending count",
            json!(pending.len()) == row["len"],
        );
        t.check(
            json!(i),
            "best ever",
            offer.best_ever == genesis::wide(&row["best_ever"])?,
        );
        let saved = pending.peek(&key).ok_or("offered key missing")?;
        t.check(
            json!(i),
            "first equal score retained",
            encode_hex(&saved.tips()[&vec![]].id) == row["stored_prime"].as_str().unwrap(),
        );
    }
    let expected = v["coordinator"]["retained_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(genesis::wide)
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    let actual = pending
        .descending()
        .map(|(k, _)| *k)
        .collect::<std::collections::BTreeSet<_>>();
    t.check(json!(0), "retained pending keys", actual == expected);
    t.check(
        json!(0),
        "evicted historical best",
        pending.peek(&pending.best_ever()).is_some()
            == v["coordinator"]["best_retained"].as_bool().unwrap(),
    );
    let mut r = t.finish();
    r["score_cases"] = json!(v["scores"].as_array().unwrap().len());
    r["nodeset_cases"] = json!(v["coordinator"]["updates"].as_array().unwrap().len());
    r["pool_offers"] = json!(v["coordinator"]["offers"].as_array().unwrap().len());
    Ok(r)
}
fn compare(a: &CandidateScore, b: &CandidateScore) -> Result<i8, String> {
    Ok(match a.compare(b).map_err(|e| e.to_string())? {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}
pub fn native(v: &Value) -> Result<Value, String> {
    let mut t = Checks::default();
    let (mut accepted, mut attempts, mut startups, mut updates) = (0, 0, 0, 0);
    let mut verifier = Verifier::default();
    for (row, fc) in v["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .zip(v["native_frontiers"].as_array().unwrap())
    {
        let mut p = genesis::placement::profile(row)?;
        if row["default_genesis"].is_string() {
            p.default_genesis = Some(genesis::hash(&row["default_genesis"])?)
        }
        let root = genesis::placement::history::record(&row["root"])?;
        if row["spec"].is_object() {
            startups += 1;
            let r = quai_consensus::initialization::build_genesis(&genesis::spec(&row["spec"])?)
                .map_err(|e| e.to_string())?;
            t.check(
                row["genesis"].clone(),
                "constructed startup root",
                r.bytes() == root.bytes(),
            );
        }
        let mut state = CandidateState::new(p.clone()).map_err(|e| e.to_string())?;
        state
            .seed_trusted(root.clone())
            .map_err(|e| e.to_string())?;
        let anchor = scoring::score_candidate_components(
            &mut verifier,
            state.history(),
            state.index(),
            &root,
            &p,
            row["now"].as_u64().unwrap(),
        )
        .map_err(|e| e.to_string())?;
        t.check(
            row["genesis"].clone(),
            "trusted anchor score",
            anchor.total() == Uint::ZERO
                && !anchor.work_checked()
                && !anchor.fully_validated()
                && !anchor.history_verified(),
        );
        let mut scores = [anchor.clone(), anchor];
        let seed = frontier(fc["expansion"].as_u64().unwrap() as u8, &fc["initial"])?;
        let mut fronts = [seed.clone(), seed];
        let mut update_idx = 0;
        for step in row["steps"].as_array().unwrap() {
            attempts += 1;
            let b = genesis::placement::history::record(&step["block"])?;
            let before = (
                state.history().len(),
                state.history().encoded_bytes(),
                state.index().len(),
                state.index().tracked_payload_bytes(),
            );
            let actual = if step["wrong_default"] == true {
                let mut bad = p.clone();
                bad.default_genesis = Some([0xde; 32]);
                scoring::score_candidate_components(
                    &mut verifier,
                    state.history(),
                    state.index(),
                    &b,
                    &bad,
                    row["now"].as_u64().unwrap(),
                )
            } else {
                state.score_and_insert(&mut verifier, b.clone(), row["now"].as_u64().unwrap())
            };
            t.check(
                step["id"].clone(),
                "native score admission",
                actual.is_ok() == step["valid"].as_bool().unwrap(),
            );
            match actual {
                Ok(s) => {
                    accepted += 1;
                    let c = s.components().unwrap();
                    t.check(
                        step["id"].clone(),
                        "native metrics",
                        metrics_match(s.metrics().unwrap(), &step["metrics"])?,
                    );
                    t.check(
                        step["id"].clone(),
                        "next ancestry",
                        genesis::snapshot(&c.next) == step["next"],
                    );
                    t.check(
                        step["id"].clone(),
                        "no canonical authority",
                        s.work_checked() && !s.fully_validated() && !s.history_verified(),
                    );
                    t.check(
                        step["id"].clone(),
                        "both stores grew",
                        state.history().len() == before.0 + 1
                            && state.index().len() == before.2 + 1,
                    );
                    let branch = step["branch"].as_u64().unwrap() as usize;
                    scores[branch] = s.clone();
                    t.check(
                        step["id"].clone(),
                        "branch score comparison",
                        json!(compare(&scores[0], &scores[1])?) == step["comparison"],
                    );
                    let next = fronts[branch]
                        .updated_with_score(&b, &s)
                        .map_err(|e| format!("frontier {}: {e}", step["id"]))?;
                    let expected = &fc["steps"][update_idx];
                    t.check(
                        step["id"].clone(),
                        "native frontier identity",
                        step["id"] == expected["id"],
                    );
                    t.check(
                        step["id"].clone(),
                        "native frontier tips",
                        tips_match(&next, &expected["tips"])?,
                    );
                    t.check(
                        step["id"].clone(),
                        "native frontier entropy",
                        next.total() == genesis::wide(&expected["total"])?,
                    );
                    fronts[branch] = next;
                    update_idx += 1;
                    updates += 1;
                }
                Err(_) => t.check(
                    step["id"].clone(),
                    "score failure atomic",
                    before
                        == (
                            state.history().len(),
                            state.history().encoded_bytes(),
                            state.index().len(),
                            state.index().tracked_payload_bytes(),
                        ),
                ),
            }
        }
        for s in row["snapshots"].as_array().unwrap() {
            let id = genesis::hash(&s["hash"])?;
            t.check(
                s["hash"].clone(),
                "branch ancestry retained",
                genesis::snapshot(state.index().get(&id).map_err(|e| e.to_string())?.as_ref())
                    == s["snapshot"],
            );
        }
    }
    let mut r = t.finish();
    r["attempts"] = json!(attempts);
    r["accepted"] = json!(accepted);
    r["profiles"] = json!(v["profiles"].as_array().unwrap().len());
    r["constructed_startup_profiles"] = json!(startups);
    r["frontier_updates"] = json!(updates);
    r["canonical_history_verified"] = json!(false);
    r["full_fork_choice"] = json!(false);
    Ok(r)
}
