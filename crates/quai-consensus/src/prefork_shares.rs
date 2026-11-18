//! Native pre-KAWPOW strong/weak share entropy with exact reference rounding.
use crate::{
    history::{History, Record},
    rules::{self, Uint},
    share_rules,
    verifier::Verifier,
    Error,
};
use crypto_bigint::{Encoding, U512};
#[derive(Debug)]
pub struct Entropy {
    pub workshares: Uint,
    pub uncled: Uint,
    pub count: usize,
    pub full_target: usize,
    pub weak: usize,
}
fn narrow(v: U512) -> Result<Uint, Error> {
    if v.bits_vartime() > 256 {
        return Err(Error::Unsupported("uncle entropy magnitude"));
    }
    Ok(v.resize())
}
pub fn native_components(
    v: &mut Verifier,
    history: &History,
    block: &Record,
) -> Result<Entropy, Error> {
    if block.prime() >= rules::KAWPOW_FORK {
        return Err(Error::Unsupported("pre-fork workshare profile required"));
    }
    let headers = block.uncles()?;
    if headers.len() > share_rules::count_limits(block.prime()).0 {
        return Err(Error::Invalid("too many entropy uncles"));
    }
    if headers.is_empty() {
        return Ok(Entropy {
            workshares: Uint::ZERO,
            uncled: Uint::ZERO,
            count: 0,
            full_target: 0,
            weak: 0,
        });
    }
    let difficulty = block.difficulty()?;
    if difficulty <= Uint::ONE {
        return Err(Error::Unsupported("pre-fork entropy target truncation"));
    }
    let target = narrow(
        U512::ONE
            .shl_vartime(256)
            .wrapping_div(&difficulty.resize()),
    )?;
    let threshold = rules::intrinsic(target.to_be_bytes())?;
    let mut workshares = U512::ZERO;
    let mut uncled = U512::ZERO;
    let mut full_target = 0;
    for h in &headers {
        if h.wire().aux_pow.is_some() {
            return Err(Error::Unsupported("pre-fork auxiliary uncle"));
        }
        // Do not promote inclusion-only work to a full-target proof.
        let location = &block.header().wire().location.as_ref().unwrap().value;
        let work = v.work_for_inclusion(h, location)?;
        let own_difficulty = share_rules::uint(h.wire().difficulty.as_deref().unwrap())?;
        let intrinsic = rules::intrinsic(work.hash())?;
        if quai_primitives::meets_blake3_target(&work.hash(), &own_difficulty.to_be_bytes())
            .map_err(|_| Error::Invalid("uncle difficulty"))?
        {
            full_target += 1;
            let proof = v.verify_header_proof(h)?;
            uncled = uncled.wrapping_add(&rules::intrinsic(proof.hash())?.resize());
        }
        let full_relative =
            quai_primitives::meets_blake3_target(&work.hash(), &difficulty.to_be_bytes())
                .map_err(|_| Error::Invalid("entropy target"))?;
        let distance = history.workshare_distance(block, h)?;
        let weight = if full_relative {
            if intrinsic < threshold {
                return Err(Error::Invalid("negative full-share weight"));
            }
            intrinsic
                .wrapping_sub(&threshold)
                .shr_vartime(distance as usize)
        } else {
            if threshold < intrinsic {
                return Err(Error::Invalid("negative weak-share deficit"));
            }
            crate::weak_float::discount(threshold.wrapping_sub(&intrinsic), distance)?
        };
        workshares = workshares.wrapping_add(&weight.resize());
    }
    Ok(Entropy {
        workshares: narrow(workshares)?,
        uncled: narrow(uncled)?,
        count: headers.len(),
        full_target,
        weak: headers.len() - full_target,
    })
}
