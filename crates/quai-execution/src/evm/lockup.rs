//! Lockup contract dispatch preserves its distinct gas and snapshot boundaries.
use super::{
    calls,
    journal::Journal,
    word::{bytes, from_bytes, uint, Word},
    CapabilityError, Context, Fault, Outcome,
};
use crate::lockup::{self, Key};
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
            let owner: Address = input.try_into().unwrap();
            if !calls::local(&c.address, c.location) || !calls::local(&owner, c.location) {
                return Err(Fault::Lockup.into());
            }
            let k = lockup::deposit_key(c.address, owner);
            let balance = s
                .world()
                .account(&address)?
                .map(|v| v.get(k))
                .transpose()?
                .unwrap_or([0; 32]);
            if balance == [0; 32] {
                return Err(Fault::Lockup.into());
            }
            s.set_storage(address, k, [0; 32])?;
            let key = lockup::owner_key(c.address);
            let old = s.world().account(&address)?.unwrap().get(key)?;
            s.set_storage(
                address,
                key,
                bytes(from_bytes(&old).wrapping_add(&from_bytes(&balance))),
            )?;
            Ok(balance.to_vec())
        }
        60 => {
            let to: Address = input[..20].try_into().unwrap();
            let value = from_bytes(&input[20..52]);
            let limit = u64::from_be_bytes(input[52..].try_into().unwrap());
            if *gas < limit {
                return Err(Fault::OutOfGas.into());
            }
            *gas -= limit;
            if !local_any(&to, c.location)
                || to[1] & 128 == 0
                || !calls::local(&c.address, c.location)
            {
                return Err(Fault::Lockup.into());
            }
            let key = lockup::owner_key(c.address);
            let balance = s
                .world()
                .account(&address)?
                .map(|v| v.get(key))
                .transpose()?
                .map(|v| from_bytes(&v))
                .unwrap_or(Word::ZERO);
            if balance == Word::ZERO || balance < value {
                return Err(Fault::Lockup.into());
            }
            s.set_storage(address, key, bytes(balance.wrapping_sub(&value)))?;
            emit(s, c, to, value, limit, 6)?;
            Ok(vec![1])
        }
        21 | 25 | 53 => {
            let beneficiary: Address = input[..20].try_into().unwrap();
            let height = uint(c.block_number).ok_or(Fault::Lockup)?;
            let (lock, epoch) = if input.len() == 53 {
                (
                    input[40],
                    u32::from_be_bytes(input[41..45].try_into().unwrap()),
                )
            } else if input.len() == 25 {
                (
                    input[20],
                    u32::from_be_bytes(input[21..25].try_into().unwrap()),
                )
            } else {
                (input[20], (height / 50000 + 1) as u32)
            };
            let key = Key {
                owner: c.address,
                beneficiary,
                lock,
                epoch,
            };
            if input.len() == 25
                && (!calls::local(&c.address, c.location) || !local_any(&beneficiary, c.location))
            {
                return Err(Fault::Lockup.into());
            }
            let value = s.world().lockup(&key)?.unwrap_or_default();
            if input.len() != 53 {
                return Ok(value.abi());
            }
            let to: Address = input[20..40].try_into().unwrap();
            let limit = u64::from_be_bytes(input[45..53].try_into().unwrap());
            if *gas < limit {
                return Err(Fault::OutOfGas.into());
            }
            *gas -= limit;
            if !calls::local(&c.address, c.location)
                || !local_any(&beneficiary, c.location)
                || epoch >= (height / 50000 + 1) as u32
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
            if prime >= 1_755_000 {
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
