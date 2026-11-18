//! Shared-state CALL/CALLCODE/DELEGATECALL/STATICCALL lifecycle.
use super::{
    journal::Journal,
    run_code,
    state::{Account, World},
    state_gas,
    word::{low_address, Word},
    CapabilityError, Context, Fault, Outcome,
};
use quai_primitives::spec::FORKS;
use quai_primitives::transaction::{AccessTuple, Address};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Call,
    CallCode,
    Delegate,
    Static,
}
pub fn local(a: &Address, location: [u8; 2]) -> bool {
    a[0] == (location[0] << 4 | location[1]) && a[1] & 128 == 0
}
/// The error of go-quai `Address.InternalAndQuaiAddress` for an address that
/// is not `local` (common/address.go:60-75): it checks the ledger first.
pub(crate) fn scope_fault(a: &Address) -> Fault {
    if a[1] > 127 {
        Fault::QiAddress
    } else {
        Fault::ExternalAddress
    }
}
/// Go `StateDB.Empty` for an account that exists.
pub(crate) fn empty_account(v: &Account) -> bool {
    v.nonce == 0 && v.code.is_empty() && v.balance == Word::ZERO && v.storage_size == Word::ZERO
}
pub(crate) fn empty(s: &Journal, a: &Address) -> Result<bool, CapabilityError> {
    Ok(s.world().account(a)?.is_none_or(|v| empty_account(&v)))
}
pub(crate) fn result(s: &Journal, gas: u64, error: Option<Fault>) -> Outcome {
    Outcome {
        auxiliary: Default::default(),
        error,
        reverted: false,
        output: vec![],
        gas_remaining: gas,
        state_gas: 0,
        refund: s.refund,
        logs: vec![],
        outbound_etxs: vec![],
        trace_hash: s.trace_hash,
        trace_steps: s.trace_steps,
    }
}
pub(crate) fn invoke(
    s: &mut Journal,
    parent: &Context,
    kind: Kind,
    target: Address,
    value: Word,
    input: &[u8],
    mut gas: u64,
    depth: usize,
) -> Result<Outcome, CapabilityError> {
    if depth > 1024 {
        return Ok(result(s, gas, Some(Fault::Depth)));
    }
    // go-quai `EVM.Call` and `EVM.CallCode` check the value against the
    // caller's balance first: before the lockup branch, the precompile lookup
    // and the address scope (core/vm/evm.go:260-262, 366-368). The failure
    // keeps the gas for the caller.
    if matches!(kind, Kind::Call | Kind::CallCode)
        && s.world()
            .account(&parent.address)?
            .map(|a| a.balance)
            .unwrap_or(Word::ZERO)
            < value
    {
        return Ok(result(s, gas, Some(Fault::InsufficientBalance)));
    }
    if kind == Kind::Call && target == crate::lockup::contract(parent.location) {
        return super::lockup::invoke(s, parent, input, gas);
    }
    let precompile = super::precompile::translate(target, parent.location);
    let target = precompile.map(|(_, a)| a).unwrap_or(target);
    if !local(&target, parent.location) {
        // EVM.CallCode, DelegateCall and StaticCall (core/vm/evm.go:377,
        // 420, 464): no gas left.
        if kind != Kind::Call {
            return Ok(result(s, 0, Some(scope_fault(&target))));
        }
        let point = s.snapshot()?;
        let outcome = super::etx::transfer(s, parent, target, value, gas, input)?;
        return match outcome {
            Ok(()) => {
                s.commit(point)?;
                let mut out = result(s, 0, None);
                out.outbound_etxs = s.etxs.clone();
                Ok(out)
            }
            Err(error) => {
                s.revert(point)?;
                Ok(result(s, 0, Some(error)))
            }
        };
    }
    let checkpoint = s.snapshot()?;
    let mut state_gas = 0;
    if kind == Kind::Call && s.world().account(&target)?.is_none() {
        if value == Word::ZERO && precompile.is_none() {
            s.commit(checkpoint)?;
            return Ok(result(s, gas, None));
        }
        let charge = state_gas::scaled(parent.state_size, Word::ZERO, 25000);
        if gas <= charge {
            s.revert(checkpoint)?;
            return Ok(result(s, gas, Some(Fault::InsufficientBalance)));
        }
        gas -= charge;
        state_gas += charge;
        s.create(target)?;
    }
    if kind == Kind::Call && !s.transfer(parent.address, target, value)? {
        s.revert(checkpoint)?;
        return Ok(result(s, gas, Some(Fault::InsufficientBalance)));
    }
    let mut context = parent.clone();
    context.address = if matches!(kind, Kind::Delegate | Kind::CallCode) {
        parent.address
    } else {
        target
    };
    context.caller = if kind == Kind::Delegate {
        parent.caller
    } else {
        parent.address
    };
    context.value = if kind == Kind::Delegate {
        parent.value
    } else if kind == Kind::Static {
        Word::ZERO
    } else {
        value
    };
    context.read_only = parent.read_only || kind == Kind::Static;
    let code = s
        .world()
        .account(&target)?
        .map(|a| a.code.clone())
        .unwrap_or_default();
    let mut outcome = if let Some((id, _)) = precompile {
        match super::precompile::run(id, input, gas)? {
            Ok((output, left)) => {
                let mut out = result(s, left, None);
                out.output = output;
                out
            }
            Err(error) => result(s, 0, Some(error)),
        }
    } else {
        run_code(s, &context, input, gas, code, depth)?
    };
    outcome.state_gas += state_gas;
    if kind == Kind::Static {
        outcome.state_gas = 0;
    }
    if outcome.error.is_none() && !outcome.reverted {
        s.commit(checkpoint)?;
    } else {
        s.revert(checkpoint)?;
    }
    Ok(outcome)
}
/// One transaction-scoped call with an explicit access list and value transfer.
/// Unsupported domains leave the input world unchanged. Returned EVM failures
/// are frame failures; transaction nonce and fee settlement belong to the caller.
#[derive(Clone, Debug, Default)]
pub struct Environment {
    pub prime_number: Option<u64>,
    pub block_hashes: std::collections::BTreeMap<u64, quai_primitives::Hash>,
    pub transaction_hash: Option<quai_primitives::Hash>,
    pub eligible_slices: Option<quai_primitives::Hash>,
    pub delete_empty: bool,
}
pub fn execute_call(
    world: &mut World,
    context: &Context,
    access: &[AccessTuple],
    input: &[u8],
    gas: u64,
) -> Result<Outcome, CapabilityError> {
    execute_call_with_env(world, context, access, input, gas, &Environment::default())
}
pub fn execute_call_with_env(
    world: &mut World,
    context: &Context,
    access: &[AccessTuple],
    input: &[u8],
    gas: u64,
    environment: &Environment,
) -> Result<Outcome, CapabilityError> {
    super::with_call_stack(world, |world| {
        call_with_env(world, context, access, input, gas, environment)
    })
}
fn call_with_env(
    world: &mut World,
    context: &Context,
    access: &[AccessTuple],
    input: &[u8],
    gas: u64,
    environment: &Environment,
) -> Result<Outcome, CapabilityError> {
    if !local(&context.caller, context.location) {
        return Err(CapabilityError::Context("local Quai call required"));
    }
    // No input or access-list cap: go-quai bounds both only through intrinsic
    // gas (core/state_transition.go `IntrinsicGas`: 4 or 16 gas per data byte,
    // 2,400 per address, 1,900 per key). A 50,000,000-gas block allows about
    // 12.5 MB of zero data, 20,824 addresses or 26,304 keys in one transaction.
    // BLOCKHASH serves at most the 256 previous blocks, so a longer history is
    // a caller error.
    if environment.block_hashes.len() > 256 {
        return Err(CapabilityError::Limit("BLOCKHASH history"));
    }
    let mut state = Journal::new(world.clone())?;
    state.allow_calls = true;
    state.delete_empty = environment.delete_empty;
    state.prime_number = environment.prime_number;
    state.block_hashes = environment.block_hashes.clone();
    state.transaction_hash = environment.transaction_hash;
    state.eligible_slices = environment.eligible_slices;
    state.access_list = access.to_vec();
    // go-quai `StateDB.PrepareAccessList`: sender, recipient, precompiles and
    // the transaction's list.
    state
        .addresses
        .extend(super::precompile::active(context.location));
    state.addresses.insert(context.caller);
    state.addresses.insert(context.address);
    for a in access {
        state.addresses.insert(a.address);
        state.slots.extend(a.keys.iter().map(|k| (a.address, *k)));
    }
    let outer = state.snapshot()?;
    let mut parent = context.clone();
    parent.address = context.caller;
    let kind = if context.read_only {
        Kind::Static
    } else {
        Kind::Call
    };
    let mut outcome = invoke(
        &mut state,
        &parent,
        kind,
        context.address,
        context.value,
        input,
        gas,
        0,
    )?;
    if outcome.error.is_none() && !outcome.reverted
        || (!context.read_only
            && context.address == crate::lockup::contract(context.location)
            && environment
                .prime_number
                .is_some_and(|p| p < FORKS.sha_equivalent_difficulty))
    {
        state.finalize_sizes()?;
        outcome.logs = state.logs.clone();
        outcome.outbound_etxs = state.etxs.clone();
        outcome.refund = state.refund;
        outcome.auxiliary = super::AuxiliaryEffects {
            deleted_hashes: state.deleted_hashes.clone(),
            deleted: (*state.deleted).clone(),
        };
        state.commit(outer)?;
        *world = state.into_world()?;
    } else {
        state.revert(outer)?;
        world.replace_lockups(state.world());
        outcome.logs.clear();
        outcome.refund = 0;
        if environment.delete_empty {
            let cleanup = state.snapshot()?;
            state.finalize_sizes()?;
            state.commit(cleanup)?;
            *world = state.into_world()?;
        }
    }
    Ok(outcome)
}
/// Quai CREATE address derivation includes init code and grinds into the local
/// Quai scope. CREATE2 uses the supplied salt and never grinds.
pub fn creation_address(
    sender: Address,
    nonce: u64,
    code: &[u8],
    salt: Option<[u8; 32]>,
    block_number: u64,
    location: [u8; 2],
    mut gas: u64,
) -> Result<(Address, u64), Fault> {
    use quai_primitives::transaction::keccak;
    let code_hash = keccak(code);
    if let Some(salt) = salt {
        let mut b = vec![255];
        b.extend(sender);
        b.extend(salt);
        b.extend(code_hash);
        return Ok((low_address(keccak(&b)), gas));
    }
    let mut b = sender.to_vec();
    b.extend(nonce.to_be_bytes());
    b.extend(code);
    let direct = low_address(keccak(&b));
    if local(&direct, location) {
        return Ok((direct, gas));
    }
    let cost = 30 + (code.len() as u64).div_ceil(32) * 6;
    let attempts = if block_number < FORKS.max_grind_increase {
        1000
    } else {
        10000
    };
    for i in 0u64..attempts {
        gas = gas.checked_sub(cost).ok_or(Fault::AddressGrind)?;
        let mut salt = [0; 32];
        salt[16..24].copy_from_slice(&i.to_be_bytes());
        salt[24..].copy_from_slice(&nonce.to_be_bytes());
        let mut b = vec![255];
        b.extend(sender);
        b.extend(salt);
        b.extend(code_hash);
        let a = low_address(keccak(&b));
        if local(&a, location) {
            return Ok((a, gas));
        }
    }
    Err(Fault::AddressGrind)
}
pub(crate) fn create(
    s: &mut Journal,
    parent: &Context,
    code: &[u8],
    value: Word,
    gas: u64,
    salt: Option<[u8; 32]>,
    depth: usize,
) -> Result<(Outcome, Option<Address>), CapabilityError> {
    use super::word::uint;
    let nonce = s
        .world()
        .account(&parent.address)?
        .map(|a| a.nonce)
        .unwrap_or(0);
    let (address, mut gas) = match creation_address(
        parent.address,
        nonce,
        code,
        salt,
        uint(parent.block_number).unwrap_or(u64::MAX),
        parent.location,
        gas,
    ) {
        Ok(v) => v,
        Err(e) => return Ok((result(s, 0, Some(e)), None)),
    };
    s.set_nonce(parent.address, nonce.wrapping_add(1))?;
    if depth > 1024 {
        return Ok((result(s, gas, Some(Fault::Depth)), None));
    }
    let fee = state_gas::scaled(parent.state_size, Word::ZERO, 25000);
    if gas <= fee {
        return Ok((result(s, gas, Some(Fault::InsufficientBalance)), None));
    }
    gas -= fee;
    if s.world()
        .account(&parent.address)?
        .map(|a| a.balance)
        .unwrap_or(Word::ZERO)
        < value
    {
        return Ok((result(s, gas, Some(Fault::InsufficientBalance)), None));
    }
    if !local(&address, parent.location) {
        return Ok((result(s, 0, Some(scope_fault(&address))), None));
    }
    if !s.address_listed(&address) {
        return Ok((result(s, 0, Some(Fault::InvalidAccessList)), None));
    }
    if s.world()
        .account(&address)?
        .is_some_and(|a| a.nonce != 0 || !a.code.is_empty())
    {
        return Ok((result(s, 0, Some(Fault::ContractCollision)), None));
    }
    let point = s.snapshot()?;
    s.create(address)?;
    s.set_nonce(address, 1)?;
    if !s.transfer(parent.address, address, value)? {
        return Err(CapabilityError::Context("creation balance changed"));
    }
    let mut context = parent.clone();
    context.address = address;
    context.caller = parent.address;
    context.value = value;
    // go-quai `EVM.create` adds every new contract to the access-list tracer.
    if let Some(tracer) = s.debug.as_mut().and_then(|d| d.tracer.as_mut()) {
        tracer.add_address(address);
    }
    let mut out = run_code(s, &context, &[], gas, code.to_vec(), depth)?;
    if out.error.is_none() && !out.reverted {
        let max = if parent.block_number < Word::from_u64(FORKS.max_code_size) {
            24576
        } else {
            32768
        };
        if out.output.len() > max {
            out.error = Some(Fault::MaxCodeSize);
        } else if out.output.first() == Some(&0xef) {
            out.error = Some(Fault::InvalidCode);
        } else {
            let charge = out.output.len() as u64 * 200;
            if out.gas_remaining >= charge {
                out.gas_remaining -= charge;
                s.set_code(address, out.output.clone())?;
            } else {
                out.error = Some(Fault::CodeStoreOutOfGas);
            }
        }
    }
    // The pinned implementation deliberately retains creation state on a code
    // deposit gas failure. Preserve this historical exception for exact replay.
    let retained =
        out.error.is_none() && !out.reverted || out.error == Some(Fault::CodeStoreOutOfGas);
    if retained {
        s.commit(point)?;
    } else {
        s.revert(point)?;
        if !out.reverted {
            out.gas_remaining = 0;
        }
    }
    out.state_gas = fee;
    Ok((out, Some(address)))
}

