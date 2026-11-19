//! Bounded binary-precision reproduction of the pinned Go weak-share cubic.
//! No host floating-point operations or general exponent approximation.
use crate::bigdiv::Quotient as _;
use crate::{rules::Uint, Error};
use crypto_bigint::U256;
type Nat = U256;
#[derive(Clone, Copy)]
struct F {
    m: Nat,
    e: i32,
    p: u32,
}
fn unsupported() -> Error {
    Error::Unsupported("weak-share binary arithmetic bounds")
}
fn shift(m: Nat, n: usize) -> Result<Nat, Error> {
    if m == Nat::ZERO {
        return Ok(m);
    }
    if m.bits_vartime() + n > 256 {
        return Err(unsupported());
    }
    Ok(m.shl_vartime(n))
}
impl F {
    fn rounded(mut m: Nat, mut e: i32, p: u32) -> Result<Self, Error> {
        if !(53..=73).contains(&p) {
            return Err(unsupported());
        }
        if m.bits_vartime() > p as usize {
            let n = m.bits_vartime() - p as usize;
            let q = m.shr_vartime(n);
            let remainder = m.wrapping_sub(&q.shl_vartime(n));
            let half = Nat::ONE.shl_vartime(n - 1);
            m = q;
            if remainder > half || (remainder == half && (m.as_words()[0] & 1) == 1) {
                m = m.wrapping_add(&Nat::ONE);
            }
            e += n as i32;
            if m.bits_vartime() > p as usize {
                m = m.shr_vartime(1);
                e += 1;
            }
        }
        Ok(Self { m, e, p })
    }
    fn int(m: Nat) -> Self {
        Self {
            m,
            e: 0,
            p: (m.bits_vartime() as u32).max(64),
        }
    }
    fn int64(n: u64) -> Self {
        Self {
            m: Nat::from_u64(n),
            e: 0,
            p: 64,
        }
    }
    fn binary64(n: f64) -> Self {
        let bits = n.to_bits();
        Self {
            m: Nat::from_u64((bits & ((1 << 52) - 1)) | (1 << 52)),
            e: ((bits >> 52) & 2047) as i32 - 1023 - 52,
            p: 53,
        }
    }
    fn plus(self, b: Self, subtract: bool) -> Result<Self, Error> {
        let p = self.p.max(b.p);
        if self.m == Nat::ZERO {
            if subtract && b.m != Nat::ZERO {
                return Err(unsupported());
            }
            return Self::rounded(b.m, b.e, p);
        }
        if b.m == Nat::ZERO {
            return Self::rounded(self.m, self.e, p);
        }
        let e = self.e.min(b.e);
        let a = shift(self.m, (self.e - e) as usize)?;
        let b = shift(b.m, (b.e - e) as usize)?;
        if subtract && a < b {
            return Err(unsupported());
        }
        let m = if subtract {
            a.wrapping_sub(&b)
        } else {
            a.wrapping_add(&b)
        };
        Self::rounded(m, e, p)
    }
    fn times(self, b: Self) -> Result<Self, Error> {
        if self.m.bits_vartime() + b.m.bits_vartime() > 256 {
            return Err(unsupported());
        }
        Self::rounded(self.m.wrapping_mul(&b.m), self.e + b.e, self.p.max(b.p))
    }
    fn divided(self, b: Self) -> Result<Self, Error> {
        if b.m == Nat::ZERO {
            return Err(unsupported());
        }
        let p = self.p.max(b.p);
        if self.m == Nat::ZERO {
            return Ok(Self {
                m: Nat::ZERO,
                e: 0,
                p,
            });
        }
        let mut k = p as usize + b.m.bits_vartime() - self.m.bits_vartime();
        let mut n = shift(self.m, k)?;
        let mut q = n.quotient(&b.m);
        if q.bits_vartime() > p as usize {
            k -= 1;
            n = shift(self.m, k)?;
            q = n.quotient(&b.m);
        }
        let rem = n.wrapping_sub(&q.wrapping_mul(&b.m));
        let twice = rem.shl_vartime(1);
        if twice > b.m || (twice == b.m && (q.as_words()[0] & 1) == 1) {
            q = q.wrapping_add(&Nat::ONE);
        }
        Self::rounded(q, self.e - b.e - k as i32, p)
    }
    fn integer(self) -> Result<Uint, Error> {
        let m = if self.e >= 0 {
            shift(self.m, self.e as usize)?
        } else if -self.e as usize >= self.m.bits_vartime() {
            Nat::ZERO
        } else {
            self.m.shr_vartime(-self.e as usize)
        };
        if m.bits_vartime() > 256 {
            return Err(unsupported());
        }
        Ok(m.resize())
    }
}
/// The deficit is threshold entropy minus work entropy in Q64. Reference
/// precision is max(64, bitlen(deficit), 65); distance division truncates last.
// 0.6931 is go-quai `common/math/big.go` `ln2 = big.NewFloat(0.6931)`, not
// `LN_2`: consensus needs the exact Go value.
#[allow(clippy::approx_constant)]
pub fn discount(deficit: Uint, distance: u64) -> Result<Uint, Error> {
    if deficit > Uint::from_u64(256).shl_vartime(64) || distance > 4 {
        return Err(unsupported());
    }
    let scale = F::int(Nat::ONE.shl_vartime(64));
    let x = F::int(deficit.resize()).divided(scale)?;
    let integral = x.integer()?.as_words()[0];
    let dec = x.plus(F::int64(integral), true)?;
    let squared = dec.times(dec)?;
    let cubed = squared.times(dec)?;
    let t1 = F::binary64(0.6931).times(dec)?;
    let t2 = F::binary64(0.24022).times(squared)?;
    let t3 = F::binary64(0.055).times(cubed)?;
    let sum = t1
        .plus(t2, false)?
        .plus(t3, false)?
        .plus(F::binary64(1.0), false)?;
    let power = sum.times(F {
        m: Nat::ONE,
        e: integral as i32,
        p: 64,
    })?;
    Ok(scale
        .divided(power)?
        .integer()?
        .shr_vartime(distance as usize))
}
