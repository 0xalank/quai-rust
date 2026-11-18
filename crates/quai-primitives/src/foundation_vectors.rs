//! Frozen complete-schema/body/receipt/trie differential checks; no Go at runtime.
use crate::{
    decode_hex, encode_hex,
    receipt::{self, Log, Receipt},
    schema,
    transaction::Transaction,
    trie,
    work::{BodyHeader, Context, WorkObject},
    Schedule,
};
use serde_json::{json, Value};
fn raw(v: &Value, key: &str) -> Result<Vec<u8>, String> {
    decode_hex(v[key].as_str().ok_or_else(|| format!("missing {key}"))?)
}
fn context(v: &Value) -> Result<Context, String> {
    match v["context"].as_str() {
        Some("prime") => Ok(Context::Prime),
        Some("region") => Ok(Context::Region),
        Some("zone") => Ok(Context::Zone),
        _ => Err("invalid context".into()),
    }
}
fn array<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>, String> {
    v[key].as_array().ok_or_else(|| format!("missing {key}"))
}
pub fn verify(corpus: &Value) -> Result<Value, String> {
    let schedule: Schedule =
        serde_json::from_value(corpus["schedule"].clone()).map_err(|e| e.to_string())?;
    let mut checks = 0;
    let mut failures = Vec::new();
    let mut compare = |id: &Value, field: &str, actual: Value, expected: &Value| {
        checks += 1;
        if &actual != expected {
            failures.push(json!({"id":id,"field":field,"actual":if actual.to_string().len()>512{json!("large value mismatch")}else{actual},"expected":if expected.to_string().len()>512{json!("large value mismatch")}else{expected.clone()}}));
        }
    };
    for v in array(corpus, "schema_vectors")? {
        let name = v["message"].as_str().ok_or("missing schema name")?;
        let bytes = match schema::canonicalize(name, &raw(v, "input")?) {
            Ok(b) => b,
            Err(e) => {
                compare(&v["id"], "schema decode", json!(e), &json!("accepted"));
                continue;
            }
        };
        compare(
            &v["id"],
            "canonical",
            json!(encode_hex(&bytes)),
            &v["canonical"],
        );
        compare(
            &v["id"],
            "schema idempotence",
            json!(schema::canonicalize(name, &bytes)? == bytes),
            &json!(true),
        );
    }
    for v in array(corpus, "trie_vectors")? {
        let values = array(v, "values")?
            .iter()
            .map(|x| decode_hex(x.as_str().ok_or("bad trie value")?))
            .collect::<Result<Vec<_>, _>>()?;
        compare(
            &v["id"],
            "ordered root",
            json!(encode_hex(&trie::ordered_root(&values)?)),
            &v["root"],
        );
    }
    for v in array(corpus, "state_tries")? {
        let entries = array(v, "entries")?
            .iter()
            .map(|e| Ok((raw(e, "key")?, raw(e, "value")?)))
            .collect::<Result<Vec<_>, String>>()?;
        let root = if v["secure"] == true {
            trie::secure_root(&entries)?
        } else {
            trie::key_value_root(&entries)?
        };
        compare(
            &v["id"],
            "state trie root",
            json!(encode_hex(&root)),
            &v["root"],
        );
    }
    for v in array(corpus, "body_headers")? {
        let header = BodyHeader::decode(&raw(v, "input")?)?;
        compare(
            &v["id"],
            "canonical",
            json!(encode_hex(&header.encode())),
            &v["canonical"],
        );
        compare(
            &v["id"],
            "hash",
            json!(encode_hex(&header.hash())),
            &v["hash"],
        );
    }
    for v in array(corpus, "work_objects")? {
        let work = match WorkObject::decode(&raw(v, "input")?, schedule) {
            Ok(w) => w,
            Err(e) => {
                compare(&v["id"], "work decode", json!(e), &json!("accepted"));
                continue;
            }
        };
        compare(
            &v["id"],
            "canonical",
            json!(encode_hex(&work.encode())),
            &v["canonical"],
        );
        compare(
            &v["id"],
            "body_header_hash",
            json!(encode_hex(&work.header().hash())),
            &v["body_header_hash"],
        );
        compare(
            &v["id"],
            "transaction_root",
            json!(encode_hex(&work.transaction_root()?)),
            &v["transaction_root"],
        );
        compare(
            &v["id"],
            "outbound_root",
            json!(encode_hex(&work.outbound_root()?)),
            &v["outbound_root"],
        );
        compare(
            &v["id"],
            "uncle_root",
            json!(encode_hex(&work.uncle_root())),
            &v["uncle_root"],
        );
        compare(
            &v["id"],
            "manifest_root",
            json!(encode_hex(&work.manifest_root()?)),
            &v["manifest_root"],
        );
        compare(
            &v["id"],
            "interlink_root",
            json!(encode_hex(&work.interlink_root()?)),
            &v["interlink_root"],
        );
        compare(
            &v["id"],
            "commitments_valid",
            json!(work.verify_commitments(context(v)?).is_ok()),
            &v["commitments_valid"],
        );
        compare(
            &v["id"],
            "work idempotence",
            json!(WorkObject::decode(&work.encode(), schedule)?.encode() == work.encode()),
            &json!(true),
        );
    }
    for v in array(corpus, "invalid_commitments")? {
        let work = WorkObject::decode(&raw(v, "input")?, schedule)?;
        compare(
            &v["id"],
            "commitments_valid",
            json!(work.verify_commitments(context(v)?).is_ok()),
            &json!(false),
        );
    }
    for v in array(corpus, "rejected")? {
        let bytes = raw(v, "input")?;
        let accepted = match v["kind"].as_str() {
            Some("header") => BodyHeader::decode(&bytes).is_ok(),
            Some("work") => WorkObject::decode(&bytes, schedule).is_ok(),
            _ => return Err("unknown rejection kind".into()),
        };
        compare(&v["id"], "invalid accepted", json!(accepted), &json!(false));
    }
    let mut receipts = Vec::new();
    for v in array(corpus, "receipts")? {
        let logs = array(v, "logs")?
            .iter()
            .map(|l| {
                Ok(Log {
                    address: raw(l, "address")?
                        .try_into()
                        .map_err(|_| "bad log address")?,
                    topics: array(l, "topics")?
                        .iter()
                        .map(|h| {
                            decode_hex(h.as_str().ok_or("bad topic")?)?
                                .try_into()
                                .map_err(|_| "bad topic length".into())
                        })
                        .collect::<Result<Vec<_>, String>>()?,
                    data: raw(l, "data")?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let receipt = Receipt {
            transaction_type: u8::try_from(v["type"].as_u64().ok_or("bad receipt type")?)
                .map_err(|_| "bad receipt type")?,
            post_state: raw(v, "post_state")?,
            status: v["status"].as_u64().ok_or("bad status")?,
            cumulative_gas_used: v["cumulative_gas_used"]
                .as_u64()
                .ok_or("bad cumulative gas")?,
            bloom: receipt::logs_bloom(&logs),
            logs,
            outbound_etxs: array(v, "outbound")?
                .iter()
                .map(|x| Transaction::decode(&decode_hex(x.as_str().ok_or("bad outbound")?)?))
                .collect::<Result<Vec<_>, String>>()?,
        };
        compare(
            &v["id"],
            "bloom",
            json!(encode_hex(&receipt.bloom)),
            &v["bloom"],
        );
        compare(
            &v["id"],
            "typed_rlp",
            json!(encode_hex(&receipt.typed_rlp()?)),
            &v["typed_rlp"],
        );
        receipts.push(receipt);
    }
    for v in array(corpus, "receipt_roots")? {
        let count = usize::try_from(v["count"].as_u64().ok_or("bad receipt count")?)
            .map_err(|_| "bad receipt count")?;
        compare(
            &v["count"],
            "receipt root",
            json!(encode_hex(&receipt::receipts_root(
                receipts.get(..count).ok_or("bad receipt range")?
            )?)),
            &v["root"],
        );
    }
    Ok(
        json!({"passed":failures.is_empty(),"checks":checks,"schema_vectors":array(corpus,"schema_vectors")?.len(),"body_headers":array(corpus,"body_headers")?.len(),"work_objects":array(corpus,"work_objects")?.len(),"commitment_rejections":array(corpus,"invalid_commitments")?.len(),"rejected_inputs":array(corpus,"rejected")?.len(),"trie_vectors":array(corpus,"trie_vectors")?.len(),"state_trie_vectors":array(corpus,"state_tries")?.len(),"receipts":receipts.len(),"receipt_roots":array(corpus,"receipt_roots")?.len(),"failures":failures,"runtime":"Rust only","full_block_validation":false,"state_execution":false,"native_libp2p_sync":false}),
    )
}
