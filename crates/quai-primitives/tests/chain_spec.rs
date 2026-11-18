//! The colosseum chain spec and the fork schedule against the Go oracle:
//! fixtures/protocol/chain-spec.json comes from the pinned go-quai v0.57.0
//! sources through scripts/gen_chain_spec_vectors.py (`go build -overlay`,
//! GOPROXY=off, GOTOOLCHAIN=local; `--check` rebuilds it and compares).
use quai_primitives::{
    decode_hex, encode_hex,
    spec::{self, Allocations, ChainSpec, FORKS},
    REFERENCE_REVISION,
};
use serde_json::Value;

fn oracle() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/chain-spec.json")).unwrap()
}
fn u(v: &Value) -> u64 {
    match v {
        Value::String(s) => s.parse().unwrap(),
        _ => v.as_u64().unwrap(),
    }
}
fn u128_of(v: &Value) -> u128 {
    match v {
        Value::String(s) => s.parse().unwrap(),
        _ => v.as_u64().unwrap() as u128,
    }
}

#[test]
fn fork_schedule_equals_go_params() {
    let o = oracle();
    assert_eq!(o["reference_revision"], REFERENCE_REVISION);
    let f = &o["forks"];
    let rust = [
        ("kawpow", FORKS.kawpow),
        ("kawpow_transition", FORKS.kawpow_transition),
        ("controller_kick_in", FORKS.controller_kick_in),
        ("conversion_slip_change", FORKS.conversion_slip_change),
        ("k_quai_change", FORKS.k_quai_change),
        ("k_quai_change_hold", FORKS.k_quai_change_hold),
        (
            "k_quai_reset_after_kawpow",
            FORKS.k_quai_reset_after_kawpow(),
        ),
        (
            "exchange_rate_hold_after_kawpow",
            FORKS.exchange_rate_hold_after_kawpow,
        ),
        (
            "exchange_rate_hold_after_sha_equivalent",
            FORKS.exchange_rate_hold_after_sha_equivalent,
        ),
        ("inclusion_depth_change", FORKS.inclusion_depth_change),
        (
            "inclusion_depth_update_period",
            FORKS.inclusion_depth_update_period,
        ),
        ("singularity", FORKS.singularity),
        ("qi_wrapping_change", FORKS.qi_wrapping_change),
        ("sha_equivalent_difficulty", FORKS.sha_equivalent_difficulty),
        ("conversion_stability", FORKS.conversion_stability),
        ("self_destruct_refund", FORKS.self_destruct_refund),
        ("conversion_lock_change", FORKS.conversion_lock_change),
        ("k_quai_freeze", FORKS.k_quai_freeze()),
        ("security_hardening", FORKS.security_hardening),
        ("polaris", FORKS.polaris),
        ("start_tx", FORKS.start_tx),
        (
            "coinbase_lockup_precompile",
            FORKS.coinbase_lockup_precompile,
        ),
        ("qi_activation", FORKS.qi_activation),
        ("max_grind_increase", FORKS.max_grind_increase),
        ("max_code_size", FORKS.max_code_size),
        (
            "unwrap_qi_lock_period_before",
            FORKS.unwrap_qi_lock_period(FORKS.conversion_lock_change - 1),
        ),
        (
            "unwrap_qi_lock_period_at",
            FORKS.unwrap_qi_lock_period(FORKS.conversion_lock_change),
        ),
        (
            "max_code_size_before",
            FORKS.max_code_size_at(FORKS.max_code_size - 1) as u64,
        ),
        (
            "max_code_size_at",
            FORKS.max_code_size_at(FORKS.max_code_size) as u64,
        ),
    ];
    for (name, value) in rust {
        assert_eq!(u(&f[name]), value, "fork {name}");
    }
    let table: Vec<(u64, u64)> = f["k_quai_change_table"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (u(&r[0]), u(&r[1])))
        .collect();
    assert_eq!(table, FORKS.k_quai_change_table);
    // Every fork the oracle prints is in the schedule above.
    assert_eq!(f.as_object().unwrap().len(), rust.len() + 1);
    let p = &o["params"];
    // lockup_start = 2 * params.BlocksPerMonth.
    assert_eq!(FORKS.lockup_start, 2 * u(&p["blocks_per_month"]));
    assert_eq!(
        FORKS.header_schedule().kawpow_fork,
        u(&f["kawpow"]),
        "header codec schedule"
    );
}

