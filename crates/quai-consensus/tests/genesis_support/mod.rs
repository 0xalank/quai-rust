#[allow(dead_code)]
#[path = "../placement_support/mod.rs"]
pub mod placement;
use crypto_bigint::U512;
use quai_consensus::{
    candidate_state::CandidateState,
    initialization::{self, GenesisSpec},
    placement::{Profile, Snapshot, Termini},
    rules::Uint,
    validation,
    verifier::Verifier,
};
use quai_primitives::{decode_hex, encode_hex, Hash};
use serde_json::{json, Value};
pub fn hash(v: &Value) -> Result<Hash, String> {
    decode_hex(v.as_str().ok_or("hash")?)?
        .try_into()
        .map_err(|_| "hash width".into())
}
pub fn wide(v: &Value) -> Result<U512, String> {
    let s = v.as_str().ok_or("decimal")?;
    if s.len() > 154 {
        return Err("decimal width".into());
    };
    let mut n = U512::ZERO;
    for c in s.bytes() {
        if !c.is_ascii_digit() {
            return Err("digit".into());
        };
        n = n
            .wrapping_mul(&U512::from_u64(10))
            .wrapping_add(&U512::from_u64((c - b'0') as u64))
    }
    Ok(n)
}
pub fn spec(v: &Value) -> Result<GenesisSpec, String> {
    let d = wide(&v["difficulty"])?;
    if d.bits_vartime() > 256 {
        return Err("difficulty width".into());
    };
    Ok(GenesisSpec {
        context: v["context"].as_u64().unwrap() as usize,
        nonce: v["nonce"].as_u64().unwrap(),
        timestamp: v["timestamp"].as_u64().unwrap(),
        extra: decode_hex(v["extra"].as_str().unwrap())?,
        allocation_hash: hash(&v["allocation_hash"])?,
        gas_limit: v["gas_limit"].as_u64().unwrap(),
        difficulty: d.resize(),
        starting_expansion: v["starting_expansion"].as_u64().unwrap(),
    })
}
pub fn snapshot(v: &Snapshot) -> Value {
    let mut r = json!({});
    if let Some(a) = &v.manifest {
        r["manifest"] = json!(a.iter().map(|h| encode_hex(h)).collect::<Vec<_>>())
    }
    if let Some(a) = &v.interlinks {
        r["interlinks"] = json!(a.iter().map(|h| encode_hex(h)).collect::<Vec<_>>())
    }
    if let Some(a) = &v.termini {
        r["termini"] = json!(encode_hex(&a.encode()))
    }
    r
}
#[derive(Default)]
struct Checks {
    checks: usize,
    failures: Vec<Value>,
    mismatches: usize,
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
        json!({"checks":self.checks,"mismatches":self.mismatches,"failures":self.failures,"passed":self.mismatches==0})
    }
}
pub fn policy(v: &Value) -> Result<Value, String> {
    let mut t = Checks::default();
    for (i, row) in v["specs"].as_array().unwrap().iter().enumerate() {
        let s = spec(row)?;
        let r = initialization::build_genesis(&s).map_err(|e| format!("genesis {i}: {e}"))?;
        for (field, yes) in [
            (
                "bytes",
                encode_hex(r.bytes()) == row["block"].as_str().unwrap(),
            ),
            ("hash", encode_hex(&r.id()) == row["hash"].as_str().unwrap()),
            (
                "body hash",
                encode_hex(&r.object().header().hash()) == row["body_hash"].as_str().unwrap(),
            ),
            ("commit payload", row["commit_hash"] == row["hash"]),
            (
                "commit empty termini",
                row["commit_termini"] == encode_hex(&Termini::genesis([0; 32]).encode()),
            ),
        ] {
            t.check(json!(i), field, yes)
        }
        let raw = placement::history::record(&row["raw_block"])?;
        t.check(
            json!(i),
            "raw payload canonicalization",
            raw.bytes() == r.bytes(),
        );
        let p = Profile {
            location: vec![0; s.context],
            genesis: [r.id()].into_iter().collect(),
            default_genesis: Some(r.id()),
            minimum_difficulty: Uint::ONE,
            duration: 5,
            gas_ceiling: 50_000_000,
            p2p_threshold: 7,
        };
        let mut state = CandidateState::new(p).map_err(|e| e.to_string())?;
        state.seed_trusted(r.clone()).map_err(|e| e.to_string())?;
        t.check(
            json!(i),
            "seed",
            snapshot(&state.index().get(&r.id()).unwrap()) == row["seed"],
        );
    }
    for row in v["geometry"].as_array().unwrap() {
        let exp = row["expansion"].as_u64().unwrap() as u8;
        let (r, z) = initialization::hierarchy_size(exp);
        t.check(json!(exp), "regions", json!(r) == row["regions"]);
        t.check(json!(exp), "zones", json!(z) == row["zones"]);
        let active = initialization::active_slices(exp);
        let new = initialization::new_chains(exp);
        t.check(
            json!(exp),
            "active domain",
            active.is_ok() == row["active_supported"].as_bool().unwrap(),
        );
        t.check(
            json!(exp),
            "new domain",
            new.is_ok() == row["new_supported"].as_bool().unwrap(),
        );
        if let Ok(a) = active {
            t.check(json!(exp), "active values", json!(a) == row["active"])
        }
        if let Ok(a) = new {
            t.check(json!(exp), "new values", json!(a) == row["new_chains"])
        }
    }
    let mut wide_cases = 0;
    let mut invalid = 0;
    for (i, row) in v["difficulties"].as_array().unwrap().iter().enumerate() {
        let b = placement::history::record(&row["block"])?;
        let actual = initialization::genesis_difficulty(
            &b,
            row["expansion"].as_u64().unwrap() as u8,
            Some(hash(&row["default_genesis"])?),
        );
        let valid = row["difficulty"].is_string();
        t.check(
            json!(i),
            "genesis difficulty validity",
            actual.is_ok() == valid,
        );
        if !valid {
            invalid += 1
        }
        if let Ok(a) = actual {
            t.check(
                json!(i),
                "genesis difficulty value",
                a == wide(&row["difficulty"])?,
            );
            if a.bits_vartime() > 256 {
                wide_cases += 1
            }
        }
        let mut p = placement::history::profile();
        p.genesis.insert(b.id());
        p.default_genesis = Some(hash(&row["default_genesis"])?);
        let bounded = validation::expected_difficulty(&Default::default(), &b, &p);
        let fits = valid && wide(&row["difficulty"])?.bits_vartime() <= 256;
        t.check(
            json!(i),
            "bounded header difficulty",
            bounded.is_ok() == fits,
        );
        if let Ok(n) = bounded {
            t.check(
                json!(i),
                "bounded difficulty value",
                n.resize() == wide(&row["difficulty"])?,
            )
        }
    }
    let mut r = t.finish();
    r["spec_cases"] = json!(v["specs"].as_array().unwrap().len());
    r["geometry_cases"] = json!(v["geometry"].as_array().unwrap().len());
    r["difficulty_cases"] = json!(v["difficulties"].as_array().unwrap().len());
    r["wide_difficulty_cases"] = json!(wide_cases);
    r["invalid_entropy_cases"] = json!(invalid);
    Ok(r)
}
pub fn native(v: &Value) -> Result<Value, String> {
    let mut t = Checks::default();
    let mut accepted = 0;
    let mut cases = 0;
    let mut verifier = Verifier::default();
    for row in v["branches"].as_array().unwrap() {
        let mut state = CandidateState::new(placement::profile(row)?).map_err(|e| e.to_string())?;
        let root = placement::history::record(&row["root"])?;
        state
            .seed_trusted(root.clone())
            .map_err(|e| e.to_string())?;
        t.check(
            row["genesis"].clone(),
            "root seed",
            snapshot(&state.index().get(&root.id()).unwrap()) == row["seed"],
        );
        for step in row["steps"].as_array().unwrap() {
            cases += 1;
            let b = placement::history::record(&step["block"])?;
            let before = (
                state.history().len(),
                state.history().encoded_bytes(),
                state.index().len(),
                state.index().tracked_payload_bytes(),
            );
            let actual = state.prepare_and_insert(
                &mut verifier,
                b.clone(),
                row["now"].as_u64().unwrap(),
                false,
                hash(&row["genesis"])?,
            );
            let valid = step["valid"].as_bool().unwrap();
            t.check(
                step["id"].clone(),
                "append validity",
                actual.is_ok() == valid,
            );
            if let Ok(p) = actual {
                accepted += 1;
                t.check(
                    step["id"].clone(),
                    "next snapshot",
                    snapshot(&p.next) == step["next"],
                );
                t.check(
                    step["id"].clone(),
                    "returned reference",
                    encode_hex(&p.previous_dominant) == step["returned_dominant"].as_str().unwrap(),
                );
                t.check(
                    step["id"].clone(),
                    "candidate authority",
                    !p.fully_validated
                        && !p.history_verified
                        && p.placement.candidate_work_checked
                        && p.placement.candidate_order_checked
                        && p.header_policy_checked
                        && p.body_checked
                        && p.ancestry_checked,
                );
                t.check(
                    step["id"].clone(),
                    "both stores grew",
                    state.history().len() == before.0 + 1 && state.index().len() == before.2 + 1,
                );
            } else {
                t.check(
                    step["id"].clone(),
                    "failure is atomic",
                    before
                        == (
                            state.history().len(),
                            state.history().encoded_bytes(),
                            state.index().len(),
                            state.index().tracked_payload_bytes(),
                        ),
                );
            }
        }
        for s in row["snapshots"].as_array().unwrap() {
            let id = hash(&s["hash"])?;
            t.check(
                s["hash"].clone(),
                "branch snapshot retained",
                snapshot(state.index().get(&id).map_err(|e| e.to_string())?.as_ref())
                    == s["snapshot"],
            );
        }
    }
    let mut r = t.finish();
    r["cases"] = json!(cases);
    r["accepted"] = json!(accepted);
    r["branch_profiles"] = json!(v["branches"].as_array().unwrap().len());
    r["canonical_history_verified"] = json!(false);
    r["fork_choice_performed"] = json!(false);
    Ok(r)
}
