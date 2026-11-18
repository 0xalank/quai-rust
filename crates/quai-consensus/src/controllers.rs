//! Prime economic controller arithmetic from the pinned official source.
//! Callers must authenticate history and conversion inputs independently.
use crate::{rules::Uint, Error};
use crypto_bigint::{U1024, U512};
use quai_primitives::spec::FORKS;

/// params.MinerDifficultyWindow (= params.TokenChoiceSetSize).
pub const WINDOW: u64 = 4_000;
pub const DISCOUNT_SCALE: u64 = 100_000;

fn wide(n: Uint) -> U512 {
    n.resize()
}
fn u(n: u64) -> U512 {
    U512::from_u64(n)
}
fn narrow(n: U512) -> Result<Uint, Error> {
    if n.bits_vartime() > 256 {
        return Err(Error::Unsupported("controller result exceeds 256 bits"));
    }
    Ok(n.resize())
}

/// Go applies the minimum before the two-times-previous cap. Preserve that
/// order even for a supplied previous flow below the protocol minimum.
pub fn conversion_flow(previous: Uint, current: Uint) -> Result<Uint, Error> {
    let next = wide(previous)
        .wrapping_mul(&u(WINDOW - 1))
        .wrapping_add(&wide(current))
        .wrapping_div(&u(WINDOW));
    let minimum = u(100).wrapping_mul(&u(1_000_000_000_000_000_000));
    if next < minimum {
        return narrow(minimum);
    }
    narrow(next.min(wide(previous).wrapping_mul(&u(2))))
}

/// Input height belongs to the parent, as in HeaderChain.ComputeMinerDifficulty.
pub fn miner_difficulty(number: u64, difficulty: Uint, previous: Uint) -> Result<Uint, Error> {
    if number <= FORKS.controller_kick_in {
        return Ok(difficulty);
    }
    narrow(
        wide(previous)
            .wrapping_mul(&u(WINDOW - 1))
            .wrapping_add(&wide(difficulty))
            .wrapping_div(&u(WINDOW)),
    )
}

/// core/headerchain.go ComputeKQuaiDiscount. `old_exchange` is the rate of
/// the canonical prime block WINDOW blocks behind the input block. A missing
/// historical rate never becomes a zero/default.
pub fn k_quai_discount(
    number: u64,
    previous: Uint,
    old_exchange: Option<Uint>,
    exchange: Uint,
) -> Result<Uint, Error> {
    if number <= FORKS.controller_kick_in {
        return Ok(Uint::from_u128(
            quai_primitives::spec::ECONOMICS.k_quai_discount,
        ));
    }
    if number <= WINDOW {
        return Ok(Uint::ZERO);
    }
    let old = old_exchange.ok_or(Error::Unavailable("controller historical exchange rate"))?;
    if old == Uint::ZERO {
        // big.Int.Div by zero: go-quai panics, it has no rule to apply.
        return Err(Error::Unsupported(
            "controller zero historical exchange rate (go-quai panics)",
        ));
    }
    let increasing = exchange > old;
    let delta = if increasing {
        wide(exchange).wrapping_sub(&wide(old))
    } else {
        wide(old).wrapping_sub(&wide(exchange))
    };
    let scaled = delta.wrapping_mul(&u(DISCOUNT_SCALE));
    let mut magnitude = scaled.wrapping_div(&wide(old));
    // big.Int.Div uses Euclidean division. The negative (increasing-rate)
    // branch therefore rounds its magnitude upward before taking abs().
    if increasing && scaled.wrapping_rem(&wide(old)) != U512::ZERO {
        magnitude = magnitude.wrapping_add(&U512::ONE);
    }
    let bounded = magnitude.min(u(DISCOUNT_SCALE));
    narrow(
        wide(previous)
            .wrapping_mul(&u(WINDOW - 1))
            .wrapping_add(&bounded)
            .wrapping_div(&u(WINDOW)),
    )
}

