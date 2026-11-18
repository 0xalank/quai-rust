//! Message calls for the RPC: go-quai `core.ApplyMessage` as `DoCall`,
//! `DoEstimateGas` and `AccessList` run it (internal/quaiapi/api.go), with
//! gas price zero and `vm.Config{Debug: true, NoBaseFee: true}`.
//!
//! The debug flag makes `StateDB.PrepareAccessList` set
//! `bypassAccessListCheck` (core/state/statedb.go): every account and slot
//! is warm and no access-list check fails. `apply` runs the message on a
//! journal in that mode (`Journal::debug`), optionally with an access-list
//! tracer (`access_list`), and discards every change. Consensus execution
//! never sets the mode, so block results cannot change.
//!
//! The results carry go-quai's error texts: `core.ErrIntrinsicGas` and the
//! other consensus errors of `StateTransition.TransitionDb`, and the EVM
//! errors of the top frame (core/vm/errors.go, `EVM.CreateETX`,
//! `GrindContract`, `Address.InternalAndQuaiAddress`). Errors of inner
//! frames never reach a call result.
use super::{
    access_list::AccessListTracer,
    calls::{self, Environment, Kind},
    journal::Journal,
    opcodes,
    state::World,
    word::{from_bytes, low_u64, uint, Word},
    CapabilityError, Context, Fault, Outcome,
};
use quai_primitives::{
    spec::FORKS,
    transaction::{keccak, AccessTuple, Address},
};

/// `kQuaiSettingAddress` (core/state_transition.go).
pub const KQUAI_AUTHORITY: Address = [
    0x00, 0x64, 0x0d, 0x82, 0xef, 0x65, 0x52, 0x08, 0x5e, 0x49, 0x4d, 0xf2, 0xa2, 0xea, 0xec, 0x18,
    0xd8, 0x21, 0x59, 0x13,
];
/// `params.MinQuaiConversionAmount`.
const MIN_CONVERSION: u128 = 10_000_000_000_000_000_000;

/// The RPC call mode of a journal.
#[derive(Default)]
pub(crate) struct Debug {
    pub(crate) tracer: Option<AccessListTracer>,
    /// The Go detail of the last fault of the top frame.
    pub(crate) top_fault: Option<FaultDetail>,
}

/// What Go's error of a top-frame fault says beyond the `Fault` itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaultDetail {
    /// `ErrStackUnderflow{stackLen, required}`.
    StackUnderflow { len: usize, required: usize },
    /// `ErrStackOverflow{stackLen, limit}`.
    StackOverflow { len: usize, limit: usize },
    /// `ErrInvalidOpCode{opcode}`.
    InvalidOpcode(u8),
    /// `Address.InternalAndQuaiAddress` refused this address.
    Scope(Address),
    /// A dynamic gas function failed, which Go reports as `ErrOutOfGas`.
    OutOfGas,
}

/// A call message (`TransactionArgs.ToMessage`): gas price zero and the
/// sender's state nonce.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Message {
    pub from: Address,
    /// None for a contract creation.
    pub to: Option<Address>,
    pub value: [u8; 32],
    pub gas: u64,
    pub data: Vec<u8>,
    pub access_list: Vec<AccessTuple>,
}

/// `core.ExecutionResult` of a message that ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Executed {
    /// `UsedGas`, after the refund.
    pub used_gas: u64,
    /// `Err.Error()`; None on success.
    pub error: Option<String>,
    /// `Err == vm.ErrExecutionReverted`.
    pub reverted: bool,
    /// `ReturnData`: the output of a success or of a revert.
    pub return_data: Vec<u8>,
    /// The access-list tracer after the run, when `apply` had one.
    pub tracer: Option<AccessListTracer>,
}

/// What `core.ApplyMessage` returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    Done(Executed),
    /// A consensus error instead of a result; `intrinsic` for
    /// `core.ErrIntrinsicGas`.
    Rejected {
        error: String,
        intrinsic: bool,
    },
}

