//! Frozen differential vectors can be checked with only Rust installed.
use crate::{decode_hex, encode_hex, Header, Schedule};
use serde_json::{json, Value};

pub fn verify(bytes: &[u8]) -> Result<Value, String> {
    let corpus: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if corpus["schema"] != 1 || corpus["reference_revision"] != crate::REFERENCE_REVISION {
        return Err("unsupported vector schema/reference".into());
    }
    if corpus["kind"] == "native-foundation" {
        return crate::foundation_vectors::verify(&corpus);
    }
    if corpus.get("transactions").is_some() {
        return crate::transaction_vectors::verify(&corpus);
    }
    let schedule: Schedule =
        serde_json::from_value(corpus["schedule"].clone()).map_err(|e| e.to_string())?;
    let mut checks = 0usize;
    let mut failures = Vec::new();
    let string = |v: &Value, key: &str| {
        v.get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("missing vector {key}"))
    };
    for vector in corpus["headers"].as_array().ok_or("missing headers")? {
        let header = Header::decode(&decode_hex(&string(vector, "input")?)?, schedule)?;
        let actual = [
            ("canonical", encode_hex(&header.encode())),
            ("seal_bytes", encode_hex(&header.seal_bytes())),
            ("seal_hash", encode_hex(&header.seal_hash())),
            ("legacy_hash", encode_hex(&header.legacy_hash())),
        ];
        for (field, value) in actual {
            checks += 1;
            if vector[field] != value {
                failures.push(json!({"id":vector["id"],"field":field}));
            }
        }
        checks += 1;
        if header.blake3_target_valid()?
            != vector["blake3_target_valid"]
                .as_bool()
                .ok_or("missing target result")?
        {
            failures.push(json!({"id":vector["id"],"field":"blake3_target_valid"}));
        }
        checks += 1;
        let hash = header.block_hash();
        if hash.as_ref().map(|h| encode_hex(h)).ok().as_deref() != vector["block_hash"].as_str() {
            failures.push(json!({"id":vector["id"],"field":"block_hash"}));
        }
    }
    for vector in corpus["rejected"].as_array().ok_or("missing rejections")? {
        checks += 1;
        if Header::decode(&decode_hex(&string(vector, "input")?)?, schedule).is_ok() {
            failures.push(json!({"id":vector["id"],"field":"missing-field accepted"}));
        }
    }
    // AuxPow shapes through the decoder that peer data takes: go-quai keeps
    // an AuxPow only with a donor header and a coinbase transaction
    // (types/wo.go:1652) and drops any other.
    let aux_shapes = corpus["aux_shapes"].as_array().map_or(&[][..], |v| v);
    for vector in aux_shapes {
        let decoded = Header::decode_auxiliary(&decode_hex(&string(vector, "input")?)?, schedule);
        checks += 1;
        match (decoded, vector["go_rejected"].as_bool()) {
            (Err(_), Some(true)) => {}
            (Ok(header), Some(false)) => {
                let actual = [
                    ("canonical", json!(encode_hex(&header.encode()))),
                    (
                        "block_hash",
                        json!(header.block_hash().map(|h| encode_hex(&h)).ok()),
                    ),
                    ("aux", json!(header.wire().aux_pow.is_some())),
                ];
                for (field, value) in actual {
                    checks += 1;
                    if vector[field] != value {
                        failures.push(json!({"id":vector["id"],"field":field}));
                    }
                }
            }
            (decoded, _) => failures.push(json!({
                "id":vector["id"],
                "field":"go_rejected",
                "rust":decoded.err(),
            })),
        }
    }
    for vector in corpus["targets"]
        .as_array()
        .ok_or("missing target vectors")?
    {
        let hash: crate::Hash = decode_hex(&string(vector, "hash")?)?
            .try_into()
            .map_err(|_| "bad vector hash length")?;
        let valid =
            crate::meets_blake3_target(&hash, &decode_hex(&string(vector, "difficulty")?)?)?;
        checks += 1;
        if vector["valid"] != valid {
            failures.push(json!({"field":"target comparison","hash":vector["hash"]}));
        }
    }
    Ok(
        json!({"passed":failures.is_empty(),"header_vectors":corpus["headers"].as_array().map_or(0, Vec::len),"aux_shape_vectors":aux_shapes.len(),"rejected_vectors":corpus["rejected"].as_array().map_or(0, Vec::len),"target_vectors":corpus["targets"].as_array().map_or(0, Vec::len),"checks":checks,"failures":failures,"runtime":"Rust only","consensus_validation":false}),
    )
}
