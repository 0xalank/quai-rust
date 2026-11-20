//! Atomic Qi spends/creation, output policy, fees, denomination rules and ETXs.
use crate::{
    evm::{
        calls,
        word::{low_u64, Word},
    },
    gas,
    qi::{self, OutPoint, Utxo, UtxoAddress, UtxoView, DENOMINATIONS},
    settlement::Error,
    utxo::UtxoState,
};
use crypto_bigint::U512;
use quai_primitives::spec::FORKS;
use quai_primitives::{
    receipt::Receipt,
    transaction::{External, Kind, Transaction},
    Hash,
};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug)]
pub struct Context {
    pub inputs: qi::Context,
    pub prime_number: u64,
    pub scaling_factor: f64,
    pub base_fee: Word,
    pub quai_conversion_reward: Word,
    pub qi_reward: Word,
    pub eligible_slices: Hash,
    pub first_qi: bool,
    pub block_gas_limit: u64,
    pub gas_remaining: u64,
    pub used_gas: u64,
    pub region_etx_remaining: u64,
    pub prime_etx_remaining: u64,
}
pub struct AppliedQi {
    pub created: Vec<(OutPoint, Utxo)>,
    pub spent: Vec<OutPoint>,
    pub receipt: Receipt,
    pub gas_used: u64,
    pub fee_qit: u64,
    pub fee_quai: Word,
    pub added_qit: u64,
    pub removed_qit: u64,
    pub outbound: Vec<Transaction>,
}
fn wide(v: Word) -> U512 {
    v.resize()
}
fn narrow(v: U512) -> Result<Word, Error> {
    if v.bits_vartime() > 256 {
        return Err(Error::Invalid("Qi conversion magnitude"));
    }
    Ok(v.resize())
}
fn to_quai(qit: u64, ctx: &Context) -> Result<Word, Error> {
    if ctx.qi_reward == Word::ZERO {
        return Err(Error::Invalid("zero Qi reward"));
    }
    narrow(
        wide(ctx.quai_conversion_reward)
            .wrapping_mul(&U512::from_u64(qit))
            .wrapping_div(&wide(ctx.qi_reward)),
    )
}
fn minimum(ctx: &Context, gas: u64) -> Result<Word, Error> {
    narrow(wide(ctx.base_fee).wrapping_mul(&U512::from_u64(gas)))
}
fn magnitude(value: u64) -> Vec<u8> {
    let b = value.to_be_bytes();
    b[b.iter().position(|v| *v != 0).unwrap_or(8)..].to_vec()
}
fn allowed(ctx: &Context, a: [u8; 20]) -> bool {
    let n = a[0] as usize;
    ctx.eligible_slices[n / 8] & (1 << (n % 8)) != 0
}
pub fn check_denominations(
    inputs: &BTreeMap<u8, u64>,
    outputs: &BTreeMap<u8, u64>,
) -> Result<(), Error> {
    let mut carry = 0u64;
    for i in (1..DENOMINATIONS.len()).rev() {
        let total = inputs
            .get(&(i as u8))
            .copied()
            .unwrap_or(0)
            .wrapping_add(carry);
        let out = outputs.get(&(i as u8)).copied().unwrap_or(0);
        if out > total {
            return Err(Error::Invalid("denomination combining"));
        }
        carry = (total - out).wrapping_mul(DENOMINATIONS[i] / DENOMINATIONS[i - 1]);
    }
    Ok(())
}
pub fn apply_qi(
    state: &mut UtxoState,
    tx: &Transaction,
    ctx: &mut Context,
) -> Result<AppliedQi, Error> {
    let Kind::Qi(qi) = tx.kind() else {
        return Err(Error::Invalid("not a Qi transaction"));
    };
    let total_in =
        qi::check_input_values(tx, state, &ctx.inputs).map_err(|e| Error::utxo(e, "Qi inputs"))?;
    let scaling = if ctx.scaling_factor < 15.0 {
        0.0
    } else {
        ctx.scaling_factor
    };
    let intrinsic = gas::intrinsic_qi_gas(tx, scaling).map_err(|_| {
        Error::Unsupported(crate::evm::CapabilityError::Context(
            "Qi intrinsic gas scaling domain",
        ))
    })?;
    if ctx
        .used_gas
        .checked_add(intrinsic)
        .is_none_or(|n| n > ctx.block_gas_limit)
    {
        return Err(Error::Invalid("block_gas"));
    }
    let id = tx
        .id(None)
        .map_err(|_| Error::Invalid("Qi transaction ID"))?;
    let mut addresses = BTreeSet::new();
    let mut input_counts = BTreeMap::new();
    let mut output_counts = BTreeMap::new();
    let mut removed = vec![];
    for input in &qi.inputs {
        let p = OutPoint {
            hash: input.hash,
            index: input.index,
        };
        let v = state
            .get(&p)
            .map_err(|e| Error::utxo(e, "Qi inputs"))?
            .ok_or(Error::Invalid("Qi inputs"))?;
        // `common.AddressBytes(utxo.Address)`: the first 20 bytes of a
        // longer address (the outputs below use the last 20).
        addresses.insert(v.address.first20());
        *input_counts.entry(v.denomination).or_insert(0) += 1;
        removed.push(p);
    }
    let mut total_out = 0u64;
    let mut aggregate = 0u64;
    let mut conversion = false;
    let mut wrapping = false;
    let mut aggregate_to = [0; 20];
    let mut region = 0u64;
    let mut prime = 0u64;
    let mut outbound = vec![];
    let mut created = vec![];
    let mut added = 0u64;
    let mut used = intrinsic;
    let prefix = ctx.inputs.location[0] << 4 | ctx.inputs.location[1];
    for (index, output) in qi.outputs.iter().enumerate() {
        // core/state_processor.go:1999: fewer than 20 bytes fail always
        // (they would panic the [20]byte gas conversion), more than 20 fail
        // from Polaris on. Before Polaris a longer address keeps all its
        // bytes in the UTXO and in its hash; the rules here use
        // `common.BytesToAddress`, its last 20 bytes (the gas price of
        // `CalculateBlockQiTxGas` uses the first 20).
        let raw = output.address.as_deref().unwrap_or_default();
        if raw.len() != 20 && ctx.prime_number >= FORKS.polaris {
            return Err(Error::Invalid("Qi output address length"));
        }
        let full = UtxoAddress::new(raw).ok_or(Error::Invalid("Qi output address length"))?;
        let a = full.last20();
        let value = *DENOMINATIONS
            .get(output.denomination as usize)
            .ok_or(Error::Invalid("Qi output denomination"))?;
        total_out = total_out
            .checked_add(value)
            .ok_or(Error::Invalid("Qi output value overflow"))?;
        if !addresses.insert(a) {
            return Err(Error::Invalid("Qi address reuse"));
        }
        let local_quai = a[0] == prefix && a[1] & 128 == 0;
        // 20 bytes of Qi data name the wrapping contract.
        let wrapper = <[u8; 20]>::try_from(qi.data.as_slice()).ok();
        let converts = local_quai && qi.data.len() == 22;
        let aggregated = converts || local_quai && wrapper.is_some();
        // Go counts every output and then uncounts the ones that conversion or
        // wrapping aggregates (core/state_processor.go ProcessQiTx).
        if !aggregated {
            *output_counts.entry(output.denomination).or_insert(0) += 1;
        }
        if converts {
            if conversion && a != aggregate_to {
                return Err(Error::Invalid("multiple conversion destinations"));
            }
            conversion = true;
            aggregate_to = a;
            aggregate += value;
            addresses.remove(&a);
            continue;
        } else if let (true, Some(owner)) = (local_quai, wrapper) {
            if !calls::local(&owner, ctx.inputs.location) {
                return Err(Error::Invalid("Qi wrapping contract"));
            }
            wrapping = true;
            aggregate_to = a;
            aggregate += value;
            addresses.remove(&a);
            if ctx.prime_number >= FORKS.qi_wrapping_change {
                continue;
            }
        } else if a[1] & 128 == 0 {
            return Err(Error::Invalid("Qi output ledger"));
        }
        if a[0] != prefix {
            if a[0] >> 4 == ctx.inputs.location[0] {
                region = region
                    .checked_add(21000)
                    .ok_or(Error::Invalid("ETX region gas overflow"))?;
            } else {
                prime = prime
                    .checked_add(21000)
                    .ok_or(Error::Invalid("ETX prime gas overflow"))?;
            }
            if !allowed(ctx, a) {
                return Err(Error::Invalid("Qi ETX ineligible"));
            }
            let mut sender = [0; 20];
            sender[0] = prefix;
            let out = Transaction::external(External {
                originating_hash: id,
                index: u16::try_from(index).map_err(|_| Error::Invalid("Qi output index"))?,
                gas: 21000,
                to: a,
                value: magnitude(output.denomination as u64),
                data: vec![],
                access_list: vec![],
                sender,
                etx_type: 0,
            })
            .map_err(|_| Error::Invalid("Qi ETX encoding"))?;
            outbound.push(out);
            used = used
                .checked_add(21000)
                .ok_or(Error::Invalid("Qi gas overflow"))?;
        } else {
            let v = Utxo {
                denomination: output.denomination,
                address: full,
                lock: vec![],
            };
            created.push((
                OutPoint {
                    hash: id,
                    index: u16::try_from(index).map_err(|_| Error::Invalid("Qi output index"))?,
                },
                v,
            ));
            added += value;
        }
    }
    if total_out > total_in {
        return Err(Error::Invalid("Qi overspend"));
    }
    let fee = total_in - total_out;
    let fee_quai = to_quai(fee, ctx)?;
    let mut required = intrinsic
        .checked_add(outbound.len() as u64 * 42000)
        .ok_or(Error::Invalid("Qi required gas overflow"))?;
    if fee_quai < minimum(ctx, required)? {
        return Err(Error::Invalid("Qi minimum fee"));
    }
    if conversion && FORKS.conversion_hold(ctx.prime_number) {
        return Err(Error::Invalid("Qi conversion hold"));
    }
    if conversion || wrapping {
        if conversion && wrapping {
            return Err(Error::Invalid("mixed conversion and wrapping"));
        }
        required = required
            .checked_add(100000)
            .ok_or(Error::Invalid("Qi conversion gas overflow"))?;
        if fee_quai < minimum(ctx, required)? {
            return Err(Error::Invalid("Qi conversion minimum fee"));
        }
        if ctx.base_fee == Word::ZERO {
            return Err(Error::Invalid("zero conversion base fee"));
        }
        prime = prime
            .checked_add(100000)
            .ok_or(Error::Invalid("ETX conversion gas overflow"))?;
        let excess = fee_quai
            .wrapping_sub(&minimum(ctx, required)?)
            .wrapping_div(&ctx.base_fee);
        let gas = low_u64(excess);
        let mut sender = [0; 20];
        sender[0] = prefix;
        outbound.push(
            Transaction::external(External {
                originating_hash: id,
                index: 0,
                gas,
                to: aggregate_to,
                value: magnitude(aggregate),
                data: qi.data.clone(),
                access_list: vec![],
                sender,
                etx_type: if wrapping { 4 } else { 2 },
            })
            .map_err(|_| Error::Invalid("conversion ETX encoding"))?,
        );
        used = used
            .checked_add(21000)
            .ok_or(Error::Invalid("Qi gas overflow"))?;
    }
    if used > ctx.gas_remaining {
        return Err(Error::Invalid("block_gas"));
    }
    if region > ctx.region_etx_remaining || prime > ctx.prime_etx_remaining {
        return Err(Error::Invalid("Qi ETX budget"));
    }
    if !ctx.first_qi {
        check_denominations(&input_counts, &output_counts)?;
    }
    if !tx
        .verify_qi_signature()
        .map_err(|_| Error::Invalid("Qi signature"))?
    {
        return Err(Error::Invalid("Qi signature"));
    }
    let cumulative = ctx
        .used_gas
        .checked_add(used)
        .ok_or(Error::Invalid("Qi cumulative gas overflow"))?;
    state
        .apply_delta(&removed, &created)
        .map_err(|e| Error::utxo(e, "Qi state delta"))?;
    ctx.gas_remaining -= used;
    ctx.used_gas = cumulative;
    ctx.region_etx_remaining -= region;
    ctx.prime_etx_remaining -= prime;
    ctx.first_qi = false;
    let receipt = Receipt {
        transaction_type: 2,
        post_state: vec![],
        status: 1,
        cumulative_gas_used: 0,
        bloom: vec![0; quai_primitives::receipt::BLOOM_BYTES],
        logs: vec![],
        outbound_etxs: outbound.clone(),
    };
    Ok(AppliedQi {
        created,
        spent: removed,
        receipt,
        gas_used: used,
        fee_qit: fee,
        fee_quai,
        added_qit: added,
        removed_qit: total_in,
        outbound,
    })
}