/// The precompiles of `location` (`vm.ActivePrecompiles`): `0x…01` to
/// `0x…09`, with and without the location prefix.
pub fn precompiles(location: [u8; 2]) -> Vec<Address> {
    let mut out = vec![];
    for id in 1..=9u8 {
        let mut a = [0; 20];
        a[19] = id;
        out.push(a);
        a[0] = location[0] << 4 | location[1];
        out.push(a);
    }
    out
}

/// EIP-55 hex, as `common.Address.Hex` and `%v` print an address.
pub fn checksum(a: &Address) -> String {
    let lower = hex(a);
    let hash = keccak(lower.as_bytes());
    let mixed: String = lower
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let nibble = hash[i / 2] >> (if i % 2 == 0 { 4 } else { 0 }) & 0xf;
            if c.is_ascii_alphabetic() && nibble > 7 {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect();
    format!("0x{mixed}")
}
/// Lower-case hex without "0x" (`%x` of an address).
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
/// Decimal digits (`%v` and `%d` of a `*big.Int`).
pub fn decimal(v: Word) -> String {
    let mut n = super::word::bytes(v);
    let mut digits = vec![];
    while n.iter().any(|b| *b != 0) {
        let mut rem = 0u32;
        for b in n.iter_mut() {
            let cur = rem << 8 | *b as u32;
            *b = (cur / 10) as u8;
            rem = cur % 10;
        }
        digits.push(b'0' + rem as u8);
    }
    if digits.is_empty() {
        return "0".into();
    }
    digits.reverse();
    String::from_utf8(digits).unwrap_or_default()
}
/// `common.IsInChainScope` of a zone.
pub fn in_scope(a: &Address, location: [u8; 2]) -> bool {
    *a == [0; 20] || a[0] == (location[0] << 4 | location[1])
}
/// The error of `Address.InternalAndQuaiAddress`, if any.
pub fn scope_error(a: &Address, location: [u8; 2]) -> Option<String> {
    if a[1] > 127 {
        return Some(format!(
            "expected Quai address, but found Qi address: {}",
            checksum(a)
        ));
    }
    if !in_scope(a, location) {
        return Some("Address belongs to other zone".into());
    }
    None
}
/// Go's `%v` of a byte slice: "[1 2 3]".
fn byte_list(b: &[u8]) -> String {
    let items: Vec<String> = b.iter().map(|v| v.to_string()).collect();
    format!("[{}]", items.join(" "))
}

fn rejected(error: String) -> Applied {
    Applied::Rejected {
        error,
        intrinsic: false,
    }
}

/// Run `message` on `world` in the block of `block` (its block fields) and
/// `environment`, as `core.ApplyMessage` with gas price zero in the debug
/// mode, and discard every change. With `tracer`, the tracer records what
/// the call touches and comes back in the result.
pub fn apply(
    world: &World,
    block: &Context,
    environment: &Environment,
    message: &Message,
    tracer: Option<AccessListTracer>,
) -> Result<Applied, CapabilityError> {
    let location = block.location;
    let from = message.from;
    // preCheck: a local Quai sender with the state nonce (by construction)
    // and without code; buyGas checks gas * 0 + value against the balance.
    if let Some(e) = scope_error(&from, location) {
        return Ok(rejected(e));
    }
    let account = world.account(&from)?;
    if let Some(a) = account.as_ref().filter(|a| !a.code.is_empty()) {
        return Ok(rejected(format!(
            "sender not an eoa: address {}, codehash: 0x{}",
            checksum(&from),
            hex(&keccak(&a.code))
        )));
    }
    let balance = account.as_ref().map_or(Word::ZERO, |a| a.balance);
    let value = from_bytes(&message.value);
    if balance < value {
        return Ok(rejected(format!(
            "insufficient funds for gas * price + value: address {} have {} want {}",
            checksum(&from),
            decimal(balance),
            decimal(value)
        )));
    }
    let creation = message.to.is_none();
    let Ok(intrinsic) = crate::gas::intrinsic_gas(&message.data, &message.access_list, creation)
    else {
        return Ok(rejected("gas uint64 overflow".into()));
    };
    if message.gas < intrinsic {
        return Ok(Applied::Rejected {
            error: format!(
                "intrinsic gas too low: have {}, want {intrinsic}",
                message.gas
            ),
            intrinsic: true,
        });
    }
    let gas = message.gas - intrinsic;
    let done = |error: Option<String>, tracer: Option<AccessListTracer>| {
        Applied::Done(Executed {
            used_gas: intrinsic,
            error,
            reverted: false,
            return_data: vec![],
            tracer,
        })
    };
    // The kQuai setting address runs no code.
    if from == KQUAI_AUTHORITY {
        let height = uint(block.block_number).unwrap_or(u64::MAX);
        if height >= crate::economic_state::YEAR {
            return Ok(done(
                Some(
                    "transaction is not allowed from the kQuaiSettingAddress after the first year"
                        .into(),
                ),
                tracer,
            ));
        }
        let data = message.data.as_slice();
        let valid = !creation
            && ((data.len() > 6 && data.starts_with(b"update"))
                || data == b"freeze"
                || data == b"unfreeze");
        return Ok(done(
            (!valid).then(|| format!("invalid data used: {}", byte_list(data))),
            tracer,
        ));
    }
    // A self-destruct request: "Suicide" and a beneficiary, to the sender.
    if message.to == Some(from) && message.data.len() == 27 && message.data.starts_with(b"Suicide")
    {
        let mut beneficiary = [0; 20];
        beneficiary.copy_from_slice(&message.data[7..]);
        if let Some(e) = scope_error(&beneficiary, location) {
            return Ok(rejected(format!("Unable to self-destruct: {e}")));
        }
        return Ok(done(None, tracer));
    }
    let mut world = world.clone();
    world.begin_block();
    let (outcome, refund, debug) = super::with_call_stack(&mut world, |world| {
        run(
            world,
            block,
            environment,
            message,
            value,
            gas,
            tracer.clone(),
        )
    })?;
    let used = message.gas - outcome.gas_remaining;
    let used_gas = used - refund.min(used / 5);
    let error = match outcome.error {
        _ if outcome.reverted => Some("execution reverted".to_string()),
        None => None,
        Some(fault) => Some(fault_text(
            fault,
            debug.top_fault.as_ref(),
            &Top {
                block,
                environment,
                message,
                balance,
                gas,
            },
        )),
    };
    Ok(Applied::Done(Executed {
        used_gas,
        error,
        reverted: outcome.reverted,
        return_data: if outcome.error.is_none() {
            outcome.output
        } else {
            vec![]
        },
        tracer: debug.tracer,
    }))
}

/// `PrepareAccessList`, then `EVM.Create` or the nonce increment and
/// `EVM.Call`. Returns the top outcome, the refund counter and the mode.
fn run(
    world: &mut World,
    block: &Context,
    environment: &Environment,
    message: &Message,
    value: Word,
    gas: u64,
    tracer: Option<AccessListTracer>,
) -> Result<(Outcome, u64, Debug), CapabilityError> {
    let from = message.from;
    let mut state = Journal::new(world.clone())?;
    state.allow_calls = true;
    state.delete_empty = environment.delete_empty;
    state.prime_number = environment.prime_number;
    state.block_hashes = environment.block_hashes.clone();
    state.transaction_hash = environment.transaction_hash;
    state.eligible_slices = environment.eligible_slices;
    state.access_list = message.access_list.clone();
    state.addresses.extend(precompiles(block.location));
    state.addresses.insert(from);
    state.addresses.extend(message.to);
    for a in &message.access_list {
        state.addresses.insert(a.address);
        state.slots.extend(a.keys.iter().map(|k| (a.address, *k)));
    }
    state.debug = Some(Box::new(Debug {
        tracer,
        top_fault: None,
    }));
    let outer = state.snapshot()?;
    let mut parent = block.clone();
    parent.address = from;
    parent.caller = from;
    parent.origin = from;
    parent.value = value;
    parent.gas_price = Word::ZERO;
    parent.tx_type = 0;
    parent.etx_sender = [0; 20];
    parent.read_only = false;
    parent.trace = false;
    parent.address_listed = false;
    parent.warm_slots.clear();
    let outcome = match message.to {
        None => calls::create(&mut state, &parent, &message.data, value, gas, None, 0)?.0,
        Some(to) => {
            let nonce = state.world().account(&from)?.map_or(0, |a| a.nonce);
            state.set_nonce(from, nonce.wrapping_add(1))?;
            calls::invoke(
                &mut state,
                &parent,
                Kind::Call,
                to,
                value,
                &message.data,
                gas,
                0,
            )?
        }
    };
    // Frames that fail revert the refund counter as Go's snapshots do; a
    // creation that fails only to store its code keeps it, also as in Go.
    let refund = state.refund;
    state.revert(outer)?;
    let debug = state.debug.take().map(|d| *d).unwrap_or_default();
    Ok((outcome, refund, debug))
}

/// The facts of the top frame that Go's error texts name.
struct Top<'a> {
    block: &'a Context,
    environment: &'a Environment,
    message: &'a Message,
    balance: Word,
    /// The gas of the top frame (after intrinsic gas).
    gas: u64,
}

