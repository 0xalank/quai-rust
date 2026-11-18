//! Workshare target/controller arithmetic. Projections alone do not validate PoW.
use crate::{
    rules::{self, Uint},
    Error,
};
use crypto_bigint::{Encoding, U1024};
use quai_primitives::{
    spec::{self, FORKS},
    Hash, Header,
};
const Q: u64 = 1 << 32;
fn w(n: Uint) -> U1024 {
    n.resize()
}
fn u(n: u64) -> Uint {
    Uint::from_u64(n)
}
fn narrow(n: U1024) -> Result<Uint, Error> {
    if n.bits_vartime() > 256 {
        return Err(Error::Unsupported(
            "share arithmetic exceeds 256-bit domain",
        ));
    }
    Ok(n.resize())
}
fn qdiv(n: U1024, d: U1024, negative: bool) -> U1024 {
    let q = n.wrapping_div(&d);
    if negative && n.wrapping_rem(&d) != U1024::ZERO {
        q.wrapping_add(&U1024::ONE)
    } else {
        q
    }
}
fn signed_update(
    base: Uint,
    n: U1024,
    d: U1024,
    negative: bool,
    minimum: Uint,
) -> Result<Uint, Error> {
    let change = qdiv(n, d, negative);
    if negative {
        if change > w(base) {
            return Ok(minimum);
        }
        Ok(narrow(w(base).wrapping_sub(&change))?.max(minimum))
    } else {
        Ok(narrow(w(base).wrapping_add(&change))?.max(minimum))
    }
}
pub fn uint(bytes: &[u8]) -> Result<Uint, Error> {
    if bytes.len() > 32 {
        return Err(Error::Unsupported("integer domain"));
    }
    let mut out = [0; 32];
    out[32 - bytes.len()..].copy_from_slice(bytes);
    Ok(Uint::from_be_bytes(out))
}
pub fn number(bytes: &[u8]) -> Result<u64, Error> {
    let n = uint(bytes)?;
    if n.bits_vartime() > 64 {
        return Err(Error::Unsupported("history number exceeds u64"));
    }
    Ok(u64::from_be_bytes(
        n.to_be_bytes()[24..].try_into().unwrap(),
    ))
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Triple {
    pub difficulty: Uint,
    pub count: Uint,
    pub uncled: Uint,
}
impl Triple {
    pub fn from_header(header: &Header, sha: bool) -> Result<Self, Error> {
        let x = if sha {
            header.wire().sha_diff_and_count.as_ref()
        } else {
            header.wire().scrypt_diff_and_count.as_ref()
        };
        Ok(match x {
            None => Self::default(),
            Some(x) => Self {
                difficulty: uint(x.difficulty.as_deref().unwrap_or_default())?,
                count: uint(x.count.as_deref().unwrap_or_default())?,
                uncled: uint(x.uncled.as_deref().unwrap_or_default())?,
            },
        })
    }
}
pub fn inclusion_depth(prime: u64) -> usize {
    FORKS.inclusion_depth(prime)
}
pub fn count_limits(prime: u64) -> (usize, usize, usize) {
    FORKS.workshare_counts(prime)
}
pub fn kawpow_share_difficulty(
    diff: Uint,
    sha_count: Uint,
    scrypt_count: Uint,
    sha_target: Uint,
    scrypt_target: Uint,
    kaw_diff: Uint,
) -> Result<Uint, Error> {
    if diff == Uint::ZERO {
        return Err(Error::Invalid("zero block difficulty"));
    }
    let actual = w(sha_count.min(sha_target)).wrapping_add(&w(scrypt_count.min(scrypt_target)));
    let max = U1024::from_u64(8 * Q);
    if actual >= max || kaw_diff == Uint::ZERO {
        return Ok(diff);
    }
    let mut target = U1024::from_u64(9 * Q).wrapping_sub(&actual);
    let percent = w(diff)
        .wrapping_mul(&U1024::from_u64(10000))
        .wrapping_div(&w(kaw_diff));
    if percent >= U1024::from_u64(7500) {
        let discounted = if percent >= U1024::from_u64(9000) {
            U1024::from_u64(Q)
        } else {
            U1024::from_u64(9 * Q)
                .wrapping_mul(&U1024::from_u64(9000).wrapping_sub(&percent))
                .wrapping_div(&U1024::from_u64(1500))
        };
        target = target.min(discounted);
    }
    if target < U1024::from_u64(Q) {
        return Ok(diff);
    }
    narrow(
        w(diff)
            .wrapping_mul(&U1024::from_u64(Q))
            .wrapping_div(&target),
    )
}
pub fn share_target(
    parent_diff: Uint,
    parent_kaw: Uint,
    parent_target: Uint,
    prime: u64,
) -> Result<Uint, Error> {
    if prime < FORKS.kawpow {
        return Err(Error::Unsupported("pre-fork share target"));
    }
    if prime == FORKS.kawpow {
        return Ok(u(3 * Q));
    }
    if prime >= FORKS.inclusion_depth_change {
        // core/headerchain.go CalculateShareTarget: a linear ramp from
        // TargetShaShares to MaxShaShares over InclusionDepthUpdatePeriod.
        let period = FORKS.inclusion_depth_update_period;
        let progress = (prime - FORKS.inclusion_depth_change).min(period);
        return Ok(u(
            3 * Q + (Q as u128 * progress as u128 / period as u128) as u64
        ));
    }
    if parent_diff == Uint::ZERO {
        return Err(Error::Invalid("zero parent difficulty"));
    }
    let cap = w(parent_kaw)
        .wrapping_mul(&U1024::from_u64(3))
        .wrapping_div(&U1024::from_u64(4));
    let negative = cap > w(parent_diff);
    let delta = if negative {
        cap.wrapping_sub(&w(parent_diff))
    } else {
        w(parent_diff).wrapping_sub(&cap)
    };
    let n = delta.wrapping_mul(&w(parent_target));
    let d = w(parent_diff).wrapping_mul(&U1024::from_u64(spec::BLOCKS_PER_DAY));
    let change = qdiv(n, d, negative);
    let next = if negative {
        if change > w(parent_target) {
            U1024::ZERO
        } else {
            w(parent_target).wrapping_sub(&change)
        }
    } else {
        w(parent_target).wrapping_add(&change)
    };
    narrow(next.max(U1024::from_u64(3 * Q)).min(U1024::from_u64(4 * Q)))
}
pub fn next_pow_triple(
    parent_block_diff: Uint,
    parent: &Triple,
    target: Uint,
    share_count: u32,
    uncled_count: u32,
    prime: u64,
    sha: bool,
) -> Result<Triple, Error> {
    if prime < FORKS.kawpow {
        return Err(Error::Unsupported("pre-fork share controller"));
    }
    if prime == FORKS.kawpow {
        let factor = if sha { 167000 * 600 } else { 12 * 60 };
        return Ok(Triple {
            difficulty: narrow(
                w(parent_block_diff)
                    .wrapping_div(&U1024::from_u64(5))
                    .wrapping_mul(&U1024::from_u64(factor))
                    .wrapping_div(&U1024::from_u64(3)),
            )?,
            count: u(3 * Q),
            uncled: Uint::ZERO,
        });
    }
    if parent.difficulty == Uint::ZERO {
        return Err(Error::Unsupported("nonpositive share controller input"));
    }
    let samples = u(share_count as u64 * Q);
    let negative = samples < target;
    let delta = if negative {
        w(target).wrapping_sub(&w(samples))
    } else {
        w(samples).wrapping_sub(&w(target))
    };
    let factor = if prime >= FORKS.conversion_stability {
        3_000_000
    } else {
        300_000
    };
    let period = if prime >= FORKS.conversion_stability {
        20_000
    } else {
        1000
    };
    let n = delta
        .wrapping_mul(&w(parent.difficulty))
        .wrapping_mul(&U1024::from_u64(
            parent.difficulty.bits_vartime() as u64 - 1,
        ));
    let denominator = U1024::from_u64(factor).wrapping_mul(&U1024::from_u64(Q));
    let minimum = u(if sha {
        7_000_000_000_000_000
    } else {
        700_000_000_000
    });
    let difficulty = signed_update(parent.difficulty, n, denominator, negative, minimum)?;
    let ema = |previous: Uint, samples: Uint| {
        narrow(
            w(previous)
                .wrapping_mul(&U1024::from_u64(period - 1))
                .wrapping_add(&w(samples))
                .wrapping_div(&U1024::from_u64(period)),
        )
    };
    Ok(Triple {
        difficulty,
        count: ema(parent.count, samples)?,
        uncled: ema(parent.uncled, u(uncled_count as u64 * Q))?,
    })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Classification {
    Valid,
    Sub,
    Invalid,
    Block,
}
/// A controlled-hash projection for differentials; runtime inclusion uses native recomputed work.
pub fn classify(header: &Header, pow: Hash, p2p_threshold: u16) -> Result<Classification, Error> {
    let wh = header.wire();
    let prime = number(wh.prime_terminus_number.as_ref().unwrap())?;
    let difficulty = uint(wh.difficulty.as_ref().unwrap())?;
    if difficulty == Uint::ZERO {
        return Ok(Classification::Invalid);
    }
    let id = wh
        .aux_pow
        .as_ref()
        .map(|bytes| crate::auxiliary::Auxiliary::decode(bytes).map(|a| a.id()))
        .transpose()?;
    if prime >= FORKS.progpow_grace_end() && id.is_none() {
        return Ok(Classification::Invalid);
    }
    if prime >= FORKS.kawpow && matches!(id, Some(2..=4)) {
        let triple = Triple::from_header(header, id != Some(4))?;
        if triple.difficulty == Uint::ZERO {
            return Ok(Classification::Invalid);
        }
        let target = U1024::ONE
            .shl_vartime(256)
            .wrapping_div(&w(triple.difficulty));
        return Ok(if w(Uint::from_be_bytes(pow)) < target {
            Classification::Valid
        } else {
            Classification::Invalid
        });
    }
    if quai_primitives::meets_blake3_target(&pow, &difficulty.to_be_bytes())
        .map_err(|_| Error::Invalid("difficulty"))?
    {
        return Ok(Classification::Block);
    }
    if prime < FORKS.kawpow {
        if w(Uint::from_be_bytes(pow))
            <= rules::share_target(difficulty, spec::WORKSHARES_THRESHOLD_DIFF)?.resize()
        {
            return Ok(Classification::Valid);
        }
    } else {
        let diff = kawpow_share_difficulty(
            difficulty,
            Triple::from_header(header, true)?.count,
            Triple::from_header(header, false)?.count,
            uint(wh.sha_share_target.as_deref().unwrap_or_default())?,
            uint(wh.scrypt_share_target.as_deref().unwrap_or_default())?,
            uint(wh.kawpow_difficulty.as_deref().unwrap_or_default())?,
        )?;
        if diff == Uint::ZERO {
            return Err(Error::Unsupported("zero computed KAWPOW share difficulty"));
        }
        let target = U1024::ONE.shl_vartime(256).wrapping_div(&w(diff));
        if w(Uint::from_be_bytes(pow)) <= target {
            return Ok(Classification::Valid);
        }
    }
    Ok(
        if w(Uint::from_be_bytes(pow)) <= rules::share_target(difficulty, p2p_threshold)?.resize() {
            Classification::Sub
        } else {
            Classification::Invalid
        },
    )
}
/// KAWPOW difficulty EMA with the reference's u32 liveness-time addition.
/// Zero/negative compact targets and magnitudes outside 256 bits are unqualified.
pub fn next_kawpow_difficulty(
    previous: Uint,
    prime: u64,
    parent: Option<&crate::auxiliary::Auxiliary>,
) -> Result<Uint, Error> {
    if prime < FORKS.kawpow {
        return Err(Error::Unsupported("pre-fork KAWPOW controller"));
    }
    if prime == FORKS.kawpow {
        return Ok(u(21_500_000_000_000));
    }
    let Some(parent) = parent else {
        return Ok(previous);
    };
    let template = match parent.template() {
        Ok(t) => t,
        Err(_) => return Ok(previous),
    };
    let signature = template
        .signature_time
        .ok_or(Error::Invalid("template signature time"))?;
    let timestamp = u32::from_le_bytes(crate::slice::array(&parent.header()[68..72])?);
    if signature.wrapping_add(18) < timestamp {
        return Ok(previous);
    }
    let bits = u32::from_le_bytes(crate::slice::array(&parent.header()[72..76])?);
    if bits & 0x00800000 != 0 {
        return Err(Error::Unsupported("negative compact target"));
    }
    let size = bits >> 24;
    let mantissa = bits & 0x007fffff;
    let target = if size <= 3 {
        U1024::from_u64((mantissa >> (8 * (3 - size))) as u64)
    } else if size <= 128 {
        U1024::from_u64(mantissa as u64).shl_vartime(8 * (size - 3) as usize)
    } else {
        return Err(Error::Unsupported("compact target domain"));
    };
    if target == U1024::ZERO {
        return Err(Error::Unsupported("zero compact target"));
    }
    let donor = U1024::ONE
        .shl_vartime(256)
        .wrapping_div(&target)
        .wrapping_div(&U1024::from_u64(12));
    narrow(
        w(previous)
            .wrapping_mul(&U1024::from_u64(999))
            .wrapping_add(&donor)
            .wrapping_div(&U1024::from_u64(1000)),
    )
}
