//! Reward-depth assembly and fee feedback from the pinned StateProcessor rules.
//! Imported history and arithmetic projections do not confer block admission.
use crate::bigdiv::Quotient as _;
use crate::{
    economics::{self, RewardQuote},
    fields,
    history::{History, Record},
    rules::{self, Uint},
    share_rules::{self, Triple},
    verifier::Verifier,
    Error,
};
use crypto_bigint::{Encoding, U1024};
use quai_primitives::spec::FORKS;
use quai_primitives::{
    transaction::{External, Transaction},
    Hash, Header,
};
fn u(n: u64) -> Uint {
    Uint::from_u64(n)
}
fn wide(n: Uint) -> U1024 {
    n.resize()
}
fn narrow(n: U1024) -> Result<Uint, Error> {
    if n.bits_vartime() > 256 {
        return Err(Error::Unsupported("payout magnitude exceeds 256 bits"));
    }
    Ok(n.resize())
}
fn magnitude(n: Uint) -> Vec<u8> {
    let b = n.to_be_bytes();
    b[b.iter().position(|v| *v != 0).unwrap_or(32)..].to_vec()
}
fn body_number(record: &Record, name: &str) -> Result<Uint, Error> {
    let b = record.object.header().wire();
    let raw = match name {
        "avg" => b.avg_tx_fees.as_deref(),
        "total" => b.total_fees.as_deref(),
        _ => None,
    };
    share_rules::uint(raw.ok_or(Error::Invalid("economic header field"))?)
}
pub fn convert_qi(quote: &RewardQuote, amount: Uint) -> Result<Uint, Error> {
    if quote.qi == Uint::ZERO {
        return Err(Error::Invalid("zero Qi reward"));
    }
    narrow(
        wide(amount)
            .wrapping_mul(&wide(quote.conversion_quai))
            .quotient(&wide(quote.qi)),
    )
}
pub fn average_fees(
    current: &Record,
    parent: &Record,
    parent_is_genesis: bool,
    quote: &RewardQuote,
    quai_fees: Uint,
    qi_fees: Uint,
) -> Result<Uint, Error> {
    if parent_is_genesis {
        return Ok(Uint::ZERO);
    }
    let previous = body_number(parent, "avg")?;
    let signal = if current.prime >= FORKS.conversion_lock_change {
        if parent.prime < FORKS.conversion_lock_change {
            return Ok(previous);
        }
        let cap = wide(quote.quai).wrapping_mul(&U1024::from_u64(4));
        wide(body_number(parent, "total")?)
            .min(cap)
            .quotient(&U1024::from_u64(2))
    } else {
        wide(quai_fees.quotient(&u(2)))
            .wrapping_add(&wide(convert_qi(quote, qi_fees.quotient(&u(2)))?))
    };
    narrow(
        wide(previous)
            .wrapping_mul(&U1024::from_u64(99))
            .wrapping_add(&signal)
            .quotient(&U1024::from_u64(100)),
    )
}
/// AuxPoW template age using Quai time as a floor (`a60f36fb1`):
/// cannot be reduced by freezing the donor timestamp, and invalid future
/// signatures stay stale (`u64::MAX`).
pub fn auxpow_template_age(quai_time: u64, donor_timestamp: u32, signature: u32) -> u64 {
    let timestamp = quai_time.max(u64::from(donor_timestamp));
    if timestamp < u64::from(signature) {
        u64::MAX
    } else {
        timestamp - u64::from(signature)
    }
}
/// Includes the original u32 subtraction/addition wraparound and per-algorithm
/// liveness cutoff. Signature-time extraction errors supply zero in Go.
pub fn time_discount(reward: Uint, id: u8, timestamp: u32, signature: u32) -> Result<Uint, Error> {
    let live = if matches!(id, 2 | 3) { 30u32 } else { 18 };
    let elapsed = timestamp.wrapping_sub(signature).clamp(3, live);
    let span = live - 3;
    let numerator = 70 * span + 30 * (live - elapsed);
    narrow(
        wide(reward)
            .wrapping_mul(&U1024::from_u64(numerator as u64))
            .quotient(&U1024::from_u64((100 * span) as u64)),
    )
}
/// Fork-aware share reward time discount (`go-quai a60f36fb1`): after
/// `StateRepairForkBlock`, uses `auxpow_template_age(quai_time, donor_ts, sig)`
/// instead of raw `u32` wraparound subtraction on the donor timestamp alone.
pub fn time_discount_at(
    reward: Uint,
    id: u8,
    donor_timestamp: u32,
    quai_time: u64,
    signature: u32,
    prime: u64,
    forks: &quai_primitives::spec::ForkSchedule,
) -> Result<Uint, Error> {
    if !forks.state_repair_active(prime) {
        return time_discount(reward, id, donor_timestamp, signature);
    }
    let live = if matches!(id, 2 | 3) { 30u32 } else { 18 };
    let age =
        auxpow_template_age(quai_time, donor_timestamp, signature).min(u64::from(u32::MAX)) as u32;
    let elapsed = age.clamp(3, live);
    let span = live - 3;
    let numerator = 70 * span + 30 * (live - elapsed);
    narrow(
        wide(reward)
            .wrapping_mul(&U1024::from_u64(numerator as u64))
            .quotient(&U1024::from_u64((100 * span) as u64)),
    )
}
pub struct Distribution {
    pub outbound: Vec<Transaction>,
    pub work_evaluations: usize,
    pub fully_validated: bool,
}
fn reward_pool(target: &Record, quote: &RewardQuote) -> Result<Uint, Error> {
    if target.prime >= FORKS.conversion_lock_change {
        Ok(quote.conversion_quai)
    } else {
        narrow(
            wide(quote.quai)
                .wrapping_add(&wide(body_number(target, "avg")?))
                .wrapping_add(&wide(body_number(target, "total")?.quotient(&u(2)))),
        )
    }
}
/// Controlled-work arithmetic projection for explicit offline differential tests.
/// Native execution calls `native_components`, which recomputes work itself.
pub fn project<F: FnMut(&Header) -> Result<Hash, Error>>(
    history: &History,
    block: &Record,
    location: [u8; 2],
    exchange: Uint,
    first_index: usize,
    mut work: F,
) -> Result<Distribution, Error> {
    if block.numbers[2] <= 3 {
        return Ok(Distribution {
            outbound: vec![],
            work_evaluations: 0,
            fully_validated: false,
        });
    }
    let depth = share_rules::inclusion_depth(block.prime);
    let target_number = block.numbers[2]
        .checked_sub(depth as u64)
        .ok_or(Error::Invalid("reward depth"))?;
    let mut chain = vec![];
    let mut parent_hash = block.parents[2];
    for _ in 0..depth {
        let parent = history.get(&parent_hash)?;
        parent_hash = parent.parents[2];
        chain.push(parent);
    }
    let target = &chain[depth - 1];
    if target.numbers[2] != target_number {
        return Err(Error::Invalid("reward target height"));
    }
    let parent = &chain[0];
    let quote = economics::reward_quote(&target.header, exchange, body_number(&target, "avg")?)?;
    let pool = reward_pool(&target, &quote)?;
    let base_quai = pool.quotient(&u(9));
    let base_qi = quote.qi.quotient(&u(9));
    let mut evaluations = 0;
    let mut hashes = std::collections::BTreeMap::<Vec<u8>, Hash>::new();
    let mut hash = |header: &Header| -> Result<Hash, Error> {
        let key = header.encode();
        if let Some(value) = hashes.get(&key) {
            return Ok(*value);
        }
        let value = work(header)?;
        evaluations += 1;
        hashes.insert(key, value);
        Ok(value)
    };
    let target_entropy = rules::intrinsic(hash(&target.header)?)?;
    let mut total = wide(target_entropy);
    let mut shares = vec![(target.header.clone(), target_entropy)];
    for index in 0..=depth {
        let headers = if index == depth {
            block.uncles()?
        } else {
            chain[index].uncles()?
        };
        for header in headers {
            let h = header.wire();
            if share_rules::number(fields::bytes(&h.number, "missing number")?)? != target_number {
                continue;
            }
            let mut entropy = Uint::ZERO;
            if block.prime < FORKS.kawpow {
                let pow = hash(&header)?;
                let difficulty =
                    share_rules::uint(fields::bytes(&h.difficulty, "missing difficulty")?)?;
                if difficulty == Uint::ZERO {
                    return Err(Error::Invalid("zero share difficulty"));
                }
                entropy = if quai_primitives::meets_blake3_target(&pow, &difficulty.to_be_bytes())
                    .map_err(|_| Error::Invalid("share difficulty"))?
                {
                    let bound = U1024::ONE.shl_vartime(256).quotient(&wide(difficulty));
                    let low: Uint = bound.resize();
                    rules::intrinsic(low.to_be_bytes())?
                } else {
                    rules::intrinsic(pow)?
                };
                total = total.wrapping_add(&wide(entropy));
            } else {
                let coinbase = fields::hash(&h.primary_coinbase, "missing primary coinbase")?;
                if !crate::validation::internal_address(coinbase, &location) {
                    continue;
                }
            }
            shares.push((header, entropy));
        }
    }
    if total == U1024::ZERO {
        return Err(Error::Invalid("zero reward entropy"));
    }
    let mut out = vec![];
    for (header, entropy) in shares {
        let h = header.wire();
        let to = fields::address20(&h.primary_coinbase, "missing primary coinbase")?;
        let qi = to[1] & 128 != 0;
        let mut reward;
        if block.prime < FORKS.kawpow {
            reward = narrow(wide(pool).wrapping_mul(&wide(entropy)).quotient(&total))?;
            if reward > pool {
                return Err(Error::Invalid("share exceeds reward pool"));
            }
        } else {
            reward = if qi && target.prime >= FORKS.conversion_lock_change {
                base_qi
            } else {
                base_quai
            };
            let aux = h
                .aux_pow
                .as_ref()
                .map(|v| crate::auxiliary::Auxiliary::decode(v))
                .transpose()?;
            if let Some(aux) = aux {
                if matches!(aux.id(), 2..=4) {
                    let triple = Triple::from_header(&block.header, aux.id() != 4)?;
                    if triple.count > triple.uncled {
                        let mut valid = triple.count.wrapping_sub(&triple.uncled);
                        if triple.uncled != Uint::ZERO && valid < u(128849018) {
                            valid = u(128849018);
                        }
                        reward = narrow(
                            wide(reward)
                                .wrapping_mul(&wide(triple.count))
                                .quotient(&wide(valid)),
                        )?;
                    }
                }
                let signature_result =
                    crate::auxiliary::coinbase::Coinbase::decode(aux.transaction())
                        .map(|v| v.signature_time);
                let signature = signature_result.clone().unwrap_or(0);
                // A donor header has 80 or 120 bytes (canonical_pow).
                let d = aux.header();
                let timestamp = u32::from_le_bytes([d[68], d[69], d[70], d[71]]);
                reward = if block.prime < FORKS.inclusion_depth_change {
                    if signature_result.is_err() || signature.wrapping_add(18) < timestamp {
                        narrow(
                            wide(reward)
                                .wrapping_mul(&U1024::from_u64(70))
                                .quotient(&U1024::from_u64(100)),
                        )?
                    } else {
                        reward
                    }
                } else {
                    let share_prime = h
                        .prime_terminus_number
                        .as_deref()
                        .and_then(|b| share_rules::number(b).ok())
                        .unwrap_or(block.prime);
                    let quai_time = h.time.unwrap_or(0);
                    time_discount_at(
                        reward,
                        aux.id(),
                        timestamp,
                        quai_time,
                        signature,
                        share_prime,
                        &FORKS,
                    )?
                };
            } else {
                reward = narrow(
                    wide(reward)
                        .wrapping_mul(&U1024::from_u64(70))
                        .quotient(&U1024::from_u64(100)),
                )?;
            }
        }
        let mut origin = parent.id;
        let prefix = location[0] << 4 | location[1];
        origin[0] = prefix;
        origin[2] = prefix;
        if qi {
            origin[1] |= 128;
            origin[3] |= 128;
            if target.prime < FORKS.conversion_lock_change {
                if quote.conversion_quai == Uint::ZERO {
                    return Err(Error::Invalid("zero conversion quote"));
                }
                reward = narrow(
                    wide(reward)
                        .wrapping_mul(&wide(quote.qi))
                        .quotient(&wide(quote.conversion_quai)),
                )?;
            }
        } else {
            origin[1] &= 127;
            origin[3] &= 127;
        }
        reward = reward.max(Uint::ONE);
        let index = first_index
            .checked_add(out.len())
            .and_then(|n| u16::try_from(n).ok())
            .ok_or(Error::ResourceLimit("reward ETX index"))?;
        let mut data = h.data.clone();
        data.extend(
            header
                .block_hash()
                .map_err(|_| Error::Invalid("share hash"))?,
        );
        out.push(
            Transaction::external(External {
                originating_hash: origin,
                index,
                gas: 21000,
                to,
                value: magnitude(reward),
                data,
                access_list: vec![],
                sender: to,
                etx_type: 1,
            })
            .map_err(|_| Error::Unsupported("reward ETX domain"))?,
        );
    }
    Ok(Distribution {
        outbound: out,
        work_evaluations: evaluations,
        fully_validated: false,
    })
}
/// Full-size native proof work and bounded branch-derived payouts. This result
/// remains a component report until ancestry and complete block admission pass.
pub fn native_components(
    verifier: &mut Verifier,
    history: &History,
    block: &Record,
    location: [u8; 2],
    exchange: Uint,
    first_index: usize,
) -> Result<Distribution, Error> {
    project(history, block, location, exchange, first_index, |h| {
        verifier.work_for_inclusion(h, &location).map(|v| v.hash())
    })
}

#[cfg(test)]
mod liveness_tests {
    use super::*;

    #[test]
    fn auxpow_liveness_switches_to_quai_time_floor_at_state_repair_fork() {
        let mut forks = FORKS;
        forks.state_repair = 1000;
        let reward = Uint::from_u64(10_000);
        // Frozen donor timestamp (t=100, sig=100) while Quai time has moved forward to 200:
        // Before fork: uses donor_timestamp - sig = 0 -> full 100% reward.
        let pre = time_discount_at(reward, 2, 100, 200, 100, 999, &forks).unwrap();
        assert_eq!(pre, reward);
        // At/after fork: Quai time 200 floors the age at 100s (>30s cutoff) -> discounted to 70%.
        let post = time_discount_at(reward, 2, 100, 200, 100, 1000, &forks).unwrap();
        assert_eq!(post, Uint::from_u64(7_000));
        // Future signature after fork stays stale (70%).
        let future = time_discount_at(reward, 2, 100, 100, 500, 1000, &forks).unwrap();
        assert_eq!(future, Uint::from_u64(7_000));
        assert_eq!(auxpow_template_age(100, 100, 500), u64::MAX);
        assert_eq!(auxpow_template_age(200, 100, 150), 50);
    }
}