/// Go's error text of a top-frame fault.
fn fault_text(fault: Fault, detail: Option<&FaultDetail>, top: &Top) -> String {
    match (fault, detail) {
        (Fault::StackUnderflow, Some(FaultDetail::StackUnderflow { len, required })) => {
            format!("stack underflow ({len} <=> {required})")
        }
        (Fault::StackOverflow, Some(FaultDetail::StackOverflow { len, limit })) => {
            format!("stack limit reached {len} ({limit})")
        }
        (Fault::InvalidOpcode, Some(FaultDetail::InvalidOpcode(op))) => {
            format!("invalid opcode: {}", opcodes::name(*op))
        }
        (Fault::ExternalAddress, Some(FaultDetail::OutOfGas)) => "out of gas".into(),
        (Fault::ExternalAddress, Some(FaultDetail::Scope(a))) => scope_error(a, top.block.location)
            .unwrap_or_else(|| "Address belongs to other zone".into()),
        (Fault::Etx, _) => create_etx_error(top),
        (Fault::AddressGrind, _) => grind_error(top),
        (Fault::Precompile, _) => top
            .message
            .to
            .and_then(|to| super::precompile::translate(to, top.block.location))
            .and_then(|(id, _)| precompile_error(id, &top.message.data))
            .unwrap_or_else(|| plain(fault).into()),
        _ => plain(fault).into(),
    }
}

