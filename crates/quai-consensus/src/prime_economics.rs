//! Parent conversion processing and prime controller composition.
//! Inputs are authenticated by the caller. This component grants no checkpoint,
//! block-admission, private-profile or canonical authority.
use crate::{
    choice_window::{self, Window},
    controllers, cubic_discount, economics,
    history::Record,
    rules::Uint,
    share_rules, Error,
};
use crypto_bigint::{Encoding, U512};
use quai_primitives::spec::FORKS;
use quai_primitives::{
    transaction::{External, Kind, Transaction},
    Hash,
};
/// params.BlocksPerYear.
const BLOCKS_PER_YEAR: u64 = quai_primitives::spec::BLOCKS_PER_YEAR;
fn u(n: u64) -> Uint {
    Uint::from_u64(n)
}
fn narrow(n: U512) -> Result<Uint, Error> {
    if n.bits_vartime() > 256 {
        return Err(Error::Unsupported("prime conversion magnitude"));
    }
    Ok(n.resize())
}
fn add(a: Uint, b: Uint) -> Result<Uint, Error> {
    narrow(a.resize::<8>().wrapping_add(&b.resize()))
}
pub(crate) fn ratio(a: Uint, b: Uint, d: Uint) -> Result<Uint, Error> {
    if d == Uint::ZERO {
        return Err(Error::Invalid("zero prime conversion divisor"));
    }
    narrow(
        a.resize::<8>()
            .wrapping_mul(&b.resize::<8>())
            .wrapping_div(&d.resize()),
    )
}
fn slip(e: &External) -> u64 {
    if e.etx_type != 2 {
        return 0;
    }
    if e.data.len() < 2 {
        return 9000;
    }
    u16::from_be_bytes([e.data[0], e.data[1]]).clamp(30, 9000) as u64
}
fn external(t: &Transaction) -> Result<External, Error> {
    match t.kind() {
        Kind::External(e) => Ok(e.clone()),
        _ => Err(Error::Invalid("prime inbound non-ETX")),
    }
}
pub(crate) fn bytes(n: Uint) -> Vec<u8> {
    let b = n.to_be_bytes();
    b[b.iter().position(|v| *v != 0).unwrap_or(b.len())..].to_vec()
}
#[derive(Clone)]
pub struct Converted {
    pub etxs: Vec<Transaction>,
    pub actual: Uint,
    pub realized: Uint,
    pub(crate) pricing: Vec<Pricing>,
}
#[derive(Clone)]
pub(crate) struct Pricing {
    pub original: Uint,
    pub before: Option<Uint>,
}
/// Stable decreasing slip order, first-pass filtering and second-pass pricing.
/// Original values remain separate from zeroed/repriced records, as in Go.
pub fn conversions(
    parent: &Record,
    inbound: &[Transaction],
    old_exchange: Option<Uint>,
) -> Result<Converted, Error> {
    conversions_impl(parent, inbound, old_exchange, false)
}
pub(crate) fn conversions_for_forward(
    parent: &Record,
    inbound: &[Transaction],
    old_exchange: Option<Uint>,
) -> Result<Converted, Error> {
    conversions_impl(parent, inbound, old_exchange, true)
}
fn conversions_impl(
    parent: &Record,
    inbound: &[Transaction],
    old_exchange: Option<Uint>,
    retain_pricing: bool,
) -> Result<Converted, Error> {
    let number = parent.number(0)?;
    let h = parent.object().header().wire();
    let exchange = share_rules::uint(h.exchange_rate.as_deref().unwrap())?;
    let discount = share_rules::uint(h.k_quai_discount.as_deref().unwrap())?;
    if discount > u(controllers::DISCOUNT_SCALE) {
        return Err(Error::Invalid("prime discount range"));
    }
    let increasing = if number > controllers::WINDOW {
        exchange > old_exchange.ok_or(Error::Unavailable("prime conversion historical rate"))?
    } else {
        false
    };
    let quote = economics::reward_quote_at_difficulty(
        parent.header(),
        exchange,
        share_rules::uint(h.avg_tx_fees.as_deref().unwrap())?,
        share_rules::uint(h.miner_difficulty.as_deref().unwrap())?,
    )?;
    let to_quai = |n| ratio(n, quote.conversion_quai, quote.qi);
    let to_qi = |n| ratio(n, quote.qi, quote.conversion_quai);
    let cubic = |amount| {
        if number > quai_primitives::spec::FORKS.conversion_slip_change {
            cubic_discount::apply(
                amount,
                share_rules::uint(h.conversion_flow_amount.as_deref().unwrap())?,
            )
        } else {
            cubic_discount::apply(
                share_rules::uint(h.conversion_flow_amount.as_deref().unwrap())?,
                amount,
            )
        }
    };
    let mut rows = inbound
        .iter()
        .map(external)
        .collect::<Result<Vec<_>, _>>()?;
    rows.sort_by_key(|e| std::cmp::Reverse(slip(e)));
    let originals = rows
        .iter()
        .map(|e| share_rules::uint(&e.value))
        .collect::<Result<Vec<_>, _>>()?;
    let adjust = |original: Uint, total: Uint, cubic: Uint, qi: bool| -> Result<Uint, Error> {
        let after_k = ratio(
            cubic,
            u(controllers::DISCOUNT_SCALE).wrapping_sub(&discount),
            u(controllers::DISCOUNT_SCALE),
        )?;
        let mut value = ratio(original, cubic, total)?;
        if qi == increasing && cubic != Uint::ZERO {
            value = ratio(value, after_k, cubic)?;
        }
        Ok(value.max(original.wrapping_div(&u(10))))
    };
    let mut accepted = Uint::ZERO;
    for (e, original) in rows.iter_mut().zip(&originals) {
        if e.etx_type != 2 || *original == Uint::ZERO {
            continue;
        }
        let qi = e.to[1] & 128 != 0;
        let total = add(accepted, if qi { *original } else { to_quai(*original)? })?;
        let value = adjust(*original, total, cubic(total)?, qi)?;
        let minimum = ratio(*original, u(10000 - slip(e)), u(10000))?;
        if value < minimum {
            e.value.clear();
        } else {
            accepted = total;
        }
    }
    let mut actual = Uint::ZERO;
    for e in &rows {
        if e.etx_type == 2 {
            let value = share_rules::uint(&e.value)?;
            if value != Uint::ZERO {
                actual = add(
                    actual,
                    if e.to[1] & 128 != 0 {
                        value
                    } else {
                        to_quai(value)?
                    },
                )?;
            }
        }
    }
    let priced = cubic(actual)?;
    let mut realized = Uint::ZERO;
    let mut pricing = if retain_pricing {
        Vec::with_capacity(rows.len())
    } else {
        Vec::new()
    };
    for (e, original) in rows.iter_mut().zip(originals) {
        if e.etx_type != 2 || share_rules::uint(&e.value)? == Uint::ZERO {
            if retain_pricing {
                pricing.push(Pricing {
                    original,
                    before: None,
                });
            }
            continue;
        }
        let qi = e.to[1] & 128 != 0;
        let mut value = adjust(original, actual, priced, qi)?;
        if retain_pricing {
            pricing.push(Pricing {
                original,
                before: Some(value),
            });
        }
        if qi {
            realized = add(realized, value)?;
            value = to_qi(value)?;
        } else {
            value = to_quai(value)?;
            realized = add(realized, value)?;
        }
        e.value = bytes(value);
    }
    Ok(Converted {
        etxs: rows
            .into_iter()
            .map(|e| {
                Transaction::external(e).map_err(|_| Error::Invalid("prime converted ETX encoding"))
            })
            .collect::<Result<_, _>>()?,
        actual,
        realized,
        pricing,
    })
}
#[derive(Clone, Copy)]
pub struct Override {
    pub block: Hash,
    pub exchange: Uint,
    pub update_bit: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fields {
    pub conversion_flow: Uint,
    pub exchange_rate: Uint,
    pub discount: Uint,
    pub miner_difficulty: Uint,
}
/// A component result only. Native canonical admission cannot consume this as
/// an execution receipt or use it to import supplied state/history.
pub struct Prepared {
    parent: Hash,
    number: u64,
    window: Window,
    converted: Converted,
    fields: Fields,
}
impl Prepared {
    pub fn fields(&self) -> Fields {
        self.fields
    }
    pub fn window(&self) -> &Window {
        &self.window
    }
    pub fn converted(&self) -> &Converted {
        &self.converted
    }
    pub fn verify_child(&self, child: &Record) -> Result<(), Error> {
        child.verify_binding()?;
        if child.parents[0] != self.parent || self.number.checked_add(1) != Some(child.number(0)?) {
            return Err(Error::Invalid("prime controller child binding"));
        }
        let h = child.object().header().wire();
        if share_rules::uint(h.conversion_flow_amount.as_deref().unwrap())?
            != self.fields.conversion_flow
            || share_rules::uint(h.exchange_rate.as_deref().unwrap())? != self.fields.exchange_rate
            || share_rules::uint(h.k_quai_discount.as_deref().unwrap())? != self.fields.discount
            || share_rules::uint(h.miner_difficulty.as_deref().unwrap())?
                != self.fields.miner_difficulty
        {
            return Err(Error::Invalid("prime economic controller fields"));
        }
        Ok(())
    }
}
/// Compose supplied authenticated parent inputs. Missing inbound history is
/// distinct from a valid empty list; the override is read even after expiry.
pub fn prepare_component(
    parent: &Record,
    grandparent: &Record,
    inbound: Option<&[Transaction]>,
    prior_window: &Window,
    old_exchange: Option<Uint>,
    authority: Option<Override>,
) -> Result<Prepared, Error> {
    parent.verify_binding()?;
    grandparent.verify_binding()?;
    let number = parent.number(0)?;
    if number < FORKS.controller_kick_in {
        return Err(Error::Unsupported("prime controller before activation"));
    }
    if parent.parents[0] != grandparent.id()
        || grandparent.number(0)?.checked_add(1) != Some(number)
        || prior_window.block() != grandparent.id()
        || prior_window.number() != grandparent.number(0)?
    {
        return Err(Error::Invalid("prime controller parent/window binding"));
    }
    let converted = conversions(
        parent,
        inbound.ok_or(Error::Unavailable("prime inbound history"))?,
        old_exchange,
    )?;
    let authority = authority.ok_or(Error::Unavailable("prime controller override history"))?;
    if authority.block != parent.parents[2] {
        return Err(Error::Invalid("prime controller override branch"));
    }
    let h = parent.object().header().wire();
    let previous = share_rules::uint(h.exchange_rate.as_deref().unwrap())?;
    let miner = share_rules::uint(h.miner_difficulty.as_deref().unwrap())?;
    let choice = choice_window::derive(
        grandparent,
        previous,
        &converted.etxs,
        converted.actual,
        converted.realized,
        miner,
    )?;
    let window = prior_window.advance(parent.id(), grandparent.id(), number, choice)?;
    let exchange = if parent.number(2)? <= BLOCKS_PER_YEAR && authority.update_bit == 0 {
        authority.exchange
    } else {
        window.exchange_rate(previous, miner)?
    };
    let fields = Fields {
        conversion_flow: controllers::conversion_flow(
            share_rules::uint(h.conversion_flow_amount.as_deref().unwrap())?,
            converted.actual,
        )?,
        exchange_rate: exchange,
        discount: controllers::k_quai_discount(
            number,
            share_rules::uint(h.k_quai_discount.as_deref().unwrap())?,
            old_exchange,
            exchange,
        )?,
        miner_difficulty: controllers::miner_difficulty(
            number,
            share_rules::uint(parent.header().wire().difficulty.as_deref().unwrap())?,
            miner,
        )?,
    };
    Ok(Prepared {
        parent: parent.id(),
        number,
        window,
        converted,
        fields,
    })
}
