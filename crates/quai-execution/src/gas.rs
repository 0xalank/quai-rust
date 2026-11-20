//! Intrinsic gas rules from pinned core/state_transition.go and core/types/qi_tx.go.
use quai_primitives::transaction::{AccessTuple, Kind, Transaction};
/// Counts duplicate access-list entries/keys exactly as the reference does.
/// This computes the transaction charge, not EVM opcode execution or fee settlement.
pub fn intrinsic_gas(
    data: &[u8],
    access_list: &[AccessTuple],
    creation: bool,
) -> Result<u64, String> {
    let nonzero = u64::try_from(data.iter().filter(|v| **v != 0).count())
        .map_err(|_| "data length overflow")?;
    let zero = u64::try_from(data.len()).map_err(|_| "data length overflow")? - nonzero;
    intrinsic_gas_counts(
        zero,
        nonzero,
        access_list.len() as u64,
        access_list
            .iter()
            .try_fold(0u64, |sum, v| sum.checked_add(v.keys.len() as u64))
            .ok_or("access-list overflow")?,
        creation,
    )
}
/// Count-based form supports overflow qualification without allocating enormous inputs.
pub fn intrinsic_gas_counts(
    zero: u64,
    nonzero: u64,
    addresses: u64,
    keys: u64,
    creation: bool,
) -> Result<u64, String> {
    let mut gas: u64 = if creation { 53_000 } else { 21_000 };
    for (count, price) in [(zero, 4), (nonzero, 16), (addresses, 2400), (keys, 1900)] {
        gas = gas
            .checked_add(count.checked_mul(price).ok_or("gas overflow")?)
            .ok_or("gas overflow")?;
    }
    Ok(gas)
}
/// Finite nonnegative scaling only, with product below 2^64. This excludes
/// architecture-dependent float-to-uint behavior on invalid/extreme contexts.
pub fn intrinsic_qi_gas(transaction: &Transaction, scaling_factor: f64) -> Result<u64, String> {
    let Kind::Qi(tx) = transaction.kind() else {
        return Err("not a Qi transaction".into());
    };
    if !scaling_factor.is_finite() || scaling_factor < 0.0 {
        return Err("invalid Qi scaling factor".into());
    }
    let base = (tx.inputs.len() as u64)
        .checked_mul(800)
        .and_then(|n| n.checked_add((tx.outputs.len() as u64) * 9000))
        .and_then(|n| n.checked_add(3000))
        .ok_or("Qi gas overflow")?;
    if scaling_factor < 15.0 {
        return Ok(base);
    }
    let scaled = scaling_factor * base as f64;
    if scaled >= 18_446_744_073_709_551_616.0 {
        return Err("Qi scaled gas overflow".into());
    }
    Ok(scaled as u64 / 15)
}
/// go-quai `types.CalculateBlockQiTxGas`, the gas that prices a Qi
/// transaction in a block (its fee in Quai divided by this gas): the
/// intrinsic gas plus `ETXGas` (21,000) for each output that leaves the zone
/// or converts to Quai. Go judges both by `common.AddressBytes(output.Address)`,
/// the first 20 bytes of the address (ProcessQiTx judges by the last 20, so
/// the two differ for a longer address before Polaris). A shorter address
/// panics Go's conversion; ProcessQiTx refuses it before a block prices the
/// transaction.
pub fn block_qi_gas(
    transaction: &Transaction,
    scaling_factor: f64,
    location: [u8; 2],
) -> Result<u64, String> {
    let Kind::Qi(tx) = transaction.kind() else {
        return Err("not a Qi transaction".into());
    };
    let scaling = if scaling_factor < 15.0 {
        0.0
    } else {
        scaling_factor
    };
    let mut total = intrinsic_qi_gas(transaction, scaling)?;
    let prefix = location[0] << 4 | location[1];
    for output in &tx.outputs {
        let address = output.address.as_deref().unwrap_or_default();
        if address.len() < 20 {
            return Err("Qi output address shorter than 20 bytes".into());
        }
        if address[0] != prefix || address[1] & 128 == 0 {
            total = total.checked_add(21000).ok_or("Qi block gas overflow")?;
        }
    }
    Ok(total)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_arithmetic_and_duplicate_charges() {
        assert_eq!(intrinsic_gas_counts(0, 0, 0, 0, false).unwrap(), 21_000);
        assert_eq!(intrinsic_gas_counts(0, 0, 0, 0, true).unwrap(), 53_000);
        assert_eq!(intrinsic_gas_counts(0, 0, 2, 4, false).unwrap(), 33_400);
        for args in [
            (u64::MAX, 0, 0, 0),
            (0, u64::MAX, 0, 0),
            (0, 0, u64::MAX, 0),
            (0, 0, 0, u64::MAX),
        ] {
            assert!(intrinsic_gas_counts(args.0, args.1, args.2, args.3, false).is_err());
        }
    }
}