/// The text of a fault without operands (core/vm/errors.go).
pub fn plain(f: Fault) -> &'static str {
    match f {
        Fault::OutOfGas => "out of gas",
        Fault::StackUnderflow => "stack underflow",
        Fault::StackOverflow => "stack limit reached",
        Fault::InvalidJump => "invalid jump destination",
        Fault::WriteProtection => "write protection",
        Fault::ReturnDataBounds => "return data out of bounds",
        Fault::GasOverflow => "gas uint64 overflow",
        Fault::InvalidOpcode => "invalid opcode",
        Fault::InvalidAccessList => "invalid access list",
        Fault::Depth => "max call depth exceeded",
        Fault::InsufficientBalance => "insufficient balance for transfer",
        Fault::ContractCollision => "contract address collision",
        Fault::InvalidCode => "invalid code: must not begin with 0xef",
        Fault::CodeStoreOutOfGas => "contract creation code storage out of gas",
        Fault::AddressGrind => "contract address grinding failed",
        Fault::ExternalAddress => "Address belongs to other zone",
        Fault::MaxCodeSize => "max code size exceeded",
        Fault::Precompile => "precompile failed",
        Fault::Lockup => "lockup error",
        Fault::Etx => "ETX error",
    }
}

/// `EVM.CreateETX` (core/vm/evm.go) for a top-level call to an address
/// outside the zone's Quai scope: the first check that fails, in Go's order.
fn create_etx_error(top: &Top) -> String {
    let Some(to) = top.message.to else {
        return plain(Fault::Etx).into();
    };
    let from = top.message.from;
    let value = from_bytes(&top.message.value);
    let prime = top.environment.prime_number.unwrap_or(0);
    let location = top.block.location;
    let same = in_scope(&to, location);
    let qi = to[1] > 127;
    let conversion = qi && same;
    let hold = FORKS.k_quai_change_hold;
    if conversion && prime < FORKS.controller_kick_in {
        return format!(
            "CreateETX conversion error: Quai->Qi conversion is not allowed before block {}",
            FORKS.controller_kick_in
        );
    }
    if conversion {
        for fork in [FORKS.kawpow, FORKS.sha_equivalent_difficulty] {
            if prime >= fork && prime < fork + hold {
                return format!(
                    "CreateETX error: ETX is not eligible to be sent to {} until block {}",
                    hex(&to),
                    fork + hold
                );
            }
        }
    }
    if qi && !same {
        return format!(
            "{} is in qi scope and is not in the same location, but CreateETX was called",
            hex(&to)
        );
    } else if conversion && value < Word::from_u128(MIN_CONVERSION) {
        return format!(
            "CreateETX conversion error: {} is not sufficient value, required amount: {MIN_CONVERSION}",
            decimal(value)
        );
    }
    let gas = top.gas;
    if gas < 21000 {
        return format!("CreateETX error: {gas} is not sufficient gas, required amount: 21000");
    }
    let gas = gas - 21000;
    if gas < 21000 {
        return format!(
            "CreateETX error: {gas} is not sufficient gas for ETX, required amount: 21000"
        );
    }
    if !conversion && prime >= FORKS.security_hardening {
        // etxGasCoversIntrinsicForRecipient
        let creation = to[1..].iter().all(|b| *b == 0);
        let covered =
            crate::gas::intrinsic_gas(&top.message.data, &top.message.access_list, creation)
                .is_ok_and(|required| required <= gas);
        if !covered {
            return format!("CreateETX error: {gas} is not sufficient gas for ETX intrinsic cost");
        }
    }
    if top.balance < value {
        return format!(
            "CreateETX: {} cannot transfer {}",
            hex(&from),
            low_u64(value)
        );
    }
    let eligible = top.environment.eligible_slices.is_some_and(|bits| {
        let n = to[0] as usize;
        bits[n / 8] & (1 << (n % 8)) != 0
    });
    if !conversion && !eligible {
        return format!(
            "CreateETX error: ETX is not eligible to be sent to {}",
            hex(&to)
        );
    }
    plain(Fault::Etx).into()
}

