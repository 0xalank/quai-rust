#[allow(dead_code)]
#[path = "../frontier_support/mod.rs"]
pub mod frontier;
use quai_consensus::{
    candidate_state::CandidateState,
    eligibility::{Constraints, CoordinatorView},
    frontier::{Frontier, Tip},
    history::Record,
    placement::{self, Profile, Termini},
    rules::Uint,
    scoring,
    verifier::Verifier,
    Error,
};
use quai_primitives::{decode_hex, Hash};
use serde_json::{json, Value};
use std::collections::BTreeMap;
pub fn hash(v: &Value) -> Result<Hash, String> {
    frontier::genesis::hash(v)
}
pub fn location(v: &Value) -> Vec<u8> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_u64().unwrap() as u8)
        .collect()
}
pub fn constraints(v: &Value) -> Result<Constraints, String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|r| Ok((location(&r["location"]), hash(&r["hash"])?)))
        .collect()
}
pub fn profile(l: Vec<u8>, gen: Hash) -> Profile {
    Profile {
        location: l,
        genesis: [gen].into_iter().collect(),
        default_genesis: Some(gen),
        minimum_difficulty: Uint::ONE,
        duration: 5,
        gas_ceiling: 50_000_000,
        p2p_threshold: 7,
    }
}
pub fn record(v: &Value) -> Result<Record, String> {
    frontier::genesis::placement::history::record(v)
}
pub fn term(v: &Value) -> Result<Termini, String> {
    Termini::decode(&decode_hex(v.as_str().unwrap())?).map_err(|e| e.to_string())
}
struct Dataset {
    legacy: bool,
    gen: Hash,
    views: Vec<Vec<u8>>,
    entries: Vec<(Vec<u8>, Record, Termini)>,
    orders: BTreeMap<Hash, u8>,
}
impl Dataset {
    fn decode(v: &Value) -> Result<Self, String> {
        let mut entries = vec![];
        let mut orders = BTreeMap::new();
        for e in v["entries"].as_array().unwrap() {
            let r = record(&e["block"])?;
            orders.insert(r.id(), e["order"].as_u64().unwrap() as u8);
            entries.push((location(&e["location"]), r, term(&e["termini"])?));
        }
        Ok(Self {
            legacy: v["legacy"].as_bool().unwrap_or(false),
            gen: hash(&v["genesis"])?,
            views: v["views"]
                .as_array()
                .unwrap()
                .iter()
                .map(location)
                .collect(),
            entries,
            orders,
        })
    }
    fn build(
        &self,
        missing_header: Option<(&[u8], Hash)>,
        missing_term: Option<(&[u8], Hash)>,
    ) -> Result<CoordinatorView, String> {
        let mut view = CoordinatorView::new(self.legacy);
        for l in &self.views {
            view.register(profile(l.clone(), self.gen))
                .map_err(|e| e.to_string())?
        }
        for (l, r, t) in &self.entries {
            if missing_header.is_some_and(|(q, h)| q == l.as_slice() && h == r.id()) {
                continue;
            }
            let terms = if missing_term.is_some_and(|(q, h)| q == l.as_slice() && h == r.id()) {
                None
            } else {
                Some(t.clone())
            };
            view.import_unverified(l, r.clone(), terms)
                .map_err(|e| e.to_string())?;
        }
        Ok(view)
    }
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
pub fn make_policy_view(v: &Value) -> Result<CoordinatorView, String> {
    Dataset::decode(v)?.build(None, None)
}
pub fn policy(v: &Value) -> Result<Value, String> {
    let mut t = Checks::default();
    let p = &v["policy"];
    let datasets = p["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(Dataset::decode)
        .collect::<Result<Vec<_>, _>>()?;
    let views = datasets
        .iter()
        .map(|d| d.build(None, None))
        .collect::<Result<Vec<_>, _>>()?;
    for (i, r) in p["ancestors"].as_array().unwrap().iter().enumerate() {
        let d = r["dataset"].as_u64().unwrap() as usize;
        let loc = location(&p["datasets"][d]["location"]);
        let a = views[d].is_ancestor(
            hash(&r["ancestor"])?,
            hash(&r["header"])?,
            &loc,
            r["order"].as_u64().unwrap() as u8,
        );
        t.check(
            json!(i),
            "ancestor value",
            a.as_ref().copied().unwrap_or(false) == r["value"].as_bool().unwrap(),
        );
        t.check(
            json!(i),
            "ancestor availability",
            a.is_ok() == r["available"].as_bool().unwrap(),
        );
    }
    for (i, r) in p["pcrc"].as_array().unwrap().iter().enumerate() {
        let d = r["dataset"].as_u64().unwrap() as usize;
        let loc = location(&r["location"]);
        let h = hash(&r["header"])?;
        let mut canonical = loc.clone();
        if datasets[d].legacy && canonical[0] >= 3 {
            canonical[0] = 3
        };
        let missing = r["missing"].as_bool().unwrap();
        let temporary;
        if missing {
            temporary = datasets[d].build(None, Some((&canonical, h)))?;
        } else {
            temporary = datasets[d].build(None, None)?;
        }
        let a = temporary.previous_coincident_matches(&loc, h, hash(&r["dominant"])?);
        t.check(
            json!(i),
            "PCRC value",
            a.as_ref().copied().unwrap_or(false) == r["value"].as_bool().unwrap(),
        );
        t.check(json!(i), "PCRC availability", a.is_ok() != missing);
    }
    for (i, r) in p["traces"].as_array().unwrap().iter().enumerate() {
        let d = r["dataset"].as_u64().unwrap() as usize;
        let input = constraints(&r["constraints"])?;
        let saved = input.clone();
        let a = views[d].trace_projection(
            &input,
            hash(&r["leader"])?,
            &location(&r["leader_location"]),
            r["first"].as_bool().unwrap(),
            &datasets[d].orders,
        );
        t.check(
            json!(i),
            "constraint outcome",
            a.is_ok() == r["valid"].as_bool().unwrap(),
        );
        if let Ok(m) = a {
            t.check(
                json!(i),
                "constraint result",
                m == constraints(&r["result"])?,
            );
        } else {
            t.check(
                json!(i),
                "source error returns starting constraints",
                constraints(&r["result"])? == saved,
            );
        }
        t.check(json!(i), "caller constraints unchanged", input == saved);
    }
    let mut boundaries = 0;
    for (i, r) in p["boundaries"].as_array().unwrap().iter().enumerate() {
        let d = r["dataset"].as_u64().unwrap() as usize;
        let l = location(&r["missing_location"]);
        let view = datasets[d].build(Some((&l, hash(&r["missing_header"])?)), None)?;
        let a = view.trace_projection(
            &constraints(&r["constraints"])?,
            hash(&r["leader"])?,
            &location(&r["leader_location"]),
            false,
            &datasets[d].orders,
        );
        t.check(
            json!(i),
            "source nil-error boundary observed",
            r["go_success"] == true,
        );
        t.check(
            json!(i),
            "missing source header rejected",
            matches!(a, Err(Error::Unavailable(_))),
        );
        boundaries += 1;
    }
    let mut r = t.finish();
    r["datasets"] = json!(datasets.len());
    r["ancestor_cases"] = json!(p["ancestors"].as_array().unwrap().len());
    r["pcrc_cases"] = json!(p["pcrc"].as_array().unwrap().len());
    r["trace_cases"] = json!(p["traces"].as_array().unwrap().len());
    r["safe_rejection_boundaries"] = json!(boundaries);
    Ok(r)
}
pub fn native_view(v: &Value) -> Result<CoordinatorView, String> {
    let n = &v["native"];
    let mut view = CoordinatorView::new(false);
    let gen = hash(&n["genesis"])?;
    for l in [vec![], vec![0], vec![0, 0]] {
        view.register(profile(l, gen)).map_err(|e| e.to_string())?
    }
    for r in n["entries"].as_array().unwrap() {
        view.import_unverified(
            &location(&r["location"]),
            record(&r["block"])?,
            Some(term(&r["termini"])?),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(view)
}
pub fn selected_frontier(v: &Value, ids: [Hash; 3]) -> Result<Frontier, String> {
    let n = &v["native"];
    let mut records = BTreeMap::new();
    let root = record(&n["root"])?;
    records.insert(root.id(), root);
    for s in n["steps"].as_array().unwrap() {
        let r = record(&s["block"])?;
        records.insert(r.id(), r);
    }
    Frontier::from_imported(
        0,
        [vec![], vec![0], vec![0, 0]]
            .into_iter()
            .enumerate()
            .map(|(i, l)| {
                let r = &records[&ids[i]];
                (
                    l,
                    Tip {
                        id: r.id(),
                        numbers: [
                            r.number(0).unwrap(),
                            r.number(1).unwrap(),
                            r.number(2).unwrap(),
                        ],
                        entropy: Uint::ZERO,
                    },
                )
            })
            .collect(),
    )
    .map_err(|e| e.to_string())
}
pub fn native(v: &Value) -> Result<Value, String> {
    let mut t = Checks::default();
    let n = &v["native"];
    let gen = hash(&n["genesis"])?;
    let root = record(&n["root"])?;
    let profiles = [
        profile(vec![], gen),
        profile(vec![0], gen),
        profile(vec![0, 0], gen),
    ];
    let mut states = profiles
        .iter()
        .cloned()
        .map(CandidateState::new)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut graph = CoordinatorView::new(false);
    for p in &profiles {
        graph.register(p.clone()).map_err(|e| e.to_string())?
    }
    for (i, s) in n["seeds"].as_array().unwrap().iter().enumerate() {
        let snap = frontier::genesis::placement::snapshot(&s["snapshot"])?;
        states[i]
            .import_unverified_ancestry(root.clone(), snap.clone())
            .map_err(|e| e.to_string())?;
        graph
            .import_unverified(&profiles[i].location, root.clone(), snap.termini.clone())
            .map_err(|e| e.to_string())?;
    }
    let initial = selected_frontier(v, [gen; 3])?;
    let mut fronts = [initial.clone(), initial];
    let mut verifier = Verifier::default();
    let mut components = 0;
    let mut accepted = 0;
    for s in n["steps"].as_array().unwrap() {
        let b = record(&s["block"])?;
        let score = scoring::score_candidate_components(
            &mut verifier,
            states[0].history(),
            states[0].index(),
            &b,
            &profiles[0],
            n["now"].as_u64().unwrap(),
        )
        .map_err(|e| format!("native prime {}: {e}", s["id"]))?;
        for c in 0..3 {
            let local = record(&s["contexts"][c]["block"])?;
            let prepared = if c == 0 {
                score.components().unwrap().clone()
            } else {
                placement::prepare_append_components(
                    &mut verifier,
                    states[c].history(),
                    states[c].index(),
                    &local,
                    &profiles[c],
                    n["now"].as_u64().unwrap(),
                    true,
                    frontier::genesis::placement::parent_id(&b, c),
                )
                .map_err(|e| format!("native context {c} {}: {e}", s["id"]))?
            };
            t.check(
                s["id"].clone(),
                "multi-context next ancestry",
                frontier::genesis::snapshot(&prepared.next) == s["contexts"][c]["snapshot"],
            );
            t.check(
                s["id"].clone(),
                "multi-context returned reference",
                quai_primitives::encode_hex(&prepared.previous_dominant)
                    == s["contexts"][c]["returned_dominant"].as_str().unwrap(),
            );
            t.check(
                s["id"].clone(),
                "component authority remains false",
                !prepared.fully_validated && !prepared.history_verified,
            );
            states[c]
                .import_unverified_ancestry(local.clone(), prepared.next.clone())
                .map_err(|e| e.to_string())?;
            graph
                .import_unverified(&profiles[c].location, local, prepared.next.termini)
                .map_err(|e| e.to_string())?;
            components += 1;
        }
        let branch = s["branch"].as_u64().unwrap() as usize;
        let (next, report) = fronts[branch]
            .updated_with_eligibility(&b, &score, &graph)
            .map_err(|e| format!("native global {}: {e}", s["id"]))?;
        t.check(
            s["id"].clone(),
            "native global edges",
            report.checked_edges == 2 && !report.fully_validated && !report.history_verified,
        );
        fronts[branch] = next;
        accepted += 1;
    }
    let mut matrix_ok = 0;
    for (i, r) in n["matrix"].as_array().unwrap().iter().enumerate() {
        let f = selected_frontier(
            v,
            [hash(&r["prime"])?, hash(&r["region"])?, hash(&r["zone"])?],
        )?;
        let a = graph.check_frontier(&f);
        t.check(
            json!(i),
            "native PCRC frontier selection",
            a.is_ok() == r["valid"].as_bool().unwrap(),
        );
        if let Ok(e) = a {
            matrix_ok += 1;
            t.check(
                json!(i),
                "eligibility remains candidate-only",
                e.checked_edges == 2 && !e.fully_validated && !e.history_verified,
            );
        }
    }
    let mut trace_ok = 0;
    for (i, r) in n["traces"].as_array().unwrap().iter().enumerate() {
        let input = constraints(&r["constraints"])?;
        let a = graph.trace_native(
            &mut verifier,
            &input,
            hash(&r["leader"])?,
            &[0, 0],
            r["first"].as_bool().unwrap(),
        );
        t.check(
            json!(i),
            "native constraint outcome",
            a.is_ok() == r["valid"].as_bool().unwrap(),
        );
        if let Ok(m) = a {
            trace_ok += 1;
            t.check(
                json!(i),
                "native constraint map",
                m == constraints(&r["result"])?,
            );
        }
    }
    let mut r = t.finish();
    r["native_blocks"] = json!(accepted);
    r["context_component_bundles"] = json!(components);
    r["frontier_combinations"] = json!(n["matrix"].as_array().unwrap().len());
    r["eligible_frontiers"] = json!(matrix_ok);
    r["native_trace_cases"] = json!(n["traces"].as_array().unwrap().len());
    r["accepted_traces"] = json!(trace_ok);
    r["canonical_history_verified"] = json!(false);
    r["full_coordinator_qualified"] = json!(false);
    Ok(r)
}
