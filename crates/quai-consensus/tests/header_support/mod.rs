#[allow(dead_code)]
#[path = "../history_support/mod.rs"]
pub mod history;
use crypto_bigint::Encoding;
use quai_consensus::{
    economics,
    rules::Uint,
    verifier::{Verifier, PINNED_SCHEDULE},
    zone_header::{self, ParentEntropyProjection},
};
use quai_primitives::{decode_hex, Header};
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
pub fn profile(c: &Value) -> Result<quai_consensus::validation::Profile, String> {
    let mut p = history::profile();
    p.genesis.insert(
        decode_hex(c["genesis"].as_str().ok_or("genesis")?)?
            .try_into()
            .map_err(|_| "genesis hash")?,
    );
    Ok(p)
}
pub fn verify_policy(v: &Value) -> Result<Value, String> {
    if v["reference_revision"] != quai_primitives::REFERENCE_REVISION {
        return Err("reference revision".into());
    }
    let mut checks = 0;
    let mut mismatches = 0;
    let mut failures = vec![];
    let mut compare = |id: Value, field: &str, actual: Value, expected: &Value| {
        checks += 1;
        if &actual != expected {
            mismatches += 1;
            if failures.len() < 60 {
                failures.push(json!({"id":id,"field":field,"actual":actual,"expected":expected}));
            }
        }
    };
    for (i, c) in v["quotes"].as_array().ok_or("quotes")?.iter().enumerate() {
        let h = Header::decode_auxiliary(
            &decode_hex(c["header"].as_str().ok_or("header")?)?,
            PINNED_SCHEDULE,
        )?;
        let quote =
            economics::reward_quote(&h, decimal(&c["exchange"])?, decimal(&c["average_fees"])?)
                .map_err(|e| format!("quote/{i}: {e}"))?;
        for (name, n) in [
            ("equivalent", quote.equivalent),
            ("quai", quote.quai),
            ("qi", quote.qi),
            ("conversion_quai", quote.conversion_quai),
            ("base_fee", quote.base_fee),
            (
                "one_over_kqi",
                economics::one_over_kqi(
                    quai_consensus::share_rules::number(h.wire().number.as_deref().unwrap())
                        .map_err(|e| e.to_string())?,
                ),
            ),
        ] {
            compare(json!(i), name, dec(n), &c[name]);
        }
    }
    for (i, c) in v["expansions"]
        .as_array()
        .ok_or("expansions")?
        .iter()
        .enumerate()
    {
        let hist = history::history(&c["records"])?;
        let parent = history::record(&c["parent"])?;
        let mut p = profile(c)?;
        p.location = c["location"]
            .as_array()
            .ok_or("location")?
            .iter()
            .map(|x| x.as_u64().unwrap() as u8)
            .collect();
        let actual =
            zone_header::expected_expansion(&hist, &parent, &p, c["order"].as_u64().unwrap() as u8)
                .map_err(|e| e.to_string())?;
        compare(json!(i), "expansion", json!(actual), &c["expected"]);
    }
    let mut verifier = Verifier::default();
    let mut accepted = 0;
    let mut orders = [0usize; 3];
    for c in v["headers"].as_array().ok_or("headers")? {
        let b = history::record(&c["block"])?;
        let hist = history::history(&c["records"])?;
        let p = profile(c)?;
        let m = ParentEntropyProjection {
            order: c["parent_order"].as_u64().ok_or("order")? as u8,
            total: decimal(&c["parent_total"])?,
            delta: decimal(&c["parent_delta"])?,
            uncled_delta: decimal(&c["parent_uncled_delta"])?,
        };
        orders[m.order as usize] += 1;
        let actual = zone_header::check_zone_header_projection(
            &mut verifier,
            &hist,
            &b,
            &p,
            c["now"].as_u64().unwrap(),
            c["uncle"].as_bool().unwrap(),
            &m,
        );
        compare(
            c["id"].clone(),
            &format!("zone policy {:?}", actual.as_ref().err()),
            json!(actual.is_ok()),
            &c["valid"],
        );
        if let Ok(r) = actual {
            accepted += 1;
            compare(
                c["id"].clone(),
                "no authority or candidate proof claim",
                json!(r.fully_validated || r.history_verified || r.candidate_proof_checked),
                &json!(false),
            );
        }
    }
    Ok(
        json!({"passed":mismatches==0,"checks":checks,"mismatches":mismatches,"failures":failures,"expansion_cases":v["expansions"].as_array().unwrap().len(),"quote_cases":v["quotes"].as_array().unwrap().len(),"header_policy_cases":v["headers"].as_array().unwrap().len(),"accepted_header_projections":accepted,"parent_orders":orders,"full_consensus":false,"canonical_history_verified":false,"scope":v["scope"]}),
    )
}
pub fn verify_native(v: &Value) -> Result<Value, String> {
    let mut checks = 0;
    let mut mismatches = 0;
    let mut failures = vec![];
    let mut accepted = 0;
    let mut complete = 0;
    let mut verifier = Verifier::default();
    for c in v["native"].as_array().ok_or("native")? {
        let b = history::record(&c["block"])?;
        let hist = history::history(&c["records"])?;
        let p = profile(c)?;
        let actual = zone_header::verify_zone_header_components(
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
            failures
                .push(json!({"id":c["id"],"actual":format!("{actual:?}"),"expected":c["valid"]}));
        }
        if let Ok(r) = actual {
            accepted += 1;
            checks += 1;
            if r.fully_validated || r.history_verified || r.candidate_proof_checked {
                mismatches += 1
            }
        }
        if c["inspect_valid"].is_boolean() {
            let actual = zone_header::inspect_zone_candidate(
                &mut verifier,
                &hist,
                &b,
                &p,
                c["now"].as_u64().unwrap(),
            );
            checks += 1;
            if actual.is_ok() != c["inspect_valid"].as_bool().unwrap() {
                mismatches += 1;
                failures.push(json!({"id":c["id"],"inspect_actual":format!("{actual:?}"),"expected":c["inspect_valid"]}));
            }
            if let Ok(r) = actual {
                complete += 1;
                checks += 1;
                if !r.candidate_proof_checked || r.fully_validated || r.history_verified {
                    mismatches += 1
                }
            }
        }
    }
    Ok(
        json!({"passed":mismatches==0,"checks":checks,"mismatches":mismatches,"failures":failures,"cases":v["native"].as_array().unwrap().len(),"accepted_headers":accepted,"accepted_component_bundles":complete,"full_consensus":false,"canonical_history_verified":false}),
    )
}
