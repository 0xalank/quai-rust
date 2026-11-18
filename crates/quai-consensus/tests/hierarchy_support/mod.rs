#[allow(dead_code)]
#[path = "../history_support/mod.rs"]
pub mod history;
use crypto_bigint::Encoding;
use quai_consensus::{
    hierarchy_header::{self, Profile},
    rules::Uint,
    verifier::Verifier,
    zone_header::ParentEntropyProjection,
};
use quai_primitives::decode_hex;
use serde_json::{json, Value};
fn decimal(v: &Value) -> Result<Uint, String> {
    let mut n = Uint::ZERO;
    for c in v.as_str().ok_or("decimal")?.bytes() {
        if !c.is_ascii_digit() {
            return Err("decimal digit".into());
        }
        n = n
            .wrapping_mul(&Uint::from_u64(10))
            .wrapping_add(&Uint::from_u64((c - b'0') as u64));
    }
    Ok(n)
}
fn dec(mut n: Uint) -> Value {
    let mut ds = vec![];
    while n != Uint::ZERO {
        ds.push((n.wrapping_rem(&Uint::from_u64(10)).to_be_bytes()[31] + b'0') as char);
        n = n.wrapping_div(&Uint::from_u64(10));
    }
    if ds.is_empty() {
        json!("0")
    } else {
        json!(ds.into_iter().rev().collect::<String>())
    }
}
fn hash(v: &Value) -> Result<[u8; 32], String> {
    decode_hex(v.as_str().ok_or("hash")?)?
        .try_into()
        .map_err(|_| "hash length".into())
}
fn location(v: &Value) -> Result<Vec<u8>, String> {
    v.as_array()
        .ok_or("location")?
        .iter()
        .map(|v| {
            v.as_u64()
                .and_then(|v| u8::try_from(v).ok())
                .ok_or("location value".into())
        })
        .collect()
}
pub fn profile(c: &Value) -> Result<Profile, String> {
    Ok(Profile {
        location: location(&c["location"])?,
        genesis: [hash(&c["genesis"])?].into_iter().collect(),
    })
}
pub fn verify_policy(v: &Value) -> Result<Value, String> {
    if v["reference_revision"] != quai_primitives::REFERENCE_REVISION {
        return Err("reference revision".into());
    }
    let (mut checks, mut mismatches) = (0, 0);
    let mut failures = vec![];
    let mut compare = |id: Value, key: &str, a: Value, e: &Value| {
        checks += 1;
        if &a != e {
            mismatches += 1;
            if failures.len() < 60 {
                failures.push(json!({"id":id,"field":key,"actual":a,"expected":e}))
            }
        }
    };
    for (i, c) in v["scores"].as_array().ok_or("scores")?.iter().enumerate() {
        let a = hierarchy_header::efficiency_score(
            c["previous"].as_u64().unwrap() as u16,
            [decimal(&c["delta"][0])?, decimal(&c["delta"][1])?],
            [decimal(&c["uncled"][0])?, decimal(&c["uncled"][1])?],
            c["parent_of_parent_genesis"].as_bool().unwrap(),
        )
        .map_err(|e| e.to_string())?;
        compare(json!(i), "efficiency", json!(a), &c["expected"]);
    }
    for (i, c) in v["miners"].as_array().ok_or("miners")?.iter().enumerate() {
        let a = hierarchy_header::miner_difficulty(
            c["number"].as_u64().unwrap(),
            decimal(&c["difficulty"])?,
            decimal(&c["previous"])?,
        )
        .map_err(|e| e.to_string())?;
        compare(json!(i), "miner EMA", dec(a), &c["expected"]);
    }
    for (i, c) in v["bitmaps"].as_array().ok_or("bitmaps")?.iter().enumerate() {
        let a = hierarchy_header::update_etx_eligible(
            hash(&c["initial"])?,
            &location(&c["location"])?,
            c["number"].as_u64().unwrap(),
        )
        .map_err(|e| e.to_string())?;
        compare(
            json!(i),
            "bitmap",
            json!(quai_primitives::encode_hex(&a)),
            &c["expected"],
        );
    }
    let (mut accepted, mut prime, mut region) = (0, 0, 0);
    let mut verifier = Verifier::default();
    let mut orders = [0usize; 3];
    for name in ["headers", "thresholds"] {
        for c in v[name].as_array().ok_or("header rows")? {
            let h = history::history(&c["records"])?;
            let b = history::record(&c["block"])?;
            let p = profile(c)?;
            let metrics = ParentEntropyProjection {
                order: c["parent_order"].as_u64().unwrap() as u8,
                total: decimal(&c["parent_total"])?,
                delta: decimal(&c["parent_delta"])?,
                uncled_delta: decimal(&c["parent_uncled_delta"])?,
            };
            orders[metrics.order as usize] += 1;
            if p.context() == 0 {
                prime += 1
            } else {
                region += 1
            };
            let actual = hierarchy_header::check_projection(
                &mut verifier,
                &h,
                &b,
                &p,
                c["now"].as_u64().unwrap(),
                c["uncle"].as_bool().unwrap(),
                &metrics,
            );
            compare(
                c["id"].clone(),
                &format!("{name}/policy {:?}", actual.as_ref().err()),
                json!(actual.is_ok()),
                &c["valid"],
            );
            if let Ok(r) = actual {
                accepted += 1;
                compare(
                    c["id"].clone(),
                    "projection grants no proof/authority",
                    json!(
                        r.fully_validated
                            || r.history_verified
                            || r.parent_work_checked
                            || r.candidate_work_checked
                            || r.candidate_order_checked
                    ),
                    &json!(false),
                );
            }
        }
    }
    Ok(
        json!({"passed":mismatches==0,"checks":checks,"mismatches":mismatches,"failures":failures,"score_cases":v["scores"].as_array().unwrap().len(),"miner_cases":v["miners"].as_array().unwrap().len(),"bitmap_cases":v["bitmaps"].as_array().unwrap().len(),"header_policy_cases":v["headers"].as_array().unwrap().len(),"threshold_policy_cases":v["thresholds"].as_array().unwrap().len(),"prime_cases":prime,"region_cases":region,"parent_orders":orders,"accepted_header_projections":accepted,"full_consensus":false,"canonical_history_verified":false,"scope":v["scope"]}),
    )
}
pub fn verify_native(v: &Value) -> Result<Value, String> {
    let (
        mut checks,
        mut mismatches,
        mut accepted,
        mut inspected,
        mut checked_parents,
        mut zero_parents,
    ) = (0, 0, 0, 0, 0, 0);
    let mut failures = vec![];
    let mut verifier = Verifier::default();
    for c in v["native"].as_array().ok_or("native")? {
        let hist = history::history(&c["records"])?;
        let b = history::record(&c["block"])?;
        let p = profile(c)?;
        let ctx = p.context();
        let parent_id = b.object().header().wire().parent_hash[ctx]
            .value
            .as_slice()
            .try_into()
            .unwrap();
        let parent = hist.get(&parent_id).map_err(|e| e.to_string())?;
        let actual = hierarchy_header::verify_header_components(
            &mut verifier,
            &hist,
            &b,
            &p,
            c["now"].as_u64().unwrap(),
            false,
        );
        checks += 1;
        if actual.is_ok() != c["valid"].as_bool().unwrap() {
            mismatches += 1;
            failures.push(json!({"id":c["id"],"actual":format!("{actual:?}"),"expected":c["valid"],"go_error":c["error"]}));
        }
        if let Ok(r) = actual {
            accepted += 1;
            checks += 1;
            let expected_parent = parent.number(ctx).map_err(|e| e.to_string())? != 0;
            if r.parent_work_checked {
                checked_parents += 1
            } else {
                zero_parents += 1
            };
            if r.fully_validated
                || r.history_verified
                || r.candidate_work_checked
                || r.candidate_order_checked
                || r.parent_work_checked != expected_parent
            {
                mismatches += 1;
                failures.push(json!({"id":c["id"],"bad_report":true}));
            }
        }
        let actual = hierarchy_header::inspect_candidate(
            &mut verifier,
            &hist,
            &b,
            &p,
            c["now"].as_u64().unwrap(),
        );
        checks += 1;
        if actual.is_ok() != c["inspect_valid"].as_bool().unwrap() {
            mismatches += 1;
            failures.push(json!({"id":c["id"],"inspect_actual":format!("{actual:?}"),"expected":c["inspect_valid"],"go_error":c["inspect_error"]}));
        }
        if let Ok(r) = actual {
            inspected += 1;
            checks += 1;
            if !r.candidate_work_checked
                || r.candidate_order_checked
                || r.fully_validated
                || r.history_verified
            {
                mismatches += 1
            }
        }
    }
    Ok(
        json!({"passed":mismatches==0,"checks":checks,"mismatches":mismatches,"failures":failures,"cases":v["native"].as_array().unwrap().len(),"accepted_headers":accepted,"accepted_component_bundles":inspected,"accepted_with_parent_work":checked_parents,"accepted_with_zero_context_parent":zero_parents,"full_consensus":false,"canonical_history_verified":false}),
    )
}
