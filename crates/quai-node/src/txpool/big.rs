//! Unsigned integers of any length as minimal big-endian bytes, as go-quai
//! holds them in `*big.Int`: transaction values, prices and fees are not
//! bounded by the decoder. Only the operations the pool needs.
use std::cmp::Ordering;

/// Strip leading zero bytes.
pub fn trim(b: &[u8]) -> Vec<u8> {
    let start = b.iter().position(|x| *x != 0).unwrap_or(b.len());
    b[start..].to_vec()
}
fn trimmed(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|x| *x != 0).unwrap_or(b.len());
    &b[start..]
}
pub fn is_zero(b: &[u8]) -> bool {
    b.iter().all(|x| *x == 0)
}
pub fn cmp(a: &[u8], b: &[u8]) -> Ordering {
    let (a, b) = (trimmed(a), trimmed(b));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}
pub fn from_u64(v: u64) -> Vec<u8> {
    trim(&v.to_be_bytes())
}
/// `big.Int.Uint64()`: the low 64 bits.
pub fn low_u64(b: &[u8]) -> u64 {
    let b = trimmed(b);
    let n = b.len().min(8);
    let mut out = [0u8; 8];
    out[8 - n..].copy_from_slice(&b[b.len() - n..]);
    u64::from_be_bytes(out)
}
/// `big.Int.BitLen()`.
pub fn bit_len(b: &[u8]) -> usize {
    let b = trimmed(b);
    match b.first() {
        None => 0,
        Some(top) => (b.len() - 1) * 8 + (8 - top.leading_zeros() as usize),
    }
}

/// Little-endian 32-bit limbs.
fn limbs(b: &[u8]) -> Vec<u32> {
    let b = trimmed(b);
    let mut out = Vec::with_capacity(b.len() / 4 + 1);
    let mut i = b.len();
    while i > 0 {
        let lo = i.saturating_sub(4);
        let mut v = 0u32;
        for x in &b[lo..i] {
            v = (v << 8) | *x as u32;
        }
        out.push(v);
        i = lo;
    }
    out
}
fn bytes(l: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(l.len() * 4);
    for v in l.iter().rev() {
        out.extend_from_slice(&v.to_be_bytes());
    }
    trim(&out)
}
pub fn add(a: &[u8], b: &[u8]) -> Vec<u8> {
    let (x, y) = (limbs(a), limbs(b));
    let mut out = Vec::with_capacity(x.len().max(y.len()) + 1);
    let mut carry = 0u64;
    for i in 0..x.len().max(y.len()) {
        let s = *x.get(i).unwrap_or(&0) as u64 + *y.get(i).unwrap_or(&0) as u64 + carry;
        out.push(s as u32);
        carry = s >> 32;
    }
    if carry > 0 {
        out.push(carry as u32);
    }
    bytes(&out)
}
/// a - b for a >= b.
pub fn sub(a: &[u8], b: &[u8]) -> Vec<u8> {
    let (x, y) = (limbs(a), limbs(b));
    let mut out = Vec::with_capacity(x.len());
    let mut borrow = 0i64;
    for (i, xi) in x.iter().enumerate() {
        let mut d = *xi as i64 - *y.get(i).unwrap_or(&0) as i64 - borrow;
        borrow = 0;
        if d < 0 {
            d += 1 << 32;
            borrow = 1;
        }
        out.push(d as u32);
    }
    bytes(&out)
}
pub fn mul_u64(a: &[u8], m: u64) -> Vec<u8> {
    mul(a, &from_u64(m))
}
pub fn mul(a: &[u8], b: &[u8]) -> Vec<u8> {
    let (x, y) = (limbs(a), limbs(b));
    if x.is_empty() || y.is_empty() {
        return vec![];
    }
    let mut out = vec![0u32; x.len() + y.len()];
    for (i, xi) in x.iter().enumerate() {
        let mut carry = 0u64;
        for (j, yj) in y.iter().enumerate() {
            let t = *xi as u64 * *yj as u64 + out[i + j] as u64 + carry;
            out[i + j] = t as u32;
            carry = t >> 32;
        }
        let mut k = i + y.len();
        while carry > 0 {
            let t = out[k] as u64 + carry;
            out[k] = t as u32;
            carry = t >> 32;
            k += 1;
        }
    }
    bytes(&out)
}
/// (a / d, a % d) for a small divisor.
pub fn divmod_u32(a: &[u8], d: u32) -> (Vec<u8>, u32) {
    let mut x = limbs(a);
    let mut rem = 0u64;
    for v in x.iter_mut().rev() {
        let cur = (rem << 32) | *v as u64;
        *v = (cur / d as u64) as u32;
        rem = cur % d as u64;
    }
    (bytes(&x), rem as u32)
}
/// Division by an arbitrary divisor (schoolbook on bits; values are short).
pub fn div(a: &[u8], d: &[u8]) -> Vec<u8> {
    divmod(a, d).0
}
pub fn divmod(a: &[u8], d: &[u8]) -> (Vec<u8>, Vec<u8>) {
    if is_zero(d) {
        return (vec![], vec![]);
    }
    let a = trimmed(a);
    let mut q = vec![0u8; a.len()];
    let mut r: Vec<u8> = vec![];
    for (i, byte) in a.iter().enumerate() {
        for bit in (0..8).rev() {
            r = shl1(&r, (byte >> bit) & 1);
            if cmp(&r, d).is_ge() {
                r = sub(&r, d);
                q[i] |= 1 << bit;
            }
        }
    }
    (trim(&q), r)
}
fn shl1(a: &[u8], low: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(a.len() + 1);
    let mut carry = 0u8;
    for b in a.iter().rev() {
        out.push((b << 1) | carry);
        carry = b >> 7;
    }
    if carry > 0 {
        out.push(carry);
    }
    out.reverse();
    if let Some(last) = out.last_mut() {
        *last |= low;
    } else if low > 0 {
        out.push(low);
    }
    trim(&out)
}
/// Decimal digits, as `big.Int.String()`.
pub fn decimal(a: &[u8]) -> String {
    let mut digits = vec![];
    let mut x = trim(a);
    while !x.is_empty() {
        let (q, r) = divmod_u32(&x, 1_000_000_000);
        digits.push(r);
        x = q;
    }
    match digits.split_last() {
        None => "0".into(),
        Some((top, rest)) => {
            let mut s = top.to_string();
            for d in rest.iter().rev() {
                s.push_str(&format!("{d:09}"));
            }
            s
        }
    }
}

