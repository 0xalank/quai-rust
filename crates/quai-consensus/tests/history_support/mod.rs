use crypto_bigint::Encoding;
use quai_consensus::{
    auxiliary::Auxiliary,
    history::{History, Record},
    rules::Uint,
    share_rules::{self, Triple},
    validation::{self, Profile},
    verifier::PINNED_SCHEDULE,
};
use quai_primitives::{decode_hex, Header};
use serde_json::{json, Value};
fn bytes(v: &Value) -> Result<Vec<u8>, String> {
    decode_hex(v.as_str().ok_or("missing hex")?)
}
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
fn triple(t: Triple) -> Value {
    json!([dec(t.difficulty), dec(t.count), dec(t.uncled)])
}
fn number(v: &Value) -> u64 {
    v.as_u64().unwrap()
}
pub fn profile() -> Profile {
    Profile {
        location: vec![0, 0],
        minimum_difficulty: Uint::ONE,
        duration: 5,
        gas_ceiling: 50_000_000,
        p2p_threshold: 7,
        default_genesis: None,
        genesis: Default::default(),
    }
}
pub fn record(v: &Value) -> Result<Record, String> {
    Record::decode(&bytes(v)?).map_err(|e| e.to_string())
}
pub fn history(v: &Value) -> Result<History, String> {
    let mut h = History::default();
    for r in v.as_array().ok_or("records")? {
        h.insert(record(r)?).map_err(|e| e.to_string())?;
    }
    Ok(h)
}
pub fn verify(v: &Value) -> Result<Value, String> {
    if v["reference_revision"] != quai_primitives::REFERENCE_REVISION {
        return Err("reference revision".into());
    }
    let mut checks = 0;
    let mut mismatches = 0;
    let mut failures = vec![];
    let mut cmp = |id: String, field: &str, a: Value, e: &Value| {
        checks += 1;
        if &a != e {
            mismatches += 1;
            if failures.len() < 50 {
                failures.push(json!({"id":id,"field":field,"actual":a,"expected":e}));
            }
        }
    };
    for (i, c) in v["controllers"]
        .as_array()
        .ok_or("controllers")?
        .iter()
        .enumerate()
    {
        let prime = number(&c["prime"]);
        let parent = Triple {
            difficulty: decimal(&c["parent_diff"])?,
            count: decimal(&c["count"])?,
            uncled: decimal(&c["uncled"])?,
        };
        let bd = decimal(&c["block_diff"])?;
        let target = decimal(&c["target"])?;
        let actual = share_rules::share_target(bd, decimal(&c["parent_kaw"])?, target, prime)
            .map_err(|e| e.to_string())?;
        cmp(
            format!("controller/{i}"),
            "target",
            dec(actual),
            &c["expected_target"],
        );
        for (name, sha) in [("sha", true), ("scrypt", false)] {
            let actual = share_rules::next_pow_triple(
                bd,
                &parent,
                target,
                number(&c["samples"]) as u32,
                number(&c["uncled_samples"]) as u32,
                prime,
                sha,
            )
            .map_err(|e| e.to_string())?;
            cmp(format!("controller/{i}"), name, triple(actual), &c[name]);
        }
    }
    for (i, c) in v["kaw_share"]
        .as_array()
        .ok_or("kaw shares")?
        .iter()
        .enumerate()
    {
        let actual = share_rules::kawpow_share_difficulty(
            decimal(&c["difficulty"])?,
            decimal(&c["sha_count"])?,
            decimal(&c["scrypt_count"])?,
            decimal(&c["sha_target"])?,
            decimal(&c["scrypt_target"])?,
            decimal(&c["kaw"])?,
        )
        .map_err(|e| e.to_string())?;
        cmp(
            format!("kaw share/{i}"),
            "difficulty",
            dec(actual),
            &c["expected"],
        );
    }
    for (i, c) in v["kaw_ema"].as_array().ok_or("kaw ema")?.iter().enumerate() {
        let aux = if c["aux"].is_string() {
            Some(Auxiliary::decode(&bytes(&c["aux"])?).map_err(|e| e.to_string())?)
        } else {
            None
        };
        let actual = share_rules::next_kawpow_difficulty(
            decimal(&c["previous"])?,
            number(&c["prime"]),
            aux.as_ref(),
        )
        .map_err(|e| e.to_string())?;
        cmp(
            format!("kaw ema/{i}"),
            "difficulty",
            dec(actual),
            &c["expected"],
        );
    }
    for (i, c) in v["classifications"]
        .as_array()
        .ok_or("classifications")?
        .iter()
        .enumerate()
    {
        let h = Header::decode_auxiliary(&bytes(&c["header"])?, PINNED_SCHEDULE)?;
        let hash = bytes(&c["hash"])?.try_into().map_err(|_| "hash")?;
        let actual = share_rules::classify(&h, hash, 7).map_err(|e| e.to_string())? as u8;
        cmp(
            format!("classification/{i}"),
            "class",
            json!(actual),
            &c["expected"],
        );
    }
    let (mut accepted_distances, mut accepted_inclusions) = (0, 0);
    for (i, c) in v["distances"]
        .as_array()
        .ok_or("distances")?
        .iter()
        .enumerate()
    {
        let hist = history(&c["records"])?;
        let b = record(&c["block"])?;
        let h = Header::decode_auxiliary(&bytes(&c["share"])?, PINNED_SCHEDULE)?;
        let actual = hist.workshare_distance(&b, &h);
        cmp(
            format!("distance/{i}"),
            "valid",
            json!(actual.is_ok()),
            &c["valid"],
        );
        if let Ok(d) = actual {
            accepted_distances += 1;
            cmp(
                format!("distance/{i}"),
                "distance",
                json!(d.to_string()),
                &c["distance"],
            );
        }
    }
    for (i, c) in v["inclusions"]
        .as_array()
        .ok_or("inclusions")?
        .iter()
        .enumerate()
    {
        let hist = history(&c["records"])?;
        let b = record(&c["block"])?;
        let uncles = b.uncles().map_err(|e| e.to_string())?;
        let hashes = uncles
            .iter()
            .map(|h| {
                h.wire()
                    .mix_hash
                    .as_ref()
                    .unwrap()
                    .value
                    .as_slice()
                    .try_into()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let actual = validation::check_inclusions_projection(
            &hist,
            &b,
            &profile(),
            &uncles,
            &hashes,
            |_| Ok(2),
        );
        cmp(
            format!("inclusion/{i}"),
            "valid",
            json!(actual.is_ok()),
            &c["valid"],
        );
        if let Ok(r) = actual {
            accepted_inclusions += 1;
            cmp(
                format!("inclusion/{i}"),
                "no authority",
                json!(r.fully_validated || r.history_verified),
                &json!(false),
            );
        }
    }
    Ok(
        json!({"passed":mismatches==0,"checks":checks,"mismatches":mismatches,"failures":failures,"controller_cases":v["controllers"].as_array().unwrap().len(),"kaw_share_cases":v["kaw_share"].as_array().unwrap().len(),"kaw_ema_cases":v["kaw_ema"].as_array().unwrap().len(),"classification_cases":v["classifications"].as_array().unwrap().len(),"distance_cases":v["distances"].as_array().unwrap().len(),"accepted_distances":accepted_distances,"inclusion_projection_cases":v["inclusions"].as_array().unwrap().len(),"accepted_inclusion_projections":accepted_inclusions,"full_consensus":false,"canonical_history_verified":false,"scope":v["scope"]}),
    )
}

/// Full native work and binding checks against actual Go VerifyUncles cases.
/// This still consumes an imported candidate history, never validated authority.
pub fn verify_native(v: &Value) -> Result<Value, String> {
    let mut verifier = quai_consensus::verifier::Verifier::default();
    let mut checks = 0;
    let mut mismatches = 0;
    let mut failures = vec![];
    let mut accepted = 0;
    for c in v["native_inclusions"]
        .as_array()
        .ok_or("native inclusions")?
    {
        let h = history(&c["records"])?;
        let b = record(&c["block"])?;
        let mut p = profile();
        p.genesis
            .insert(bytes(&c["genesis"])?.try_into().map_err(|_| "genesis")?);
        let actual = validation::verify_inclusions(&mut verifier, &h, &b, &p);
        checks += 1;
        if actual.is_ok() != c["valid"].as_bool().ok_or("expected")? {
            mismatches += 1;
            failures.push(json!({"variant":c["variant"],"actual":format!("{actual:?}"),"expected":c["valid"],"go_error":c["error"]}));
        }
        if let Ok(r) = actual {
            accepted += 1;
            checks += 1;
            if r.fully_validated || r.history_verified {
                mismatches += 1;
                failures.push(json!("unexpected authority"));
            }
        }
    }
    Ok(
        json!({"passed":mismatches==0,"checks":checks,"mismatches":mismatches,"failures":failures,"cases":v["native_inclusions"].as_array().unwrap().len(),"accepted":accepted,"full_consensus":false,"canonical_history_verified":false,"scope":"Actual native KAWPOW mix/work, pinned quorum, auxiliary bindings, body commitments and zone inclusion checks against Go VerifyUncles on supplied unverified history."}),
    )
}
