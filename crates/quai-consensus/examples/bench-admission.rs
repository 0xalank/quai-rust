//! One pass over distinct native headers. Epoch construction and decoding are untimed.
use quai_consensus::{
    history::{History, Record},
    placement::{self, CandidateIndex},
    pow::{Algorithm, LightCache},
    verifier::Verifier,
    zone_admission,
};
use quai_primitives::{decode_hex, encode_hex};
use serde_json::{json, Value};
fn record(v: &Value) -> Record {
    Record::decode(&decode_hex(v.as_str().unwrap()).unwrap()).unwrap()
}
fn main() {
    let c: Value = serde_json::from_slice(
        &std::fs::read(std::env::args().nth(1).expect("frozen admission corpus")).unwrap(),
    )
    .unwrap();
    assert_eq!(c["reference_revision"], quai_primitives::REFERENCE_REVISION);
    let p = zone_admission::profile().unwrap();
    let genesis = record(&c["genesis"]);
    let mut history = History::default();
    let mut index = CandidateIndex::default();
    index
        .insert_imported(
            genesis.id(),
            placement::bootstrap_candidate(&genesis, &p).unwrap(),
        )
        .unwrap();
    history.insert(genesis).unwrap();
    let rows = c["branches"][0]["vectors"].as_array().unwrap();
    let records = rows
        .iter()
        .map(|r| record(&r["work_object"]))
        .collect::<Vec<_>>();
    let mut v = Verifier::with_cache(LightCache::new(Algorithm::Progpow, 0).unwrap());
    let now = c["now"].as_u64().unwrap();
    let mut snapshots = Vec::with_capacity(records.len());
    let start = std::time::Instant::now();
    for b in &records {
        let prepared = placement::prepare_append_components(
            &mut v,
            std::hint::black_box(&history),
            &index,
            std::hint::black_box(b),
            &p,
            now,
            false,
            [0; 32],
        )
        .unwrap();
        assert!(!prepared.fully_validated);
        snapshots.push(prepared.next.clone());
        index.insert_imported(b.id(), prepared.next).unwrap();
        history.insert(b.clone()).unwrap();
    }
    let ns = start.elapsed().as_nanos();
    for ((b, row), snapshot) in records.iter().zip(rows).zip(snapshots.iter()) {
        assert_eq!(
            encode_hex(&v.verify_header_proof(b.header()).unwrap().hash()),
            row["work_hash"]
        );
        assert_eq!(
            json!(snapshot
                .manifest
                .as_ref()
                .unwrap()
                .iter()
                .map(|h| encode_hex(h))
                .collect::<Vec<_>>()),
            row["manifest"]
        );
    }
    println!(
        "{}",
        json!({"headers":records.len(),"final_id":encode_hex(&records.last().unwrap().id()),"manifest_length":snapshots.last().unwrap().manifest.as_ref().unwrap().len(),"ns_per_header":ns/records.len()as u128,"full_consensus":false,"scope":"Sixteen distinct local-zone header/body/manifest/PCRC preparations in order, including history/index insertion; native epoch and input decoding excluded. Execution/disk/network excluded."})
    );
}
