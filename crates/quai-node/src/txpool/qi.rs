//! go-quai `ValidateQiTxInputs` and `ValidateQiTxOutputsAndSignature`
//! (core/state_processor.go) as the pool calls them in `addQiTxs`, with
//! Go's error texts, and the "inactive chain" check of `addQiTxs`.
use super::big;
use super::tx::{PoolTx, QiBody};
use super::{block, checksum, hex0x, PoolChain};
use quai_consensus::{economics, rules::Uint, share_rules};
use quai_primitives::spec::FORKS;
use quai_primitives::transaction::keccak;
use quai_rpc::view::BlockData;
use std::collections::BTreeSet;

/// `types.Denominations` in qits.
pub const DENOMINATIONS: [u64; 15] = [
    1,
    5,
    10,
    50,
    100,
    500,
    1000,
    5000,
    10_000,
    20_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
];
const MAX_DENOMINATION: u8 = 14;
const MAX_QI_TX_DATA_LENGTH: usize = 22;
const TX_GAS: u64 = 21_000;
const ETX_GAS: u64 = 21_000;
const QI_TO_QUAI_CONVERSION_GAS: u64 = 100_000;
const K_QUAI_CHANGE_HOLD_INTERVAL: u64 = 20_000;
const QI_WRAPPING_CHANGE_BLOCK: u64 = 1_570_000;

/// `%032x` of a `common.Hash`: 64 hex digits without a prefix.
fn x(h: &[u8; 32]) -> String {
    hex0x(h)[2..].to_string()
}
fn denomination_error(d: u8) -> String {
    format!(
        "transaction output value of {d} is higher than max allowed value of {MAX_DENOMINATION}"
    )
}
/// The address of an input key (`crypto.PubkeyBytesToAddress`).
fn key_address(key: &[u8]) -> [u8; 20] {
    let h = keccak(&key[1..]);
    let mut a = [0; 20];
    a.copy_from_slice(&h[12..]);
    a
}
fn fixed20(b: &[u8]) -> [u8; 20] {
    let mut out = [0; 20];
    let n = b.len().min(20);
    out[20 - n..].copy_from_slice(&b[b.len() - n..]);
    out
}

/// `common.GetHierarchySizeForExpansionNumber` (regions, zones).
fn hierarchy(expansion: u8) -> (u64, u64) {
    let (mut regions, mut zones) = (1u64, 2u64);
    match expansion {
        0 => return (1, 1),
        1 => return (1, 2),
        _ => {}
    }
    for e in 2..=expansion {
        if e % 2 == 0 {
            regions += 1;
        } else {
            zones += 1;
        }
    }
    (regions, zones)
}
/// `common.NewChainsAdded(expansion)` as [region, zone] bytes; a region
/// location is `None` for the zone byte.
fn new_chains_added(expansion: u8) -> Vec<(u8, Option<u8>)> {
    let (old_regions, _) = hierarchy(expansion.wrapping_sub(1));
    let (new_regions, new_zones) = hierarchy(expansion);
    let mut out = vec![];
    if new_regions <= old_regions {
        for i in 0..old_regions {
            out.push((i as u8, Some((new_zones - 1) as u8)));
        }
    } else {
        out.push(((new_regions - 1) as u8, None));
        for i in 0..new_zones {
            out.push(((new_regions - 1) as u8, Some(i as u8)));
        }
    }
    out
}
/// `common.IsInChainScope(b, loc)`.
fn in_chain_scope(b: &[u8], loc: (u8, Option<u8>)) -> bool {
    let Some(zone) = loc.1 else {
        return false;
    };
    let prefix = (loc.0 << 4).wrapping_add(zone);
    let mut zero = [0u8; 32];
    zero[12] = prefix;
    let mut h = [0u8; 32];
    let n = b.len().min(32);
    h[32 - n..].copy_from_slice(&b[b.len() - n..]);
    if h == zero {
        return true;
    }
    !b.is_empty() && b[0] == prefix
}
/// `addQiTxs`: refuse an output to a chain the expansion did not add.
pub fn inactive_output(tx: &QiBody, expansion: u8) -> bool {
    let active = new_chains_added(expansion);
    tx.outputs.iter().any(|o| {
        let addr = o.address.as_deref().unwrap_or_default();
        !active.iter().any(|l| in_chain_scope(addr, *l))
    })
}

