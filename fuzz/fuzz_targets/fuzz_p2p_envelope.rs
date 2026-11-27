#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(env) = quai_network::decode_envelope(data) {
        let _ = quai_network::response(env);
    }
    let _ = quai_primitives::schema::preflight(".quaiprotocol.QuaiMessage", data);
});
