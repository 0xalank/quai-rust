//! Read-only message calls for `quai_call` and `quai_estimateGas`: go-quai's
//! `DoCall` (internal/quaiapi/api.go) and `StateTransition.TransitionDb`
//! (core/state_transition.go) for a message with gas price zero, on a clone
//! of the state. Every change is dropped. Only the public `quai_execution`
//! entry points are used.
//!
//! Known difference: go-quai runs these calls with `vm.Config{Debug: true}`,
//! which turns off access-list checks (`PrepareAccessList(..., debug)`), so
//! every account and slot counts as warm and none is missing from the list.
//! `quai_execution` has no such mode: a call that touches accounts or slots
//! outside the target, the sender, the precompiles and the given access list
//! pays cold-access gas, and BALANCE, EXTCODE*, CALL and SELFDESTRUCT on an
//! unlisted account fail with "invalid access list". Calls that touch only
//! listed accounts and slots give go-quai's result.
use crate::rpc_view::ExportView;
use quai_execution::{
    evm::{
        self,
        calls::{self, Environment},
        state::{Account, World},
        word::{bytes, from_bytes, Word},
        Fault,
    },
    gas,
};
use quai_primitives::{transaction::keccak, Hash};
use quai_rpc::view::{BlockData, CallError, CallRequest, CallResult, ChainView, ViewError};
use std::collections::BTreeSet;

/// `kQuaiSettingAddress` (core/state_transition.go).
const KQUAI_AUTHORITY: [u8; 20] = [
    0x00, 0x64, 0x0d, 0x82, 0xef, 0x65, 0x52, 0x08, 0x5e, 0x49, 0x4d, 0xf2, 0xa2, 0xea, 0xec, 0x18,
    0xd8, 0x21, 0x59, 0x13,
];

fn message(text: String) -> CallError {
    CallError::Message {
        text,
        intrinsic_gas: false,
    }
}
fn view_error(e: impl std::fmt::Display) -> CallError {
    CallError::View(ViewError::Unavailable(format!("execution: {e}")))
}
/// Decimal digits of a 256-bit value (Go `%v` of a `*big.Int`).
pub fn decimal(v: &[u8; 32]) -> String {
    let mut digits = vec![];
    let mut n = *v;
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
/// The go-quai error text of an EVM fault (core/vm/errors.go). Stack and
/// opcode errors carry operands in Go that `Outcome` does not report.
fn fault_text(f: Fault) -> &'static str {
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
        Fault::ExternalAddress => "address is not in the scope of this chain",
        Fault::MaxCodeSize => "max code size exceeded",
        Fault::Precompile => "precompile failed",
        Fault::Lockup => "lockup error",
        Fault::Etx => "ETX error",
    }
}

/// Run `request` on `world` at `block`, as `DoCall` with `NoBaseFee`.
pub fn call(
    view: &ExportView,
    mut world: World,
    block: &BlockData,
    request: &CallRequest,
) -> Result<CallResult, CallError> {
    let from = request.from;
    let checksum = |a: &[u8; 20]| quai_rpc::native::json::checksum(a);
    if from == KQUAI_AUTHORITY {
        return Err(CallError::View(ViewError::Unavailable(
            "calls from the kQuai setting address".into(),
        )));
    }
    // preCheck: the nonce is the state nonce by construction; the sender must
    // not have code; buyGas checks gas * 0 + value against the balance.
    let account = world.account(&from).map_err(view_error)?;
    if let Some(a) = account.as_ref().filter(|a| !a.code.is_empty()) {
        return Err(message(format!(
            "sender not an eoa: address {}, codehash: {}",
            checksum(&from),
            quai_primitives::encode_hex(&keccak(&a.code))
        )));
    }
    let balance = account
        .as_ref()
        .map(|a| bytes(a.balance))
        .unwrap_or([0; 32]);
    if balance < request.value {
        return Err(message(format!(
            "insufficient funds for gas * price + value: address {} have {} want {}",
            checksum(&from),
            decimal(&balance),
            decimal(&request.value)
        )));
    }
    let creation = request.to.is_none();
    let intrinsic =
        gas::intrinsic_gas(&request.data, &request.access_list, creation).map_err(view_error)?;
    if request.gas < intrinsic {
        return Err(CallError::Message {
            text: format!(
                "intrinsic gas too low: have {}, want {intrinsic}",
                request.gas
            ),
            intrinsic_gas: true,
        });
    }
    let available = request.gas - intrinsic;
    // A self-destruct request ("Suicide" + beneficiary, to the sender).
    if let Some(to) = request.to {
        if to == from && request.data.len() == 27 && request.data.starts_with(b"Suicide") {
            return Ok(CallResult {
                used_gas: intrinsic,
                error: None,
                reverted: false,
                return_data: vec![],
            });
        }
    }
    let context = context(view, block, request)?;
    let environment = environment(view, block)?;
    world.begin_block();
    let outcome = if creation {
        calls::execute_creation_with_env(
            &mut world,
            &context,
            &request.access_list,
            &request.data,
            available,
            &environment,
        )
        .map_err(view_error)?
        .0
    } else {
        // The sender's nonce goes up before the call.
        let mut a: Account = account.as_deref().cloned().unwrap_or_default();
        a.nonce = a.nonce.wrapping_add(1);
        world.insert(from, a).map_err(view_error)?;
        calls::execute_call_with_env(
            &mut world,
            &context,
            &request.access_list,
            &request.data,
            available,
            &environment,
        )
        .map_err(view_error)?
    };
    // refundGas: at most a fifth of the gas used.
    let used = request.gas - outcome.gas_remaining;
    let refund = outcome.refund.min(used / 5);
    let error = if outcome.reverted {
        Some("execution reverted".to_string())
    } else {
        outcome.error.map(|f| fault_text(f).to_string())
    };
    Ok(CallResult {
        used_gas: used - refund,
        reverted: outcome.reverted,
        return_data: if outcome.reverted || outcome.error.is_none() {
            outcome.output
        } else {
            vec![]
        },
        error,
    })
}

