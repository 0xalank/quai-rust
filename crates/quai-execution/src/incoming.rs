//! Incoming ETX dispatch. Ordered queue membership precedes all state effects.
use crate::{
    economic_state::{self as economy, CONVERSION_LOCK, LOCK_PERIODS},
    evm::{
        state::World,
        word::{bytes, from_bytes, low_u64, Word},
    },
    lockup,
    qi::{OutPoint, Utxo, DENOMINATIONS},
    settlement::{self, BlockContext, Error},
    utxo::UtxoState,
};
use quai_primitives::spec::FORKS;
use quai_primitives::{
    receipt::{Log, Receipt, BLOOM_BYTES},
    transaction::{keccak, Address, Kind, Transaction},
    Hash,
};
#[derive(Clone)]
pub struct Context {
    pub block: BlockContext,
    pub parent_height: u64,
    pub total_etx_gas: u64,
    pub region_remaining: u64,
    pub prime_remaining: u64,
}
pub struct Applied {
    pub created: Vec<(OutPoint, u8)>,
    pub deleted_hashes: Vec<Hash>,
    pub receipt: Option<IncomingReceipt>,
    pub supply_added: Word,
    pub outbound: Vec<Transaction>,
}
fn number(c: &Context) -> Result<u64, Error> {
    crate::evm::word::uint(c.block.vm.block_number).ok_or(Error::Invalid("block number"))
}
fn prime(c: &Context) -> Result<u64, Error> {
    c.block
        .environment
        .prime_number
        .ok_or(Error::Invalid("ETX prime context"))
}
fn local(a: Address, c: &Context) -> bool {
    a[0] == c.block.vm.location[0] << 4 | c.block.vm.location[1]
}
#[derive(Clone)]
pub struct IncomingReceipt {
    pub receipt: Receipt,
    pub gas_used: u64,
}
impl std::ops::Deref for IncomingReceipt {
    type Target = Receipt;
    fn deref(&self) -> &Receipt {
        &self.receipt
    }
}
impl std::ops::DerefMut for IncomingReceipt {
    fn deref_mut(&mut self) -> &mut Receipt {
        &mut self.receipt
    }
}
fn receipt(status: u64, gas: u64) -> IncomingReceipt {
    IncomingReceipt {
        gas_used: gas,
        receipt: Receipt {
            transaction_type: 1,
            post_state: vec![],
            status,
            cumulative_gas_used: 0,
            bloom: vec![0; BLOOM_BYTES],
            logs: vec![],
            outbound_etxs: vec![],
        },
    }
}
fn charge(c: &mut Context, gas: u64, etx: bool) -> Result<(), Error> {
    if c.block.gas_remaining < gas {
        return Err(Error::Invalid("block_gas"));
    }
    c.block.gas_remaining -= gas;
    c.block.cumulative_gas = c
        .block
        .cumulative_gas
        .checked_add(gas)
        .ok_or(Error::Invalid("ETX gas overflow"))?;
    if etx {
        c.total_etx_gas = c
            .total_etx_gas
            .checked_add(gas)
            .ok_or(Error::Invalid("ETX gas overflow"))?;
    }
    Ok(())
}
fn emit_utxos(
    state: &mut UtxoState,
    id: Hash,
    to: Address,
    value: Word,
    lock: Word,
    gas: &mut u64,
    min: u8,
    charge_each: bool,
    c: &mut Context,
    strict: bool,
    created: &mut Vec<(OutPoint, u8)>,
) -> Result<(Word, bool), Error> {
    let mut remainder = value;
    let mut total = Word::ZERO;
    let mut index = 0u16;
    let mut success = true;
    for denomination in (min as usize..DENOMINATIONS.len()).rev() {
        let amount = Word::from_u64(DENOMINATIONS[denomination]);
        let count = remainder.wrapping_div(&amount);
        remainder = remainder.wrapping_rem(&amount);
        let count = low_u64(count);
        for _ in 0..count {
            if index == u16::MAX || charge_each && *gas < 9000 {
                if strict {
                    success = false;
                }
                break;
            }
            if charge_each {
                *gas -= 9000;
                charge(c, 9000, true)?;
            }
            state
                .create_raw(
                    OutPoint { hash: id, index },
                    Utxo {
                        denomination: denomination as u8,
                        address: to.into(),
                        lock: economy::magnitude(lock),
                    },
                )
                .map_err(|e| Error::utxo(e, "created UTXO"))?;
            created.push((OutPoint { hash: id, index }, denomination as u8));
            total = total.wrapping_add(&amount);
            index += 1;
        }
    }
    Ok((total, success))
}
/// The owner contract (bytes 1..21) and delegate (bytes 21..41 of the 73-byte
/// form) of a coinbase ETX that pays into a lockup contract.
fn coinbase_lockup(data: &[u8]) -> Option<(Address, Address)> {
    let owner = crate::slice::field(data, 1).ok()?;
    match data.len() {
        53 => Some((owner, [0; 20])),
        73 => Some((owner, crate::slice::field(data, 21).ok()?)),
        _ => None,
    }
}
fn log(r: &mut IncomingReceipt, to: Address, topic: &[u8], total: Word) {
    r.logs.push(Log {
        address: to,
        topics: vec![keccak(topic)],
        data: economy::magnitude(total),
    });
}
fn inner(
    world: &mut World,
    state: &mut UtxoState,
    tx: &Transaction,
    c: &mut Context,
) -> Result<Applied, Error> {
    let Kind::External(t) = tx.kind() else {
        return Err(Error::Invalid("not external transaction"));
    };
    let id = tx.id(None).map_err(|_| Error::Invalid("ETX hash"))?;
    let mut created = vec![];
    let mut deleted_hashes = vec![];
    let queued = world
        .etxs_mut()
        .pop()
        .map_err(|error| Error::utxo(error, "ETX queue"))?
        .ok_or(Error::Invalid("empty ETX queue"))?;
    if queued
        .id(None)
        .map_err(|_| Error::Invalid("queued ETX hash"))?
        != id
    {
        return Err(Error::Invalid("ETX queue order"));
    }
    let h = number(c)?;
    let p = prime(c)?;
    let qi = t.to[1] & 128 != 0;
    let value = from_bytes(&t.value);
    let mut added = Word::ZERO;
    let mut out = vec![];
    let r;
    if matches!(t.etx_type, 3 | 6) && qi {
        let mut gas = t.gas;
        let min = if t.etx_type == 6 { 6 } else { 0 };
        let period = if t.etx_type == 6 {
            FORKS.unwrap_qi_lock_period(p)
        } else {
            0
        };
        let lock = if period == 0 {
            Word::ZERO
        } else {
            Word::from_u64(h).wrapping_add(&Word::from_u64(period))
        };
        let (total, success) = emit_utxos(
            state,
            id,
            t.to,
            value,
            lock,
            &mut gas,
            min,
            true,
            c,
            true,
            &mut created,
        )?;
        added = total;
        let mut v = receipt(
            if success { 1 } else { 0 },
            if success { t.gas - gas } else { t.gas },
        );
        log(&mut v, t.to, b"QuaiToQiConversion", total);
        return Ok(Applied {
            created,
            deleted_hashes,
            receipt: Some(v),
            supply_added: added,
            outbound: out,
        });
    }
    if t.etx_type == 4 {
        let owner: Address = t
            .data
            .as_slice()
            .try_into()
            .map_err(|_| Error::Invalid("wrapping data length"))?;
        lockup::wrap(world, owner, t.to, value, c.block.vm.location).map_err(
            |error| match error {
                crate::state_error::ReadError::Unavailable(v) => Error::Unavailable(v),
                _ => Error::Invalid("wrapping state"),
            },
        )?;
        charge(c, 100000, true)?;
        return Ok(Applied {
            created,
            deleted_hashes,
            receipt: Some(receipt(1, 100000)),
            supply_added: added,
            outbound: out,
        });
    }
    if t.etx_type == 1 {
        let lock = *t.data.first().ok_or(Error::Invalid("coinbase data"))?;
        if lock > 3 || !local(t.to, c) {
            return Err(Error::Invalid("coinbase recipient/lock"));
        }
        // core/state_processor.go: coinbase gas is free before TimeToStartTx.
        let gas = if c.parent_height < FORKS.start_tx {
            0
        } else {
            21000
        };
        let mut status = 2;
        if qi && p < FORKS.controller_kick_in {
            return Ok(Applied {
                created,
                deleted_hashes,
                receipt: Some(receipt(0, gas)),
                supply_added: added,
                outbound: out,
            });
        } else {
            let reward = economy::reward(value, lock, h)?;
            let unlock = h.wrapping_add(LOCK_PERIODS[lock as usize]);
            if t.data.len() == 33 {
                if qi {
                    let mut unused = 0;
                    let (total, _) = emit_utxos(
                        state,
                        id,
                        t.to,
                        reward,
                        Word::from_u64(unlock),
                        &mut unused,
                        0,
                        false,
                        c,
                        false,
                        &mut created,
                    )?;
                    added = total;
                }
            } else if let Some((owner, delegate)) = coinbase_lockup(&t.data) {
                if !crate::evm::calls::local(&owner, c.block.vm.location) {
                    return Err(Error::Invalid("coinbase owner scope"));
                }
                if h < FORKS.coinbase_lockup_precompile
                    || world.account(&owner)?.is_none_or(|v| v.code.is_empty())
                {
                    status = 0;
                } else {
                    let key = lockup::Key {
                        owner,
                        beneficiary: t.to,
                        lock,
                        epoch: (h / 50000 + 1) as u32,
                    };
                    let (old, new) = lockup::add(world, key, delegate, unlock, reward)
                        .map_err(Error::Unsupported)?;
                    if let Some(old) = old {
                        state.remove_auxiliary_hash(old);
                        deleted_hashes.push(old);
                    }
                    state.add_auxiliary_hash(new);
                }
            } else {
                status = 0;
            }
        }
        let v = receipt(status, gas);
        if h > FORKS.start_tx {
            charge(c, gas, true)?;
        }
        r = Some(v);
    } else if t.etx_type == 5 {
        let sender = if !qi {
            crate::slice::field(&t.data, 2).map_err(|_| Error::Invalid("conversion refund data"))?
        } else {
            t.sender
        };
        if sender[1] & 128 == 0 && qi {
            if !local(sender, c) {
                return Err(Error::Invalid("refund sender scope"));
            }
            economy::add_balance(world, sender, value)?;
            charge(c, 100000, true)?;
            r = Some(receipt(1, 100000));
        } else if sender[1] & 128 != 0 && !qi {
            let mut gas = t.gas;
            let (total, _) = emit_utxos(
                state,
                id,
                sender,
                value,
                Word::from_u64(h).wrapping_add(&Word::from_u64(CONVERSION_LOCK)),
                &mut gas,
                6,
                true,
                c,
                false,
                &mut created,
            )?;
            added = total;
            let mut v = receipt(1, t.gas - gas);
            log(&mut v, t.to, b"QiToQuaiRevert", total);
            r = Some(v);
        } else {
            r = None;
        }
    } else if qi {
        if t.sender[0] == t.to[0] {
            if p < FORKS.controller_kick_in {
                r = Some(receipt(0, 0));
            } else if t.gas < 21000 {
                charge(c, t.gas, false)?;
                r = Some(receipt(0, t.gas));
            } else {
                charge(c, 21000, true)?;
                let mut gas = t.gas - 21000;
                let (total, success) = emit_utxos(
                    state,
                    id,
                    t.to,
                    value,
                    Word::from_u64(h).wrapping_add(&Word::from_u64(CONVERSION_LOCK)),
                    &mut gas,
                    0,
                    true,
                    c,
                    true,
                    &mut created,
                )?;
                added = total;
                let mut v = receipt(
                    if success { 2 } else { 0 },
                    if success { t.gas - gas } else { t.gas },
                );
                log(&mut v, t.to, b"QuaiToQiConversion", total);
                r = Some(v);
            }
        } else {
            if p >= FORKS.security_hardening && value > Word::from_u64(14) {
                return Err(Error::Invalid("regular Qi denomination"));
            }
            let denom = bytes(value)[31];
            let n = *DENOMINATIONS
                .get(denom as usize)
                .ok_or(Error::Invalid("legacy Qi denomination"))?;
            state
                .create_raw(
                    OutPoint {
                        hash: t.originating_hash,
                        index: t.index,
                    },
                    Utxo {
                        denomination: denom,
                        address: t.to.into(),
                        lock: vec![],
                    },
                )
                .map_err(|e| Error::utxo(e, "regular Qi UTXO"))?;
            created.push((
                OutPoint {
                    hash: t.originating_hash,
                    index: t.index,
                },
                denom,
            ));
            added = Word::from_u64(n);
            charge(c, 9000, true)?;
            r = Some(receipt(1, 9000));
        }
    } else if t.etx_type == 2 {
        charge(c, 100000, true)?;
        r = Some(receipt(2, 100000));
    } else {
        let v = settlement::apply_external(world, tx, &mut c.block)?;
        c.total_etx_gas = c
            .total_etx_gas
            .checked_add(v.gas_used)
            .ok_or(Error::Invalid("ETX gas overflow"))?;
        if v.receipt.status == 1 {
            for hash in v.auxiliary.deleted_hashes {
                state.remove_auxiliary_hash(hash);
                deleted_hashes.push(hash);
            }
            for etx in &v.receipt.outbound_etxs {
                let Kind::External(e) = etx.kind() else {
                    return Err(Error::Invalid("outbound ETX type"));
                };
                let budget = if e.to[0] >> 4 == c.block.vm.location[0] {
                    if e.to[0] & 15 == c.block.vm.location[1] {
                        continue;
                    }
                    &mut c.region_remaining
                } else {
                    &mut c.prime_remaining
                };
                *budget = budget
                    .checked_sub(e.gas)
                    .ok_or(Error::Invalid("outbound ETX budget"))?;
            }
            out = v.receipt.outbound_etxs.clone();
        }
        r = Some(IncomingReceipt {
            gas_used: v.gas_used,
            receipt: v.receipt,
        });
    }
    Ok(Applied {
        created,
        deleted_hashes,
        receipt: r,
        supply_added: added,
        outbound: out,
    })
}
/// Incoming rejection leaves accounts, raw lockups, UTXOs, queue and counters intact.
pub fn apply(
    world: &mut World,
    state: &mut UtxoState,
    tx: &Transaction,
    context: &mut Context,
) -> Result<Applied, Error> {
    let mut w = world.clone();
    let mut s = state.clone();
    let mut c = context.clone();
    let out = inner(&mut w, &mut s, tx, &mut c)?;
    *world = w;
    *state = s;
    *context = c;
    Ok(out)
}
