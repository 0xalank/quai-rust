use crypto_bigint::{CheckedAdd, CheckedMul, Encoding};
use quai_consensus::{controllers, rules::Uint};
use serde_json::{json, Value};
fn uint(v: &Value) -> Uint {
    let mut n = Uint::ZERO;
    for c in v.as_str().expect("decimal string").bytes() {
        assert!(c.is_ascii_digit());
        n = Option::<Uint>::from(n.checked_mul(&Uint::from_u64(10))).expect("input magnitude");
        n = Option::<Uint>::from(n.checked_add(&Uint::from_u64((c - b'0') as u64)))
            .expect("input magnitude");
    }
    n
}
pub fn verify(v: &Value) -> Value {
    assert_eq!(v["reference_revision"], quai_primitives::REFERENCE_REVISION);
    let mut checks = 0;
    for row in v["flow"].as_array().unwrap() {
        assert_eq!(
            controllers::conversion_flow(uint(&row["previous"]), uint(&row["current"])).unwrap(),
            uint(&row["expected"]),
            "flow {row}"
        );
        checks += 1;
    }
    for row in v["miner_difficulty"].as_array().unwrap() {
        assert_eq!(
            controllers::miner_difficulty(
                row["number"].as_u64().unwrap(),
                uint(&row["difficulty"]),
                uint(&row["previous"])
            )
            .unwrap(),
            uint(&row["expected"]),
            "difficulty {row}"
        );
        checks += 1;
    }
    for row in v["discounts"].as_array().unwrap() {
        assert_eq!(
            controllers::k_quai_discount(
                row["number"].as_u64().unwrap(),
                uint(&row["previous"]),
                Some(uint(&row["old_exchange"])),
                uint(&row["exchange"])
            )
            .unwrap(),
            uint(&row["expected"]),
            "discount {row}"
        );
        checks += 1;
    }
    for row in v["updates"].as_array().unwrap() {
        assert_eq!(
            controllers::calculate_k_quai(
                row["number"].as_u64().unwrap(),
                uint(&row["parent"]),
                uint(&row["miner"]),
                uint(&row["xb_star"])
            )
            .unwrap(),
            uint(&row["expected"]),
            "update {row}"
        );
        checks += 1;
    }
    for row in v["exchange_rates"].as_array().unwrap() {
        let mut choices = Box::new([uint(&row["choice_base"]); controllers::WINDOW as usize]);
        choices[controllers::WINDOW as usize - 1] = uint(&row["choice_last"]);
        assert_eq!(
            controllers::exchange_rate(
                row["number"].as_u64().unwrap(),
                uint(&row["parent"]),
                uint(&row["miner"]),
                &choices
            )
            .unwrap(),
            uint(&row["expected"]),
            "exchange {row}"
        );
        checks += 1;
    }
    let max = Uint::from_be_bytes([255; 32]);
    assert_eq!(controllers::conversion_flow(max, max).unwrap(), max);
    json!({"passed":true,"go_comparisons":checks,"flow_cases":v["flow"].as_array().unwrap().len(),"difficulty_cases":v["miner_difficulty"].as_array().unwrap().len(),"update_cases":v["updates"].as_array().unwrap().len(),"exchange_rate_cases":v["exchange_rates"].as_array().unwrap().len(),"discount_cases":v["discounts"].as_array().unwrap().len(),"canonical_authority":false,"full_consensus":false,"runtime_go_dependency":false})
}
