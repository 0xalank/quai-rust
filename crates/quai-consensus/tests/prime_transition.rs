//! Linked prime blocks through `prime_controller::State::step` against the
//! go-quai v0.57.0 oracle (`scripts/gen_prime_transition_vectors.py`): the
//! byte-identical prime branch of Slice.Append, which calls the unmodified
//! verifyParentExchangeRateAndFlowAmount, across ControllerKickInBlock, the
//! first feedback rate, ConversionSlipChangeBlock, the first two
//! KQuaiChangeTable rows and their hold ends, the KawPow reset and the KQuai
//! freeze. Every accepted block, every changed header field and every changed
//! history input must get Go's verdict, and every accepted block must pass
//! the same priced ETX list to the region.
use crypto_bigint::{CheckedAdd, CheckedMul, U512};
use quai_consensus::{
    choice_window::{Choice, Window},
    hierarchy_header,
    history::Record,
    prime_controller::{Parts, Refusal, State, RATES},
    rules::Uint,
};
use quai_primitives::{
    decode_hex, encode_hex, spec::ECONOMICS, transaction::Transaction, work::transactions_root,
    Hash,
};
use serde_json::Value;

fn uint(v: &Value) -> Uint {
    let mut n = Uint::ZERO;
    for c in v.as_str().unwrap().bytes() {
        assert!(c.is_ascii_digit());
        n = Option::<Uint>::from(n.checked_mul(&Uint::from_u64(10))).unwrap();
        n = Option::<Uint>::from(n.checked_add(&Uint::from_u64((c - b'0') as u64))).unwrap();
    }
    n
}
fn wide(v: &Value) -> U512 {
    let mut n = U512::ZERO;
    for c in v.as_str().unwrap().bytes() {
        n = Option::<U512>::from(n.checked_mul(&U512::from_u64(10))).unwrap();
        n = Option::<U512>::from(n.checked_add(&U512::from_u64((c - b'0') as u64))).unwrap();
    }
    n
}
fn hash(v: &Value) -> Hash {
    decode_hex(v.as_str().unwrap()).unwrap().try_into().unwrap()
}
fn record(v: &Value) -> Record {
    Record::decode(&decode_hex(v.as_str().unwrap()).unwrap()).unwrap()
}
fn txs(v: &Value) -> Vec<Transaction> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| Transaction::decode(&decode_hex(s.as_str().unwrap()).unwrap()).unwrap())
        .collect()
}
fn wires(list: &[Transaction]) -> Vec<String> {
    list.iter().map(|t| encode_hex(&t.encode())).collect()
}
fn field(r: &Record, name: &str) -> Uint {
    let h = r.object().header().wire();
    let v = match name {
        "exchange_rate" => &h.exchange_rate,
        "k_quai_discount" => &h.k_quai_discount,
        "conversion_flow_amount" => &h.conversion_flow_amount,
        _ => &h.miner_difficulty,
    };
    quai_consensus::share_rules::uint(v.as_deref().unwrap_or_default()).unwrap()
}
/// The seeded token choice rows of the oracle (ptRow).
fn row(n: u64, base: Uint, step: Uint) -> Choice {
    Choice {
        quai: n % 13,
        qi: n % 11,
        difficulty: base.wrapping_add(&step.wrapping_mul(&Uint::from_u64(n % 997))),
    }
}
fn window(block: &Record, spec: &Value, base: Uint, step: Uint) -> Window {
    let first = spec["first"].as_u64().unwrap();
    let count = spec["count"].as_u64().unwrap() as usize;
    let mut rows = vec![Choice::default(); quai_consensus::controllers::WINDOW as usize];
    for (i, r) in rows.iter_mut().enumerate().take(count) {
        *r = row(first + i as u64, base, step);
    }
    let w = Window::from_choices(block.id(), block.number(0).unwrap(), None, &rows).unwrap();
    assert_eq!(encode_hex(&w.logical_digest()), spec["sha256"]);
    assert_eq!(w.difficulty_sum(), wide(&spec["difficulty_sum"]));
    w
}
/// Go's refusal text: (Rust field name, remote, local).
fn go_refusal(message: &str) -> (&'static str, Uint, Uint) {
    let name = if message.starts_with("invalid conversion flow amount used") {
        "conversion flow amount"
    } else if message.starts_with("invalid exchange rate used") {
        "exchange rate"
    } else if message.starts_with("invalid newKQuaiDiscount used") {
        "kQuai discount"
    } else {
        panic!("unexpected go-quai refusal {message}")
    };
    let number = |key: &str| {
        let at = message.find(key).unwrap() + key.len();
        let digits: String = message[at..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        uint(&Value::String(digits))
    };
    (name, number("remote: "), number("local: "))
}
fn assert_verdict(
    result: Result<quai_consensus::prime_controller::Step, Refusal>,
    go: &Value,
    what: &str,
) {
    match go.as_str() {
        None => {
            if let Err(e) = result {
                panic!("{what}: go-quai accepts, Rust refuses: {e:?}");
            }
        }
        Some(message) => {
            let (name, remote, local) = go_refusal(message);
            match result {
                Err(Refusal::Field {
                    field,
                    expected,
                    actual,
                }) => {
                    assert_eq!(
                        (field, expected, actual),
                        (name, local, remote),
                        "{what}: {message}"
                    )
                }
                Err(e) => panic!("{what}: go-quai refuses with {message}, Rust: {e:?}"),
                Ok(_) => panic!("{what}: go-quai refuses with {message}, Rust accepts"),
            }
        }
    }
}

#[test]
fn prime_blocks_across_kick_in_table_rows_and_holds_match_go_append() {
    let v: Value = serde_json::from_str(include_str!(
        "../../../fixtures/protocol/prime-transition.json"
    ))
    .unwrap();
    assert_eq!(v["reference_revision"], quai_primitives::REFERENCE_REVISION);
    let (mut blocks, mut fields, mut history_rejected, mut history_accepted, mut priced) =
        (0, 0, 0, 0, 0);
    for segment in v["segments"].as_array().unwrap() {
        let name = segment["name"].as_str().unwrap();
        let seed = &segment["seed"];
        let mut grand = record(&seed["grandparent"]);
        let mut parent = record(&seed["parent"]);
        assert_eq!(grand.id(), hash(&seed["grandparent_hash"]));
        assert_eq!(parent.id(), hash(&seed["parent_hash"]));
        let base = uint(&seed["row_base"]);
        let step = uint(&seed["row_step"]);
        let parent_number = parent.number(0).unwrap();
        // Rates of the canonical blocks the segment reads; the others are
        // never read and keep the starting rate.
        let first = parent_number + 1 - RATES as u64;
        let mut rates = vec![Uint::from_u128(ECONOMICS.exchange_rate); RATES];
        for pair in seed["rates"].as_array().unwrap() {
            let n = pair[0].as_u64().unwrap();
            rates[(n - first) as usize] = uint(&pair[1]);
        }
        let mut state = State::from_parts(Parts {
            head: parent.id(),
            number: parent_number,
            first,
            rates,
            inbound: Some(txs(&seed["parent_inbound"])),
            parent_window: Some(window(&grand, &seed["grandparent_window"], base, step)),
            head_window: window(&parent, &seed["parent_window"], base, step),
        })
        .unwrap();
        for row in segment["blocks"].as_array().unwrap() {
            let block = record(&row["block"]);
            let n = row["number"].as_u64().unwrap();
            let what = format!("{name} {n}");
            assert_eq!(block.id(), hash(&row["hash"]), "{what}");
            let raw = txs(&row["inbound"]);
            // The header rule of core/headerchain_validation.go.
            let miner = hierarchy_header::miner_difficulty(
                parent.number(0).unwrap(),
                parent.difficulty().unwrap(),
                field(&parent, "miner_difficulty"),
            )
            .unwrap();
            assert_eq!(miner, field(&block, "miner_difficulty"), "{what}");
            for m in row["mutations"].as_array().unwrap() {
                let changed = record(&m["block"]);
                let name = m["field"].as_str().unwrap();
                assert_eq!(field(&changed, name), uint(&m["value"]), "{what} {name}");
                let message = m["go_error"].as_str().unwrap();
                if name == "miner_difficulty" {
                    assert!(message.starts_with("invalid miner difficulty"), "{message}");
                    assert_ne!(miner, field(&changed, name), "{what}");
                } else {
                    assert_verdict(
                        state.step(&changed, &parent, Some(&grand), raw.clone()),
                        &m["go_error"],
                        &format!("{what} changed {name}"),
                    );
                }
                fields += 1;
            }
            for h in row["history"].as_array().unwrap() {
                let kind = h["kind"].as_str().unwrap();
                let mut parts = state.parts();
                match kind {
                    "parent_inbound" => parts.inbound = Some(txs(&h["inbound"])),
                    "old_rate" => {
                        let at = h["number"].as_u64().unwrap() - parts.first;
                        parts.rates[at as usize] = uint(&h["rate"]);
                    }
                    "grandparent_window" | "parent_window" => {
                        let w = if kind == "grandparent_window" {
                            parts.parent_window.as_ref().unwrap()
                        } else {
                            &parts.head_window
                        };
                        let mut rows = w.choices();
                        let at = h["index"].as_u64().unwrap() as usize;
                        rows[at].difficulty = uint(&h["difficulty"]);
                        let changed =
                            Window::from_choices(w.block(), w.number(), w.parent(), &rows).unwrap();
                        if kind == "grandparent_window" {
                            parts.parent_window = Some(changed);
                        } else {
                            parts.head_window = changed;
                        }
                    }
                    other => panic!("history kind {other}"),
                }
                let changed = State::from_parts(parts).unwrap();
                let result = changed.step(&block, &parent, Some(&grand), raw.clone());
                if let (Ok(s), Some(expected)) = (&result, h.get("output")) {
                    let expected: Vec<String> = expected
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_str().unwrap().to_string())
                        .collect();
                    assert_eq!(wires(s.dominant()), expected, "{what} {kind} output");
                }
                if h["go_error"].is_null() {
                    history_accepted += 1;
                } else {
                    history_rejected += 1;
                }
                assert_verdict(result, &h["go_error"], &format!("{what} changed {kind}"));
            }
            // The block itself.
            let step = state
                .step(&block, &parent, Some(&grand), raw)
                .unwrap_or_else(|e| panic!("{what}: {e:?}"));
            let expected: Vec<String> = row["output"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().to_string())
                .collect();
            assert_eq!(wires(step.dominant()), expected, "{what} output");
            assert_eq!(
                transactions_root(step.dominant()).unwrap(),
                hash(&row["output_root"]),
                "{what}"
            );
            let refunds = step
                .dominant()
                .iter()
                .filter(|t| {
                    matches!(t.kind(), quai_primitives::transaction::Kind::External(e) if e.etx_type == 5)
                })
                .count();
            assert_eq!(refunds as u64, row["refunds"].as_u64().unwrap(), "{what}");
            if n > quai_primitives::spec::FORKS.controller_kick_in {
                assert_eq!(
                    step.forward_rate(),
                    Some(uint(&row["forward_rate"])),
                    "{what}"
                );
                let w = step.state().head_window();
                assert_eq!(
                    encode_hex(&w.logical_digest()),
                    row["window_sha256"],
                    "{what}"
                );
                assert_eq!(w.difficulty_sum(), wide(&row["difficulty_sum"]), "{what}");
            } else {
                assert_eq!(step.forward_rate(), None, "{what}");
            }
            for name in ["exchange_rate", "k_quai_discount", "conversion_flow_amount"] {
                assert_eq!(field(&block, name), uint(&row["fields"][name]), "{what}");
            }
            priced += step.dominant().len();
            blocks += 1;
            state = step.into_state();
            assert_eq!(state.head(), (block.id(), n));
            assert_eq!(state.rate_at(n).unwrap(), field(&block, "exchange_rate"));
            grand = parent;
            parent = block;
        }
    }
    assert_eq!(blocks, v["accepted_blocks"].as_u64().unwrap());
    assert_eq!(fields, v["go_rejected_field_changes"].as_u64().unwrap());
    assert_eq!(history_rejected, v["go_rejected_history"].as_u64().unwrap());
    assert_eq!(history_accepted, v["go_accepted_history"].as_u64().unwrap());
    assert_eq!(priced as u64, v["priced_etxs"].as_u64().unwrap());
    println!(
        "{{\"segments\":{},\"accepted_blocks\":{blocks},\"go_rejected_field_changes\":{fields},\"go_rejected_history\":{history_rejected},\"go_accepted_history\":{history_accepted},\"priced_etxs\":{priced}}}",
        v["segments"].as_array().unwrap().len()
    );
}
