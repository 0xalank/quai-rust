use prost::Message;
use quai_execution::{checkpoint, early_replay::EarlyReplay};
use quai_primitives::{
    decode_hex, encode_hex,
    schema::{block::ProtoWorkObject, common::ProtoHash},
    work::{BodyHeader, WorkObject},
    Schedule,
};
use serde_json::{json, Value};
pub fn verify(corpus: &Value) -> Value {
    assert_eq!(
        corpus["reference_revision"],
        quai_primitives::REFERENCE_REVISION
    );
    let schedule = Schedule {
        kawpow_fork: 1_171_500,
        transition_period: 129600,
    };
    let genesis = WorkObject::decode(
        &decode_hex(corpus["genesis"].as_str().unwrap()).unwrap(),
        schedule,
    )
    .unwrap();
    let g = genesis.work_header(schedule).unwrap();
    let id = g.block_hash().unwrap();
    assert_eq!(
        encode_hex(&id),
        "0xaff962ff2366b9d1efd767e7f1406aa080fc18e267ea87614e3b7655f24ceb55"
    );
    let mut state = EarlyReplay::empty(id, g.wire().time.unwrap());
    let mut failures = vec![];
    let mut checks = 1;
    for row in corpus["vectors"].as_array().unwrap() {
        let wire = decode_hex(row["work_object"].as_str().unwrap()).unwrap();
        let object = WorkObject::decode(&wire, schedule).unwrap();
        let before = checkpoint::encode(&state).unwrap();
        // A consistent body-header binding with a dishonest gas counter must fail
        // during replay and must never publish partially mutated transaction state.
        let mut malformed = ProtoWorkObject::decode(wire.as_slice()).unwrap();
        let body = malformed.wo_body.as_mut().unwrap().header.as_mut().unwrap();
        body.gas_used = Some(body.gas_used.unwrap() + 1);
        let header = BodyHeader::decode(&body.encode_to_vec()).unwrap();
        malformed.wo_header.as_mut().unwrap().header_hash = Some(ProtoHash {
            value: header.hash().to_vec(),
        });
        let bad = WorkObject::decode(&malformed.encode_to_vec(), schedule).unwrap();
        assert!(state.apply(&bad).is_err());
        assert_eq!(checkpoint::encode(&state).unwrap(), before);
        checks += 2;
        match state.apply(&object) {
            Err(e) => failures.push(json!({"id":row["id"],"error":e.to_string()})),
            Ok(out) => {
                let mut cmp = |name: &str, v: Value| {
                    checks += 1;
                    if row[name] != v {
                        failures.push(
                            json!({"id":row["id"],"field":name,"expected":row[name],"actual":v}),
                        );
                    }
                };
                cmp("block_hash", json!(encode_hex(&state.head())));
                cmp("evm_root", json!(encode_hex(&out.evm_root)));
                cmp("etx_root", json!(encode_hex(&out.etx_root)));
                cmp("utxo_root", json!(encode_hex(&out.utxo_root)));
                cmp("gas_used", json!(out.gas_used));
                cmp("state_used", json!(out.state_used));
                cmp("quai_state_size", json!(state.world().account_count()));
                cmp(
                    "receipt_rlp",
                    json!(out
                        .receipts
                        .iter()
                        .map(|r| encode_hex(&r.typed_rlp().unwrap()))
                        .collect::<Vec<_>>()),
                );
            }
        }
        let bytes = checkpoint::encode(&state).unwrap();
        state = checkpoint::decode(&bytes, id).unwrap();
        let mut broken = bytes.clone();
        broken.push(0);
        assert!(checkpoint::decode(&broken, id).is_err());
        assert!(checkpoint::decode(&bytes, [99; 32]).is_err());
        checks += 3;
    }
    let out = json!({"passed":failures.is_empty(),"blocks":corpus["vectors"].as_array().unwrap().len(),"checks":checks,"failures":failures,"reference_revision":quai_primitives::REFERENCE_REVISION,"actual_go_state_processor":true,"header_consensus_admission":false,"full_block_replay":false,"public_network":false});
    out
}
fn main() {
    let path = std::env::args().nth(1).expect("early-blocks corpus");
    let corpus: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let result = verify(&corpus);
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    if result["passed"] != true {
        std::process::exit(1);
    }
}