#[test]
fn colosseum_spec_equals_go_params_and_genesis() {
    let o = oracle();
    let c = &o["colosseum"];
    let p = &o["params"];
    let s = ChainSpec::colosseum();
    assert_eq!(s.environment, c["name"].as_str().unwrap());
    assert_eq!(s.chain_id, u(&c["chain_id"]));
    assert_eq!(c["consensus_engine"], "progpow");
    assert_eq!(
        encode_hex(&s.genesis.hash),
        c["genesis_hash"].as_str().unwrap()
    );
    assert_eq!(c["genesis_hash"], c["rebuilt_genesis_hash"]);
    assert_eq!(
        spec::COLOSSEUM_GENESIS_NONCE_FLAG,
        c["genesis_nonce_flag"].as_str().unwrap()
    );
    assert_eq!(s.genesis.nonce, u(&c["genesis_nonce"]));
    assert_eq!(
        s.genesis.extra,
        decode_hex(c["genesis_extra"].as_str().unwrap()).unwrap()
    );
    assert_eq!(s.genesis.timestamp, u(&c["genesis_timestamp"]));
    assert_eq!(s.genesis.gas_limit, u(&c["genesis_gas_limit"]));
    assert_eq!(s.genesis.difficulty, u128_of(&c["genesis_difficulty"]));
    assert_eq!(
        encode_hex(&s.genesis.allocation_hash),
        c["allocation_hash"].as_str().unwrap()
    );
    assert_eq!(s.genesis.starting_expansion, 0);
    let h = &s.header;
    assert_eq!(h.minimum_difficulty, u128_of(&c["min_difficulty"]));
    assert_eq!(h.duration, u(&c["duration_limit"]));
    assert_eq!(h.gas_ceiling, u(&c["gas_ceil"]));
    assert_eq!(h.p2p_threshold as u64, u(&c["workshare_p2p_threshold"]));
    assert_eq!(
        h.p2p_threshold as u64,
        u(&p["workshare_p2p_threshold_diff"])
    );
    assert_eq!(h.state_ceiling, u(&p["state_ceil"]));
    assert_eq!(h.min_gas_limit, u(&p["min_gas_limit"]));
    assert_eq!(
        h.workshare_threshold as u64,
        u(&p["workshares_threshold_diff"])
    );
    assert_eq!(
        h.future_block_time,
        u(&o["core"]["allowed_future_block_time_seconds"])
    );
    assert_eq!(h.max_time_diff, u(&p["max_time_diff_between_blocks"]));
    assert_eq!(h.max_extra_data as u64, u(&p["maximum_extra_data_size"]));
    assert_eq!(
        h.difficulty_adjustment_period,
        u(&p["difficulty_adjustment_period"])
    );
    assert_eq!(
        h.difficulty_adjustment_factor,
        u(&p["difficulty_adjustment_factor"])
    );
    assert_eq!(h.default_genesis, Some(s.genesis.hash));
    let keys: Vec<String> = s
        .quorum_keys
        .iter()
        .map(|k| encode_hex(k)[2..].to_string())
        .collect();
    let go: Vec<&str> = p["musig2_public_keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap())
        .collect();
    assert_eq!(keys, go);
    assert_eq!(s.economics.exchange_rate, u128_of(&p["exchange_rate"]));
    assert_eq!(
        s.economics.k_quai_discount,
        u128_of(&p["starting_k_quai_discount"])
    );
    assert_eq!(
        s.economics.conversion_flow_amount,
        u128_of(&p["starting_conversion_flow_amount"])
    );
    assert_eq!(s.allocations, Allocations::Official);
    let locks: Vec<u64> = p["lockup_byte_to_block_depth"]
        .as_array()
        .unwrap()
        .iter()
        .map(u)
        .collect();
    assert_eq!(s.lock_periods.by_lock_byte.to_vec(), locks);
    assert_eq!(
        s.lock_periods.by_lock_byte.len() as u64,
        u(&p["max_lockup_byte"]) + 1
    );
    assert_eq!(s.lock_periods.conversion, u(&p["conversion_lock_period"]));
    assert_eq!(s.lock_periods.unwrap_qi, u(&p["unwrap_qi_lock_period"]));
    for (d, depth) in s.trim_depths.iter().enumerate() {
        assert_eq!(*depth, u(&p["trim_depths"][d.to_string()]), "trim {d}");
    }
    assert_eq!(p["trim_depths"].as_object().unwrap().len(), 6);
    assert_eq!(s.forks, FORKS);
}

#[test]
fn calendar_counts_and_hierarchy_geometry_equal_go() {
    let o = oracle();
    let p = &o["params"];
    assert_eq!(spec::BLOCKS_PER_DAY, u(&p["blocks_per_day"]));
    assert_eq!(spec::BLOCKS_PER_WEEK, u(&p["blocks_per_week"]));
    assert_eq!(spec::BLOCKS_PER_MONTH, u(&p["blocks_per_month"]));
    assert_eq!(
        spec::PRIME_BLOCKS_PER_MONTH,
        u(&p["prime_blocks_per_month"])
    );
    assert_eq!(spec::BLOCKS_PER_YEAR, u(&p["blocks_per_year"]));
    for (prime, (total, sha, scrypt)) in [
        (
            FORKS.singularity - 1,
            FORKS.workshare_counts(FORKS.singularity - 1),
        ),
        (FORKS.singularity, FORKS.workshare_counts(FORKS.singularity)),
    ] {
        let new = prime >= FORKS.singularity;
        let k = |old: &str, newer: &str| u(&p[if new { newer } else { old }]) as usize;
        assert_eq!(total, k("max_workshare_count", "new_max_workshare_count"));
        assert_eq!(sha, k("max_sha_shares_count", "new_max_sha_shares_count"));
        assert_eq!(
            scrypt,
            k("max_scrypt_shares_count", "new_max_scrypt_shares_count")
        );
    }
    assert_eq!(
        FORKS.inclusion_depth(FORKS.inclusion_depth_change - 1) as u64,
        u(&p["workshares_inclusion_depth"])
    );
    assert_eq!(
        FORKS.inclusion_depth(FORKS.inclusion_depth_change) as u64,
        u(&p["new_workshares_inclusion_depth"])
    );
    for (e, rz) in p["hierarchy_size_by_expansion"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        assert_eq!(
            spec::hierarchy_size(e as u8),
            (u(&rz[0]), u(&rz[1])),
            "expansion {e}"
        );
    }
    assert_eq!(
        quai_primitives::schema::MAX_MESSAGE_BYTES as u64,
        u(&p["max_gossipsub_packet_size"])
    );
}
