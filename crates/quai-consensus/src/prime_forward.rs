//! Prime append conversion forwarding. This is the exact append economic
//! component, not an admission receipt or a source of canonical authority.
use crate::{
    choice_window::{self, Window},
    controllers, economics,
    history::Record,
    prime_economics::{self, Override},
    rules::Uint,
    share_rules, Error,
};
use quai_primitives::spec::FORKS;
use quai_primitives::{
    transaction::{Kind, Transaction},
    Hash,
};
use quai_store::state_db::Workspace;
/// params.BlocksPerYear.
const BLOCKS_PER_YEAR: u64 = quai_primitives::spec::BLOCKS_PER_YEAR;
pub struct Prepared {
    block: Hash,
    window: Window,
    output: Vec<Transaction>,
    forward_rate: Uint,
    refunds: usize,
}
impl Prepared {
    pub fn block(&self) -> Hash {
        self.block
    }
    pub fn window(&self) -> &Window {
        &self.window
    }
    pub fn output(&self) -> &[Transaction] {
        &self.output
    }
    pub fn forward_rate(&self) -> Uint {
        self.forward_rate
    }
    pub fn refunds(&self) -> usize {
        self.refunds
    }
    /// Publish this local auxiliary tree only inside the caller's atomic state
    /// transaction. This does not select or admit a canonical block.
    pub fn persist_window(
        &self,
        ws: &mut Workspace<'_, '_>,
        prior: &Window,
    ) -> std::io::Result<Window> {
        self.window.persist(ws, Some(prior))
    }
}
/// Parent fields/work, historical rates, raw inbound ETXs and override state
/// must already be authenticated by the caller. This component does not widen
/// any native runtime profile. The reference's zero-source nil-refund case is
/// explicitly unavailable here; no default refund value is invented.
pub fn prepare_component(
    block: &Record,
    parent: &Record,
    inbound: Option<&[Transaction]>,
    prior: &Window,
    old_exchange: Option<Uint>,
    authority: Option<Override>,
) -> Result<Prepared, Error> {
    block.verify_binding()?;
    parent.verify_binding()?;
    let number = block.number(0)?;
    if number <= FORKS.controller_kick_in {
        return Err(Error::Unsupported("prime forward before controller"));
    }
    if block.parent(0)? != parent.id()
        || parent.number(0)?.checked_add(1) != Some(number)
        || prior.block() != parent.id()
        || prior.number() != parent.number(0)?
    {
        return Err(Error::Invalid("prime forward parent/window binding"));
    }
    let input = inbound.ok_or(Error::Unavailable("prime forward inbound history"))?;
    for tx in input {
        match tx.kind() {
            Kind::External(e) if e.etx_type == 2 && share_rules::uint(&e.value)? == Uint::ZERO => {
                return Err(Error::Unsupported("reference zero conversion nil refund"))
            }
            Kind::External(_) => {}
            _ => return Err(Error::Invalid("prime inbound non-ETX")),
        }
    }
    let converted = prime_economics::conversions_for_forward(block, input, old_exchange)?;
    let authority = authority.ok_or(Error::Unavailable("prime forward override history"))?;
    if authority.block != block.parent(2)? {
        return Err(Error::Invalid("prime forward override branch"));
    }
    let h = block.object().header().wire();
    let previous = share_rules::uint(h.exchange_rate.as_deref().unwrap_or_default())?;
    let miner = share_rules::uint(h.miner_difficulty.as_deref().unwrap_or_default())?;
    let choice = choice_window::derive(
        parent,
        previous,
        &converted.etxs,
        converted.actual,
        converted.realized,
        miner,
    )?;
    let window = prior.advance(block.id(), parent.id(), number, choice)?;
    // Official Append passes the previous block into feedback with the current
    // block's rate/window. Parent verification uses a different argument scope.
    let forward_rate = if parent.number(2)? <= BLOCKS_PER_YEAR && authority.update_bit == 0 {
        authority.exchange
    } else {
        controllers::exchange_rate_from_sum(
            parent.number(0)?,
            previous,
            share_rules::uint(
                parent
                    .object()
                    .header()
                    .wire()
                    .miner_difficulty
                    .as_deref()
                    .unwrap_or_default(),
            )?,
            window.difficulty_sum(),
        )?
    };
    let quote = economics::reward_quote_at_difficulty(
        block.header(),
        forward_rate,
        share_rules::uint(h.avg_tx_fees.as_deref().unwrap_or_default())?,
        miner,
    )?;
    if converted.pricing.len() != converted.etxs.len() {
        return Err(Error::Unavailable("prime forward pricing rows"));
    }
    let mut output = Vec::with_capacity(converted.etxs.len());
    let mut refunds = 0;
    for (tx, pricing) in converted.etxs.into_iter().zip(converted.pricing) {
        let Kind::External(e) = tx.kind() else {
            return Err(Error::Invalid("prime converted non-ETX"));
        };
        if e.etx_type != 2 {
            output.push(tx);
            continue;
        }
        let mut e = e.clone();
        let amount = if share_rules::uint(&e.value)? == Uint::ZERO {
            e.etx_type = 5;
            refunds += 1;
            pricing.original
        } else {
            let before = pricing
                .before
                .ok_or(Error::Unavailable("prime forward prequote value"))?;
            if e.to[1] & 128 != 0 {
                prime_economics::ratio(before, quote.qi, quote.conversion_quai)?
            } else {
                prime_economics::ratio(before, quote.conversion_quai, quote.qi)?
            }
        };
        e.value = prime_economics::bytes(amount);
        output.push(
            Transaction::external(e).map_err(|_| Error::Invalid("prime forwarded ETX encoding"))?,
        );
    }
    Ok(Prepared {
        block: block.id(),
        window,
        output,
        forward_rate,
        refunds,
    })
}
