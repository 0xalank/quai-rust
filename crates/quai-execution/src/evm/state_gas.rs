//! Exact pinned LogBig/BinaryLog Q64 scaling; no floating-point approximation.
//! modernc.org/mathutil v1.6.0 normalization rules are preserved; see NOTICE.md.
use super::word::{self, Word};
fn normalize(n: &mut Word, frac: &mut usize) {
    if *n == Word::ZERO {
        return;
    }
    if *frac > 64 {
        let shift = *frac - 64;
        let round = n.bit_vartime(shift - 1);
        *n = n.shr_vartime(shift);
        if round {
            *n = n.wrapping_add(&Word::ONE);
        }
        *frac -= shift;
    }
    let mut i = 0;
    while *frac > 0 && i <= *frac && !n.bit_vartime(i) {
        *frac -= 1;
        i += 1;
    }
    if i != 0 {
        *n = n.shr_vartime(i);
    }
}
pub fn log_q64(value: Word) -> u128 {
    if value <= Word::ONE {
        return 0;
    }
    let characteristic = value.bits_vartime() - 1;
    let mut n = value;
    let mut frac = characteristic;
    normalize(&mut n, &mut frac);
    let mut mantissa = 0u128;
    for _ in 0..64 {
        if frac == 0 && n.bits_vartime() == 1 {
            break;
        }
        n = n.wrapping_mul(&n);
        frac *= 2;
        normalize(&mut n, &mut frac);
        mantissa <<= 1;
        if n.bits_vartime() > frac + 1 {
            mantissa |= 1;
            frac += 1;
            normalize(&mut n, &mut frac);
        }
    }
    ((characteristic as u128) << 64) + mantissa
}
pub fn levels(state: Word, contract: Word) -> u128 {
    log_q64(state) / 4 + log_q64(contract) / 4
}
pub fn scaled(state: Word, contract: Word, base: u64) -> u64 {
    scale_levels(levels(state, contract), base)
}
pub fn scale_levels(levels: u128, base: u64) -> u64 {
    let result = Word::from_u128(levels)
        .wrapping_mul(&Word::from_u64(base))
        .wrapping_div(&Word::from_u128(5u128 << 64));
    // Go big.Int.Uint64 returns the low word. Actual VM rates are small.
    u64::from_be_bytes(word::bytes(result)[24..].try_into().unwrap())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn integer_powers_are_exact() {
        for n in 1..256 {
            assert_eq!(log_q64(Word::ONE.shl_vartime(n)), (n as u128) << 64);
        }
        assert_eq!(scaled(Word::from_u64(1 << 20), Word::ONE, 2100), 2100);
    }
}