/// `checkTxFee`'s quotient: `big.Float` fee / 1e18 at precision
/// max(64, bits(fee)), rounded to nearest even, then `Float64()` (nearest
/// even again; +Inf above the float64 range).
pub fn fee_in_ether(fee: &[u8]) -> f64 {
    let fee = trim(fee);
    if fee.is_empty() {
        return 0.0;
    }
    let prec = bit_len(&fee).max(64);
    let den = from_u64(1_000_000_000_000_000_000);
    // Scale so that the quotient has prec + 2 bits or more, then round.
    // q = floor(fee * 2^k / den) with an exact remainder flag.
    let k = prec + 64;
    let (q, r) = divmod(&shl(&fee, k), &den);
    let sticky = !is_zero(&r);
    let qbits = bit_len(&q);
    // Round q (qbits) to prec bits, nearest even, with the sticky bit.
    let (m, e) = round_bits(&q, qbits, prec, sticky);
    // value = m * 2^(e - k); round m (prec bits) to 53 bits, no sticky
    // beyond m.
    let mbits = bit_len(&m);
    let (m53, e53) = round_bits(&m, mbits, 53, false);
    let exp = e + e53 - k as i64;
    let mant = low_u64(&m53) as f64;
    let total = exp + bit_len(&m53) as i64;
    if total > 1024 {
        return f64::INFINITY;
    }
    mant * 2f64.powi(exp as i32)
}
fn shl(a: &[u8], bits: usize) -> Vec<u8> {
    let mut out = a.to_vec();
    out.extend(std::iter::repeat_n(0u8, bits / 8));
    for _ in 0..bits % 8 {
        out = shl1(&out, 0);
    }
    trim(&out)
}
/// Round `v` (with `vbits` bits) to `want` bits, nearest even, with
/// `sticky` for bits already below `v`. Returns (mantissa, exponent): the
/// value is mantissa * 2^exponent.
fn round_bits(v: &[u8], vbits: usize, want: usize, sticky: bool) -> (Vec<u8>, i64) {
    if vbits <= want {
        return (trim(v), 0);
    }
    let drop = vbits - want;
    let (q, r) = divmod(v, &shl(&[1], drop));
    let half = shl(&[1], drop - 1);
    let up = match cmp(&r, &half) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => sticky || low_u64(&q) & 1 == 1,
    };
    let q = if up { add(&q, &[1]) } else { q };
    if bit_len(&q) > want {
        // Carried into a new bit: drop one more (exact, the low bit is 0).
        (divmod_u32(&q, 2).0, drop as i64 + 1)
    } else {
        (q, drop as i64)
    }
}
/// Go's `%.2f` of a float64.
pub fn format_2f(v: f64) -> String {
    if v.is_infinite() {
        return if v > 0.0 {
            "+Inf".into()
        } else {
            "-Inf".into()
        };
    }
    if v.is_nan() {
        return "NaN".into();
    }
    format!("{v:.2}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arithmetic() {
        assert_eq!(decimal(&[]), "0");
        assert_eq!(decimal(&from_u64(1_000_000_000_000)), "1000000000000");
        assert_eq!(add(&[0xff], &[1]), vec![1, 0]);
        assert_eq!(sub(&[1, 0], &[1]), vec![0xff]);
        assert_eq!(mul_u64(&from_u64(19), 105), from_u64(1995));
        assert_eq!(divmod_u32(&from_u64(1995), 100), (from_u64(19), 95));
        assert_eq!(
            div(&from_u64(10_000_000_000), &from_u64(3)),
            from_u64(3_333_333_333)
        );
        assert_eq!(bit_len(&[0, 1, 0]), 9);
        assert_eq!(low_u64(&[1, 2, 3, 4, 5, 6, 7, 8, 9]), 0x0203040506070809);
    }
    #[test]
    fn fee_cap_quotient() {
        // 10,001 QUAI and 10,000.333... QUAI as in the Go vectors.
        let quai = from_u64(1_000_000_000_000_000_000);
        let f = fee_in_ether(&mul_u64(&quai, 10_001));
        assert_eq!(format_2f(f), "10001.00");
        let f = fee_in_ether(&mul_u64(&from_u64(333_333_333_333_333_333), 30_001));
        assert_eq!(format_2f(f), "10000.33");
        assert_eq!(fee_in_ether(&mul_u64(&quai, 10_000)), 10_000.0);
        assert_eq!(format_2f(0.125), "0.12");
        assert_eq!(format_2f(0.375), "0.38");
    }
}
