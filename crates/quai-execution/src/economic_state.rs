//! Block economic state operations follow the pinned prime/zone fork contexts.
use crate::{
    evm::{
        state::{Account, World},
        word::{bytes, from_bytes, Word},
    },
    settlement::Error,
};
use crypto_bigint::U512;
use quai_primitives::{
    spec::{self, FORKS},
    transaction::Address,
};
/// params.BlocksPerDay.
pub const DAY: u64 = spec::BLOCKS_PER_DAY;
/// params.BlocksPerMonth.
pub const MONTH: u64 = spec::BLOCKS_PER_MONTH;
/// params.BlocksPerYear.
pub const YEAR: u64 = spec::BLOCKS_PER_YEAR;
/// params.ConversionLockPeriod.
pub const CONVERSION_LOCK: u64 = spec::LOCK_PERIODS.conversion;
/// params.LockupByteToBlockDepth.
pub const LOCK_PERIODS: [u64; 4] = spec::LOCK_PERIODS.by_lock_byte;
pub fn magnitude(v: Word) -> Vec<u8> {
    let b = bytes(v);
    b[b.iter().position(|v| *v != 0).unwrap_or(32)..].to_vec()
}
pub fn reward(value: Word, lock: u8, height: u64) -> Result<Word, Error> {
    if lock > 3 {
        return Err(Error::Invalid("coinbase lock byte"));
    }
    // params.CalculateCoinbaseValueWithLockup.
    if lock == 0 || height < FORKS.lockup_start {
        return Ok(value);
    }
    let first = [0, 103500, 110000, 125000];
    let last = [0, 100218, 100625, 101562];
    let year = height / YEAR;
    let multiple = if year == 0 {
        first[lock as usize]
    } else if year > 4 {
        last[lock as usize]
    } else {
        let a = last[lock as usize] as i128 - first[lock as usize] as i128;
        ((a * (height - YEAR) as i128 + 4 * YEAR as i128 * first[lock as usize] as i128)
            / (4 * YEAR) as i128) as u64
    };
    let wide: U512 = value.resize();
    let v = wide
        .wrapping_mul(&U512::from_u64(multiple))
        .wrapping_div(&U512::from_u64(100000));
    if v.bits_vartime() > 256 {
        return Err(Error::Unsupported(crate::evm::CapabilityError::Context(
            "coinbase reward magnitude",
        )));
    }
    Ok(v.resize())
}
pub fn set_balance(world: &mut World, a: Address, v: Word) -> Result<(), Error> {
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
                Error::from(crate::evm::CapabilityError::state(
                    error,
                    crate::evm::CapabilityError::Limit("account creation"),
                ))
            })?;
    }
    Ok(())
}
pub fn add_balance(world: &mut World, a: Address, v: Word) -> Result<(), Error> {
    let old = world.account(&a)?.map(|v| v.balance).unwrap_or(Word::ZERO);
    let n = old.wrapping_add(&v);
    if n < old {
        return Err(Error::Unsupported(crate::evm::CapabilityError::Context(
            "balance magnitude",
        )));
    }
    set_balance(world, a, n)
}
pub fn authority(
    world: &mut World,
    data: &[u8],
    height: u64,
    creation: bool,
) -> Result<bool, Error> {
    if height >= YEAR || creation {
        return Ok(false);
    }
    let result = if data.starts_with(b"update") && data.len() > 6 {
        Some((250, {
            let b = &data[6..];
            b[b.iter().position(|v| *v != 0).unwrap_or(b.len())..].to_vec()
        }))
    } else if data == b"freeze" {
        Some((251, vec![0]))
    } else if data == b"unfreeze" {
        Some((251, vec![1]))
    } else {
        None
    };
    let Some((last, value)) = result else {
        return Ok(false);
    };
    let mut key = vec![0; 4];
    key.extend([255; 28]);
    key[31] = last;
    match world.etxs_mut().set_auxiliary(key, value) {
        Ok(()) => Ok(true),
        Err(crate::state_error::ReadError::Unavailable(value)) => Err(Error::Unavailable(value)),
        Err(_) => Ok(false),
    }
}
pub fn word(v: &[u8]) -> Word {
    from_bytes(v)
}
