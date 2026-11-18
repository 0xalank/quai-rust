use prost::Message;
use quai_consensus::{
    history::{History, Record},
    verifier::Verifier,
    zone_execution::{self, Profile, State},
};
use quai_execution::{historical, qi::OutPoint};
use quai_primitives::{
    decode_hex, encode_hex, schema,
    transaction::{Kind, Transaction},
    work::BodyHeader,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
#[derive(Default)]
struct Past {
    incoming: BTreeMap<u64, Vec<Transaction>>,
    created: BTreeMap<u64, Vec<(OutPoint, u8)>>,
}
impl historical::History for Past {
    fn incoming_at(&self, h: u64) -> Result<Vec<Transaction>, quai_execution::settlement::Error> {
        self.incoming
            .get(&h)
            .cloned()
            .ok_or(quai_execution::settlement::Error::Invalid(
                "missing redemption history",
            ))
    }
    fn created_at(&self, h: u64) -> Result<Vec<(OutPoint, u8)>, quai_execution::settlement::Error> {
        Ok(self.created.get(&h).cloned().unwrap_or_default())
    }
}
fn parent_hash(block: &Record) -> [u8; 32] {
    block
        .header()
        .wire()
        .parent_hash
        .as_ref()
        .unwrap()
        .value
        .as_slice()
        .try_into()
        .unwrap()
}
fn raw(v: &Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
fn tamper(record: &Record, kind: usize) -> Record {
    let mut p = schema::block::ProtoWorkObject::decode(record.bytes()).unwrap();
    let b = p.wo_body.as_mut().unwrap().header.as_mut().unwrap();
    match kind {
        0 => b.gas_used = Some(b.gas_used.unwrap() + 1),
        1 => b.state_used = Some(b.state_used.unwrap() + 1),
        2 => b.quai_state_size = Some(vec![255]),
        3 => b.evm_root.as_mut().unwrap().value[0] ^= 1,
        4 => b.etx_set_root.as_mut().unwrap().value[0] ^= 1,
        5 => b.utxo_root.as_mut().unwrap().value[0] ^= 1,
        6 => b.receipt_hash.as_mut().unwrap().value[0] ^= 1,
        _ => unreachable!(),
    };
    let h = BodyHeader::decode(&b.encode_to_vec()).unwrap().hash();
    p.wo_header.as_mut().unwrap().header_hash =
        Some(schema::common::ProtoHash { value: h.to_vec() });
    Record::decode(&p.encode_to_vec()).unwrap()
}
pub fn verify(corpus: &Value) -> Value {
    assert_eq!(
        corpus["reference_revision"],
        quai_primitives::REFERENCE_REVISION
    );
    let profile = Profile::reviewed_private().unwrap();
    let mut history = History::default();
    let genesis = Record::decode(&raw(&corpus["genesis"])).unwrap();
    history.insert(genesis).unwrap();
    let mut state = State::default();
    let mut verifier = Verifier::default();
    let mut past = Past::default();
    let mut checks = 0;
    let mut failures = vec![];
    let mut rejected = 0;
    let rows = corpus["vectors"].as_array().unwrap();
    for row in rows {
        let block = Record::decode(&raw(&row["work_object"])).unwrap();
        let inbound = row["inbound"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| Transaction::decode(&raw(v)).unwrap())
            .collect::<Vec<_>>();
        match verifier.verify_header_proof(block.header()) {
            Ok(proof) => {
                checks += 1;
                if encode_hex(&proof.hash()) != row["work_hash"].as_str().unwrap() {
                    failures.push(json!({"id":row["id"],"field":"native work hash"}));
                }
            }
            Err(e) => {
                failures.push(json!({"id":row["id"],"proof_error":e.to_string()}));
                continue;
            }
        }
        for kind in 0..7 {
            let wrong = tamper(&block, kind);
            let before = state.clone();
            let result = zone_execution::native_components(
                &mut verifier,
                &mut state,
                &history,
                &past,
                &wrong,
                &profile,
                &inbound,
            );
            checks += 1;
            if result.is_ok() || state != before {
                failures.push(
                    json!({"id":row["id"],"field":"forgery rejection/atomicity","kind":kind}),
                );
            } else {
                rejected += 1;
            }
        }
        match zone_execution::native_components(
            &mut verifier,
            &mut state,
            &history,
            &past,
            &block,
            &profile,
            &inbound,
        ) {
            Err(e) => {
                failures.push(json!({"id":row["id"],"error":e.to_string()}));
                break;
            }
            Ok(result) => {
                let pairs = [
                    ("gas_used", json!(result.gas_used)),
                    ("state_used", json!(result.state_used)),
                    ("utxo_set_size", json!(state.utxos.consensus_size())),
                    ("quai_state_size", json!(state.world.account_count())),
                    ("evm_root", json!(encode_hex(&state.world.root().unwrap()))),
                    (
                        "etx_root",
                        json!(encode_hex(&state.world.etxs().root().unwrap())),
                    ),
                    ("utxo_root", json!(encode_hex(&state.utxos.root()))),
                    (
                        "receipt_rlp",
                        json!(result
                            .receipts
                            .iter()
                            .map(|r| encode_hex(&r.typed_rlp().unwrap()))
                            .collect::<Vec<_>>()),
                    ),
                    ("outbound_count", json!(result.outbound.len())),
                    ("unlock_count", json!(result.unlocks.len())),
                ];
                for (field, value) in pairs {
                    checks += 1;
                    if value != row[field] {
                        failures.push(json!({"id":row["id"],"field":field,"actual":value,"expected":row[field]}));
                    }
                }
                checks += 1;
                if result.fully_validated {
                    failures.push(json!({"id":row["id"],"field":"component boundary"}));
                }
                let profile_hash = quai_consensus::private_profile::PrivateChain::reviewed()
                    .unwrap()
                    .fingerprint;
                let anchor = quai_execution::state_image::Anchor {
                    profile: profile_hash,
                    block: block.id(),
                    parent: parent_hash(&block),
                    height: block.number(2).unwrap(),
                    timestamp: block.header().wire().time.unwrap(),
                };
                let (image, reference) =
                    quai_execution::state_image::encode(anchor.clone(), &state.world, &state.utxos)
                        .unwrap();
                let restored = quai_execution::state_image::decode(&image, &reference).unwrap();
                checks += 2;
                assert_eq!(
                    quai_execution::state_image::encode(anchor, &restored.world, &restored.utxos)
                        .unwrap()
                        .0,
                    image
                );
                state = State {
                    world: restored.world,
                    utxos: restored.utxos,
                };
                past.created
                    .insert(block.number(2).unwrap(), result.created_keys);
                past.incoming.insert(
                    block.number(2).unwrap(),
                    block
                        .object()
                        .transactions()
                        .iter()
                        .filter(|t| matches!(t.kind(), Kind::External(_)))
                        .cloned()
                        .collect(),
                );
                history.insert(block).unwrap();
            }
        }
    }
    json!({"passed":failures.is_empty(),"cases":rows.len(),"checks":checks,"forged_blocks_rejected":rejected,"full_consensus":false,"canonical_sync":false,"failures":failures,"scope":"Whole Process replay with real native proof kernels, payouts and incoming coinbases; full hierarchy/header admission remains separate."})
}
fn main() {
    let p = std::env::args().nth(1).expect("zone block corpus");
    let v: Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
    let result = verify(&v);
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    if result["passed"] != true {
        std::process::exit(1);
    }
}
