#[allow(dead_code)]
#[path = "../history_support/mod.rs"]
pub mod history;
use quai_consensus::{
    placement::{self, CandidateIndex, Profile, Route, Snapshot, Termini},
    rules::Uint,
    verifier::Verifier,
};
use quai_primitives::{decode_hex, encode_hex, Hash};
use serde_json::{json, Value};
fn hash(v: &Value) -> Result<Hash, String> {
    decode_hex(v.as_str().ok_or("hash")?)?
        .try_into()
        .map_err(|_| "hash width".into())
}
fn hashes(v: &Value) -> Result<Vec<Hash>, String> {
    v.as_array().ok_or("hashes")?.iter().map(hash).collect()
}
fn location(v: &Value) -> Result<Vec<u8>, String> {
    v.as_array()
        .ok_or("location")?
        .iter()
        .map(|v| {
            v.as_u64()
                .and_then(|n| u8::try_from(n).ok())
                .ok_or("location value".into())
        })
        .collect()
}
fn termini(v: &Value) -> Result<Termini, String> {
    Termini::decode(&decode_hex(v.as_str().ok_or("termini")?)?).map_err(|e| e.to_string())
}
pub fn profile(v: &Value) -> Result<Profile, String> {
    let genesis = if v["genesis"].is_array() {
        hashes(&v["genesis"])?
    } else {
        vec![hash(&v["genesis"])?]
    };
    Ok(Profile {
        location: location(&v["location"])?,
        genesis: genesis.into_iter().collect(),
        minimum_difficulty: Uint::ONE,
        duration: 5,
        gas_ceiling: 50000000,
        p2p_threshold: 7,
        default_genesis: None,
    })
}
pub fn snapshot(v: &Value) -> Result<Snapshot, String> {
    Ok(Snapshot {
        manifest: if v["manifest"].is_array() {
            Some(hashes(&v["manifest"])?)
        } else {
            None
        },
        interlinks: if v["interlinks"].is_array() {
            Some(
                hashes(&v["interlinks"])?
                    .try_into()
                    .map_err(|_| "interlink width")?,
            )
        } else {
            None
        },
        termini: if v["termini"].is_string() {
            Some(termini(&v["termini"])?)
        } else {
            None
        },
    })
}
fn links_json(v: &[Hash]) -> Value {
    json!(v.iter().map(|h| encode_hex(h)).collect::<Vec<_>>())
}
fn snapshot_json(v: &Snapshot) -> Value {
    let mut out = json!({});
    if let Some(v) = &v.manifest {
        out["manifest"] = links_json(v)
    }
    if let Some(v) = &v.interlinks {
        out["interlinks"] = links_json(v)
    }
    if let Some(v) = &v.termini {
        out["termini"] = json!(encode_hex(&v.encode()))
    }
    out
}
fn route_name(r: Route) -> &'static str {
    match r {
        Route::Local => "local",
        Route::Dominant => "dominant",
        Route::OtherContext => "other",
    }
}
pub fn parent_id(record: &quai_consensus::history::Record, ctx: usize) -> Hash {
    if ctx == 2 {
        record
            .header()
            .wire()
            .parent_hash
            .as_ref()
            .unwrap()
            .value
            .as_slice()
            .try_into()
            .unwrap()
    } else {
        record.object().header().wire().parent_hash[ctx]
            .value
            .as_slice()
            .try_into()
            .unwrap()
    }
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
            if failures.len() < 50 {
                failures.push(json!({"id":id,"field":key,"actual":a,"expected":e}))
            }
        }
    };
    let mut orders = [0usize; 3];
    let mut ranks = [0usize; 5];
    for (i, c) in v["orders"].as_array().ok_or("orders")?.iter().enumerate() {
        let b = history::record(&c["block"])?;
        let ctx = c["context"].as_u64().unwrap() as usize;
        let projection = placement::order_projection(&b, hash(&c["pow_hash"])?, ctx, false)
            .map_err(|e| e.to_string())?;
        orders[projection.order as usize] += 1;
        if let Some(r) = projection.rank {
            ranks[r as usize] += 1
        };
        let route = placement::route(ctx, projection.order).map_err(|e| e.to_string())?;
        compare(json!(i), "order", json!(projection.order), &c["order"]);
        compare(json!(i), "rank", json!(projection.rank), &c["rank"]);
        compare(
            json!(i),
            "queue eligibility",
            json!(route == Route::Local),
            &c["queued"],
        );
        compare(
            json!(i),
            "route projection",
            json!(route_name(route)),
            &c["route"],
        );
    }
    for (i, c) in v["updates"].as_array().ok_or("updates")?.iter().enumerate() {
        let b = history::record(&c["block"])?;
        let ctx = c["context"].as_u64().unwrap() as usize;
        let manifest = hashes(&c["parent_manifest"])?;
        let next = placement::next_manifest(
            ctx,
            c["order"].as_u64().unwrap() as u8,
            b.id(),
            Some(&manifest),
        )
        .map_err(|e| e.to_string())?;
        compare(
            json!(i),
            "manifest advance",
            links_json(&next),
            &c["manifest"],
        );
        if c["interlinks"].is_array() {
            let links = hashes(&c["parent_interlinks"])?
                .try_into()
                .map_err(|_| "links")?;
            let next = placement::next_interlinks(
                b.id(),
                Some(&links),
                c["rank"].as_u64().unwrap() as u8,
                false,
            )
            .map_err(|e| e.to_string())?;
            compare(
                json!(i),
                "interlink advance",
                links_json(&next),
                &c["interlinks"],
            );
        }
    }
    let mut accepted_append = 0;
    for (i, c) in v["append_commitments"]
        .as_array()
        .ok_or("append commitments")?
        .iter()
        .enumerate()
    {
        let b = history::record(&c["block"])?;
        let ctx = c["context"].as_u64().unwrap() as usize;
        let mut index = CandidateIndex::default();
        if c["parent_present"] == true {
            index
                .insert_imported(
                    parent_id(&b, ctx),
                    Snapshot {
                        manifest: Some(hashes(&c["parent_manifest"])?),
                        interlinks: Some(
                            hashes(&c["parent_interlinks"])?
                                .try_into()
                                .map_err(|_| "links")?,
                        ),
                        termini: None,
                    },
                )
                .map_err(|e| e.to_string())?
        };
        let actual = placement::check_append_commitments(&index, &b, ctx);
        if actual.is_ok() {
            accepted_append += 1
        };
        compare(
            json!(i),
            "append commitment",
            json!(actual.is_ok()),
            &c["valid"],
        );
    }
    let mut codec_valid = 0;
    for (i, c) in v["termini_codecs"]
        .as_array()
        .ok_or("codecs")?
        .iter()
        .enumerate()
    {
        let actual = Termini::decode(&decode_hex(c["input"].as_str().unwrap())?);
        compare(
            json!(i),
            "termini validity",
            json!(actual.is_ok()),
            &c["valid"],
        );
        if let Ok(t) = actual {
            codec_valid += 1;
            compare(
                json!(i),
                "termini canonical",
                json!(encode_hex(&t.encode())),
                &c["canonical"],
            );
        }
    }
    let mut pcrc_valid = 0;
    for (i, c) in v["pcrc"].as_array().ok_or("pcrc")?.iter().enumerate() {
        let parent = termini(&c["parent"])?;
        let saved = parent.clone();
        let p = profile(c)?;
        let actual = placement::pcrc(
            &parent,
            hash(&c["id"])?,
            &location(&c["header_location"])?,
            &p.location,
            c["dominant_origin"].as_bool().unwrap(),
            hash(&c["claimed_dominant"])?,
            &p.genesis,
        );
        compare(
            json!(i),
            "PCRC validity",
            json!(actual.is_ok()),
            &c["valid"],
        );
        compare(
            json!(i),
            "PCRC immutable parent",
            json!(saved == parent),
            &c["input_unchanged"],
        );
        if let Ok((returned, next)) = actual {
            pcrc_valid += 1;
            compare(
                json!(i),
                "returned previous dominant",
                json!(encode_hex(&returned)),
                &c["returned_dominant"],
            );
            compare(
                json!(i),
                "next termini",
                json!(encode_hex(&next.encode())),
                &c["next"],
            );
            compare(
                json!(i),
                "stored candidate termini",
                json!(encode_hex(&next.encode())),
                &c["stored"],
            );
        }
    }
    Ok(
        json!({"passed":mismatches==0,"checks":checks,"mismatches":mismatches,"failures":failures,"order_cases":v["orders"].as_array().unwrap().len(),"orders":orders,"ranks":ranks,"update_cases":v["updates"].as_array().unwrap().len(),"append_cases":v["append_commitments"].as_array().unwrap().len(),"accepted_append":accepted_append,"termini_codec_cases":v["termini_codecs"].as_array().unwrap().len(),"accepted_termini_codecs":codec_valid,"pcrc_cases":v["pcrc"].as_array().unwrap().len(),"accepted_pcrc":pcrc_valid,"full_consensus":false,"canonical_history_verified":false,"scope":v["scope"]}),
    )
}
pub fn verify_native(v: &Value) -> Result<Value, String> {
    let (mut checks, mut mismatches, mut accepted) = (0, 0, 0);
    let mut failures = vec![];
    let mut verifier = Verifier::default();
    for c in v["native"].as_array().ok_or("native")? {
        let hist = history::history(&c["records"])?;
        let b = history::record(&c["block"])?;
        let p = profile(c)?;
        let mut index = CandidateIndex::default();
        let before = snapshot(&c["parent_snapshot"])?;
        index
            .insert_imported(parent_id(&b, p.context()), before.clone())
            .map_err(|e| e.to_string())?;
        let count = index.len();
        let bytes = index.tracked_payload_bytes();
        let actual = placement::prepare_append_components(
            &mut verifier,
            &hist,
            &index,
            &b,
            &p,
            c["now"].as_u64().unwrap(),
            c["dominant_origin"].as_bool().unwrap(),
            hash(&c["claimed_dominant"])?,
        );
        checks += 1;
        if actual.is_ok() != c["valid"].as_bool().unwrap() {
            mismatches += 1;
            failures.push(json!({"id":c["id"],"actual":format!("{actual:?}"),"expected":c["valid"],"go_error":c["error"]}));
        }
        checks += 1;
        if count != index.len()
            || bytes != index.tracked_payload_bytes()
            || index
                .get(&parent_id(&b, p.context()))
                .map_err(|e| e.to_string())?
                .as_ref()
                != &before
        {
            mismatches += 1;
            failures.push(json!({"id":c["id"],"state_changed":true}));
        }
        if let Ok(r) = actual {
            accepted += 1;
            for (field, a) in [
                ("next", snapshot_json(&r.next)),
                ("returned_dominant", json!(encode_hex(&r.previous_dominant))),
                ("order", json!(r.placement.order)),
                ("rank", json!(r.placement.rank)),
            ] {
                checks += 1;
                if a != c[field] {
                    mismatches += 1;
                    failures
                        .push(json!({"id":c["id"],"field":field,"actual":a,"expected":c[field]}));
                }
            }
            checks += 1;
            if r.fully_validated
                || r.history_verified
                || !r.header_policy_checked
                || !r.ancestry_checked
                || !r.body_checked
                || !r.placement.candidate_work_checked
                || !r.placement.candidate_order_checked
            {
                mismatches += 1;
                failures.push(json!({"id":c["id"],"bad_authority_report":true}));
            }
        }
    }
    Ok(
        json!({"passed":mismatches==0,"checks":checks,"mismatches":mismatches,"failures":failures,"cases":v["native"].as_array().unwrap().len(),"accepted":accepted,"full_consensus":false,"canonical_history_verified":false}),
    )
}
