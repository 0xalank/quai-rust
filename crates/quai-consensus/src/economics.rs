//! Bounded reward/conversion arithmetic used by zone header base-fee policy.
//! Does not distribute rewards, settle fees, or execute economic state.
use crate::bigdiv::Quotient as _;
use crate::{
    history::Record,
    rules::Uint,
    share_rules::{self, Triple},
    Error,
};
use crypto_bigint::U1024;
use quai_primitives::{
    spec::{self, ECONOMICS, FORKS},
    Header,
};
/// params.ExchangeRate (the starting exchange rate in Qit per Quai Its).
pub const INITIAL_EXCHANGE_RATE: u128 = ECONOMICS.exchange_rate;
fn wide(n: Uint) -> U1024 {
    n.resize()
}
fn narrow(n: U1024) -> Result<Uint, Error> {
    if n.bits_vartime() > 256 {
        return Err(Error::Unsupported("economic magnitude exceeds 256 bits"));
    }
    Ok(n.resize())
}
fn u(n: u64) -> Uint {
    Uint::from_u64(n)
}
/// params.OneOverKqi.
pub fn one_over_kqi(number: u64) -> Uint {
    let base = if number > FORKS.qi_activation {
        8_000_000_000u64
    } else {
        26_000_000
    };
    let period = 365u64 * spec::BLOCKS_PER_DAY * 269 / 100;
    if number > 2 * period {
        return u(4 * base);
    }
    u(
        (((period + number % period) as u128 * base as u128 * (1u128 << (number / period)))
            / period as u128) as u64,
    )
}
pub fn equivalent_difficulty(h: &Header, difficulty: Uint) -> Result<Uint, Error> {
    let prime = share_rules::number(h.wire().prime_terminus_number.as_deref().unwrap())?;
    if prime < FORKS.kawpow {
        return Ok(difficulty);
    }
    let sha = Triple::from_header(h, true)?;
    let scrypt = Triple::from_header(h, false)?;
    let base = if prime >= FORKS.sha_equivalent_difficulty {
        sha.difficulty.quotient(&u(167000)).max(u(750_000_000_000))
    } else {
        difficulty
    };
    let q = U1024::from_u64(1 << 32);
    let shares = wide(sha.count)
        .wrapping_add(&wide(scrypt.count))
        .min(q.wrapping_mul(&U1024::from_u64(8)));
    let total = q.wrapping_mul(&U1024::from_u64(9));
    narrow(
        wide(base)
            .wrapping_mul(&total)
            .quotient(&total.wrapping_sub(&shares)),
    )
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewardQuote {
    pub equivalent: Uint,
    pub quai: Uint,
    pub qi: Uint,
    pub conversion_quai: Uint,
    pub base_fee: Uint,
}
/// Positive magnitude domain. A negative reset-adjusted logarithm is unqualified.
pub fn reward_quote(
    header: &Header,
    exchange: Uint,
    average_fees: Uint,
) -> Result<RewardQuote, Error> {
    reward_quote_at_difficulty(
        header,
        exchange,
        average_fees,
        share_rules::uint(header.wire().difficulty.as_deref().unwrap())?,
    )
}
/// Conversion normalization uses the parent miner difficulty, which may differ
/// from the parent's work target. This is arithmetic, not proof validation.
pub fn reward_quote_at_difficulty(
    header: &Header,
    exchange: Uint,
    average_fees: Uint,
    difficulty: Uint,
) -> Result<RewardQuote, Error> {
    let h = header.wire();
    if difficulty == Uint::ZERO {
        return Err(Error::Invalid("zero reward difficulty"));
    }
    let prime = share_rules::number(h.prime_terminus_number.as_deref().unwrap())?;
    let number = share_rules::number(h.number.as_deref().unwrap())?;
    let equivalent = equivalent_difficulty(header, difficulty)?;
    let mut logarithm = quai_execution::evm::state_gas::log_q64(equivalent);
    if prime >= FORKS.kawpow {
        let reset = quai_execution::evm::state_gas::log_q64(u(300_000_000_000));
        logarithm = logarithm
            .checked_sub(reset)
            .ok_or(Error::Unsupported("negative reward logarithm"))?;
    }
    let quai = narrow(
        wide(exchange)
            .wrapping_mul(&U1024::from_u128(logarithm))
            .shr_vartime(64),
    )?
    .max(Uint::ONE);
    let qi = equivalent.quotient(&one_over_kqi(number)).max(Uint::ONE);
    let conversion_quai = if prime >= FORKS.conversion_lock_change {
        narrow(wide(quai).wrapping_add(&wide(average_fees).wrapping_mul(&U1024::from_u64(2))))?
    } else {
        quai
    };
    // Source divides the Qi-to-Quai quote first, then by TxGas (21000).
    let base_fee = narrow(
        wide(conversion_quai)
            .wrapping_mul(&U1024::from_u64(5))
            .quotient(&wide(qi))
            .quotient(&U1024::from_u64(21000)),
    )?;
    Ok(RewardQuote {
        equivalent,
        quai,
        qi,
        conversion_quai,
        base_fee,
    })
}
pub fn expected_base_fee(
    history: &crate::history::History,
    parent: &Record,
    profile: &crate::validation::Profile,
) -> Result<Uint, Error> {
    if profile.genesis.contains(&parent.id) {
        return Ok(Uint::ZERO);
    }
    let exchange = if profile.genesis.contains(&parent.parents[2]) {
        Uint::from_u128(INITIAL_EXCHANGE_RATE)
    } else {
        let hash = parent
            .object
            .header()
            .wire()
            .prime_terminus_hash
            .as_ref()
            .unwrap()
            .value
            .as_slice()
            .try_into()
            .unwrap();
        let prime = history.get(&hash)?;
        share_rules::uint(
            prime
                .object
                .header()
                .wire()
                .exchange_rate
                .as_deref()
                .unwrap(),
        )?
    };
    Ok(reward_quote(
        &parent.header,
        exchange,
        share_rules::uint(
            parent
                .object
                .header()
                .wire()
                .avg_tx_fees
                .as_deref()
                .unwrap(),
        )?,
    )?
    .base_fee)
}