/// The BN254 base field modulus, big-endian.
const BN254_P: [u8; 32] = [
    0x30, 0x64, 0x4e, 0x72, 0xe1, 0x31, 0xa0, 0x29, 0xb8, 0x50, 0x45, 0xb6, 0x81, 0x81, 0x58, 0x5d,
    0x97, 0x81, 0x6a, 0x91, 0x68, 0x71, 0xca, 0x8d, 0x3c, 0x20, 0x8c, 0x16, 0xd8, 0x7c, 0xfd, 0x47,
];
/// `gfP.Unmarshal` of github.com/dominant-strategies/bn256.
fn coordinate(b: &[u8]) -> Result<substrate_bn::Fq, String> {
    match b.cmp(&BN254_P[..]) {
        std::cmp::Ordering::Greater => Err("bn256: coordinate exceeds modulus".into()),
        std::cmp::Ordering::Equal => Err("bn256: coordinate equals modulus".into()),
        std::cmp::Ordering::Less => {
            substrate_bn::Fq::from_slice(b).map_err(|_| "bn256: coordinate exceeds modulus".into())
        }
    }
}
/// `G1.Unmarshal` of 64 bytes: the error Go returns, if any.
fn g1_error(b: &[u8]) -> Option<String> {
    let x = match coordinate(&b[..32]) {
        Ok(v) => v,
        Err(e) => return Some(e),
    };
    let y = match coordinate(&b[32..64]) {
        Ok(v) => v,
        Err(e) => return Some(e),
    };
    if b.iter().all(|v| *v == 0) {
        return None;
    }
    substrate_bn::AffineG1::new(x, y)
        .err()
        .map(|_| "bn256: malformed point".into())
}
/// `G2.Unmarshal` of 128 bytes. Go checks only that the point is on the
/// curve; a point outside the subgroup passes there.
fn g2_error(b: &[u8]) -> Option<String> {
    let mut c = vec![];
    for i in 0..4 {
        match coordinate(&b[i * 32..i * 32 + 32]) {
            Ok(v) => c.push(v),
            Err(e) => return Some(e),
        }
    }
    if b.iter().all(|v| *v == 0) {
        return None;
    }
    let x = substrate_bn::Fq2::new(c[1], c[0]);
    let y = substrate_bn::Fq2::new(c[3], c[2]);
    match substrate_bn::AffineG2::new(x, y) {
        Err(substrate_bn::GroupError::NotOnCurve) => Some("bn256: malformed point".into()),
        _ => None,
    }
}
/// The error of a precompile run (core/vm/contracts.go) on `input`, for a
/// top-level call that failed in Rust. None where Go would not fail.
fn precompile_error(id: u8, input: &[u8]) -> Option<String> {
    let padded = |n: usize| {
        let mut b = input.to_vec();
        b.resize(b.len().max(n), 0);
        b
    };
    match id {
        6 => {
            let b = padded(128);
            g1_error(&b[..64]).or_else(|| g1_error(&b[64..128]))
        }
        7 => g1_error(&padded(96)[..64]),
        8 => {
            if input.len() % 192 != 0 {
                return Some("bad elliptic curve pairing size".into());
            }
            input
                .chunks_exact(192)
                .find_map(|p| g1_error(&p[..64]).or_else(|| g2_error(&p[64..])))
        }
        9 => {
            if input.len() != 213 {
                Some("invalid input length".into())
            } else if input[212] > 1 {
                Some("invalid final flag".into())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `GrindContract` of a top-level creation: Go stops with out of gas when
/// the gas ends before the attempts.
fn grind_error(top: &Top) -> String {
    let attempts: u64 =
        if uint(top.block.block_number).unwrap_or(u64::MAX) < FORKS.max_grind_increase {
            1000
        } else {
            10000
        };
    let cost = 30 + (top.message.data.len() as u64).div_ceil(32) * 6;
    let sender = checksum(&top.message.from);
    if (top.gas as u128) < attempts as u128 * cost as u128 {
        format!("out of gas grinding contract address for {sender}")
    } else {
        format!("exceeded number of attempts grinding address {sender}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn texts_follow_go() {
        assert_eq!(decimal(Word::ZERO), "0");
        assert_eq!(decimal(Word::from_u64(1234567890123)), "1234567890123");
        assert_eq!(
            decimal(Word::MAX),
            "115792089237316195423570985008687907853269984665640564039457584007913129639935"
        );
        assert_eq!(byte_list(b"ab"), "[97 98]");
        assert_eq!(byte_list(&[]), "[]");
        // EIP-55 vector.
        let mut a = [0u8; 20];
        a.copy_from_slice(
            &quai_primitives::decode_hex("0x5aaeb6053f3e94c9b9a09f33669435e7ef1beaed").unwrap(),
        );
        assert_eq!(checksum(&a), "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed");
        assert_eq!(precompiles([0, 0]).len(), 18);
    }
    #[test]
    fn precompile_errors_follow_go() {
        assert_eq!(
            precompile_error(9, &[0; 212]).as_deref(),
            Some("invalid input length")
        );
        let mut flag = [0u8; 213];
        flag[212] = 2;
        assert_eq!(
            precompile_error(9, &flag).as_deref(),
            Some("invalid final flag")
        );
        assert_eq!(
            precompile_error(8, &[0; 100]).as_deref(),
            Some("bad elliptic curve pairing size")
        );
        assert_eq!(
            precompile_error(6, &[0xff; 64]).as_deref(),
            Some("bn256: coordinate exceeds modulus")
        );
        assert_eq!(
            precompile_error(7, &BN254_P).as_deref(),
            Some("bn256: coordinate equals modulus")
        );
        // (1, 1) is not on y^2 = x^3 + 3.
        let mut point = [0u8; 64];
        point[31] = 1;
        point[63] = 1;
        assert_eq!(
            precompile_error(6, &point).as_deref(),
            Some("bn256: malformed point")
        );
        // The generator (1, 2) is on the curve; zeros are the infinity.
        point[63] = 2;
        assert_eq!(precompile_error(6, &point), None);
        assert_eq!(precompile_error(6, &[0; 128]), None);
    }
}
