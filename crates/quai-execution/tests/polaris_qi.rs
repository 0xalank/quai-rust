use quai_execution::qi::{self, OutPoint, Utxo, UtxoView};
use quai_primitives::{decode_hex, transaction::Transaction};
use std::cell::Cell;
struct NoReads(Cell<usize>);
impl UtxoView for NoReads {
    fn get(&self, _: &OutPoint) -> Result<Option<std::borrow::Cow<'_, Utxo>>, qi::ReadError> {
        self.0.set(self.0.get() + 1);
        Ok(None)
    }
}
#[test]
fn duplicate_qi_inputs_are_rejected_before_utxo_lookup() {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/foundation.json")).unwrap();
    let row = v["execution"]["qi_inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["unique_inputs"] == false)
        .unwrap();
    let tx =
        Transaction::decode(&decode_hex(row["transaction"].as_str().unwrap()).unwrap()).unwrap();
    let context = qi::Context {
        chain_id: decode_hex(row["chain_id"].as_str().unwrap()).unwrap(),
        block_height: row["height"].as_u64().unwrap(),
        location: [0, 0],
    };
    let view = NoReads(Cell::new(0));
    assert_eq!(
        qi::check_input_values(&tx, &view, &context)
            .unwrap_err()
            .to_string(),
        "duplicate Qi input"
    );
    assert_eq!(view.0.get(), 0);
}
