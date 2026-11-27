#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let mut parts = text.splitn(2, '\n');
        let user = parts.next().unwrap_or("");
        let pass = parts.next().unwrap_or("");
        let _ = quai_node::stratum::server::parse_auth_for_fuzz(user, pass, &[0, 0]);
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
            let _ = val.get("method").and_then(|m| m.as_str());
            let _ = val.get("params").and_then(|p| p.as_array());
        }
    }
});
