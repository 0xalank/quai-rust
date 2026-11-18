//! Reward-depth assembly and fee feedback from the pinned StateProcessor rules.
//! Imported history and arithmetic projections do not confer block admission.
use crate::{
    economics::{self, RewardQuote},
    history::{History, Record},
    rules::{self, Uint},
    share_rules::{self, Triple},
    verifier::Verifier,
    Error,
};
use crypto_bigint::{Encoding, U1024};
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
            .wrapping_div(&wide(quote.qi)),
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
    let signal = if current.prime >= economics::CONVERSION_LOCK_FORK {
        if parent.prime < economics::CONVERSION_LOCK_FORK {
            return Ok(previous);
        }
        let cap = wide(quote.quai).wrapping_mul(&U1024::from_u64(4));
        wide(body_number(parent, "total")?)
            .min(cap)
            .wrapping_div(&U1024::from_u64(2))
    } else {
        wide(quai_fees.wrapping_div(&u(2)))
            .wrapping_add(&wide(convert_qi(quote, qi_fees.wrapping_div(&u(2)))?))
    };
    narrow(
        wide(previous)
            .wrapping_mul(&U1024::from_u64(99))
            .wrapping_add(&signal)
            .wrapping_div(&U1024::from_u64(100)),
    )
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
            .wrapping_div(&U1024::from_u64((100 * span) as u64)),
    )
}
pub struct Distribution {
    pub outbound: Vec<Transaction>,
    pub work_evaluations: usize,
    pub fully_validated: bool,
}
fn reward_pool(target: &Record, quote: &RewardQuote) -> Result<Uint, Error> {
    if target.prime >= economics::CONVERSION_LOCK_FORK {
        Ok(quote.conversion_quai)
    } else {
        narrow(
            wide(quote.quai)
                .wrapping_add(&wide(body_number(target, "avg")?))
                .wrapping_add(&wide(body_number(target, "total")?.wrapping_div(&u(2)))),
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
    let base_quai = pool.wrapping_div(&u(9));
    let base_qi = quote.qi.wrapping_div(&u(9));
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
            if share_rules::number(h.number.as_deref().unwrap())? != target_number {
                continue;
            }
            let mut entropy = Uint::ZERO;
            if block.prime < rules::KAWPOW_FORK {
                let pow = hash(&header)?;
                let difficulty = share_rules::uint(h.difficulty.as_deref().unwrap())?;
                if difficulty == Uint::ZERO {
                    return Err(Error::Invalid("zero share difficulty"));
                }
                entropy = if quai_primitives::meets_blake3_target(&pow, &difficulty.to_be_bytes())
                    .map_err(|_| Error::Invalid("share difficulty"))?
                {
                    let bound = U1024::ONE.shl_vartime(256).wrapping_div(&wide(difficulty));
                    let low: Uint = bound.resize();
                    rules::intrinsic(low.to_be_bytes())?
                } else {
                    rules::intrinsic(pow)?
                };
                total = total.wrapping_add(&wide(entropy));
            } else {
                let coinbase = &h.primary_coinbase.as_ref().unwrap().value;
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
        let to: [u8; 20] = h
            .primary_coinbase
            .as_ref()
            .unwrap()
            .value
            .as_slice()
            .try_into()
            .unwrap();
        let qi = to[1] & 128 != 0;
        let mut reward;
        if block.prime < rules::KAWPOW_FORK {
            reward = narrow(wide(pool).wrapping_mul(&wide(entropy)).wrapping_div(&total))?;
            if reward > pool {
                return Err(Error::Invalid("share exceeds reward pool"));
            }
        } else {
            reward = if qi && target.prime >= economics::CONVERSION_LOCK_FORK {
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
                                .wrapping_div(&wide(valid)),
                        )?;
                    }
                }
                let signature_result =
                    crate::auxiliary::coinbase::Coinbase::decode(aux.transaction())
                        .map(|v| v.signature_time);
                let signature = signature_result.clone().unwrap_or(0);
                let timestamp = u32::from_le_bytes(aux.header()[68..72].try_into().unwrap());
                reward = if block.prime < 1_330_000 {
                    if signature_result.is_err() || signature.wrapping_add(18) < timestamp {
                        narrow(
                            wide(reward)
                                .wrapping_mul(&U1024::from_u64(70))
                                .wrapping_div(&U1024::from_u64(100)),
                        )?
                    } else {
                        reward
                    }
                } else {
                    time_discount(reward, aux.id(), timestamp, signature)?
                };
            } else {
                reward = narrow(
                    wide(reward)
                        .wrapping_mul(&U1024::from_u64(70))
                        .wrapping_div(&U1024::from_u64(100)),
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
            if target.prime < economics::CONVERSION_LOCK_FORK {
                if quote.conversion_quai == Uint::ZERO {
                    return Err(Error::Invalid("zero conversion quote"));
                }
                reward = narrow(
                    wide(reward)
                        .wrapping_mul(&wide(quote.qi))
                        .wrapping_div(&wide(quote.conversion_quai)),
                )?;
            }
        } else {
            origin[1] &= 127;
            origin[3] &= 127;
        }
        reward = reward.max(Uint::ONE);
        let index = first_index
            .checked_add(out.len())
            .ok_or(Error::ResourceLimit("reward ETX index"))? as u16;
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