#[cfg(test)]
mod storage_tests {
    use super::*;
    use std::{collections::BTreeMap, sync::Arc};
    #[derive(Debug)]
    struct Broken;
    impl crate::utxo::Reader for Broken {
        fn identity(&self) -> Hash {
            [13; 32]
        }
        fn get(&self, _: &OutPoint) -> Result<Option<Utxo>, qi::ReadError> {
            Err(qi::ReadError::Unavailable("missing database node".into()))
        }
        fn scan(&self, _: usize, _: usize) -> Result<BTreeMap<OutPoint, Utxo>, qi::ReadError> {
            Err(qi::ReadError::Unavailable("missing database node".into()))
        }
        fn loaded(&self) -> usize {
            0
        }
    }
    #[test]
    fn signed_qi_backend_failure_remains_unavailable_without_state_or_gas_mutation() {
        let c: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/protocol/qi-settlement.json"
        ))
        .unwrap();
        let row = c["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["accepted"] == true)
            .unwrap();
        let bytes =
            |v: &serde_json::Value| quai_primitives::decode_hex(v.as_str().unwrap()).unwrap();
        let tx = Transaction::decode(&bytes(&row["transaction"])).unwrap();
        let mut state = UtxoState::from_reader(
            Arc::new(Broken),
            row["utxos"].as_array().unwrap().len() as u64,
            &bytes(&row["before_accumulator"]),
            row["utxos"].as_array().unwrap().len() as u64,
        )
        .unwrap();
        let mut ctx = Context {
            inputs: qi::Context {
                chain_id: vec![5, 57],
                block_height: row["height"].as_u64().unwrap(),
                location: [
                    row["location"][0].as_u64().unwrap() as u8,
                    row["location"][1].as_u64().unwrap() as u8,
                ],
            },
            prime_number: row["prime_number"].as_u64().unwrap(),
            scaling_factor: 0.0,
            base_fee: Word::ZERO,
            quai_conversion_reward: Word::from_u64(1),
            qi_reward: Word::from_u64(1),
            eligible_slices: bytes(&row["eligible_slices"]).try_into().unwrap(),
            first_qi: true,
            block_gas_limit: 30000000,
            gas_remaining: 30000000,
            used_gas: 0,
            region_etx_remaining: 1000000,
            prime_etx_remaining: 1000000,
        };
        let before = state.clone();
        let gas = (
            ctx.gas_remaining,
            ctx.used_gas,
            ctx.region_etx_remaining,
            ctx.prime_etx_remaining,
            ctx.first_qi,
        );
        assert!(matches!(
            apply_qi(&mut state, &tx, &mut ctx),
            Err(Error::Unavailable(_))
        ));
        assert_eq!(state, before);
        assert_eq!(
            (
                ctx.gas_remaining,
                ctx.used_gas,
                ctx.region_etx_remaining,
                ctx.prime_etx_remaining,
                ctx.first_qi
            ),
            gas
        );
    }
}
