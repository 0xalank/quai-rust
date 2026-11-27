#![no_main]
use libfuzzer_sys::fuzz_target;
use quai_primitives::transaction::Transaction;

fuzz_target!(|data: &[u8]| {
    let _ = quai_primitives::schema::preflight(".block.ProtoTransaction", data);
    if let Ok(tx) = Transaction::decode(data) {
        let _ = tx.signing_hash();
        let _ = tx.sender();
        let _ = tx.id(Some([0, 0]));
        let _ = tx.verify_qi_signature();
    }
});