/// `NewEVMBlockContext` and `NewEVMTxContext` for a call message.
fn context(
    view: &ExportView,
    block: &BlockData,
    request: &CallRequest,
) -> Result<evm::Context, CallError> {
    let w = block.header.wire();
    let body = block.object.header().wire();
    let parent = view.parent_header(block)?;
    let genesis_parent = block.number == 1;
    let coinbase: [u8; 20] = w
        .primary_coinbase
        .as_ref()
        .and_then(|c| c.value.as_slice().try_into().ok())
        .unwrap_or([0; 20]);
    Ok(evm::Context {
        address: request.to.unwrap_or([0; 20]),
        caller: request.from,
        origin: request.from,
        coinbase,
        etx_sender: [0; 20],
        location: view.location(),
        tx_type: 0,
        value: from_bytes(&request.value),
        gas_price: Word::ZERO,
        block_number: Word::from_u64(block.number),
        timestamp: Word::from_u64(parent.header().wire().time.unwrap_or_default()),
        difficulty: from_bytes(w.difficulty.as_deref().unwrap_or_default()),
        base_fee: from_bytes(body.base_fee.as_deref().unwrap_or_default()),
        chain_id: Word::from_u64(view.chain_id()),
        // The parent's state size; the genesis parent counts as zero.
        state_size: if genesis_parent {
            Word::ZERO
        } else {
            from_bytes(
                parent
                    .object()
                    .header()
                    .wire()
                    .quai_state_size
                    .as_deref()
                    .unwrap_or_default(),
            )
        },
        block_gas_limit: body.gas_limit.unwrap_or_default(),
        address_listed: false,
        warm_slots: BTreeSet::new(),
        read_only: false,
        trace: false,
    })
}
fn environment(view: &ExportView, block: &BlockData) -> Result<Environment, CallError> {
    let body = block.object.header().wire();
    let prime_hash: Hash = body
        .prime_terminus_hash
        .as_ref()
        .and_then(|h| h.value.as_slice().try_into().ok())
        .unwrap_or([0; 32]);
    let eligible = view
        .header_by_hash(&prime_hash)?
        .and_then(|r| {
            r.object()
                .header()
                .wire()
                .etx_eligible_slices
                .as_ref()
                .and_then(|h| h.value.as_slice().try_into().ok())
        })
        .ok_or_else(|| CallError::View(ViewError::Unavailable("prime terminus header".into())))?;
    Ok(Environment {
        prime_number: Some(
            block
                .header
                .wire()
                .prime_terminus_number
                .as_deref()
                .map_or(0, |b| {
                    let mut n = [0u8; 8];
                    let take = b.len().min(8);
                    n[8 - take..].copy_from_slice(&b[b.len() - take..]);
                    u64::from_be_bytes(n)
                }),
        ),
        block_hashes: view.ancestor_hashes(block),
        transaction_hash: None,
        eligible_slices: Some(eligible),
        delete_empty: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decimals() {
        let mut v = [0u8; 32];
        assert_eq!(decimal(&v), "0");
        v[31] = 1;
        assert_eq!(decimal(&v), "1");
        v[30] = 1;
        assert_eq!(decimal(&v), "257");
        assert_eq!(
            decimal(&[0xff; 32]),
            "115792089237316195423570985008687907853269984665640564039457584007913129639935"
        );
    }
}