/// Top-level CREATE keeps the creator nonce on EVM failures, matching the
/// transaction/frame boundary in Go; capability failures commit nothing.
pub fn execute_creation_with_env(
    world: &mut World,
    context: &Context,
    access: &[AccessTuple],
    code: &[u8],
    gas: u64,
    environment: &Environment,
) -> Result<(Outcome, Option<Address>), CapabilityError> {
    super::with_call_stack(world, |world| {
        creation_with_env(world, context, access, code, gas, environment)
    })
}
fn creation_with_env(
    world: &mut World,
    context: &Context,
    access: &[AccessTuple],
    code: &[u8],
    gas: u64,
    environment: &Environment,
) -> Result<(Outcome, Option<Address>), CapabilityError> {
    if !local(&context.caller, context.location) {
        return Err(CapabilityError::Context("local creator required"));
    }
    let mut state = Journal::new(world.clone())?;
    state.allow_calls = true;
    state.delete_empty = environment.delete_empty;
    state.prime_number = environment.prime_number;
    state.block_hashes = environment.block_hashes.clone();
    state.transaction_hash = environment.transaction_hash;
    state.eligible_slices = environment.eligible_slices;
    state.access_list = access.to_vec();
    // go-quai `StateDB.PrepareAccessList` adds the precompiles to a creation
    // too (core/state_transition.go:380, core/state/statedb.go:1302); only
    // the new contract's address must be in the transaction's list
    // (`EVM.create`, core/vm/evm.go:533). An ETX creation sends from the
    // zone's zero address to itself, so the sender is also the recipient.
    state
        .addresses
        .extend(super::precompile::active(context.location));
    state.addresses.insert(context.caller);
    for a in access {
        state.addresses.insert(a.address);
        state.slots.extend(a.keys.iter().map(|k| (a.address, *k)));
    }
    let outer = state.snapshot()?;
    let mut parent = context.clone();
    parent.address = context.caller;
    let (mut outcome, address) = create(&mut state, &parent, code, context.value, gas, None, 0)?;
    state.finalize_sizes()?;
    outcome.logs = state.logs.clone();
    outcome.outbound_etxs = state.etxs.clone();
    outcome.refund = state.refund;
    outcome.auxiliary = super::AuxiliaryEffects {
        deleted_hashes: state.deleted_hashes.clone(),
        deleted: (*state.deleted).clone(),
    };
    state.commit(outer)?;
    *world = state.into_world()?;
    Ok((outcome, address))
}
