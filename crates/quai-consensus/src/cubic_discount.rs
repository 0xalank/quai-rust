//! Native conversion slippage with the pinned Go math/big.Float precision.
//! Every arithmetic operation rounds to nearest even at the reference operand
//! precision. The result truncates only after the complete cubic calculation.
use crate::{rules::Uint, Error};
use crypto_bigint::U1024;
type Nat = U1024;
#[derive(Clone, Copy)]
struct F {
    m: Nat,
    e: i32,
    p: u32,
}
fn unsupported() -> Error {
    Error::Unsupported("conversion binary arithmetic bounds")
}
fn shift(m: Nat, n: usize) -> Result<Nat, Error> {
    if m == Nat::ZERO {
        return Ok(m);
    }
    if m.bits_vartime() + n > 1024 {
        return Err(unsupported());
    }
    Ok(m.shl_vartime(n))
}
impl F {
    fn rounded(mut m: Nat, mut e: i32, p: u32) -> Result<Self, Error> {
        if !(53..=256).contains(&p) {
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
        if self.m.bits_vartime() + b.m.bits_vartime() > 1024 {
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
        let mut q = n.wrapping_div(&b.m);
        if q.bits_vartime() > p as usize {
            k -= 1;
            n = shift(self.m, k)?;
            q = n.wrapping_div(&b.m);
        }
        let rem = n.wrapping_sub(&q.wrapping_mul(&b.m));
        let twice = rem.shl_vartime(1);
        if twice > b.m || (twice == b.m && (q.as_words()[0] & 1) == 1) {
            q = q.wrapping_add(&Nat::ONE);
        }
        Self::rounded(q, self.e - b.e - k as i32, p)
    }
    fn compare(self, b: Self) -> Result<std::cmp::Ordering, Error> {
        let e = self.e.min(b.e);
        Ok(shift(self.m, (self.e - e) as usize)?.cmp(&shift(b.m, (b.e - e) as usize)?))
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

/// Equivalent to misc.ApplyCubicDiscount(value, mean).Int(nil) for nonnegative
/// 256-bit inputs. No host floating point or rational simplification is used.
pub fn apply(value: Uint, mean: Uint) -> Result<Uint, Error> {
    use std::cmp::Ordering;
    let v = F::int(value.resize());
    let m = F::int(mean.resize());
    let ten_mean = m.times(F::int64(10))?;
    let minimum = v
        .times(F::int64(10000).plus(F::int64(20), true)?)?
        .divided(F::int64(10000))?;
    if value <= mean {
        return minimum.integer();
    }
    if v.compare(ten_mean)? == Ordering::Greater {
        return Ok(Uint::ZERO);
    }
    let normalized = v.divided(ten_mean)?;
    let cube = normalized.times(normalized)?.times(normalized)?;
    let discount = cube.plus(F::int64(1).divided(F::int64(1000))?, false)?;
    let one = F::int64(1);
    if one.compare(discount)? == Ordering::Less {
        return Ok(Uint::ZERO);
    }
    one.plus(discount, true)?.times(v)?.integer()
}
