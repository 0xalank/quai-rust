//! Fork-aware ETX emission and Quai-to-Qi conversion inside an EVM transaction.
use super::{
    journal::Journal,
    word::{self, Word},
    CapabilityError, Context, Fault,
};
use crypto_bigint::U512;
use quai_primitives::transaction::{AccessTuple, Address, External, Transaction};
fn magnitude(n: Word) -> Vec<u8> {
    let b = word::bytes(n);
    b[b.iter().position(|b| *b != 0).unwrap_or(32)..].to_vec()
}
fn conversion_allowed(prime: u64) -> bool {
    prime >= 262000
        && !(1_171_500..1_191_500).contains(&prime)
        && !(1_755_000..1_775_000).contains(&prime)
}
fn eligible(s: &Journal, to: Address) -> Result<bool, CapabilityError> {
    let bits = s
        .eligible_slices
        .ok_or(CapabilityError::Context("ETX eligibility unavailable"))?;
    let n = to[0] as usize;
    Ok(bits[n / 8] & (1 << (n % 8)) != 0)
}
fn append(
    s: &mut Journal,
    to: Address,
    from: Address,
    value: Word,
    gas: u64,
    data: Vec<u8>,
    access: Vec<AccessTuple>,
    kind: u64,
) -> Result<(), CapabilityError> {
    let origin = s
        .transaction_hash
        .ok_or(CapabilityError::Context("ETX transaction hash unavailable"))?;
    let index = u16::try_from(s.etxs.len()).map_err(|_| CapabilityError::Limit("ETX count"))?;
    let tx = Transaction::external(External {
        originating_hash: origin,
        index,
        gas,
        to,
        value: magnitude(value),
        data,
        access_list: access,
        sender: from,
        etx_type: kind,
    })
    .map_err(|_| CapabilityError::Limit("emitted ETX encoding"))?;
    s.etxs.push(tx);
    Ok(())
}
fn debit(s: &mut Journal, from: Address, value: Word) -> Result<bool, CapabilityError> {
    let balance = s
        .world()
        .account(&from)?
        .map(|a| a.balance)
        .unwrap_or(Word::ZERO);
    if balance < value {
        return Ok(false);
    }
    s.set_balance(from, balance.wrapping_sub(&value))?;
    Ok(true)
}
fn intrinsic_covers(gas: u64, to: Address, data: &[u8], access: &[AccessTuple]) -> bool {
    let creation = to[1..].iter().all(|b| *b == 0);
    crate::gas::intrinsic_gas(data, access, creation).is_ok_and(|required| required <= gas)
}
pub(crate) fn transfer(
    s: &mut Journal,
    ctx: &Context,
    to: Address,
    value: Word,
    gas: u64,
    data: &[u8],
) -> Result<Result<(), Fault>, CapabilityError> {
    let prime = s
        .prime_number
        .ok_or(CapabilityError::Context("ETX prime context"))?;
    let same = to[0] == (ctx.location[0] << 4 | ctx.location[1]);
    let qi = to[1] & 128 != 0;
    let conversion = same && qi;
    if same && !qi
        || !same && qi
        || conversion
            && (!conversion_allowed(prime) || value < Word::from_u64(10_000_000_000_000_000_000))
        || gas < 42000
    {
        return Ok(Err(Fault::Etx));
    }
    let forwarded = gas - 21000;
    let access = s.access_list.clone();
    if !conversion && prime >= 2_317_000 && !intrinsic_covers(forwarded, to, data, &access) {
        return Ok(Err(Fault::Etx));
    }
    if !debit(s, ctx.address, value)? {
        return Ok(Err(Fault::Etx));
    }
    if s.etxs.len() > u16::MAX as usize {
        return Ok(Err(Fault::Etx));
    }
    if !conversion && !eligible(s, to)? {
        return Ok(Err(Fault::Etx));
    }
    append(
        s,
        to,
        ctx.address,
        value,
        forwarded,
        data.to_vec(),
        access,
        if conversion { 2 } else { 0 },
    )?;
    Ok(Ok(()))
}
/// None preserves the pre-hardening implementation's missing stack result on
/// ineligible ETXs; all other paths produce the ordinary CALL-style 0/1 result.
pub(crate) fn opcode(
    s: &mut Journal,
    ctx: &Context,
    to: Address,
    value: Word,
    limit: Word,
    tip: Word,
    cap: Word,
    data: Vec<u8>,
    access_bytes: &[u8],
) -> Result<Option<bool>, CapabilityError> {
    if to[0] == (ctx.location[0] << 4 | ctx.location[1]) {
        return Ok(Some(false));
    }
    let prime = s
        .prime_number
        .ok_or(CapabilityError::Context("ETX prime context"))?;
    if prime >= 2_317_000 && to[1] & 128 != 0 {
        return Ok(Some(false));
    }
    let sum = tip.wrapping_add(&cap);
    let fee = sum.wrapping_mul(&limit);
    let total = value.wrapping_add(&fee);
    if prime >= 1_919_500 {
        let a: U512 = tip.resize();
        let b: U512 = cap.resize();
        let l: U512 = limit.resize();
        let v: U512 = value.resize();
        let wide = a.wrapping_add(&b);
        let cost = wide.wrapping_mul(&l);
        if wide.bits_vartime() > 256
            || cost.bits_vartime() > 256
            || cost.wrapping_add(&v).bits_vartime() > 256
            || word::uint(limit).is_none()
        {
            return Ok(Some(false));
        }
    }
    let available = s
        .world()
        .account(&ctx.address)?
        .map(|a| a.balance)
        .unwrap_or(Word::ZERO);
    if total == Word::ZERO || available < total || word::uint(limit).is_none_or(|n| n < 21000) {
        return Ok(Some(false));
    }
    let gas = word::uint(limit).unwrap();
    if prime < 2_317_000 {
        debit(s, ctx.address, total)?;
    }
    let access = if access_bytes.is_empty() {
        vec![]
    } else {
        match quai_primitives::rlp::access_list(access_bytes) {
            Ok(v) => v,
            Err(_) => return Ok(Some(false)),
        }
    };
    if prime >= 2_317_000 && !intrinsic_covers(gas, to, &data, &access) {
        return Ok(Some(false));
    }
    if s.etxs.len() > u16::MAX as usize {
        return Ok(Some(false));
    }
    if !eligible(s, to)? {
        return Ok(if prime >= 2_317_000 {
            Some(false)
        } else {
            None
        });
    }
    if prime >= 2_317_000 {
        debit(s, ctx.address, total)?;
    }
    append(s, to, ctx.address, value, gas, data, access, 0)?;
    Ok(Some(true))
}
pub(crate) fn convert(
    s: &mut Journal,
    ctx: &Context,
    to: Address,
    value: Word,
    limit: Word,
) -> Result<bool, CapabilityError> {
    let prime = s
        .prime_number
        .ok_or(CapabilityError::Context("conversion prime context"))?;
    if to[0] != (ctx.location[0] << 4 | ctx.location[1])
        || to[1] & 128 == 0
        || value < Word::from_u64(10_000_000_000_000_000_000)
        || !conversion_allowed(prime)
    {
        return Ok(false);
    }
    let fee = ctx.gas_price.wrapping_mul(&limit);
    let total = value.wrapping_add(&fee);
    if prime >= 1_919_500 {
        let p: U512 = ctx.gas_price.resize();
        let g: U512 = limit.resize();
        let v: U512 = value.resize();
        let fee = p.wrapping_mul(&g);
        if word::uint(limit).is_none()
            || fee.bits_vartime() > 256
            || fee.wrapping_add(&v).bits_vartime() > 256
        {
            return Ok(false);
        }
    }
    let b = word::bytes(limit);
    let gas = u64::from_be_bytes(b[24..].try_into().unwrap());
    if gas < 21000
        || total == Word::ZERO
        || s.world()
            .account(&ctx.address)?
            .map(|a| a.balance)
            .unwrap_or(Word::ZERO)
            < total
    {
        return Ok(false);
    }
    debit(s, ctx.address, total)?;
    if s.etxs.len() > u16::MAX as usize {
        return Ok(false);
    }
    append(s, to, ctx.address, value, gas, vec![], vec![], 2)?;
    Ok(true)
}
