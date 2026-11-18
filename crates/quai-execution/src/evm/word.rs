//! Fixed-width EVM arithmetic, including 512-bit intermediates for modular ops.
use crypto_bigint::{Encoding, U256, U512};
pub type Word = U256;
pub fn bytes(v: Word) -> [u8; 32] {
    v.to_be_bytes()
}
pub fn from_bytes(v: &[u8]) -> Word {
    let mut b = [0; 32];
    let v = &v[v.len().saturating_sub(32)..];
    b[32 - v.len()..].copy_from_slice(v);
    Word::from_be_slice(&b)
}
pub fn uint(v: Word) -> Option<u64> {
    let b = bytes(v);
    if b[..24].iter().any(|b| *b != 0) {
        None
    } else {
        Some(u64::from_be_bytes(b[24..].try_into().unwrap()))
    }
}
pub fn negative(v: Word) -> bool {
    v.bit_vartime(255)
}
pub fn boolean(v: bool) -> Word {
    if v {
        Word::ONE
    } else {
        Word::ZERO
    }
}
pub fn signed_lt(a: Word, b: Word) -> bool {
    match (negative(a), negative(b)) {
        (true, false) => true,
        (false, true) => false,
        _ => a < b,
    }
}
pub fn div(a: Word, b: Word) -> Word {
    if b == Word::ZERO {
        Word::ZERO
    } else {
        a.wrapping_div(&b)
    }
}
pub fn rem(a: Word, b: Word) -> Word {
    if b == Word::ZERO {
        Word::ZERO
    } else {
        a.wrapping_rem(&b)
    }
}
pub fn signed_div(a: Word, b: Word) -> Word {
    if b == Word::ZERO {
        return Word::ZERO;
    }
    let aa = if negative(a) { a.wrapping_neg() } else { a };
    let bb = if negative(b) { b.wrapping_neg() } else { b };
    let v = div(aa, bb);
    if negative(a) != negative(b) {
        v.wrapping_neg()
    } else {
        v
    }
}
pub fn signed_rem(a: Word, b: Word) -> Word {
    let aa = if negative(a) { a.wrapping_neg() } else { a };
    let bb = if negative(b) { b.wrapping_neg() } else { b };
    let v = rem(aa, bb);
    if negative(a) {
        v.wrapping_neg()
    } else {
        v
    }
}
pub fn addmod(a: Word, b: Word, m: Word) -> Word {
    if m == Word::ZERO {
        return Word::ZERO;
    }
    let aa: U512 = a.resize();
    let bb: U512 = b.resize();
    let mm: U512 = m.resize();
    aa.wrapping_add(&bb).wrapping_rem(&mm).resize()
}
pub fn mulmod(a: Word, b: Word, m: Word) -> Word {
    if m == Word::ZERO {
        return Word::ZERO;
    }
    let (lo, hi) = a.mul_wide(&b);
    let product: U512 = hi.concat(&lo);
    let mm: U512 = m.resize();
    product.wrapping_rem(&mm).resize()
}
pub fn pow(mut base: Word, exponent: Word) -> Word {
    let mut out = Word::ONE;
    for i in 0..exponent.bits_vartime() {
        if exponent.bit_vartime(i) {
            out = out.wrapping_mul(&base);
        }
        base = base.wrapping_mul(&base);
    }
    out
}
pub fn sar(value: Word, shift: Word) -> Word {
    let n = uint(shift).unwrap_or(u64::MAX);
    if n >= 256 {
        return if negative(value) {
            Word::MAX
        } else {
            Word::ZERO
        };
    }
    let mut out = value.shr_vartime(n as usize);
    if negative(value) && n != 0 {
        out = out.wrapping_or(&Word::MAX.shl_vartime(256 - n as usize));
    }
    out
}
pub fn sign_extend(value: Word, index: Word) -> Word {
    let Some(n) = uint(index).filter(|n| *n < 31) else {
        return value;
    };
    let bit = n as usize * 8 + 7;
    let mask = Word::MAX.shr_vartime(255 - bit);
    if value.bit_vartime(bit) {
        value.wrapping_or(&mask.wrapping_xor(&Word::MAX))
    } else {
        value.wrapping_and(&mask)
    }
}
