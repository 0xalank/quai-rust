use crypto_bigint::{CheckedAdd, CheckedMul};
use quai_consensus::{cubic_discount, rules::Uint};
use serde_json::{json, Value};
fn number(value: &Value) -> Uint {
    let text = value.as_str().unwrap();
    let mut n = Uint::ZERO;
    for digit in text.bytes() {
        assert!(digit.is_ascii_digit());
        n = Option::<Uint>::from(n.checked_mul(&Uint::from_u64(10))).expect("input magnitude");
        n = Option::<Uint>::from(n.checked_add(&Uint::from_u64((digit - b'0') as u64)))
            .expect("input magnitude");
    }
    n
}
pub fn verify(value: &Value) -> Value {
    assert_eq!(
        value["reference_revision"],
        quai_primitives::REFERENCE_REVISION
    );
    let mut families = [0usize; 16];
    for row in value["vectors"].as_array().unwrap() {
        let result = cubic_discount::apply(number(&row["value"]), number(&row["mean"]));
        assert_eq!(
            result.unwrap_or_else(|e| panic!("case {}: {e:?}", row["case"])),
            number(&row["expected"]),
            "case {}",
            row["case"]
        );
        families[row["family"].as_u64().unwrap() as usize] += 1;
    }
    assert_eq!(families, [256; 16]);
    json!({"passed":true,"go_comparisons":4096,"families":families.to_vec(),"canonical_authority":false,"full_consensus":false})
}
