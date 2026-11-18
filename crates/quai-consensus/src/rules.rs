//! Exact bounded consensus rule arithmetic. These projections are not block validity.
use crate::Error;
use crypto_bigint::{Encoding, U256, U512};
use quai_primitives::{
    spec::{self, FORKS},
    Hash,
};
pub type Uint = U256;
fn wide(n: Uint) -> U512 {
    n.resize()
}
fn narrow(n: U512) -> Result<Uint, Error> {
    if n.bits_vartime() > 256 {
        return Err(Error::Unsupported("integer exceeds 256-bit domain"));
    }
    Ok(n.resize())
}
fn add(a: Uint, b: Uint) -> Result<Uint, Error> {
    narrow(wide(a).wrapping_add(&wide(b)))
}
fn mul(a: Uint, b: Uint) -> Result<Uint, Error> {
    narrow(wide(a).wrapping_mul(&wide(b)))
}
fn log(n: Uint) -> Uint {
    Uint::from_u128(quai_execution::evm::state_gas::log_q64(n))
}
/// Intrinsic entropy uses log2(floor(2^256/hash)), not 256-log2(hash).
pub fn intrinsic(hash: Hash) -> Result<Uint, Error> {
    let n = Uint::from_be_bytes(hash);
    if n == Uint::ZERO {
        return Err(Error::Unsupported(
            "zero hash has undefined reference entropy",
        ));
    }
    let d = U512::ONE.shl_vartime(256).wrapping_div(&wide(n));
    if d == U512::ONE.shl_vartime(256) {
        return Ok(Uint::from_u64(256).shl_vartime(64));
    }
    Ok(log(d.resize()))
}
/// params.PrimeEntropyTarget and RegionEntropyTarget.
pub fn entropy_targets(expansion: u8) -> (u64, u64) {
    let (regions, zones) = spec::hierarchy_size(expansion);
    (
        regions.max(2) * zones.max(2) * regions * zones,
        zones.max(2) * zones,
    )
}
/// Mirrors block versus workshare PoW-ID rules, including the inclusive transition end.
pub fn pow_id_valid(prime: u64, aux_id: Option<u8>, workshare: bool) -> bool {
    if prime < FORKS.kawpow {
        return aux_id.is_none();
    }
    if !workshare {
        return (aux_id.is_none() || aux_id == Some(1))
            && (prime <= FORKS.progpow_grace_end() || aux_id.is_some());
    }
    if prime <= FORKS.progpow_grace_end() {
        return aux_id.is_none_or(|id| id <= 4);
    }
    aux_id.is_some_and(|id| id > 0 && id <= 4)
}
/// Ordinary parent/grandparent adjustment. Genesis/expansion initialization is separate.
pub fn adjust_difficulty(
    parent: Uint,
    min: Uint,
    parent_time: u64,
    grandparent_time: u64,
    duration: u64,
) -> Result<Uint, Error> {
    if parent == Uint::ZERO || min == Uint::ZERO || duration == 0 {
        return Err(Error::Invalid("positive difficulty/configuration required"));
    }
    let delta = (parent_time as i128 - grandparent_time as i128)
        .min(spec::MAX_TIME_DIFF_BETWEEN_BLOCKS as i128);
    let change = duration as i128 - delta;
    let value = wide(parent)
        .wrapping_mul(&U512::from_u128(change.unsigned_abs()))
        .wrapping_mul(&U512::from_u64(parent.bits_vartime() as u64 - 1));
    let denominator = U512::from_u64(duration).wrapping_mul(&U512::from_u64(
        spec::DIFFICULTY_ADJUSTMENT_FACTOR * spec::DIFFICULTY_ADJUSTMENT_PERIOD,
    ));
    let quotient = value.wrapping_div(&denominator);
    let next = if change >= 0 {
        wide(parent).wrapping_add(&quotient)
    } else {
        let ceil = quotient.wrapping_add(&if value.wrapping_rem(&denominator) != U512::ZERO {
            U512::ONE
        } else {
            U512::ZERO
        });
        if ceil > wide(parent) {
            return Ok(min);
        }
        wide(parent).wrapping_sub(&ceil)
    };
    Ok(narrow(next)?.max(min))
}
#[derive(Clone, Debug)]
pub struct EntropyInput {
    pub hash: Hash,
    pub difficulty: Uint,
    pub expansion: u8,
    pub parent_entropy: [Uint; 3],
    pub parent_delta: [Uint; 3],
    pub parent_uncled_delta: [Uint; 3],
    pub uncled_entropy: Uint,
    pub postfork_share_count: Option<u32>,
    pub node_context: u8,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entropy {
    pub intrinsic: Uint,
    pub order: u8,
    pub rank: Option<u8>,
    pub total: Uint,
    pub delta: Uint,
    pub uncled_delta: Uint,
    pub workshares: Uint,
}
/// Calculate PoEM fields from an already computed hash. No seal/aux validity is implied.
pub fn entropy(input: &EntropyInput) -> Result<Entropy, Error> {
    if input.node_context > 2 {
        return Err(Error::Invalid("node context"));
    }
    if input.difficulty == Uint::ZERO {
        return Err(Error::Invalid("difficulty must be positive"));
    }
    if input.difficulty == Uint::ONE {
        return Err(Error::Unsupported(
            "reference target truncates to zero at difficulty one",
        ));
    }
    if !quai_primitives::meets_blake3_target(&input.hash, &input.difficulty.to_be_bytes())
        .map_err(|_| Error::Invalid("difficulty"))?
    {
        return Err(Error::Invalid("proof target"));
    }
    let intrinsic = intrinsic(input.hash)?;
    let target = U512::ONE
        .shl_vartime(256)
        .wrapping_div(&wide(input.difficulty));
    let threshold = intrinsic_of_target(target)?;
    let (prime_target, region_target) = entropy_targets(input.expansion);
    let prime_limit = mul(Uint::from_u64(prime_target), threshold)?.shr_vartime(1);
    let region_limit = mul(Uint::from_u64(region_target), threshold)?.shr_vartime(1);
    let prime_bits = log(Uint::from_u64(prime_target));
    let region_bits = log(Uint::from_u64(region_target));
    let order = if intrinsic > add(threshold, prime_bits)?
        && add(
            add(input.parent_delta[1], input.parent_delta[2])?,
            intrinsic,
        )? > prime_limit
    {
        0
    } else if intrinsic > add(threshold, region_bits)?
        && add(input.parent_delta[2], intrinsic)? > region_limit
    {
        1
    } else {
        2
    };
    let workshares = if input.node_context == 2 {
        match input.postfork_share_count {
            Some(n) => {
                if n > 32 {
                    return Err(Error::Unsupported("more than bounded workshare count"));
                }
                log(Uint::from_u64(n as u64))
            }
            None => Uint::ZERO,
        }
    } else {
        Uint::ZERO
    };
    let s = add(intrinsic, workshares)?;
    let (total, delta, uncled_delta) = match order {
        0 => (
            add(
                add(
                    add(input.parent_entropy[0], input.parent_delta[1])?,
                    input.parent_delta[2],
                )?,
                s,
            )?,
            Uint::ZERO,
            Uint::ZERO,
        ),
        1 => (
            add(add(input.parent_entropy[1], input.parent_delta[2])?, s)?,
            add(add(input.parent_delta[1], input.parent_delta[2])?, s)?,
            add(
                add(input.parent_uncled_delta[1], input.parent_uncled_delta[2])?,
                input.uncled_entropy,
            )?,
        ),
        _ => (
            add(input.parent_entropy[2], s)?,
            add(input.parent_delta[2], s)?,
            add(input.parent_uncled_delta[2], input.uncled_entropy)?,
        ),
    };
    let rank = if order != 0 {
        None
    } else {
        let mut rank = 0;
        for i in (1..=4).rev() {
            let limit = add(
                add(threshold, Uint::from_u64(i).shl_vartime(64))?,
                prime_bits,
            )?;
            if intrinsic > limit {
                rank = i as u8;
                break;
            }
        }
        Some(rank)
    };
    Ok(Entropy {
        intrinsic,
        order,
        rank,
        total,
        delta,
        uncled_delta,
        workshares,
    })
}
fn intrinsic_of_target(target: U512) -> Result<Uint, Error> {
    intrinsic(narrow(target)?.to_be_bytes())
}
/// Gas and state ceiling schedule (core/block_validator.go CalcGasLimit,
/// consensus/misc/statefee.go CalcStateLimit); u64 multiplication wraps as Go does.
pub fn next_limit(parent_number: u64, parent_limit: u64, ceiling: u64) -> u64 {
    if parent_number < FORKS.start_tx {
        0
    } else if parent_limit == 0 {
        spec::MIN_GAS_LIMIT
    } else if parent_number < FORKS.lockup_start {
        (parent_number.wrapping_mul(ceiling) / FORKS.lockup_start).max(spec::MIN_GAS_LIMIT)
    } else {
        ceiling
    }
}
/// Workshare target is floor(2^256/difficulty) shifted after integer division.
/// Shifts above 255 exceed this component's bounded arithmetic domain.
pub fn share_target(difficulty: Uint, shift: u16) -> Result<U512, Error> {
    if difficulty == Uint::ZERO || shift == 0 {
        return Err(Error::Invalid("positive difficulty and threshold required"));
    }
    if shift > 255 {
        return Err(Error::Unsupported("threshold exceeds bounded domain"));
    }
    Ok(U512::ONE
        .shl_vartime(256)
        .wrapping_div(&wide(difficulty))
        .shl_vartime(shift as usize))
}

// Add authenticated pre-fork share weight after order/rank calculation: the
// reference includes it in total entropy and non-prime delta, never in order.
pub(crate) fn add_pre_fork_work(e: &mut Entropy, weight: Uint) -> Result<(), Error> {
    e.workshares = add(e.workshares, weight)?;
    e.total = add(e.total, weight)?;
    if e.order != 0 {
        e.delta = add(e.delta, weight)?;
    }
    Ok(())
}
