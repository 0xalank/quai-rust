#![no_main]
use libfuzzer_sys::fuzz_target;
use quai_consensus::{auxiliary::Auxiliary, verifier::PINNED_SCHEDULE};
use quai_primitives::work::WorkObject;

fuzz_target!(|data: &[u8]| {
    let _ = quai_primitives::schema::preflight(".block.ProtoWorkObject", data);
    if let Ok(wo) = WorkObject::decode_auxiliary(data, PINNED_SCHEDULE) {
        let _ = wo.auxiliary_work_header(PINNED_SCHEDULE);
    }
    let _ = Auxiliary::decode(data);
});