/// `ValidateQiTxInputs`: the total value of the inputs in qits.
pub fn validate_inputs(
    tx: &PoolTx,
    chain: &dyn PoolChain,
    current: &BlockData,
) -> Result<u64, String> {
    let Some(q) = tx.qi() else {
        return Err(format!("tx {} is not a QiTx", x(&tx.hash)));
    };
    if q.inputs.is_empty() {
        return Err("QiTx must have at least one input".into());
    }
    let mut seen = BTreeSet::new();
    for i in &q.inputs {
        if !seen.insert((i.hash, i.index)) {
            return Err(format!("duplicate Qi input {}:{}", hex0x(&i.hash), i.index));
        }
    }
    if big::cmp(&q.chain_id, &big::from_u64(chain.chain_id())).is_ne() {
        return Err(format!("tx {} has wrong chain ID", x(&tx.hash)));
    }
    let data = &q.data;
    if !data.is_empty() && data.len() != MAX_QI_TX_DATA_LENGTH && data.len() != 20 {
        return Err(format!(
            "tx {} emits UTXO with data {} not equal to either address length or MaxQiTxDataLength {}",
            hex0x(&tx.hash),
            data.len(),
            MAX_QI_TX_DATA_LENGTH
        ));
    }
    if data.len() == 20 && data[1] > 127 {
        return Err(format!(
            "tx {} emits UTXO with contract that is not in quai ledger scope",
            hex0x(&tx.hash)
        ));
    }
    if data.len() == MAX_QI_TX_DATA_LENGTH && data[3] <= 127 {
        return Err(format!(
            "tx {} emits UTXO with data refund address not in Qi ledger scope",
            hex0x(&tx.hash)
        ));
    }
    let number = current.number;
    let mut total = 0u64;
    for i in &q.inputs {
        let Some(utxo) = chain.utxo(&i.hash, i.index) else {
            return Err(format!(
                "tx {} spends non-existent UTXO {}:{}",
                x(&tx.hash),
                x(&i.hash),
                i.index
            ));
        };
        if big::cmp(&utxo.lock, &big::from_u64(number)).is_gt() {
            return Err(format!(
                "tx {} spends locked UTXO {}:{} locked until {}",
                x(&tx.hash),
                x(&i.hash),
                i.index,
                big::decimal(&utxo.lock)
            ));
        }
        let address = key_address(&i.public_key);
        if address[1] <= 127 {
            return Err(format!(
                "UTXO is owned by Quai address: {}",
                checksum(&address)
            ));
        }
        let owner = fixed20(&utxo.address);
        if address != owner {
            return Err(format!(
                "tx {} spends UTXO {}:{} with invalid pubkey, have {} want {}",
                x(&tx.hash),
                x(&i.hash),
                i.index,
                checksum(&address),
                checksum(&owner)
            ));
        }
        if utxo.denomination > MAX_DENOMINATION {
            return Err(denomination_error(utxo.denomination));
        }
        total += DENOMINATIONS[utxo.denomination as usize];
    }
    for o in &q.outputs {
        if o.denomination > MAX_DENOMINATION {
            return Err(denomination_error(o.denomination));
        }
        if !big::is_zero(&o.lock) {
            return Err("QiTx output has non-zero lock".into());
        }
    }
    Ok(total)
}

/// `params.CalculateQiGasWithUTXOSetSizeScalingFactor` of
/// `CalculateIntrinsicQiTxGas`.
pub fn intrinsic_gas(q: &QiBody, scaling: f64) -> u64 {
    let base = (q.inputs.len() as u64)
        .wrapping_mul(800)
        .wrapping_add((q.outputs.len() as u64).wrapping_mul(9000))
        .wrapping_add(3000);
    if scaling < 15.0 || scaling.is_nan() {
        return base;
    }
    let scaled = scaling * base as f64;
    // Go converts a float64 above the uint64 range to 2^63 on arm64 and
    // amd64; such a transaction is far above any gas limit either way.
    let scaled = if scaled >= 18_446_744_073_709_551_616.0 {
        1u64 << 63
    } else {
        scaled as u64
    };
    scaled / 15
}

