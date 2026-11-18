//! Offline transaction differential checks against the pinned Go oracle.
use crate::{decode_hex, encode_hex, transaction::Transaction};
use serde_json::{json, Value};

pub fn verify(corpus: &Value) -> Result<Value, String> {
    let mut checks = 0;
    let mut failures = Vec::new();
    let vectors = corpus["transactions"]
        .as_array()
        .ok_or("missing transactions")?;
    for v in vectors {
        let hex = |key: &str| {
            v[key]
                .as_str()
                .ok_or_else(|| format!("missing vector {key}"))
        };
        let tx = match Transaction::decode(&decode_hex(hex("input")?)?) {
            Ok(tx) => tx,
            Err(e) => {
                failures.push(json!({"id":v["id"],"field":"decode","error":e}));
                continue;
            }
        };
        let mut compare = |field: &str, actual: Value| {
            checks += 1;
            if actual != v[field] {
                failures
                    .push(json!({"id":v["id"],"field":field,"expected":v[field],"actual":actual}));
            }
        };
        compare("type", json!(tx.type_id()));
        compare("canonical", json!(encode_hex(&tx.encode())));
        compare("signing_bytes", json!(encode_hex(&tx.signing_bytes())));
        compare("signing_hash", json!(encode_hex(&tx.signing_hash())));
        compare("typed_rlp", json!(encode_hex(&tx.typed_rlp())));
        compare(
            "sender",
            tx.sender()
                .map(|v| json!(encode_hex(&v)))
                .unwrap_or(Value::Null),
        );
        compare(
            "id_automatic",
            tx.id(None)
                .map(|v| json!(encode_hex(&v)))
                .unwrap_or(Value::Null),
        );
        if let Some(location) = v["origin"].as_array() {
            let location: [u8; 2] = location
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|n| u8::try_from(n).ok())
                        .ok_or("bad origin")
                })
                .collect::<Result<Vec<_>, _>>()?
                .try_into()
                .map_err(|_| "bad origin length")?;
            compare("id_explicit", json!(encode_hex(&tx.id(Some(location))?)));
        }
        if tx.type_id() == 2 {
            compare("aggregate_key", json!(encode_hex(&tx.qi_aggregate_key()?)));
            compare("signature_valid", json!(tx.verify_qi_signature()?));
        }
        checks += 1;
        if Transaction::decode(&tx.encode())? != tx {
            failures.push(json!({"id":v["id"],"field":"canonical idempotence"}));
        }
    }
    let rejected = corpus["rejected"].as_array().ok_or("missing rejections")?;
    for v in rejected {
        checks += 1;
        let raw = decode_hex(v["input"].as_str().ok_or("bad rejection input")?)?;
        if Transaction::decode(&raw).is_ok() {
            failures.push(json!({"id":v["id"],"field":"invalid input accepted"}));
        }
    }
    Ok(
        json!({"passed":failures.is_empty(),"transaction_vectors":vectors.len(),"rejected_vectors":rejected.len(),"checks":checks,"failures":failures,"runtime":"Rust only","state_validation":false,"typed_rlp_decode":false}),
    )
}
