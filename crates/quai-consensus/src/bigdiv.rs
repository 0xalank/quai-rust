//! Exact integer division for crypto-bigint values, without the bit-serial
//! loop of `Uint::wrapping_div` (crypto-bigint 0.5): that loop runs once per
//! bit of the quotient's width, about 1,000 rounds of 16-limb operations to
//! divide a `U1024` by 2, 9 or 100. Integer division has one result; these
//! paths only reach it with fewer operations, and a zero divisor still panics
//! in `wrapping_div` as before.
use crypto_bigint::{Limb, NonZero, Uint};

/// `a / b`, the same quotient as `a.wrapping_div(b)`.
pub fn div<const L: usize>(a: &Uint<L>, b: &Uint<L>) -> Uint<L> {
    let bw = b.as_words();
    if bw.iter().all(|w| *w == 0) {
        // The divide-by-zero panic of the original call.
        return a.wrapping_div(b);
    }
    if a < b {
        return Uint::ZERO;
    }
    // A divisor of one limb: crypto-bigint's limb division, one step per limb.
    if bw[1..].iter().all(|w| *w == 0) {
        if let Some(d) = Option::<NonZero<Limb>>::from(NonZero::new(Limb(bw[0]))) {
            return a.div_rem_limb(d).0;
        }
    }
    // Both fit in four limbs: the same division at a quarter of the width.
    let aw = a.as_words();
    if L > 4 && aw[4..].iter().all(|w| *w == 0) {
        let narrow =
            |w: &[crypto_bigint::Word; L]| Uint::<4>::from_words(std::array::from_fn(|i| w[i]));
        let q = narrow(aw).wrapping_div(&narrow(bw));
        let mut words = [0; L];
        words[..4].copy_from_slice(q.as_words());
        return Uint::from_words(words);
    }
    a.wrapping_div(b)
}

/// [`div`] as a method, to replace `wrapping_div` in place.
pub trait Quotient {
    fn quotient(&self, rhs: &Self) -> Self;
}
impl<const L: usize> Quotient for Uint<L> {
    fn quotient(&self, rhs: &Self) -> Self {
        div(self, rhs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crypto_bigint::{U1024, U256, U512};

    /// Random values of every bit length, and the edges of each path.
    fn values<const L: usize>(seed: &mut u64, n: usize) -> Vec<Uint<L>> {
        let mut next = || {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            *seed
        };
        let mut out = vec![
            Uint::ZERO,
            Uint::ONE,
            Uint::from_u64(2),
            Uint::from_u64(9),
            Uint::from_u64(100),
            Uint::from_u64(u64::MAX),
            Uint::from_u64(u64::MAX).wrapping_add(&Uint::ONE),
            Uint::MAX,
            Uint::MAX.shr_vartime(Uint::<L>::BITS - 256),
            Uint::MAX.shr_vartime(Uint::<L>::BITS - 255),
            Uint::ONE.shl_vartime(256),
        ];
        for _ in 0..n {
            let words: [crypto_bigint::Word; L] = std::array::from_fn(|_| next() as _);
            let shift = (next() % Uint::<L>::BITS as u64) as usize;
            out.push(Uint::from_words(words).shr_vartime(shift));
        }
        out
    }
    fn check<const L: usize>(seed: u64) {
        let mut s = seed;
        let a = values::<L>(&mut s, 60);
        let b = values::<L>(&mut s, 60);
        for x in &a {
            for y in &b {
                if *y == Uint::ZERO {
                    continue;
                }
                assert_eq!(div(x, y), x.wrapping_div(y), "{x} / {y}");
            }
        }
    }
    #[test]
    fn quotient_equals_wrapping_div() {
        check::<{ U256::LIMBS }>(0x1234_5678_9abc_def1);
        check::<{ U512::LIMBS }>(0x0fed_cba9_8765_4321);
        check::<{ U1024::LIMBS }>(0x5555_aaaa_3333_cccc);
        assert_eq!(
            U1024::from_u64(1000).quotient(&U1024::from_u64(7)),
            U1024::from_u64(142)
        );
    }
    #[test]
    #[should_panic(expected = "divide by zero")]
    fn zero_divisor_panics_as_before() {
        let _ = div(&U512::ONE, &U512::ZERO);
    }
}
