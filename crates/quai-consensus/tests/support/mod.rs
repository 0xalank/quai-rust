use quai_consensus::pow::{Algorithm, LightCache};
use quai_primitives::{decode_hex, encode_hex};
use serde_json::{json, Value};
fn n(v: &Value, k: &str) -> Result<u64, String> {
    v[k].as_u64().ok_or_else(|| format!("missing {k}"))
}
fn hash(v: &Value, k: &str) -> Result<[u8; 32], String> {
    decode_hex(v[k].as_str().ok_or("missing hash")?)?
        .try_into()
        .map_err(|_| "bad hash length".into())
}
pub fn verify_pow(corpus: &Value) -> Result<Value, String> {
    if corpus["schema"] != 1
        || corpus["kind"] != "native-consensus"
        || corpus["reference_revision"] != quai_primitives::REFERENCE_REVISION
    {
        return Err("wrong corpus/reference".into());
    }
    let mut checks = 0usize;
    let mut failures = vec![];
    let mut mismatches = 0usize;
    let mut proofs = 0;
    let mut objects = 0;
    let mut cache_count = 0;
    let mut compare = |id: Value, key: &str, actual: Value, expected: &Value| {
        checks += 1;
        if &actual != expected {
            mismatches += 1;
            if failures.len() < 100 {
                failures.push(json!({"id":id,"field":key,"actual":actual,"expected":expected}));
            }
        }
    };
    for group in corpus["pow_groups"]
        .as_array()
        .ok_or("missing pow groups")?
    {
        let algorithm = match group["algorithm"].as_str() {
            Some("progpow") => Algorithm::Progpow,
            Some("kawpow") => Algorithm::Kawpow,
            _ => return Err("unknown algorithm".into()),
        };
        let cache = LightCache::new(algorithm, n(group, "height")?).map_err(|e| e.to_string())?;
        cache_count += 1;
        let id = json!(format!("{:?}/{}", algorithm, n(group, "height")?));
        compare(
            id.clone(),
            "cache size",
            json!(cache.cache_bytes()),
            &group["cache_bytes"],
        );
        compare(
            id.clone(),
            "dataset size",
            json!(cache.dataset_size()),
            &group["dataset_size"],
        );
        compare(
            id.clone(),
            "cache digest",
            json!(encode_hex(&cache.cache_digest())),
            &group["cache_digest"],
        );
        compare(
            id.clone(),
            "cdag digest",
            json!(encode_hex(&cache.cdag_digest())),
            &group["cdag_digest"],
        );
        for item in group["items"].as_array().ok_or("missing items")? {
            compare(
                id.clone(),
                "dataset item",
                json!(encode_hex(&cache.dataset_item(
                    u32::try_from(n(item, "index")?).map_err(|_| "bad dataset index")?
                ))),
                &item["value"],
            );
        }
        for v in group["vectors"].as_array().ok_or("missing vectors")? {
            let (height, nonce, seal) = (n(v, "height")?, n(v, "nonce")?, hash(v, "seal")?);
            let (mix, pow) = cache
                .compute(seal, nonce, height)
                .map_err(|e| e.to_string())?;
            let id = json!(format!("{:?}/{height}/{nonce}", algorithm));
            compare(id.clone(), "mix", json!(encode_hex(&mix)), &v["mix"]);
            compare(id.clone(), "pow", json!(encode_hex(&pow)), &v["hash"]);
            let accepted = cache
                .verify(seal, nonce, height, mix, &[1])
                .map_err(|e| e.to_string())?;
            compare(
                id.clone(),
                "verified proof hash",
                json!(encode_hex(&accepted.hash())),
                &v["hash"],
            );
            let mut bad = mix;
            bad[0] ^= 1;
            compare(
                id.clone(),
                "mutated mix rejected",
                json!(cache.verify(seal, nonce, height, bad, &[1]).is_err()),
                &json!(true),
            );
            compare(
                id.clone(),
                "insufficient proof rejected",
                json!(cache.verify(seal, nonce, height, mix, &[255; 32]).is_err()),
                &json!(true),
            );
            proofs += 1;
        }
        let mut verifier = quai_consensus::verifier::Verifier::with_cache(cache);
        for v in group["objects"].as_array().ok_or("missing objects")? {
            let bytes = decode_hex(v["header"].as_str().ok_or("header")?)?;
            let h =
                quai_primitives::Header::decode(&bytes, quai_consensus::verifier::PINNED_SCHEDULE)?;
            let proof = verifier.verify_header_proof(&h).map_err(|e| {
                format!(
                    "header seal={} expected={} nonce={:?} expected_nonce={} mix={:?}: {e}",
                    encode_hex(&h.seal_hash()),
                    v["seal"],
                    h.wire().nonce,
                    v["nonce"],
                    h.wire().mix_hash
                )
            })?;
            compare(
                json!(objects),
                "header-bound proof",
                json!(encode_hex(&proof.hash())),
                &v["pow"],
            );
            compare(
                json!(objects),
                "header-bound seal",
                json!(encode_hex(&proof.seal())),
                &v["seal"],
            );
            let bad = quai_primitives::Header::decode(
                &decode_hex(v["mutated_mix_header"].as_str().ok_or("bad header")?)?,
                quai_consensus::verifier::PINNED_SCHEDULE,
            )?;
            compare(
                json!(objects),
                "header mix mutation rejected",
                json!(verifier.verify_header_proof(&bad).is_err()),
                &json!(true),
            );
            let object = decode_hex(v["object"].as_str().ok_or("object")?)?;
            let report = verifier
                .inspect_components(&object, quai_primitives::work::Context::Zone)
                .map_err(|e| e.to_string())?;
            compare(
                json!(objects),
                "component-bound proof",
                json!(encode_hex(&report.proof.hash())),
                &v["pow"],
            );
            compare(
                json!(objects),
                "component-bound body",
                json!(encode_hex(&report.body_header_hash)),
                &v["body_header_hash"],
            );
            compare(
                json!(objects),
                "component report has no chain authority",
                json!(report.fully_validated),
                &json!(false),
            );
            compare(
                json!(objects),
                "a block without its ancestry fails validation",
                json!(
                    quai_consensus::history::Record::decode(&object).map_or(true, |r| {
                        quai_consensus::block::validate_on_genesis(
                            &mut verifier,
                            quai_consensus::chain::Chain::private_hierarchy().unwrap(),
                            [None, None, Some(std::sync::Arc::new(r))],
                            1_800_000_000,
                        )
                        .is_err()
                    })
                ),
                &json!(true),
            );
            objects += 1;
        }
    }
    Ok(
        json!({"passed":mismatches==0,"checks":checks,"mismatches":mismatches,"failures":failures,"full_size_epoch_caches":cache_count,"pow_inputs":proofs,"header_bound_objects":objects,"full_consensus":false,"full_auxiliary_proof_validation":false,"scope":"Native full-size light caches, DAG items, mix/PoW hashes and target/mutation checks. Not aux template/Merkle/quorum or full block/header validity."}),
    )
}
fn uint(v: &Value, k: &str) -> Result<quai_consensus::rules::Uint, String> {
    let b = decode_hex(v[k].as_str().ok_or("missing uint")?)?;
    if b.len() > 32 {
        return Err("uint overflow".into());
    }
    let mut out = [0; 32];
    out[32 - b.len()..].copy_from_slice(&b);
    use crypto_bigint::Encoding;
    Ok(quai_consensus::rules::Uint::from_be_bytes(out))
}
fn decimal(v: &Value) -> Result<quai_consensus::rules::Uint, String> {
    use quai_consensus::rules::Uint;
    let mut out = Uint::ZERO;
    for c in v.as_str().ok_or("bad decimal")?.bytes() {
        if !c.is_ascii_digit() {
            return Err("bad digit".into());
        }
        out = out
            .wrapping_mul(&Uint::from_u64(10))
            .wrapping_add(&Uint::from_u64((c - b'0') as u64));
    }
    Ok(out)
}
fn dec(n: quai_consensus::rules::Uint) -> Value {
    use quai_consensus::rules::Uint;
    let mut n = n;
    let mut digits = vec![];
    while n != Uint::ZERO {
        let rem = n.wrapping_rem(&Uint::from_u64(10));
        use crypto_bigint::Encoding;
        digits.push((rem.to_be_bytes()[31] + b'0') as char);
        n = n.wrapping_div(&Uint::from_u64(10));
    }
    if digits.is_empty() {
        json!("0")
    } else {
        json!(digits.into_iter().rev().collect::<String>())
    }
}
pub fn verify_rules(corpus: &Value) -> Result<Value, String> {
    use crypto_bigint::Encoding;
    use quai_consensus::rules::{self, EntropyInput};
    let mut checks = 0usize;
    let mut failures = vec![];
    let mut mismatches = 0usize;
    let mut orders = [0usize; 3];
    let mut compare = |id: Value, key: &str, actual: Value, expected: &Value| {
        checks += 1;
        if &actual != expected {
            mismatches += 1;
            if failures.len() < 100 {
                failures.push(json!({"id":id,"field":key,"actual":actual,"expected":expected}));
            }
        }
    };
    for (i, v) in corpus["difficulty"]
        .as_array()
        .ok_or("missing difficulties")?
        .iter()
        .enumerate()
    {
        let actual = rules::adjust_difficulty(
            uint(v, "parent")?,
            uint(v, "minimum")?,
            n(v, "parent_time")?,
            n(v, "grandparent_time")?,
            n(v, "duration")?,
        )
        .map_err(|e| e.to_string())?;
        compare(
            json!(i),
            "difficulty",
            json!(encode_hex(&actual.to_be_bytes())),
            &v["expected"],
        );
    }
    for (i, v) in corpus["entropy"]
        .as_array()
        .ok_or("missing entropy cases")?
        .iter()
        .enumerate()
    {
        let arr = |k: &str| -> Result<_, String> {
            v[k].as_array()
                .ok_or("missing entropy array")?
                .iter()
                .map(decimal)
                .collect::<Result<Vec<_>, _>>()?
                .try_into()
                .map_err(|_| "bad array length".into())
        };
        let input = EntropyInput {
            hash: hash(v, "hash")?,
            difficulty: uint(v, "difficulty")?,
            expansion: u8::try_from(n(v, "expansion")?).map_err(|_| "bad expansion")?,
            parent_entropy: arr("parent_entropy")?,
            parent_delta: arr("parent_delta")?,
            parent_uncled_delta: arr("parent_uncled_delta")?,
            uncled_entropy: decimal(&v["uncled_entropy"])?,
            postfork_share_count: if v["shares"].as_i64().ok_or("bad share count")? < 0 {
                None
            } else {
                Some(u32::try_from(n(v, "shares")?).map_err(|_| "bad shares")?)
            },
            node_context: u8::try_from(n(v, "context")?).map_err(|_| "bad context")?,
        };
        let actual = rules::entropy(&input).map_err(|e| e.to_string())?;
        orders[actual.order as usize] += 1;
        let value = json!({"intrinsic":dec(actual.intrinsic),"order":actual.order,"rank":actual.rank,"total":dec(actual.total),"delta":dec(actual.delta),"uncled_delta":dec(actual.uncled_delta),"workshares":dec(actual.workshares)});
        compare(json!(i), "entropy fields", value, &v["expected"]);
    }
    for v in corpus["pow_ids"].as_array().ok_or("missing PoW IDs")? {
        let id = v["aux_id"].as_i64().ok_or("bad pow ID")?;
        let aux = if id < 0 {
            None
        } else {
            Some(u8::try_from(id).map_err(|_| "bad ID")?)
        };
        compare(
            json!(n(v, "prime")?),
            "pow-id policy",
            json!(rules::pow_id_valid(
                n(v, "prime")?,
                aux,
                v["workshare"].as_bool().ok_or("bad share flag")?
            )),
            &v["valid"],
        );
    }
    for v in corpus["limits"].as_array().ok_or("missing limits")? {
        let limit = rules::next_limit(n(v, "number")?, n(v, "parent")?, n(v, "ceiling")?);
        compare(json!(n(v, "number")?), "gas limit", json!(limit), &v["gas"]);
        compare(
            json!(n(v, "number")?),
            "state limit",
            json!(limit),
            &v["state"],
        );
    }
    for (i, v) in corpus["thresholds"]
        .as_array()
        .ok_or("missing thresholds")?
        .iter()
        .enumerate()
    {
        let t = rules::share_target(
            uint(v, "difficulty")?,
            u16::try_from(n(v, "shift")?).map_err(|_| "bad shift")?,
        )
        .map_err(|e| e.to_string())?;
        let b = t.to_be_bytes();
        let b = &b[b.iter().position(|v| *v != 0).unwrap_or(64)..];
        compare(
            json!(i),
            "share threshold",
            json!(encode_hex(b)),
            &v["target"],
        );
    }
    for (i, v) in corpus["donors"]
        .as_array()
        .ok_or("missing donor hashes")?
        .iter()
        .enumerate()
    {
        let header = decode_hex(v["header"].as_str().ok_or("bad donor header")?)?;
        let sha = quai_consensus::donor::sha256d(&header).map_err(|e| e.to_string())?;
        let scrypt = quai_consensus::donor::scrypt(&header).map_err(|e| e.to_string())?;
        compare(
            json!(i),
            "SHA_BTC donor hash",
            json!(encode_hex(&sha)),
            &v["expected"]["SHA_BTC"],
        );
        compare(
            json!(i),
            "SHA_BCH donor hash",
            json!(encode_hex(&sha)),
            &v["expected"]["SHA_BCH"],
        );
        compare(
            json!(i),
            "Scrypt donor hash",
            json!(encode_hex(&scrypt)),
            &v["expected"]["Scrypt"],
        );
    }
    Ok(
        json!({"passed":mismatches==0,"checks":checks,"mismatches":mismatches,"failures":failures,"difficulty_cases":corpus["difficulty"].as_array().unwrap().len(),"entropy_cases":corpus["entropy"].as_array().unwrap().len(),"hierarchy_orders":orders,"pow_id_cases":corpus["pow_ids"].as_array().unwrap().len(),"limit_cases":corpus["limits"].as_array().unwrap().len(),"threshold_cases":corpus["thresholds"].as_array().unwrap().len(),"donor_headers":corpus["donors"].as_array().unwrap().len(),"full_consensus":false,"scope":"Ordinary difficulty adjustment and entropy/order/rank/total/delta projections, proof-ID fork rules, gas/state limits and workshare thresholds. No genesis/expansion initialization, full header policy, pre-fork nonempty workshare discounts or fork choice."}),
    )
}
