//! Lockup contract dispatch preserves its distinct gas and snapshot boundaries.
use super::{
    calls,
    journal::Journal,
    word::{bytes, from_bytes, uint, Word},
    CapabilityError, Context, Fault, Outcome,
};
use crate::lockup::{self, Key};
use quai_primitives::spec::FORKS;
use quai_primitives::transaction::{Address, External, Transaction};
enum Failure {
    Capability(CapabilityError),
    Revert,
    Fault(Fault),
}
impl From<crate::state_error::ReadError> for Failure {
    fn from(e: crate::state_error::ReadError) -> Self {
        match e {
            crate::state_error::ReadError::Unavailable(v) => {
                Self::Capability(CapabilityError::StateUnavailable(v))
            }
            _ => Self::Fault(Fault::Lockup),
        }
    }
}
impl From<CapabilityError> for Failure {
    fn from(e: CapabilityError) -> Self {
        match e {
            CapabilityError::StateUnavailable(_) => Self::Capability(e),
            _ => Self::Fault(Fault::Lockup),
        }
    }
}
impl From<Fault> for Failure {
    fn from(v: Fault) -> Self {
        Self::Fault(v)
    }
}
fn magnitude(v: Word) -> Vec<u8> {
    let b = bytes(v);
    b[b.iter().position(|v| *v != 0).unwrap_or(32)..].to_vec()
}
/// `N` bytes of `input` at `at`. Each arm of `run` handles one input length,
/// so every read succeeds. Go's handlers recheck the length and fail with a
/// plain error, which is `Fault::Lockup` here.
fn field<const N: usize>(input: &[u8], at: usize) -> Result<[u8; N], Failure> {
    input
        .get(at..at + N)
        .and_then(|v| v.try_into().ok())
        .ok_or(Failure::Fault(Fault::Lockup))
}
fn local_any(a: &Address, loc: [u8; 2]) -> bool {
    a[0] == loc[0] << 4 | loc[1]
}
fn emit(
    s: &mut Journal,
    c: &Context,
    to: Address,
    value: Word,
    gas: u64,
    kind: u64,
) -> Result<(), Failure> {
    if s.etxs.len() > u16::MAX as usize {
        return Err(Fault::Lockup.into());
    }
    let tx = Transaction::external(External {
        originating_hash: s.transaction_hash.ok_or(Fault::Lockup)?,
        index: s.etxs.len() as u16,
        gas,
        to,
        value: magnitude(value),
        data: vec![],
        access_list: vec![],
        sender: c.address,
        etx_type: kind,
    })
    .map_err(|_| Fault::Lockup)?;
    s.etxs.push(tx);
    Ok(())
}
fn run(s: &mut Journal, c: &Context, input: &[u8], gas: &mut u64) -> Result<Vec<u8>, Failure> {
    if c.read_only {
        return Err(Fault::WriteProtection.into());
    }
    let address = lockup::contract(c.location);
    match input.len() {
        20 => {
            let owner: Address = field(input, 0)?;
            // ClaimQiDeposit: `quaiOwner.InternalAndQuaiAddress()`
            // (core/vm/contracts.go:897).
            if !calls::local(&c.address, c.location) || owner[1] & 128 != 0 {
                return Err(Fault::Lockup.into());
            }
            if !local_any(&owner, c.location) {
                return Err(Fault::ExternalAddress.into());
            }
            let k = lockup::deposit_key(c.address, owner);
            let balance = s.storage(&address, k)?;
            if balance == [0; 32] {
                return Err(Fault::Lockup.into());
            }
            s.set_storage(address, k, [0; 32])?;
            let key = lockup::owner_key(c.address);
            let old = s.storage(&address, key)?;
            s.set_storage(
                address,
                key,
                bytes(from_bytes(&old).wrapping_add(&from_bytes(&balance))),
            )?;
            Ok(balance.to_vec())
        }
        60 => {
            let to: Address = field(input, 0)?;
            let value = from_bytes(&field::<32>(input, 20)?);
            let limit = u64::from_be_bytes(field(input, 52)?);
            if *gas < limit {
                return Err(Fault::OutOfGas.into());
            }
            *gas -= limit;
            // UnwrapQi: `beneficiaryQi.InternalAndQiAddress()` checks the
            // ledger, then the zone (core/vm/contracts.go:800).
            if to[1] & 128 == 0 {
                return Err(Fault::Lockup.into());
            }
            if !local_any(&to, c.location) {
                return Err(Fault::ExternalAddress.into());
            }
            if !calls::local(&c.address, c.location) {
                return Err(Fault::Lockup.into());
            }
            let key = lockup::owner_key(c.address);
            let balance = from_bytes(&s.storage(&address, key)?);
            if balance == Word::ZERO || balance < value {
                return Err(Fault::Lockup.into());
            }
            s.set_storage(address, key, bytes(balance.wrapping_sub(&value)))?;
            emit(s, c, to, value, limit, 6)?;
            Ok(vec![1])
        }
        21 | 25 | 53 => {
            let beneficiary: Address = field(input, 0)?;
            let height = uint(c.block_number).ok_or(Fault::Lockup)?;
            let (lock, epoch) = if input.len() == 53 {
                let [lock] = field(input, 40)?;
                (lock, u32::from_be_bytes(field(input, 41)?))
            } else if input.len() == 25 {
                let [lock] = field(input, 20)?;
                (lock, u32::from_be_bytes(field(input, 21)?))
            } else {
                let [lock] = field(input, 20)?;
                (lock, (height / 50000 + 1) as u32)
            };
            let key = Key {
                owner: c.address,
                beneficiary,
                lock,
                epoch,
            };
            // GetLockupData: `beneficiaryMiner.InternalAddress()` returns
            // common.ErrExternalAddress for another zone, of either ledger
            // (core/vm/contracts.go:552).
            if input.len() == 25 {
                if !calls::local(&c.address, c.location) {
                    return Err(Fault::Lockup.into());
                }
                if !local_any(&beneficiary, c.location) {
                    return Err(Fault::ExternalAddress.into());
                }
            }
            let value = s.world().lockup(&key)?.unwrap_or_default();
            if input.len() != 53 {
                return Ok(value.abi());
            }
            let to: Address = field(input, 20)?;
            let limit = u64::from_be_bytes(field(input, 45)?);
            if *gas < limit {
                return Err(Fault::OutOfGas.into());
            }
            *gas -= limit;
            // ClaimCoinbaseLockup: the owner, then
            // `beneficiaryMiner.InternalAddress()`, then the other checks
            // (core/vm/contracts.go:660-685).
            if !calls::local(&c.address, c.location) {
                return Err(Fault::Lockup.into());
            }
            if !local_any(&beneficiary, c.location) {
                return Err(Fault::ExternalAddress.into());
            }
            if epoch >= (height / 50000 + 1) as u32
                || (beneficiary[1] & 128) != (to[1] & 128)
                || value.unlock_height == 0
                || value.unlock_height > height as u32
                || value.elements == 0
            {
                return Err(Fault::Lockup.into());
            }
            s.delete_lockup(key, value.clone())?;
            emit(s, c, to, value.balance, limit, 3)?;
            Ok(vec![1])
        }
        _ => Err(Failure::Revert),
    }
}
pub(crate) fn invoke(
    s: &mut Journal,
    c: &Context,
    input: &[u8],
    mut gas: u64,
) -> Result<Outcome, CapabilityError> {
    let revision = s.snapshot()?;
    let prime = s
        .prime_number
        .ok_or(CapabilityError::Context("lockup fork context"))?;
    let outcome = run(s, c, input, &mut gas);
    let mut out = calls::result(s, gas, None);
    match outcome {
        Ok(data) => {
            out.output = data;
            s.commit(revision)?;
        }
        Err(Failure::Capability(error)) => return Err(error),
        Err(e) => {
            if prime >= FORKS.sha_equivalent_difficulty {
                s.revert(revision)?;
            } else {
                s.commit(revision)?;
            }
            match e {
                Failure::Revert => out.reverted = true,
                Failure::Fault(e) => out.error = Some(e),
                Failure::Capability(error) => return Err(error),
            }
        }
    }
    Ok(out)
}
