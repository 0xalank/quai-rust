//! The prime controllers of go-quai v0.57.0: core/slice.go `Append` in the
//! prime context and `Slice.verifyParentExchangeRateAndFlowAmount`.
//!
//! For each prime block go-quai
//! 1. up to ControllerKickInBlock requires the starting values
//!    (StartingKQuaiDiscount, StartingConversionFlowAmount, ExchangeRate) and
//!    passes its newly confirmed ETXs to the region unchanged;
//! 2. after it, recomputes the parent's conversions from the inbound ETXs
//!    stored for the parent and requires the conversion flow amount, the
//!    exchange rate and the kQuai discount of the header to equal the values
//!    computed on the parent (`prime_economics::prepare_component`);
//! 3. then prices its own newly confirmed ETXs: a stable sort by decreasing
//!    slip, the cubic and kQuai discounts, the refunds, the token choice set
//!    it stores and the forward exchange rate
//!    (`prime_forward::prepare_component`). The priced list is what the prime
//!    context passes to the region and the zone.
//!
//! The miner difficulty (HeaderChain.ComputeMinerDifficulty) is a header rule
//! of core/headerchain_validation.go and lives in `hierarchy_header`.
//!
//! [`State`] holds what these steps read after a prime block: the exchange
//! rates of the canonical prime blocks (HeaderChain.GetBlockByNumber), the
//! inbound ETXs stored for the head (rawdb.ReadInboundEtxs) and the token
//! choice sets stored for the head and its parent (rawdb.ReadTokenChoicesSet).
//! StateProcessor.GetKQuaiAndUpdateBit returns (params.ExchangeRate, 1) for
//! every block, so Append never takes its update-paused branch;
//! [`stored_rate`] supplies the same values.
use crate::{
    choice_window::Window,
    controllers::WINDOW,
    history::Record,
    prime_economics::{self, Override},
    prime_forward,
    rules::Uint,
    share_rules, Error,
};
use quai_primitives::{
    spec::{ECONOMICS, FORKS},
    transaction::Transaction,
    Hash,
};
use std::{collections::VecDeque, sync::Arc};

/// Exchange rates kept: the head and the WINDOW + 1 prime blocks before it,
/// enough for GetBlockByNumber(head - WINDOW) and (head + 1 - WINDOW).
pub const RATES: usize = WINDOW as usize + 2;

/// A header field that differs from the value go-quai computes, or a
/// component error (missing history, an explicit stop, a broken binding).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Field {
        /// The go-quai name of the field.
        field: &'static str,
        expected: Uint,
        actual: Uint,
    },
    Error(Error),
}
impl From<Error> for Refusal {
    fn from(e: Error) -> Self {
        Refusal::Error(e)
    }
}
impl Refusal {
    /// The refusal as a component error, for callers that report static text.
    pub fn error(&self) -> Error {
        match self {
            Refusal::Field { field, .. } => Error::Invalid(match *field {
                "conversion flow amount" => "invalid conversion flow amount used",
                "exchange rate" => "invalid exchange rate used",
                _ => "invalid newKQuaiDiscount used",
            }),
            Refusal::Error(e) => e.clone(),
        }
    }
}

/// core/slice.go GetKQuaiAndUpdateBit through StateProcessor: the stored rate
/// and the update bit go-quai v0.57.0 returns for the zone parent `block`.
pub fn stored_rate(block: Hash) -> Override {
    Override {
        block,
        exchange: Uint::from_u128(ECONOMICS.exchange_rate),
        update_bit: 1,
    }
}

fn field(v: &Option<Vec<u8>>) -> Result<Uint, Error> {
    share_rules::uint(v.as_deref().unwrap_or_default())
}

/// The prime controller state after one prime block (the head).
#[derive(Clone)]
pub struct State {
    head: Hash,
    number: u64,
    /// Exchange rates of the canonical prime blocks `first..=number`.
    first: u64,
    rates: VecDeque<Uint>,
    /// The newly confirmed ETXs stored for the head, before pricing. None
    /// when the state was rebuilt without them (before ControllerKickInBlock).
    inbound: Option<Arc<Vec<Transaction>>>,
    /// The token choice sets stored for the head's parent and for the head.
    parent_window: Option<Window>,
    head_window: Window,
}

/// The parts of a [`State`], for replay checkpoints.
#[derive(Clone)]
pub struct Parts {
    pub head: Hash,
    pub number: u64,
    pub first: u64,
    pub rates: Vec<Uint>,
    pub inbound: Option<Vec<Transaction>>,
    pub parent_window: Option<Window>,
    pub head_window: Window,
}