/// Exact integer update used after the historical schedule permits feedback.
pub fn calculate_k_quai(
    number: u64,
    parent: Uint,
    miner: Uint,
    xb_star: Uint,
) -> Result<Uint, Error> {
    if miner == Uint::ZERO {
        // big.Int.Quo by a zero denominator: go-quai panics.
        return Err(Error::Unsupported(
            "controller zero miner difficulty (go-quai panics)",
        ));
    }
    let d1 = miner.resize::<16>().shl_vartime(64);
    let denominator = d1.wrapping_mul(&U1024::from_u64(1000));
    let logarithm = quai_execution::evm::state_gas::log_q64(miner);
    let signal = xb_star
        .resize::<16>()
        .wrapping_mul(&U1024::from_u128(logarithm));
    let factor = if signal > d1 {
        let mut increase = signal.wrapping_sub(&d1);
        if number > FORKS.k_quai_change && number < FORKS.kawpow {
            increase = increase.wrapping_div(&U1024::from_u64(3));
        }
        denominator.wrapping_add(&increase)
    } else {
        denominator.wrapping_sub(&d1.wrapping_sub(&signal))
    };
    let next = parent
        .resize::<16>()
        .wrapping_mul(&factor)
        .wrapping_div(&denominator);
    if next.bits_vartime() > 256 {
        return Err(Error::Unsupported("controller exchange exceeds 256 bits"));
    }
    Ok(next.resize())
}

/// Historical resets, hold intervals, feedback and the later rate freeze.
/// Choices are the complete authenticated fixed-size difficulty window.
pub fn exchange_rate(
    number: u64,
    parent: Uint,
    miner: Uint,
    choices: &[Uint; WINDOW as usize],
) -> Result<Uint, Error> {
    exchange_rate_with_sum(number, parent, miner, || {
        choices
            .iter()
            .fold(U512::ZERO, |sum, n| sum.wrapping_add(&n.resize()))
    })
}
/// A locally maintained window sum avoids rescanning all 4000 rows. Callers
/// must establish that it belongs to the exact branch and window.
pub fn exchange_rate_from_sum(
    number: u64,
    parent: Uint,
    miner: Uint,
    sum: U512,
) -> Result<Uint, Error> {
    exchange_rate_with_sum(number, parent, miner, || sum)
}
fn exchange_rate_with_sum(
    number: u64,
    parent: Uint,
    miner: Uint,
    sum: impl FnOnce() -> U512,
) -> Result<Uint, Error> {
    // core/exchange_controller.go CalculateBetaFromMiningChoiceAndConversions.
    let initial = Uint::from_u128(crate::economics::INITIAL_EXCHANGE_RATE);
    if number >= FORKS.k_quai_freeze() {
        return Ok(parent);
    }
    if number < FORKS.controller_kick_in + WINDOW {
        return Ok(initial);
    }
    if number < FORKS.kawpow {
        for (height, percent) in FORKS.k_quai_change_table {
            if number == height {
                return if number == FORKS.k_quai_change {
                    Ok(initial)
                } else {
                    narrow(wide(parent).wrapping_mul(&u(percent)).wrapping_div(&u(100)))
                };
            }
            if number > height && number < height + FORKS.k_quai_change_hold {
                return Ok(parent);
            }
        }
    } else if number < FORKS.sha_equivalent_difficulty {
        if number == FORKS.k_quai_reset_after_kawpow() {
            return narrow(wide(initial).wrapping_mul(&u(30)));
        }
        if number < FORKS.k_quai_reset_after_kawpow() + FORKS.exchange_rate_hold_after_kawpow {
            return Ok(parent);
        }
    } else {
        if number == FORKS.sha_equivalent_difficulty {
            return narrow(wide(initial).wrapping_mul(&u(60)));
        }
        if number < FORKS.sha_equivalent_difficulty + FORKS.exchange_rate_hold_after_sha_equivalent
        {
            return Ok(parent);
        }
    }
    let best = narrow(sum().wrapping_div(&u(WINDOW)))?;
    if best <= Uint::ONE {
        // common.LogBig(bestDiff) is zero (or BinaryLog of zero): the
        // division in CalculateBetaFromMiningChoiceAndConversions panics.
        return Err(Error::Unsupported(
            "controller zero choice logarithm (go-quai panics)",
        ));
    }
    let logarithm = quai_execution::evm::state_gas::log_q64(best);
    let xb = best
        .resize::<16>()
        .shl_vartime(64)
        .wrapping_div(&U1024::from_u128(logarithm));
    if xb.bits_vartime() > 256 {
        return Err(Error::Unsupported(
            "controller choice signal exceeds 256 bits",
        ));
    }
    calculate_k_quai(number, parent, miner, xb.resize())
}
