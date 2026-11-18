use prost::Message;
use quai_consensus::{
    auxiliary::{Auxiliary, Quorum},
    verifier::{Verifier, PINNED_SCHEDULE},
};
use quai_primitives::{auxiliary, decode_hex, encode_hex, Header};
use serde_json::{json, Value};
use std::collections::BTreeMap;
fn raw(v: &Value, k: &str) -> Result<Vec<u8>, String> {
    decode_hex(v[k].as_str().ok_or_else(|| format!("missing {k}"))?)
}
fn hash(v: &Value, k: &str) -> Result<[u8; 32], String> {
    raw(v, k)?.try_into().map_err(|_| format!("bad hash {k}"))
}
pub fn verify(corpus: &Value, proofs: bool) -> Result<Value, String> {
    if corpus["schema"] != 1
        || corpus["kind"] != "auxiliary-admission"
        || corpus["reference_revision"] != quai_primitives::REFERENCE_REVISION
    {
        return Err("wrong corpus/reference".into());
    }
    let mut checks = 0usize;
    let mut mismatches = 0usize;
    let mut failures = vec![];
    let mut compare = |id: &Value, field: &str, actual: Value, expected: &Value| {
        checks += 1;
        if &actual != expected {
            mismatches += 1;
            if failures.len() < 100 {
                failures.push(json!({"id":id,"field":field,"actual":actual,"expected":expected}));
            }
        }
    };
    let mut profiles = BTreeMap::new();
    for v in corpus["profiles"].as_array().ok_or("missing profiles")? {
        let keys = v["keys"]
            .as_array()
            .ok_or("missing keys")?
            .iter()
            .map(|v| {
                decode_hex(&format!("0x{}", v.as_str().ok_or("key string")?))?
                    .try_into()
                    .map_err(|_| "key width".into())
            })
            .collect::<Result<Vec<_>, String>>()?
            .try_into()
            .map_err(|_| "quorum key count")?;
        let quorum = Quorum::from_trusted_keys(keys).map_err(|e| e.to_string())?;
        compare(
            &v["id"],
            "all ordered aggregate keys",
            json!(quorum
                .aggregate_keys()
                .iter()
                .map(|k| encode_hex(k))
                .collect::<Vec<_>>()),
            &v["aggregate"],
        );
        if v["id"] == "pinned" {
            compare(
                &v["id"],
                "runtime pinned key set",
                json!(quorum.keys() == Quorum::pinned().keys()),
                &json!(true),
            );
        }
        profiles.insert(v["id"].as_str().ok_or("profile")?.to_string(), quorum);
    }
    for v in corpus["codecs"].as_array().ok_or("aux codecs")? {
        let decoded = auxiliary::canonical_pow(&raw(v, "input")?);
        compare(
            &v["id"],
            "aux codec decode",
            json!(decoded.is_ok()),
            &v["decoded"],
        );
        if let Ok(p) = decoded {
            compare(
                &v["id"],
                "canonical aux codec",
                json!(encode_hex(&p.encode_to_vec())),
                &v["canonical"],
            );
        }
    }
    let mut template_valid = 0usize;
    let mut templates = 0usize;
    for v in corpus["templates"].as_array().ok_or("templates")? {
        let decoded = auxiliary::canonical_template(&raw(v, "input")?);
        compare(
            &v["id"],
            "template decoded",
            json!(decoded.is_ok()),
            &v["decoded"],
        );
        templates += 1;
        if let Ok(t) = decoded {
            let quorum = profiles
                .get(v["profile"].as_str().ok_or("profile")?)
                .ok_or("unknown profile")?;
            let h = auxiliary::template_hash(&t);
            let signature = quorum.verify(h, t.sigs.as_deref().unwrap_or_default());
            compare(
                &v["id"],
                "canonical template",
                json!(encode_hex(&t.encode_to_vec())),
                &v["canonical"],
            );
            compare(
                &v["id"],
                "signing bytes",
                json!(encode_hex(&auxiliary::template_signing_bytes(&t))),
                &v["signing"],
            );
            compare(&v["id"], "template hash", json!(encode_hex(&h)), &v["hash"]);
            compare(
                &v["id"],
                "quorum signature",
                json!(signature),
                &v["signature_valid"],
            );
            if signature {
                template_valid += 1;
            }
        }
    }
    let mut bindings = 0;
    let mut binding_valid = 0;
    let mut pinned_valid = 0;
    for v in corpus["auxiliaries"].as_array().ok_or("auxiliaries")? {
        let aux = Auxiliary::decode(&raw(v, "aux")?).map_err(|e| e.to_string())?;
        let quorum = profiles
            .get(v["profile"].as_str().ok_or("profile")?)
            .ok_or("unknown profile")?;
        let result = aux.admit(
            hash(v, "quai_seal")?,
            v["quai_time"].as_u64().ok_or("time")?,
            quorum,
        );
        compare(
            &v["id"],
            "signed auxiliary bindings",
            json!(result.is_ok()),
            &v["valid"],
        );
        bindings += 1;
        if result.is_ok() {
            binding_valid += 1;
            if v["profile"] == "pinned" {
                pinned_valid += 1;
            }
        }
        let header = Header::decode_auxiliary(&raw(v, "quai_header")?, PINNED_SCHEDULE)?;
        let mut verifier = Verifier::with_trusted_quorum(quorum.clone());
        compare(
            &v["id"],
            "workshare role binding",
            json!(verifier.verify_auxiliary_components(&header, true).is_ok()),
            &json!(v["valid"] == true && v["share_role"] == true),
        );
        compare(
            &v["id"],
            "block role binding",
            json!(verifier.verify_auxiliary_components(&header, false).is_ok()),
            &json!(v["valid"] == true && v["block_role"] == true),
        );

        compare(
            &v["id"],
            "canonical auxiliary block hash",
            json!(encode_hex(&header.block_hash()?)),
            &v["block_hash"],
        );
        compare(
            &v["id"],
            "Quai seal hash",
            json!(encode_hex(&header.seal_hash())),
            &v["quai_seal"],
        );
        if let Ok(template) = aux.template() {
            compare(
                &v["id"],
                "extracted template",
                json!(encode_hex(&template.encode_to_vec())),
                &v["canonical_template"],
            );
            compare(
                &v["id"],
                "extracted template hash",
                json!(encode_hex(&auxiliary::template_hash(&template))),
                &v["template_hash"],
            );
        } else if v["valid"] == true {
            compare(
                &v["id"],
                "valid auxiliary template extraction",
                json!(false),
                &json!(true),
            );
        }
    }
    let mut goldens = 0usize;
    if proofs {
        for v in corpus["golden"].as_array().ok_or("golden")? {
            let quorum = profiles
                .get(v["profile"].as_str().ok_or("profile")?)
                .ok_or("unknown profile")?
                .clone();
            let mut verifier = Verifier::with_trusted_quorum(quorum);
            let h = Header::decode_auxiliary(&raw(v, "quai_header")?, PINNED_SCHEDULE)?;
            let proof = verifier
                .verify_header_proof(&h)
                .map_err(|e| format!("{}: {e}", v["id"]))?;
            compare(
                &v["id"],
                "integrated KAWPOW proof",
                json!(encode_hex(&proof.hash())),
                &v["pow_hash"],
            );
            let object = raw(v, "work_object")?;
            let report = verifier
                .inspect_components(&object, quai_primitives::work::Context::Zone)
                .map_err(|e| format!("{} object: {e}", v["id"]))?;
            compare(
                &v["id"],
                "signed auxiliary work-object proof",
                json!(encode_hex(&report.proof.hash())),
                &v["pow_hash"],
            );
            compare(
                &v["id"],
                "no full consensus authority",
                json!(report.fully_validated),
                &json!(false),
            );
            let mut p = h.wire().clone();
            let mut aux = quai_primitives::schema::block::ProtoAuxPow::decode(
                p.aux_pow.as_ref().unwrap().as_slice(),
            )
            .unwrap();
            aux.header.as_mut().unwrap()[88] ^= 1;
            p.aux_pow = Some(aux.encode_to_vec());
            let bad = Header::decode_auxiliary(&p.encode_to_vec(), PINNED_SCHEDULE)?;
            compare(
                &v["id"],
                "altered donor mix rejected",
                json!(verifier.verify_header_proof(&bad).is_err()),
                &json!(true),
            );
            let mut p = h.wire().clone();
            let mut aux = quai_primitives::schema::block::ProtoAuxPow::decode(
                p.aux_pow.as_ref().unwrap().as_slice(),
            )
            .unwrap();
            aux.signature.as_mut().unwrap()[0] ^= 1;
            p.aux_pow = Some(aux.encode_to_vec());
            let bad = Header::decode_auxiliary(&p.encode_to_vec(), PINNED_SCHEDULE)?;
            compare(
                &v["id"],
                "altered quorum rejected",
                json!(verifier.verify_header_proof(&bad).is_err()),
                &json!(true),
            );
            if v["profile"] == "lab" {
                let mut production = Verifier::default();
                compare(
                    &v["id"],
                    "lab signatures rejected by pinned verifier",
                    json!(production.verify_header_proof(&h).is_err()),
                    &json!(true),
                );
            }
            goldens += 1;
        }
    }
    Ok(
        json!({"passed":mismatches==0,"checks":checks,"mismatches":mismatches,"failures":failures,"template_cases":templates,"codec_cases":corpus["codecs"].as_array().unwrap().len(),"valid_template_signatures":template_valid,"binding_cases":bindings,"valid_bindings":binding_valid,"valid_pinned_bindings":pinned_valid,"full_kawpow_cases":goldens,"full_consensus":false,"full_workshare_admission":false,"scope":"Canonical signed templates, 2-of-3 MuSig2 quorum, coinbase/donor Merkle/Quai seal/time and scrypt merged commitments; optional complete native KAWPOW proof binding. No full header/uncle/economic/canonical validity."}),
    )
}
