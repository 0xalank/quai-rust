use prost::Message;
use quai_primitives::{
    decode_hex, schema,
    work::{BodyHeader, Context, WorkObject},
    Schedule,
};
fn corpus() -> serde_json::Value {
    serde_json::from_slice(include_bytes!("../../../fixtures/protocol/foundation.json")).unwrap()
}
#[test]
fn frozen_go_complete_schema_and_commitment_vectors() {
    let report = quai_primitives::vectors::verify(include_bytes!(
        "../../../fixtures/protocol/foundation.json"
    ))
    .unwrap();
    assert_eq!(report["passed"], true, "{report}");
    assert_eq!(report["schema_vectors"], 536);
}
#[test]
fn each_zone_commitment_is_checked_with_rebound_header() {
    let c = corpus();
    let schedule: Schedule = serde_json::from_value(c["schedule"].clone()).unwrap();
    let v = c["work_objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["context"] == "zone" && v["commitments_valid"] == true)
        .unwrap();
    let raw = decode_hex(v["canonical"].as_str().unwrap()).unwrap();
    for field in ["tx", "outbound", "uncle"] {
        let mut wire = schema::block::ProtoWorkObject::decode(raw.as_slice()).unwrap();
        let header = wire.wo_body.as_mut().unwrap().header.as_mut().unwrap();
        let value = match field {
            "tx" => &mut header.tx_hash,
            "outbound" => &mut header.outbound_etx_hash,
            _ => &mut header.uncle_hash,
        };
        value.as_mut().unwrap().value[0] ^= 1;
        let header = BodyHeader::decode(&header.encode_to_vec()).unwrap();
        wire.wo_header
            .as_mut()
            .unwrap()
            .header_hash
            .as_mut()
            .unwrap()
            .value = header.hash().to_vec();
        let work = WorkObject::decode(&wire.encode_to_vec(), schedule).unwrap();
        assert!(work.verify_commitments(Context::Zone).is_err());
    }
}
#[test]
fn secure_and_plain_snapshots_reject_duplicate_or_empty_values() {
    for secure in [false, true] {
        let root = if secure {
            quai_primitives::trie::secure_root
        } else {
            quai_primitives::trie::key_value_root
        };
        assert!(root(&[(vec![1], vec![2]), (vec![1], vec![3])]).is_err());
        assert!(root(&[(vec![1], vec![])]).is_err());
    }
}
#[test]
fn work_object_transaction_list_has_no_count_or_node_cap() {
    // go-quai bounds the list only by block gas and the 3 MiB message size.
    // 4,097 ETXs also carry more than the former 16,384 sub-message budget.
    let c = corpus();
    let schedule: Schedule = serde_json::from_value(c["schedule"].clone()).unwrap();
    let v = c["work_objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["context"] == "zone")
        .unwrap();
    let raw = decode_hex(v["canonical"].as_str().unwrap()).unwrap();
    let mut wire = schema::block::ProtoWorkObject::decode(raw.as_slice()).unwrap();
    let txs: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../fixtures/protocol/transactions.json"
    ))
    .unwrap();
    let etx = txs["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["type"] == 1 && v["id"] == "type-1-000")
        .unwrap();
    let etx = schema::block::ProtoTransaction::decode(
        decode_hex(etx["canonical"].as_str().unwrap())
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    let body = wire.wo_body.as_mut().unwrap();
    body.transactions.get_or_insert_default().transactions = vec![etx; 4097];
    let bytes = wire.encode_to_vec();
    assert!(bytes.len() <= schema::MAX_MESSAGE_BYTES);
    let work = WorkObject::decode(&bytes, schedule).unwrap();
    assert_eq!(work.transactions().len(), 4097);
    work.transaction_root().unwrap();
}
