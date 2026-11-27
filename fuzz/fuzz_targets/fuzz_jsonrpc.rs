#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(data) {
        let _ = val.get("method").and_then(|m| m.as_str());
        let _ = val.get("params");
        let _ = val.get("id");
    }
});
