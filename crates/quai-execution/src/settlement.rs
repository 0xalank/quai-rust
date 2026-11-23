//! Serial signed Quai transaction settlement. Consensus rejection is distinct
//! from an included transaction with an EVM failure. Mutation is atomic.
use crate::{
    evm::{
        self,
        calls::{self, Environment},
        state::{Account, World},
        word::{from_bytes, Word},
        CapabilityError, Outcome,
    },
    gas,
};
use crypto_bigint::U512;
use quai_primitives::spec::FORKS;
use quai_primitives::{
    receipt::{logs_bloom, Receipt},
    transaction::{Address, Kind, Transaction},
    Hash,
};
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    Unavailable(String),
    Unsupported(CapabilityError),
}
impl Error {
    pub(crate) fn utxo(error: crate::qi::ReadError, invalid: &'static str) -> Self {
        match error {
            crate::qi::ReadError::Unavailable(v) => Self::Unavailable(v),
            _ => Self::Invalid(invalid),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
impl From<CapabilityError> for Error {
    fn from(v: CapabilityError) -> Self {
        match v {
            CapabilityError::StateUnavailable(message) => Self::Unavailable(message),
            other => Self::Unsupported(other),
        }
    }
}
impl From<crate::state_error::ReadError> for Error {
    fn from(error: crate::state_error::ReadError) -> Self {
        CapabilityError::from(error).into()
    }
}

#[derive(Clone, Debug)]
pub struct BlockContext {
    pub vm: evm::Context,
    pub environment: Environment,
    pub gas_remaining: u64,
    pub cumulative_gas: u64,
    pub state_used: u64,
}
#[derive(Clone)]
pub struct AppliedTransaction {
    pub auxiliary: evm::AuxiliaryEffects,
    pub id: Hash,
    pub receipt: Receipt,
    pub gas_used: u64,
    pub state_used: u64,
    pub fee: Word,
    pub output: Vec<u8>,
    pub error: Option<evm::Fault>,
    pub reverted: bool,
    pub contract_address: Option<Address>,
}
/// An account that `set_balance` created or updated earlier in the same
/// transaction. Only a bug can make it absent; that stops settlement as
/// unavailable state and never rejects the transaction.
fn existing<'a>(world: &'a mut World, a: &Address) -> Result<&'a mut Account, Error> {
    world
        .account_mut(a)?
        .ok_or_else(|| Error::Unavailable("settled account missing".into()))
}
fn balance(world: &World, a: &Address) -> Result<Word, Error> {
    Ok(world.account(a)?.map(|v| v.balance).unwrap_or(Word::ZERO))
}
fn set_balance(world: &mut World, a: Address, v: Word) -> Result<(), Error> {
    if let Some(account) = world.account_mut(&a)? {
        account.balance = v;
    } else {
        world
            .insert(
                a,
                Account {
                    balance: v,
                    ..Default::default()
                },
            )
            .map_err(|error| {
                Error::from(CapabilityError::state(
                    error,
                    CapabilityError::Limit("settlement account"),
                ))
            })?;
    }
    Ok(())
}
fn add_balance(world: &mut World, a: Address, v: Word) -> Result<(), Error> {
    let old = balance(world, &a)?;
    let next = old.wrapping_add(&v);
    if next < old {
        return Err(Error::Unsupported(CapabilityError::Context(
            "balance overflow",
        )));
    }
    set_balance(world, a, next)
}
/// Applies an authenticated signed Quai transaction, with full ordinary gas and
/// nonce settlement. No block validity or durable publication is implied.
pub fn apply_quai(
    world: &mut World,
    transaction: &Transaction,
    block: &mut BlockContext,
) -> Result<AppliedTransaction, Error> {
    let Kind::Quai(tx) = transaction.kind() else {
        return Err(Error::Invalid("not a Quai transaction"));
    };
    if from_bytes(&tx.chain_id) != block.vm.chain_id {
        return Err(Error::Invalid("chain_id"));
    }
    let sender = transaction
        .sender()
        .map_err(|_| Error::Invalid("signature"))?;
    apply_authenticated(world, transaction, block, sender)
}
// Private to this module: production callers must pass signature recovery above.
fn apply_authenticated(
    world: &mut World,
    transaction: &Transaction,
    block: &mut BlockContext,
    sender: Address,
) -> Result<AppliedTransaction, Error> {
    let owned = world.begin_tx();
    match apply_authenticated_in_place(world, transaction, block, sender) {
        Ok(result) => {
            world.commit_tx(owned);
            Ok(result)
        }
        Err(err) => {
            world.rollback_tx(owned);
            Err(err)
        }
    }
}

fn apply_authenticated_in_place(
    world: &mut World,
    transaction: &Transaction,
    block: &mut BlockContext,
    sender: Address,
) -> Result<AppliedTransaction, Error> {
    let Kind::Quai(tx) = transaction.kind() else {
        return Err(Error::Invalid("not a Quai transaction"));
    };
    if from_bytes(&tx.chain_id) != block.vm.chain_id {
        return Err(Error::Invalid("chain_id"));
    }
    const KQUAI_AUTHORITY: Address = [
        0x00, 0x64, 0x0d, 0x82, 0xef, 0x65, 0x52, 0x08, 0x5e, 0x49, 0x4d, 0xf2, 0xa2, 0xea, 0xec,
        0x18, 0xd8, 0x21, 0x59, 0x13,
    ];
    if !calls::local(&sender, block.vm.location) {
        return Err(Error::Invalid("sender_scope"));
    }
    let id = transaction
        .id(Some(block.vm.location))
        .map_err(|_| Error::Invalid("transaction_id"))?;
    let account = world.account(&sender)?;
    let nonce = account.as_ref().map(|v| v.nonce).unwrap_or(0);
    if nonce < tx.nonce {
        return Err(Error::Invalid("nonce_too_high"));
    }
    if nonce > tx.nonce {
        return Err(Error::Invalid("nonce_too_low"));
    }
    if account.as_ref().is_some_and(|v| !v.code.is_empty()) {
        return Err(Error::Invalid("sender_not_eoa"));
    }
    let price = from_bytes(&tx.gas_price);
    let value = from_bytes(&tx.value);
    if price < block.vm.base_fee {
        return Err(Error::Invalid("fee_below_base"));
    }
    let wide_price: U512 = price.resize();
    let wide_value: U512 = value.resize();
    let cost = wide_price.wrapping_mul(&U512::from_u64(tx.gas));
    let required = cost.wrapping_add(&wide_value);
    let sender_balance = account.as_ref().map_or(Word::ZERO, |a| a.balance);
    let available: U512 = sender_balance.resize();
    if available < required {
        return Err(Error::Invalid("insufficient_funds"));
    }
    if tx.gas > block.gas_remaining {
        return Err(Error::Invalid("block_gas"));
    }
    let intrinsic = gas::intrinsic_gas(&tx.data, &tx.access_list, tx.to.is_none())
        .map_err(|_| Error::Invalid("intrinsic_gas_overflow"))?;
    if intrinsic > tx.gas {
        return Err(Error::Invalid("intrinsic_gas"));
    }
    // Validate pre-execution conditions before mutating `world` in place.
    let authority_height = if sender == KQUAI_AUTHORITY {
        Some(evm::word::uint(block.vm.block_number).ok_or(Error::Invalid("authority height"))?)
    } else {
        None
    };
    let selfdestruct_target = if sender != KQUAI_AUTHORITY {
        if let Some(target) = tx.to {
            let suicide = tx
                .data
                .strip_prefix(b"Suicide")
                .and_then(|rest| Address::try_from(rest).ok());
            if let (Some(beneficiary), true) = (suicide, target == sender) {
                if !calls::local(&beneficiary, block.vm.location) {
                    return Err(Error::Invalid("selfdestruct_beneficiary"));
                }
                let prime = block
                    .environment
                    .prime_number
                    .ok_or(CapabilityError::Context("selfdestruct prime context"))?;
                Some((beneficiary, prime))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    set_balance(world, sender, sender_balance.wrapping_sub(&cost.resize()))?;
    let mut context = block.vm.clone();
    context.caller = sender;
    context.origin = sender;
    context.gas_price = price;
    context.value = value;
    context.tx_type = 0;
    context.read_only = false;
    context.trace = false;
    let gas = tx.gas - intrinsic;
    let mut environment = block.environment.clone();
    environment.transaction_hash = Some(id);
    let mut deleted_sender = false;
    let mut early_return = false;
    let (outcome, contract_address) = if let Some(height) = authority_height {
        early_return = true;
        if height < crate::economic_state::YEAR {
            existing(world, &sender)?.nonce = nonce.wrapping_add(1);
        }
        let successful =
            crate::economic_state::authority(world, &tx.data, height, tx.to.is_none())?;
        (
            Outcome {
                auxiliary: Default::default(),
                error: if successful {
                    None
                } else {
                    Some(evm::Fault::Lockup)
                },
                reverted: false,
                output: vec![],
                gas_remaining: gas,
                state_gas: 0,
                refund: 0,
                logs: vec![],
                outbound_etxs: vec![],
                trace_hash: [0; 32],
                trace_steps: 0,
            },
            None,
        )
    } else if let Some((beneficiary, prime)) = selfdestruct_target {
        early_return = true;
        let amount = balance(world, &sender)?;
        if prime >= FORKS.polaris {
            existing(world, &sender)?.nonce = nonce.wrapping_add(1);
            set_balance(world, sender, Word::ZERO)?;
            if beneficiary != sender {
                add_balance(world, beneficiary, amount)?;
            }
        } else {
            set_balance(world, sender, Word::ZERO)?;
            let rent = Word::from_u64(evm::state_gas::scaled(
                context.state_size,
                Word::ZERO,
                25000,
            ))
            .wrapping_mul(&context.base_fee);
            add_balance(world, beneficiary, amount.wrapping_add(&rent))?;
            deleted_sender = true;
        }
        (
            Outcome {
                auxiliary: Default::default(),
                error: None,
                reverted: false,
                output: vec![],
                gas_remaining: gas,
                state_gas: 0,
                refund: 0,
                logs: vec![],
                outbound_etxs: vec![],
                trace_hash: [0; 32],
                trace_steps: 0,
            },
            None,
        )
    } else if let Some(target) = tx.to {
        context.address = target;
        existing(world, &sender)?.nonce = nonce.wrapping_add(1);
        (
            calls::execute_call_with_env(
                world,
                &context,
                &tx.access_list,
                &tx.data,
                gas,
                &environment,
            )?,
            None,
        )
    } else {
        calls::execute_creation_with_env(
            world,
            &context,
            &tx.access_list,
            &tx.data,
            gas,
            &environment,
        )?
    };
    let refund = if early_return {
        0
    } else {
        outcome.refund.min((tx.gas - outcome.gas_remaining) / 5)
    };
    let remaining = outcome
        .gas_remaining
        .checked_add(refund)
        .ok_or(Error::Invalid("gas_refund_overflow"))?;
    let used = tx
        .gas
        .checked_sub(remaining)
        .ok_or(Error::Invalid("gas_accounting"))?;
    let cumulative = block
        .cumulative_gas
        .checked_add(used)
        .ok_or(Error::Invalid("cumulative_gas_overflow"))?;
    let state_used = block
        .state_used
        .checked_add(outcome.state_gas)
        .ok_or(Error::Invalid("state_gas_overflow"))?;
    if !early_return {
        let returned = wide_price.wrapping_mul(&U512::from_u64(remaining));
        add_balance(world, sender, returned.resize())?;
    }
    if deleted_sender {
        world.remove(&sender)?;
    }
    if block.environment.delete_empty && (outcome.error.is_some() || outcome.reverted) {
        for (k, v) in &outcome.auxiliary.deleted {
            world.set_lockup(*k, Some(v.clone()))?;
        }
    }
    let receipt = Receipt {
        transaction_type: 0,
        post_state: vec![],
        status: u64::from(outcome.error.is_none() && !outcome.reverted),
        cumulative_gas_used: cumulative,
        bloom: logs_bloom(&outcome.logs),
        logs: outcome.logs,
        outbound_etxs: if outcome.error.is_none() && !outcome.reverted {
            outcome.outbound_etxs
        } else {
            vec![]
        },
    };
    let result = AppliedTransaction {
        auxiliary: if receipt.status == 1 {
            outcome.auxiliary
        } else {
            Default::default()
        },
        id,
        receipt,
        gas_used: used,
        state_used: outcome.state_gas,
        fee: price.wrapping_mul(&Word::from_u64(used)),
        output: outcome.output,
        error: outcome.error,
        reverted: outcome.reverted,
        contract_address,
    };
    if !world.in_block() {
        world.validate().map_err(|error| {
            CapabilityError::state(error, CapabilityError::Limit("settled world"))
        })?;
    }
    block.gas_remaining -= if early_return { tx.gas } else { used };
    block.cumulative_gas = cumulative;
    block.state_used = state_used;
    Ok(result)
}

/// Execute an already-ordered incoming account ETX. Relay eligibility and queue
/// membership belong to the enclosing block transition; there is no signature.
pub fn apply_external(
    world: &mut World,
    transaction: &Transaction,
    block: &mut BlockContext,
) -> Result<AppliedTransaction, Error> {
    let owned = world.begin_tx();
    match apply_external_in_place(world, transaction, block) {
        Ok(result) => {
            world.commit_tx(owned);
            Ok(result)
        }
        Err(err) => {
            world.rollback_tx(owned);
            Err(err)
        }
    }
}

fn apply_external_in_place(
    world: &mut World,
    transaction: &Transaction,
    block: &mut BlockContext,
) -> Result<AppliedTransaction, Error> {
    let Kind::External(tx) = transaction.kind() else {
        return Err(Error::Invalid("not an ETX"));
    };
    let id = transaction
        .id(None)
        .map_err(|_| Error::Invalid("ETX identity"))?;
    let prime = block
        .environment
        .prime_number
        .ok_or(CapabilityError::Context("ETX fork context"))?;
    let maximum = block.vm.block_gas_limit / 5;
    let oversize = tx.gas > maximum;
    if oversize && prime < FORKS.security_hardening {
        return Err(Error::Invalid("ETX gas limit"));
    }
    let reserved = if oversize { 21000 } else { tx.gas };
    if reserved > block.gas_remaining {
        return Err(Error::Invalid("block_gas"));
    }
    let mut zero = [0; 20];
    zero[0] = block.vm.location[0] << 4 | block.vm.location[1];
    let creation = tx.to == zero;
    let intrinsic = gas::intrinsic_gas(&tx.data, &tx.access_list, creation)
        .map_err(|_| Error::Invalid("intrinsic_gas_overflow"))?;
    if !oversize && tx.gas < intrinsic && prime < FORKS.security_hardening {
        return Err(Error::Invalid("intrinsic_gas"));
    }
    let previous = balance(world, &zero)?;
    let value = from_bytes(&tx.value);
    set_balance(world, zero, value)?;
    let (outcome, address) = if oversize || tx.gas < intrinsic {
        (
            Outcome {
                auxiliary: Default::default(),
                error: Some(evm::Fault::Lockup),
                reverted: false,
                output: vec![],
                gas_remaining: 0,
                state_gas: 10000,
                refund: 0,
                logs: vec![],
                outbound_etxs: vec![],
                trace_hash: [0; 32],
                trace_steps: 0,
            },
            None,
        )
    } else {
        let mut c = block.vm.clone();
        c.address = tx.to;
        c.caller = zero;
        c.origin = zero;
        c.etx_sender = tx.sender;
        c.tx_type = 1;
        c.value = value;
        c.gas_price = Word::ZERO;
        c.read_only = false;
        c.trace = false;
        let mut environment = block.environment.clone();
        environment.transaction_hash = Some(id);
        if creation {
            calls::execute_creation_with_env(
                world,
                &c,
                &tx.access_list,
                &tx.data,
                tx.gas - intrinsic,
                &environment,
            )?
        } else {
            let account = existing(world, &zero)?;
            account.nonce = account.nonce.wrapping_add(1);
            (
                calls::execute_call_with_env(
                    world,
                    &c,
                    &tx.access_list,
                    &tx.data,
                    tx.gas - intrinsic,
                    &environment,
                )?,
                None,
            )
        }
    };
    let refund = outcome.refund.min((reserved - outcome.gas_remaining) / 5);
    let remaining = outcome
        .gas_remaining
        .checked_add(refund)
        .ok_or(Error::Invalid("ETX refund"))?;
    let used = reserved - remaining;
    let cumulative = block
        .cumulative_gas
        .checked_add(used)
        .ok_or(Error::Invalid("ETX cumulative gas"))?;
    let state = block
        .state_used
        .checked_add(outcome.state_gas)
        .ok_or(Error::Invalid("ETX cumulative state"))?;
    if outcome.error.is_some() || outcome.reverted {
        for (k, v) in &outcome.auxiliary.deleted {
            world.set_lockup(*k, Some(v.clone()))?;
        }
    }
    set_balance(world, zero, previous)?;
    // This dirty zero account is finalized at the block boundary in Go.
    if world.account(&zero)?.is_some_and(|a| {
        a.nonce == 0 && a.balance == Word::ZERO && a.code.is_empty() && a.storage_size == Word::ZERO
    }) {
        world.remove(&zero)?;
    }
    let receipt = Receipt {
        transaction_type: 1,
        post_state: vec![],
        status: u64::from(outcome.error.is_none() && !outcome.reverted),
        cumulative_gas_used: cumulative,
        bloom: logs_bloom(&outcome.logs),
        logs: outcome.logs,
        outbound_etxs: if outcome.error.is_none() && !outcome.reverted {
            outcome.outbound_etxs
        } else {
            vec![]
        },
    };
    let result = AppliedTransaction {
        auxiliary: if receipt.status == 1 {
            outcome.auxiliary
        } else {
            Default::default()
        },
        id,
        receipt,
        gas_used: used,
        state_used: outcome.state_gas,
        fee: Word::ZERO,
        output: outcome.output,
        error: outcome.error,
        reverted: outcome.reverted,
        contract_address: address,
    };
    block.gas_remaining -= used;
    block.cumulative_gas = cumulative;
    block.state_used = state;
    Ok(result)
}
#[cfg(test)]
mod authority_tests {
    use super::*;
    use quai_primitives::{decode_hex, encode_hex};
    use serde_json::Value;
    use std::collections::BTreeSet;
    #[test]
    fn authenticated_authority_transition_matches_go_and_public_api_requires_signature() {
        let corpus: Value =
            serde_json::from_slice(include_bytes!("../../../fixtures/protocol/authority.json"))
                .unwrap();
        assert_eq!(
            corpus["reference_revision"],
            quai_primitives::REFERENCE_REVISION
        );
        let mut checks = 0;
        for row in corpus["vectors"].as_array().unwrap() {
            let tx =
                Transaction::decode(&decode_hex(row["transaction"].as_str().unwrap()).unwrap())
                    .unwrap();
            let sender: Address = decode_hex(row["sender"].as_str().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            let mut target = [0; 20];
            target[18] = 0x11;
            target[19] = 1;
            let mut world = World::default();
            world
                .insert(
                    sender,
                    Account {
                        nonce: 7,
                        balance: Word::ONE.shl_vartime(128),
                        ..Default::default()
                    },
                )
                .unwrap();
            world
                .insert(
                    target,
                    Account {
                        nonce: 1,
                        balance: Word::from_u64(100),
                        ..Default::default()
                    },
                )
                .unwrap();
            let mut block = BlockContext {
                vm: evm::Context {
                    address: target,
                    caller: sender,
                    origin: sender,
                    coinbase: [0; 20],
                    etx_sender: [0; 20],
                    location: [0, 0],
                    tx_type: 0,
                    value: Word::ZERO,
                    gas_price: Word::from_u64(17),
                    block_number: Word::from_u64(row["height"].as_u64().unwrap()),
                    timestamp: Word::from_u64(1769112000),
                    difficulty: Word::ONE,
                    base_fee: Word::from_u64(13),
                    chain_id: Word::from_u64(1337),
                    state_size: Word::from_u64(1 << 20),
                    block_gas_limit: 30000000,
                    address_listed: false,
                    warm_slots: BTreeSet::new(),
                    read_only: false,
                    trace: false,
                },
                environment: Environment {
                    prime_number: Some(FORKS.polaris),
                    delete_empty: true,
                    ..Default::default()
                },
                gas_remaining: 20000000,
                cumulative_gas: 0,
                state_used: 0,
            };
            assert_eq!(
                Value::String(encode_hex(&world.root().unwrap())),
                row["before_root"],
                "{}",
                row["id"]
            );
            checks += 1;
            // Only a module-private test can exercise the authenticated transition with
            // this unsigned fixture. The public transaction API must reject it atomically.
            let before = world.clone();
            let mut rejected_block = block.clone();
            assert!(apply_quai(&mut world, &tx, &mut rejected_block).is_err());
            assert_eq!(world, before);
            assert_eq!(rejected_block.gas_remaining, block.gas_remaining);
            checks += 1;
            let result = apply_authenticated(&mut world, &tx, &mut block, sender).unwrap();
            for (field, actual) in [
                (
                    "root",
                    serde_json::json!(encode_hex(&world.root().unwrap())),
                ),
                (
                    "etx_root",
                    serde_json::json!(encode_hex(&world.etxs().root().unwrap())),
                ),
                ("gas_used", serde_json::json!(result.gas_used)),
                ("state_used", serde_json::json!(result.state_used)),
                ("gas_remaining", serde_json::json!(block.gas_remaining)),
                (
                    "fee",
                    serde_json::json!(encode_hex(&crate::economic_state::magnitude(result.fee))),
                ),
                ("status", serde_json::json!(result.receipt.status == 1)),
            ] {
                assert_eq!(actual, row[field], "{} {field}", row["id"]);
                checks += 1;
            }
        }
        println!("AUTHORITY_CHECKS cases=21 checks={checks}");
    }
}
