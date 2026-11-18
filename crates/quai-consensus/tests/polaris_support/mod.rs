use quai_consensus::{history::Record, placement, Error};
use quai_primitives::{decode_hex, encode_hex, Header};
use serde_json::{json, Value};
pub fn check(v: &Value) -> Value {
    let mut checks = 0;
    let mut failures = vec![];
    let mut check = |id: Value, field: &str, yes: bool| {
        checks += 1;
        if !yes && failures.len() < 30 {
            failures.push(json!({"id":id,"field":field}));
        }
    };
    for (i, row) in v["bindings"].as_array().unwrap().iter().enumerate() {
        let r = Record::decode(&decode_hex(row["block"].as_str().unwrap()).unwrap()).unwrap();
        check(
            json!(i),
            "binding policy",
            r.verify_binding().is_ok() == row["binding_valid"].as_bool().unwrap(),
        );
        let hash = decode_hex(row["pow_hash"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let order = placement::order_projection(&r, hash, 2, row["zero"].as_bool().unwrap());
        check(
            json!(i),
            "bound controlled order outcome",
            order.is_ok() == row["order_valid"].as_bool().unwrap(),
        );
        if let Ok(order) = order {
            check(
                json!(i),
                "bound controlled order",
                json!(order.order) == row["order"],
            );
        }
    }
    for (i, row) in v["identities"].as_array().unwrap().iter().enumerate() {
        let h = Header::decode_auxiliary(
            &decode_hex(row["header"].as_str().unwrap()).unwrap(),
            quai_consensus::verifier::PINNED_SCHEDULE,
        )
        .unwrap();
        check(
            json!(i),
            "proof identity",
            encode_hex(&h.pow_identity().unwrap()) == row["identity"].as_str().unwrap(),
        );
        check(
            json!(i),
            "encoded header hash",
            encode_hex(&h.block_hash().unwrap()) == row["hash"].as_str().unwrap(),
        );
    }

    for (i, row) in v["inclusions"].as_array().unwrap().iter().enumerate() {
        let mut history = quai_consensus::history::History::default();
        for b in row["records"].as_array().unwrap() {
            history
                .insert(Record::decode(&decode_hex(b.as_str().unwrap()).unwrap()).unwrap())
                .unwrap();
        }
        let b = Record::decode(&decode_hex(row["block"].as_str().unwrap()).unwrap()).unwrap();
        let headers = b.uncles().unwrap();
        let hashes = headers
            .iter()
            .map(|h| {
                h.wire()
                    .mix_hash
                    .as_ref()
                    .unwrap()
                    .value
                    .as_slice()
                    .try_into()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let profile = quai_consensus::validation::Profile {
            location: vec![0, 0],
            minimum_difficulty: quai_consensus::rules::Uint::ONE,
            duration: 5,
            gas_ceiling: 50_000_000,
            p2p_threshold: 7,
            genesis: Default::default(),
            default_genesis: None,
        };
        let result = quai_consensus::validation::check_inclusions_projection(
            &history,
            &b,
            &profile,
            &headers,
            &hashes,
            |_| Ok(2),
        );
        let kind = match result {
            Err(Error::Invalid("duplicate uncle proof identity" | "duplicate uncle")) => {
                "duplicate"
            }
            Err(Error::Invalid("uncle proof is ancestor" | "uncle is ancestor")) => "ancestor",
            _ => "none",
        };
        let expected = match row["kind"].as_str().unwrap() {
            "duplicate" => "duplicate",
            "ancestor" => "ancestor",
            _ => "none",
        };
        check(
            json!(i),
            "fork-gated inclusion identity violation",
            kind == expected,
        );
    }
    let r = json!({"passed":failures.is_empty(),"checks":checks,"failures":failures,"bindings":v["bindings"].as_array().unwrap().len(),"identities":v["identities"].as_array().unwrap().len(),"reference_revision":quai_primitives::REFERENCE_REVISION,"polaris_prime_block":quai_primitives::spec::FORKS.polaris,"full_consensus":false});
    r
}
