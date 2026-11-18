//! Bounded Qi input-value/ownership/lock prechecks matching ValidateQiTxInputs.
//! No state mutations, complete output policy, fees, signatures or block validity.
use quai_primitives::{
    transaction::{keccak, Kind, Transaction},
    Hash,
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};
pub const DENOMINATIONS: [u64; 15] = [
    1,
    5,
    10,
    50,
    100,
    500,
    1000,
    5000,
    10_000,
    20_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
];
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OutPoint {
    pub hash: Hash,
    pub index: u16,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Utxo {
    pub denomination: u8,
    pub address: [u8; 20],
    pub lock: Vec<u8>,
}
pub use crate::state_error::ReadError;
pub trait UtxoView {
    fn get(&self, outpoint: &OutPoint) -> Result<Option<Cow<'_, Utxo>>, ReadError>;
}
impl UtxoView for BTreeMap<OutPoint, Utxo> {
    fn get(&self, outpoint: &OutPoint) -> Result<Option<Cow<'_, Utxo>>, ReadError> {
        Ok(BTreeMap::get(self, outpoint).map(Cow::Borrowed))
    }
}
#[derive(Clone, Debug)]
pub struct Context {
    pub chain_id: Vec<u8>,
    pub block_height: u64,
    pub location: [u8; 2],
}
fn locked(lock: &[u8], height: u64) -> bool {
    let lock = &lock[lock.iter().position(|b| *b != 0).unwrap_or(lock.len())..];
    if lock.len() > 8 {
        return true;
    }
    let mut value = [0; 8];
    value[8 - lock.len()..].copy_from_slice(lock);
    u64::from_be_bytes(value) > height
}
fn zero_magnitude(value: &[u8]) -> bool {
    value.iter().all(|v| *v == 0)
}
/// Matches the reference input helper's checks and total within 4,096 inputs.
/// It intentionally does not claim transaction validity: cross-transaction spending,
/// signatures, output values/destinations, fees and fork rules need later stages.
pub fn check_input_values(
    transaction: &Transaction,
    view: &impl UtxoView,
    context: &Context,
) -> Result<u64, ReadError> {
    let Kind::Qi(tx) = transaction.kind() else {
        return Err("not a Qi transaction".into());
    };
    if context.location.iter().any(|v| *v > 15) {
        return Err("invalid zone location".into());
    }
    if tx.inputs.is_empty() {
        return Err("Qi transaction requires an input".into());
    }
    unique_inputs(transaction)?;
    let expected = &context.chain_id[context
        .chain_id
        .iter()
        .position(|v| *v != 0)
        .unwrap_or(context.chain_id.len())..];
    if tx.chain_id != expected {
        return Err("wrong chain ID".into());
    }
    if !matches!(tx.data.len(), 0 | 20 | 22) {
        return Err("invalid Qi data length".into());
    }
    if tx.data.len() == 20 && tx.data[1] & 0x80 != 0 {
        return Err("wrapping contract must be in Quai ledger".into());
    }
    if tx.data.len() == 22 && tx.data[3] & 0x80 == 0 {
        return Err("refund address must be in Qi ledger".into());
    }
    let mut total = 0u64;
    for input in &tx.inputs {
        let utxo = view
            .get(&OutPoint {
                hash: input.hash,
                index: input.index,
            })?
            .ok_or("missing UTXO")?;
        if locked(&utxo.lock, context.block_height) {
            return Err("locked UTXO".into());
        }
        // Transaction decoding stores every input key as a 65-byte uncompressed
        // SEC1 point; the address hashes the point without its tag byte.
        let Some((_, point)) = input.public_key.split_first() else {
            return Err("invalid input public key".into());
        };
        let address = crate::evm::word::low_address(keccak(point));
        if address[1] & 0x80 == 0 {
            return Err("UTXO owner is not in Qi ledger".into());
        }
        if address != utxo.address {
            return Err("UTXO public-key ownership mismatch".into());
        }
        total = total
            .checked_add(
                *DENOMINATIONS
                    .get(utxo.denomination as usize)
                    .ok_or("invalid input denomination")?,
            )
            .ok_or("input value overflow")?;
    }
    for output in &tx.outputs {
        if output.denomination as usize >= DENOMINATIONS.len() {
            return Err("invalid output denomination".into());
        }
        if !zero_magnitude(&output.lock) {
            return Err("Qi output has nonzero lock".into());
        }
    }
    Ok(total)
}
/// Duplicate outpoint invariant now enforced by the upstream input helper.
/// A real block transition must also remove spends from its isolated state view
/// so subsequent transactions cannot spend them again.
pub fn unique_inputs(transaction: &Transaction) -> Result<Vec<OutPoint>, String> {
    let Kind::Qi(tx) = transaction.kind() else {
        return Err("not a Qi transaction".into());
    };
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::with_capacity(tx.inputs.len());
    for input in &tx.inputs {
        let point = OutPoint {
            hash: input.hash,
            index: input.index,
        };
        if !seen.insert(point) {
            return Err("duplicate Qi input".into());
        }
        ordered.push(point);
    }
    Ok(ordered)
}