/// The outcome of one prime block: the next state and the ETXs the prime
/// context passes to the region.
#[derive(Clone)]
pub struct Step {
    next: State,
    dominant: Vec<Transaction>,
    forward_rate: Option<Uint>,
}
impl Step {
    /// core/slice.go Append newInboundEtxs after the prime economic branch.
    pub fn dominant(&self) -> &[Transaction] {
        &self.dominant
    }
    /// The rate the conversions were priced at (after ControllerKickInBlock).
    pub fn forward_rate(&self) -> Option<Uint> {
        self.forward_rate
    }
    pub fn state(&self) -> &State {
        &self.next
    }
    pub fn into_state(self) -> State {
        self.next
    }
}

impl State {
    /// The state at a genesis prime block. go-quai's Append returns before it
    /// writes anything for the genesis, so no inbound ETXs are stored.
    pub fn genesis(genesis: &Record) -> Result<Self, Error> {
        let number = genesis.number(0)?;
        Ok(Self {
            head: genesis.id(),
            number,
            first: number,
            rates: VecDeque::from([field(&genesis.object().header().wire().exchange_rate)?]),
            inbound: None,
            parent_window: None,
            head_window: Window::empty_at(genesis.id(), number),
        })
    }
    /// The state after prime block `head` (with parent `parent`), rebuilt
    /// before ControllerKickInBlock: core/slice.go Append requires
    /// params.ExchangeRate on every prime block up to the kick-in, and before
    /// it no token choice set or inbound ETX list is read.
    pub fn before_kick_in(head: Hash, number: u64, parent: Option<Hash>) -> Result<Self, Error> {
        if number >= FORKS.controller_kick_in {
            return Err(Error::Unavailable(
                "prime controller state at or after ControllerKickInBlock",
            ));
        }
        if (number == 0) != parent.is_none() {
            return Err(Error::Invalid("prime controller parent"));
        }
        let kept = (number + 1).min(RATES as u64);
        Ok(Self {
            head,
            number,
            first: number + 1 - kept,
            rates: (0..kept)
                .map(|_| Uint::from_u128(ECONOMICS.exchange_rate))
                .collect(),
            inbound: None,
            parent_window: parent.map(|p| Window::empty_at(p, number - 1)),
            head_window: Window::empty_at(head, number),
        })
    }
    pub fn from_parts(p: Parts) -> Result<Self, Error> {
        if p.rates.is_empty()
            || p.rates.len() > RATES
            || p.first.checked_add(p.rates.len() as u64 - 1) != Some(p.number)
            || p.head_window.block() != p.head
            || p.head_window.number() != p.number
            || p.parent_window
                .as_ref()
                .is_some_and(|w| w.number().checked_add(1) != Some(p.number))
        {
            return Err(Error::Invalid("prime controller state parts"));
        }
        Ok(Self {
            head: p.head,
            number: p.number,
            first: p.first,
            rates: p.rates.into(),
            inbound: p.inbound.map(Arc::new),
            parent_window: p.parent_window,
            head_window: p.head_window,
        })
    }
    pub fn parts(&self) -> Parts {
        Parts {
            head: self.head,
            number: self.number,
            first: self.first,
            rates: self.rates.iter().copied().collect(),
            inbound: self.inbound.as_ref().map(|v| v.to_vec()),
            parent_window: self.parent_window.clone(),
            head_window: self.head_window.clone(),
        }
    }
    /// The prime block this state follows: (hash, prime number).
    pub fn head(&self) -> (Hash, u64) {
        (self.head, self.number)
    }
    /// The exchange rate of the canonical prime block `number`
    /// (HeaderChain.GetBlockByNumber).
    pub fn rate_at(&self, number: u64) -> Result<Uint, Error> {
        if number < self.first || number > self.number {
            return Err(Error::Unavailable("prime exchange rate history"));
        }
        Ok(self.rates[(number - self.first) as usize])
    }
    pub fn head_window(&self) -> &Window {
        &self.head_window
    }
    pub fn parent_window(&self) -> Option<&Window> {
        self.parent_window.as_ref()
    }
    pub fn inbound(&self) -> Option<&[Transaction]> {
        self.inbound.as_ref().map(|v| v.as_slice())
    }
    fn next(&self, block: &Record, rate: Uint, inbound: Vec<Transaction>, window: Window) -> State {
        let mut rates = self.rates.clone();
        let mut first = self.first;
        rates.push_back(rate);
        if rates.len() > RATES {
            rates.pop_front();
            first += 1;
        }
        State {
            head: block.id(),
            number: self.number + 1,
            first,
            rates,
            inbound: Some(Arc::new(inbound)),
            parent_window: Some(self.head_window.clone()),
            head_window: window,
        }
    }
    /// Check prime block `block` (the child of the head `parent`, whose own
    /// parent is `grandparent`) and price `inbound`, its newly confirmed ETXs
    /// (core/slice.go CollectNewlyConfirmedEtxs in the prime context).
    pub fn step(
        &self,
        block: &Record,
        parent: &Record,
        grandparent: Option<&Record>,
        inbound: Vec<Transaction>,
    ) -> Result<Step, Refusal> {
        let number = block.number(0)?;
        if parent.id() != self.head
            || block.parent(0)? != self.head
            || parent.number(0)? != self.number
            || self.number.checked_add(1) != Some(number)
        {
            return Err(Error::Invalid("prime controller head").into());
        }
        let h = block.object().header().wire();
        let discount = field(&h.k_quai_discount)?;
        let flow = field(&h.conversion_flow_amount)?;
        let rate = field(&h.exchange_rate)?;
        let check = |rows: [(&'static str, Uint, Uint); 3]| -> Result<(), Refusal> {
            for (field, expected, actual) in rows {
                if expected != actual {
                    return Err(Refusal::Field {
                        field,
                        expected,
                        actual,
                    });
                }
            }
            Ok(())
        };
        if number <= FORKS.controller_kick_in {
            // Until the controller has kicked in the fields keep their
            // starting values, checked in this order.
            check([
                (
                    "kQuai discount",
                    Uint::from_u128(ECONOMICS.k_quai_discount),
                    discount,
                ),
                (
                    "conversion flow amount",
                    Uint::from_u128(ECONOMICS.conversion_flow_amount),
                    flow,
                ),
                (
                    "exchange rate",
                    Uint::from_u128(ECONOMICS.exchange_rate),
                    rate,
                ),
            ])?;
            let next = self.next(
                block,
                rate,
                inbound.clone(),
                Window::empty_at(block.id(), number),
            );
            return Ok(Step {
                next,
                dominant: inbound,
                forward_rate: None,
            });
        }
        // verifyParentExchangeRateAndFlowAmount: the parent's stored inbound
        // ETXs and the token choice set stored for the grandparent.
        let grandparent = grandparent.ok_or(Error::Unavailable("prime controller grandparent"))?;
        let parent_inbound = self
            .inbound
            .as_ref()
            .ok_or(Error::Unavailable("inbound ETXs of the prime parent"))?;
        let prior = self.parent_window.as_ref().ok_or(Error::Unavailable(
            "token choice set of the prime grandparent",
        ))?;
        let parent_number = parent.number(0)?;
        let parent_old = if parent_number > WINDOW {
            Some(self.rate_at(parent_number - WINDOW)?)
        } else {
            None
        };
        let checked = prime_economics::prepare_component(
            parent,
            grandparent,
            Some(parent_inbound),
            prior,
            parent_old,
            Some(stored_rate(parent.parent(2)?)),
        )?;
        let f = checked.fields();
        check([
            ("conversion flow amount", f.conversion_flow, flow),
            ("exchange rate", f.exchange_rate, rate),
            ("kQuai discount", f.discount, discount),
        ])?;
        // The economic branch of Append on this block's own inbound ETXs,
        // with the token choice set stored for the parent.
        let old = if number > WINDOW {
            Some(self.rate_at(number - WINDOW)?)
        } else {
            None
        };
        let forward = prime_forward::prepare_component(
            block,
            parent,
            Some(&inbound),
            &self.head_window,
            old,
            Some(stored_rate(block.parent(2)?)),
        )?;
        let next = self.next(block, rate, inbound, forward.window().clone());
        Ok(Step {
            next,
            dominant: forward.output().to_vec(),
            forward_rate: Some(forward.forward_rate()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuilt_state_keeps_the_starting_rate_and_stops_at_the_kick_in() {
        let k = FORKS.controller_kick_in;
        let s = State::before_kick_in([1; 32], k - 1, Some([2; 32])).unwrap();
        assert_eq!(s.head(), ([1; 32], k - 1));
        for n in [k - 1, k - 1 - WINDOW, k - RATES as u64] {
            assert_eq!(
                s.rate_at(n).unwrap(),
                Uint::from_u128(ECONOMICS.exchange_rate)
            );
        }
        assert!(matches!(
            s.rate_at(k - 1 - RATES as u64),
            Err(Error::Unavailable(_))
        ));
        assert!(matches!(s.rate_at(k), Err(Error::Unavailable(_))));
        assert_eq!(s.parent_window().map(|w| w.block()), Some([2; 32]));
        assert!(s.inbound().is_none());
        // The kick-in block's inbound ETXs and a stored set are needed after
        // it, so a state at or after the kick-in cannot be rebuilt.
        assert!(State::before_kick_in([1; 32], k, Some([2; 32])).is_err());
        assert!(State::before_kick_in([1; 32], 5, None).is_err());
        let g = State::before_kick_in([1; 32], 0, None).unwrap();
        assert_eq!(g.parts().rates.len(), 1);
        // Parts round-trip, and inconsistent parts are refused.
        let p = s.parts();
        let back = State::from_parts(p.clone()).unwrap();
        assert_eq!(back.head(), s.head());
        let mut bad = p.clone();
        bad.first += 1;
        assert!(State::from_parts(bad).is_err());
        let mut bad = p;
        bad.head = [9; 32];
        assert!(State::from_parts(bad).is_err());
    }
}
