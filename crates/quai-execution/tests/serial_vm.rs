#[path = "vm_support/mod.rs"]
mod support;
use quai_execution::evm::{
    self,
    state::{Account, World},
    word::Word,
    CapabilityError, Fault,
};
use serde_json::Value;
fn corpus() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/serial-vm.json")).unwrap()
}
fn frame(code: Vec<u8>) -> (World, evm::Context) {
    let c = corpus();
    let v = &c["vectors"][0];
    let mut world = support::parse_world(v).unwrap();
    let ctx = support::parse_context(v).unwrap();
    let mut a = (*world.account(&ctx.address).unwrap().unwrap()).clone();
    a.code = code;
    a.storage.clear();
    a.storage_size = Word::ZERO;
    world.insert(ctx.address, a).unwrap();
    (world, ctx)
}
#[test]
fn frozen_go_frames_roots_gas_logs_and_traces_match() {
    let result = support::verify(&corpus()).unwrap();
    assert_eq!(result["mismatches"], 0, "{result}");
    assert_eq!(
        result["supported_opcode_values_unobserved"],
        serde_json::json!([]),
        "Every claimed opcode must have an oracle trace"
    );
}
#[test]
fn unimplemented_valid_operations_abort_atomically_after_a_write() {
    for op in [
        0x31, 0x3b, 0x3c, 0x3f, 0x40, 0xf0, 0xf1, 0xf2, 0xf4, 0xf5, 0xf6, 0xf8, 0xfa, 0xff,
    ] {
        let (mut world, ctx) = frame(vec![0x60, 1, 0x60, 0, 0x55, op]);
        let before = world.clone();
        assert_eq!(
            evm::execute_frame(&mut world, &ctx, &[], 3_000_000).unwrap_err(),
            CapabilityError::Opcode(op)
        );
        assert_eq!(world, before);
    }
}
#[test]
fn gas_bound_vectors_pass_former_native_caps() {
    // Expected values come from the pinned Go oracle (reference/serial-vm).
    let c = corpus();
    let cases = c["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["id"].as_str().unwrap().starts_with("bounds-"))
        .collect::<Vec<_>>();
    assert_eq!(cases.len(), 6);
    let find = |id: &str| *cases.iter().find(|v| v["id"] == id).unwrap();
    assert!(find("bounds-steps-224008")["trace_steps"].as_u64().unwrap() > 200_000);
    assert_eq!(
        find("bounds-logs-4100")["logs"].as_array().unwrap().len(),
        4100
    );
    assert!(find("bounds-input-140000")["input"].as_str().unwrap().len() > 2 * 131_072);
    assert_eq!(
        find("bounds-warm-slots-5000")["context"]["warm_slots"]
            .as_array()
            .unwrap()
            .len(),
        5000
    );
    for v in &cases {
        let mut world = support::parse_world(v).unwrap();
        let mut ctx = support::parse_context(v).unwrap();
        let input = quai_primitives::decode_hex(v["input"].as_str().unwrap()).unwrap();
        let gas = v["gas"].as_u64().unwrap();
        // Run without tracing, as consensus does. The traced run is in the full corpus test.
        ctx.trace = false;
        let r = evm::execute_frame(&mut world, &ctx, &input, gas).unwrap();
        assert_eq!(
            r.error.map(|e| e.class()),
            v["error"].as_str(),
            "{}",
            v["id"]
        );
        assert_eq!(
            r.gas_remaining,
            v["gas_remaining"].as_u64().unwrap(),
            "{}",
            v["id"]
        );
        assert_eq!(
            quai_primitives::encode_hex(&r.output),
            v["output"],
            "{}",
            v["id"]
        );
        assert_eq!(
            r.logs.len(),
            v["logs"].as_array().unwrap().len(),
            "{}",
            v["id"]
        );
        assert_eq!(
            quai_primitives::encode_hex(&world.root().unwrap()),
            v["after_root"]
        );
    }
    let memory = find("bounds-memory-2mib")["output"]
        .as_str()
        .unwrap()
        .to_owned();
    // MSIZE after the 2 MiB store, returned as the second word.
    assert!(memory.ends_with("200020"));
}
#[test]
fn trace_budget_aborts_atomically() {
    let code = vec![
        0x60, 1, 0x60, 0, 0x55, 0x62, 0x0f, 0xff, 0xe0, 0x51, 0x50, 0x5b, 0x60, 11, 0x56,
    ];
    let (mut world, mut ctx) = frame(code);
    ctx.trace = true;
    let before = world.clone();
    assert_eq!(
        evm::execute_frame(&mut world, &ctx, &[], u64::MAX).unwrap_err(),
        CapabilityError::Limit("trace bytes")
    );
    assert_eq!(world, before);
}
#[test]
fn endless_loop_ends_by_gas_like_the_go_loop_vector() {
    // The Go corpus case "loop-out-of-gas" runs this loop with 90 gas. With
    // 1,000,000 gas it passes the former 200,000-step cap before gas runs out,
    // and the trace stays inside its diagnostic budget.
    let (mut world, ctx) = frame(vec![0x60, 1, 0x60, 0, 0x55, 0x5b, 0x60, 5, 0x56]);
    let before = world.clone();
    let r = evm::execute_frame(&mut world, &ctx, &[], 1_000_000).unwrap();
    assert_eq!(r.error, Some(Fault::OutOfGas));
    assert_eq!(r.gas_remaining, 0);
    assert!(r.trace_steps > 200_000);
    assert_eq!(world, before);
}
#[test]
fn transient_values_are_frame_local_and_readonly_writes_fail() {
    let (mut world, mut ctx) = frame(vec![
        0x60, 0, 0x5c, 0x60, 0, 0x52, 0x60, 42, 0x60, 0, 0x5d, 0x60, 32, 0x60, 0, 0xf3,
    ]);
    for _ in 0..2 {
        let r = evm::execute_frame(&mut world, &ctx, &[], 3_000_000).unwrap();
        assert_eq!(r.output, vec![0; 32]);
        assert!(r.error.is_none());
    }
    let before = world.clone();
    ctx.read_only = true;
    let r = evm::execute_frame(&mut world, &ctx, &[], 3_000_000).unwrap();
    assert_eq!(r.error, Some(Fault::WriteProtection));
    assert_eq!(world, before);
}
#[test]
fn storage_size_overflow_aborts_without_wrapping_the_account_root() {
    let (mut world, ctx) = frame(vec![0x60, 1, 0x60, 0, 0x55, 0]);
    let mut account = (*world.account(&ctx.address).unwrap().unwrap()).clone();
    account.storage_size = Word::MAX;
    world.insert(ctx.address, account).unwrap();
    let before = world.clone();
    assert_eq!(
        evm::execute_frame(&mut world, &ctx, &[], 3_000_000).unwrap_err(),
        CapabilityError::Context("storage size overflow")
    );
    assert_eq!(world, before);
}
#[test]
fn storage_capacity_exhaustion_rolls_back_earlier_writes() {
    let (mut world, ctx) = frame(vec![
        0x60, 1, 0x60, 0, 0x55, 0x60, 1, 0x61, 0xff, 0xff, 0x55, 0,
    ]);
    let mut account = (*world.account(&ctx.address).unwrap().unwrap()).clone();
    for n in 0..evm::state::MAX_STORAGE_SLOTS {
        let mut k = [0; 32];
        k[24..].copy_from_slice(&(n as u64).to_be_bytes());
        account.put(k, [2; 32]).unwrap();
    }
    account.storage_size = Word::from_u64(evm::state::MAX_STORAGE_SLOTS as u64);
    world.insert(ctx.address, account).unwrap();
    let before = world.clone();
    assert_eq!(
        evm::execute_frame(&mut world, &ctx, &[], 3_000_000).unwrap_err(),
        CapabilityError::Limit("storage slots")
    );
    assert_eq!(world, before);
}
#[test]
fn invalid_snapshot_replacement_preserves_previous_account() {
    let (mut world, ctx) = frame(vec![0]);
    let before = world.clone();
    assert!(world
        .insert(
            ctx.address,
            Account {
                code: vec![0; 32769],
                ..Account::default()
            }
        )
        .is_err());
    assert_eq!(world, before);
}