/// `misc.QiToQuai(current, rate, current.Difficulty(), qits)`.
fn qi_to_quai(current: &BlockData, rate: &[u8], qits: u64) -> Result<Vec<u8>, String> {
    let avg = current
        .object
        .header()
        .wire()
        .avg_tx_fees
        .clone()
        .unwrap_or_default();
    let avg = share_rules::uint(&avg).map_err(|e| format!("Qi conversion quote: {e:?}"))?;
    let rate = share_rules::uint(rate).map_err(|e| format!("Qi conversion quote: {e:?}"))?;
    let quote = economics::reward_quote(&current.header, rate, avg)
        .map_err(|e| format!("Qi conversion quote: {e:?}"))?;
    let num = big::mul_u64(&be(&quote.conversion_quai), qits);
    Ok(big::div(&num, &be(&quote.qi)))
}
fn be(v: &Uint) -> Vec<u8> {
    use crypto_bigint::Encoding;
    big::trim(&v.to_be_bytes())
}

/// The ETX limits of `addQiTxs` for the current block.
pub fn etx_limits(current: &BlockData) -> (u64, u64) {
    let n = current.object.transactions().len() as u64;
    let r = (n.wrapping_mul(TX_GAS)).max(50 * TX_GAS);
    (r, r)
}

/// `ValidateQiTxOutputsAndSignature`: the fee in qits.
pub fn validate_outputs_and_signature(
    tx: &PoolTx,
    chain: &dyn PoolChain,
    total_in: u64,
    current: &BlockData,
    scaling: f64,
    etx_r_limit: u64,
    etx_p_limit: u64,
) -> Result<u64, String> {
    let q = tx.qi().ok_or("not a Qi transaction")?;
    let location = chain.location();
    let intrinsic = intrinsic_gas(q, scaling);
    let mut used = intrinsic;
    let prime_hash = block::prime_terminus_hash(current);
    let prime = chain
        .header_by_hash(&prime_hash)
        .ok_or_else(|| format!("could not find prime terminus header {}", x(&prime_hash)))?;
    let (mut etx_r, mut etx_p) = (0u64, 0u64);
    let mut num_etxs = 0u64;
    let mut total_out = 0u64;
    let mut conversion = false;
    let mut wrapping = false;
    let mut addresses: BTreeSet<[u8; 20]> = q
        .inputs
        .iter()
        .map(|i| key_address(&i.public_key))
        .collect();
    let prime_number = block::prime_terminus_number(current);
    for (idx, o) in q.outputs.iter().enumerate() {
        if idx > u16::MAX as usize {
            return Err(format!(
                "tx [{}] exceeds max output index of {}",
                hex0x(&tx.hash),
                u16::MAX
            ));
        }
        let addr = o.address.as_deref().unwrap_or_default();
        if addr.len() != 20 {
            return Err(format!(
                "tx [{}] output {idx} has address length {}, want 20",
                hex0x(&tx.hash),
                addr.len()
            ));
        }
        if !big::is_zero(&o.lock) {
            return Err("QiTx output has non-zero lock".into());
        }
        if o.denomination > MAX_DENOMINATION {
            return Err(denomination_error(o.denomination));
        }
        total_out += DENOMINATIONS[o.denomination as usize];
        let to = fixed20(addr);
        if !addresses.insert(to) {
            return Err(format!(
                "Duplicate address in QiTx outputs: {}",
                checksum(&to)
            ));
        }
        let to_loc = [to[0] >> 4, to[0] & 0x0f];
        let here = to_loc == location;
        let quai_scope = to[1] <= 127;
        if here && quai_scope && q.data.len() == MAX_QI_TX_DATA_LENGTH {
            conversion = true;
            addresses.remove(&to);
            continue;
        } else if here && quai_scope && q.data.len() == 20 {
            let owner = fixed20(&q.data);
            if owner[1] > 127 {
                return Err(format!(
                    "expected Quai address, but found Qi address: {}",
                    checksum(&owner)
                ));
            }
            if owner[0] != location[0] << 4 | location[1] {
                return Err("Address belongs to other zone".into());
            }
            wrapping = true;
            addresses.remove(&to);
            if prime_number >= QI_WRAPPING_CHANGE_BLOCK {
                continue;
            }
        } else if quai_scope {
            return Err(format!(
                "tx [{}] emits UTXO with To address not in the Qi ledger scope",
                hex0x(&tx.hash)
            ));
        }
        if !here {
            if to_loc[0] == location[0] {
                etx_r += TX_GAS;
            } else {
                etx_p += TX_GAS;
            }
            if etx_r > etx_r_limit {
                return Err(format!(
                    "tx [{}] emits too many cross-region ETXs for block. gas emitted: {etx_r}, gas limit: {etx_r_limit}",
                    hex0x(&tx.hash)
                ));
            }
            if etx_p > etx_p_limit {
                return Err(format!(
                    "tx [{}] emits too many cross-prime ETXs for block. gas emitted: {etx_p}, gas limit: {etx_p_limit}",
                    hex0x(&tx.hash)
                ));
            }
            if quai_scope {
                return Err(format!(
                    "tx [{}] emits UTXO with To address not in the Qi ledger scope",
                    hex0x(&tx.hash)
                ));
            }
            let eligible = block::eligible_slices(&prime);
            let position = to_loc[0] as usize * 16 + to_loc[1] as usize;
            if eligible[position / 8] & (1 << (position % 8)) == 0 {
                return Err(format!(
                    "etx emitted by tx [{}] going to a slice that is not eligible to receive etx [{} {}]",
                    hex0x(&tx.hash),
                    to_loc[0],
                    to_loc[1]
                ));
            }
            used += ETX_GAS;
            num_etxs += 1;
        }
    }
    if total_out > total_in {
        return Err(format!(
            "total value of all transaction inputs for transaction {} is {total_in} which is less than the amount spent of {total_out}",
            hex0x(&tx.hash)
        ));
    }
    let fee_qit = total_in - total_out;
    let mut required = intrinsic.wrapping_add(num_etxs.wrapping_mul(TX_GAS + ETX_GAS));
    if required < intrinsic {
        return Err(format!(
            "tx {} has too many ETXs to calculate required gas",
            x(&tx.hash)
        ));
    }
    let base_fee = block::base_fee(current);
    let mut minimum = big::mul_u64(&base_fee, required);
    let rate = block::exchange_rate(&prime);
    let fee_quai = qi_to_quai(current, &rate, fee_qit)?;
    if big::cmp(&fee_quai, &minimum).is_lt() {
        return Err(format!(
            "tx {} has insufficient fee for base fee, have {} want {}",
            x(&tx.hash),
            big::low_u64(&fee_quai),
            big::low_u64(&minimum)
        ));
    }
    let hold =
        |fork: u64| prime_number >= fork && prime_number < fork + K_QUAI_CHANGE_HOLD_INTERVAL;
    if conversion && hold(FORKS.kawpow) {
        return Err(format!(
            "tx {} is a qi to quai conversion transaction  not allowed for kquai hold interval {K_QUAI_CHANGE_HOLD_INTERVAL} after the kawpow fork block",
            x(&tx.hash)
        ));
    }
    if conversion && hold(FORKS.sha_equivalent_difficulty) {
        return Err(format!(
            "tx {} is a qi to quai conversion transaction  not allowed for kquai hold interval {K_QUAI_CHANGE_HOLD_INTERVAL} after the sha equivalent difficulty fork block",
            x(&tx.hash)
        ));
    }
    if conversion || wrapping {
        if conversion && wrapping {
            return Err(format!(
                "tx {} emits both a conversion and a wrapping UTXO",
                x(&tx.hash)
            ));
        }
        required = required.wrapping_add(QI_TO_QUAI_CONVERSION_GAS);
        minimum = big::mul_u64(&base_fee, required);
        if big::cmp(&fee_quai, &minimum).is_lt() {
            return Err(format!(
                "tx {} has insufficient fee for base fee * gas, have {fee_qit} want {}",
                x(&tx.hash),
                big::low_u64(&minimum)
            ));
        }
        etx_p += QI_TO_QUAI_CONVERSION_GAS;
        if etx_p > etx_p_limit {
            return Err(format!(
                "tx [{}] emits too many cross-prime ETXs for block. gas emitted: {etx_p}, gas limit: {etx_p_limit}",
                hex0x(&tx.hash)
            ));
        }
        used += ETX_GAS;
    }
    let gas_limit = block::gas_limit(current);
    if used > gas_limit {
        return Err(format!(
            "tx {} uses too much gas, have used {used} out of {gas_limit}",
            x(&tx.hash)
        ));
    }
    let digest = tx.signing_hash();
    let valid = tx
        .primitive()
        .and_then(|p| p.verify_qi_signature().ok())
        .unwrap_or(false);
    if !valid {
        return Err(format!(
            "invalid signature for tx {} digest hash {}",
            x(&tx.hash),
            x(&digest)
        ));
    }
    Ok(fee_qit)
}
