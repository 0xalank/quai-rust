//! `quai_gasPrice`: go-quai `PublicQuaiAPI.GasPrice` (internal/quaiapi/
//! quai_api.go) returns `CalcBaseFee(CurrentHeader()) * 120 / 100`, and
//! `HeaderChain.CalcBaseFee` (core/headerchain.go) is
//! `QiToQuai(block, exchangeRate, block.Difficulty(), MinBaseFeeInQits) /
//! TxGas`. The conversion is consensus code: header validation requires
//! every zone block's base fee to equal `CalcBaseFee(parent)`
//! (core/headerchain_validation.go), which `quai_consensus::economics`
//! ports and the replay checks at every height.
use crate::view::{BlockData, ChainView, ViewError, U256};
use crypto_bigint::Encoding;
use quai_consensus::{economics, rules::Uint, share_rules};

/// Go answers with "method handler crashed" where `CalcBaseFee` returns nil
/// (no prime terminus) or divides by zero.
fn crashed() -> ViewError {
    ViewError::Go("method handler crashed".into())
}

/// `HeaderChain.CalcBaseFee(block)`.
pub fn calc_base_fee<V: ChainView + ?Sized>(
    view: &V,
    block: &BlockData,
) -> Result<U256, ViewError> {
    // The genesis block, and its child with the starting exchange rate.
    if block.number == 0 {
        return Ok([0; 32]);
    }
    let exchange = if block.number == 1 {
        Uint::from_u128(economics::INITIAL_EXCHANGE_RATE)
    } else {
        let hash: [u8; 32] = block
            .object
            .header()
            .wire()
            .prime_terminus_hash
            .as_ref()
            .and_then(|h| h.value.as_slice().try_into().ok())
            .ok_or_else(crashed)?;
        let prime = view.block_by_hash(&hash)?.ok_or_else(crashed)?;
        let rate = prime.object.header().wire().exchange_rate.clone();
        share_rules::uint(rate.as_deref().unwrap_or_default()).map_err(|_| crashed())?
    };
    let w = block.header.wire();
    if w.prime_terminus_number.is_none() || w.number.is_none() || w.difficulty.is_none() {
        return Err(crashed());
    }
    let average = block.object.header().wire().avg_tx_fees.clone();
    let average =
        share_rules::uint(average.as_deref().unwrap_or_default()).map_err(|_| crashed())?;
    let quote = economics::reward_quote(&block.header, exchange, average).map_err(|_| crashed())?;
    Ok(quote.base_fee.to_be_bytes())
}

/// `quai_gasPrice`: the base fee raised by 20% for the worst difficulty
/// adjustment, `fee * 120 / 100`.
pub fn gas_price(base_fee: &U256) -> U256 {
    let mut wide = [0u32; 9];
    // 256 bits as eight 32-bit limbs plus one for the carry of * 120.
    for (i, chunk) in base_fee.chunks(4).enumerate() {
        wide[i + 1] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    let mut carry = 0u64;
    for limb in wide.iter_mut().rev() {
        let v = *limb as u64 * 120 + carry;
        *limb = v as u32;
        carry = v >> 32;
    }
    let mut rem = 0u64;
    for limb in wide.iter_mut() {
        let cur = rem << 32 | *limb as u64;
        *limb = (cur / 100) as u32;
        rem = cur % 100;
    }
    let mut out = [0u8; 32];
    for (i, limb) in wide[1..].iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&limb.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::gas_price;
    #[test]
    fn gas_price_is_six_fifths_rounded_down() {
        let mut v = [0u8; 32];
        v[31] = 100;
        assert_eq!(gas_price(&v)[31], 120);
        v[31] = 7;
        // 7 * 120 / 100 = 8.4
        assert_eq!(gas_price(&v)[31], 8);
        // A carry across limbs: 0x10 << 248 times 1.2 is 0x1333... << 244.
        let mut big = [0u8; 32];
        big[0] = 0x10;
        let out = gas_price(&big);
        assert_eq!(out[0], 0x13);
        assert_eq!(out[1], 0x33);
    }
}
