use prost::Message;
use quai_primitives::{
    decode_hex, encode_hex,
    transaction::{self, Kind, Transaction},
    wire::{tx as w, BytesValue},
};

fn corpus() -> serde_json::Value {
    serde_json::from_slice(include_bytes!(
        "../../../fixtures/protocol/transactions.json"
    ))
    .unwrap()
}
#[test]
fn frozen_go_transaction_vectors_match() {
    let report = quai_primitives::vectors::verify(include_bytes!(
        "../../../fixtures/protocol/transactions.json"
    ))
    .unwrap();
    assert_eq!(report["passed"], true, "{report}");
    assert_eq!(report["transaction_vectors"], 288);
}
#[test]
fn commitments_bind_data_and_exclude_work_from_signing() {
    for kind in [0, 2] {
        let c = corpus();
        let v = c["transactions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| {
                v["type"] == kind
                    && if kind == 0 {
                        v["sender"].is_string()
                    } else {
                        v["signature_valid"] == true
                    }
            })
            .unwrap();
        let tx = Transaction::decode(&decode_hex(v["input"].as_str().unwrap()).unwrap()).unwrap();
        let mut wire = w::Transaction::decode(tx.encode().as_slice()).unwrap();
        wire.work_nonce = Some(wire.work_nonce.unwrap_or(0).wrapping_add(1));
        wire.parent_hash = Some(BytesValue {
            value: vec![17; 32],
        });
        let changed = Transaction::decode(&wire.encode_to_vec()).unwrap();
        assert_eq!(tx.signing_hash(), changed.signing_hash());
        assert_ne!(
            tx.id(Some([0, 0])).unwrap(),
            changed.id(Some([0, 0])).unwrap()
        );
        if kind == 2 {
            assert!(changed.verify_qi_signature().unwrap());
        }
        wire.data.as_mut().unwrap().push(19);
        let tampered = Transaction::decode(&wire.encode_to_vec()).unwrap();
        assert_ne!(tx.signing_hash(), tampered.signing_hash());
        if kind == 0 {
            assert_ne!(tx.sender().unwrap(), tampered.sender().unwrap());
        } else {
            assert!(!tampered.verify_qi_signature().unwrap());
        }
    }
}
#[test]
fn routing_bits_follow_ledger_and_origin_rules() {
    for v in corpus()["transactions"].as_array().unwrap() {
        let tx = Transaction::decode(&decode_hex(v["input"].as_str().unwrap()).unwrap()).unwrap();
        let id = tx.id(Some([1, 2])).unwrap();
        match tx.kind() {
            Kind::Quai(_) => {
                assert_eq!((id[0], id[2]), (0x12, 0x12));
                assert_eq!(id[1] & 0x80, 0);
                assert_eq!(id[3] & 0x80, 0);
            }
            Kind::Qi(q) => {
                assert_eq!(id[0], q.inputs[0].hash[2]);
                assert_eq!(id[2], id[0]);
                assert_ne!(id[1] & 0x80, 0);
                assert_ne!(id[3] & 0x80, 0);
            }
            Kind::External(e) => {
                assert_eq!(id[0], e.originating_hash[2]);
                assert_eq!(id[2], e.to[0]);
                assert_eq!(id[1] & 0x80, e.sender[1] & 0x80);
                assert_eq!(id[3] & 0x80, e.to[1] & 0x80);
            }
        }
    }
}
#[test]
fn malformed_and_out_of_domain_input_is_rejected() {
    for bytes in [vec![], vec![255], vec![0; 1024 * 1024 + 1]] {
        assert!(Transaction::decode(&bytes).is_err());
    }
    let c = corpus();
    let v = &c["transactions"][1];
    let mut p =
        w::Transaction::decode(decode_hex(v["input"].as_str().unwrap()).unwrap().as_slice())
            .unwrap();
    p.data = Some(vec![0; 128 * 1024 + 1]);
    assert!(Transaction::decode(&p.encode_to_vec()).is_err());
    p.data = Some(vec![]);
    p.chain_id = Some(vec![1; 33]);
    assert!(Transaction::decode(&p.encode_to_vec()).is_err());
    assert_eq!(
        encode_hex(&transaction::keccak(&[])),
        "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
    );
}
