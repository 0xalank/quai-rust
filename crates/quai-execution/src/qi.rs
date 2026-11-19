//! Bounded Qi input-value/ownership/lock prechecks matching ValidateQiTxInputs.
//! No state mutations, complete output policy, fees, signatures or block validity.
use quai_primitives::{
    transaction::{keccak, Kind, Transaction},
    Hash,
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OutPoint {
    pub hash: Hash,
    pub index: u16,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Utxo {
    pub denomination: u8,
    pub address: UtxoAddress,
    pub lock: Vec<u8>,
}
/// The address of an unspent output as go-quai keeps it
/// (`types.UtxoEntry.Address`): every byte of the output address. ETXs,
/// coinbases and conversions create 20-byte addresses. A Qi transaction
/// before Polaris could create a longer one: go-quai keeps all its bytes in
/// the UTXO and in its hash (core/state_processor.go ProcessQiTx refuses
/// lengths other than 20 only from Polaris on, and fewer than 20 always).
/// Which 20 bytes a rule uses depends on go-quai's helper:
/// [`UtxoAddress::last20`] for `common.BytesToAddress`,
/// [`UtxoAddress::first20`] for `common.AddressBytes(...)`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct UtxoAddress(AddressBytes);
#[derive(Clone, PartialEq, Eq, Hash)]
enum AddressBytes {
    Twenty([u8; 20]),
    /// More than 20 bytes.
    Long(Box<[u8]>),
}
impl UtxoAddress {
    /// An output address of at least 20 bytes; None for a shorter one, which
    /// go-quai never stores (ProcessQiTx refuses it).
    pub fn new(bytes: &[u8]) -> Option<Self> {
        match bytes.len() {
            0..20 => None,
            20 => Some(Self(AddressBytes::Twenty(bytes.try_into().ok()?))),
            _ => Some(Self(AddressBytes::Long(bytes.into()))),
        }
    }
    pub fn as_bytes(&self) -> &[u8] {
        match &self.0 {
            AddressBytes::Twenty(a) => a,
            AddressBytes::Long(a) => a,
        }
    }
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.as_bytes().len()
    }
    /// `common.BytesToAddress`, which crops from the left: the last 20
    /// bytes. They own the output (the spending key's address must equal
    /// them), and `quai_getUTXO` shows them.
    pub fn last20(&self) -> [u8; 20] {
        let b = self.as_bytes();
        let mut out = [0; 20];
        out.copy_from_slice(&b[b.len() - 20..]);
        out
    }
    /// `common.AddressBytes(utxo.Address)`, Go's slice-to-array
    /// conversion: the first 20 bytes. The address-reuse set of a Qi
    /// transaction's inputs, the address index and the spent-output keys of
    /// `quai_getOutpointDeltasForAddressesInRange` use them.
    pub fn first20(&self) -> [u8; 20] {
        let mut out = [0; 20];
        out.copy_from_slice(&self.as_bytes()[..20]);
        out
    }
}
impl From<[u8; 20]> for UtxoAddress {
    fn from(a: [u8; 20]) -> Self {
        Self(AddressBytes::Twenty(a))
    }
}
impl std::fmt::Debug for UtxoAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x")?;
        for b in self.as_bytes() {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}
/// Set in a stored denomination byte when a longer address follows as a
/// 4-byte big-endian length and its bytes (denominations are below 15).
const LONG_ADDRESS: u8 = 0x80;
/// The stored form of a UTXO's denomination and address in the state
/// store, state images and the block store: the denomination byte and the
/// 20 address bytes, as in every record written before longer addresses
/// existed; a longer address sets [`LONG_ADDRESS`] in the denomination byte
/// and follows as a 4-byte big-endian length and its bytes.
pub fn encode_denomination_address(denomination: u8, address: &UtxoAddress, out: &mut Vec<u8>) {
    match &address.0 {
        AddressBytes::Twenty(a) => {
            out.push(denomination);
            out.extend(a);
        }
        AddressBytes::Long(a) => {
            out.push(denomination | LONG_ADDRESS);
            out.extend((a.len() as u32).to_be_bytes());
            out.extend(a.iter());
        }
    }
}
/// Read [`encode_denomination_address`]'s form at the start of `raw`:
/// returns the denomination, the address and the rest of `raw`.
pub fn decode_denomination_address(raw: &[u8]) -> Result<(u8, UtxoAddress, &[u8]), String> {
    let (&first, rest) = raw.split_first().ok_or("UTXO denomination missing")?;
    if first & LONG_ADDRESS == 0 {
        if rest.len() < 20 {
            return Err("UTXO address truncated".into());
        }
        let (a, rest) = rest.split_at(20);
        let address = UtxoAddress::new(a).ok_or("UTXO address width")?;
        return Ok((first, address, rest));
    }
    if rest.len() < 4 {
        return Err("UTXO address length truncated".into());
    }
    let (n, rest) = rest.split_at(4);
    let n = u32::from_be_bytes([n[0], n[1], n[2], n[3]]) as usize;
    if n <= 20 || rest.len() < n {
        return Err("UTXO long address width".into());
    }
    let (a, rest) = rest.split_at(n);
    let address = UtxoAddress::new(a).ok_or("UTXO address width")?;
    Ok((first & !LONG_ADDRESS, address, rest))
}
/// A stored UTXO record after its outpoint: [`encode_denomination_address`],
/// a lock-length byte and the lock (at most 32 bytes).
pub fn encode_utxo_body(v: &Utxo, out: &mut Vec<u8>) -> Result<(), String> {
    if v.lock.len() > 32 {
        return Err("UTXO lock width".into());
    }
    encode_denomination_address(v.denomination, &v.address, out);
    out.push(v.lock.len() as u8);
    out.extend(&v.lock);
    Ok(())
}
/// Read [`encode_utxo_body`]'s form, which must fill `raw`.
pub fn decode_utxo_body(raw: &[u8]) -> Result<Utxo, String> {
    let (denomination, address, rest) = decode_denomination_address(raw)?;
    let (&len, lock) = rest.split_first().ok_or("UTXO lock length missing")?;
    if len > 32 || lock.len() != len as usize {
        return Err("UTXO lock width".into());
    }
    Ok(Utxo {
        denomination,
        address,
        lock: lock.to_vec(),
    })
}
pub use crate::state_error::ReadError;
pub trait UtxoView {
    fn get(&self, outpoint: &OutPoint) -> Result<Option<Cow<'_, Utxo>>, ReadError>;
}
impl UtxoView for BTreeMap<OutPoint, Utxo> {
    fn get(&self, outpoint: &OutPoint) -> Result<Option<Cow<'_, Utxo>>, ReadError> {
        Ok(BTreeMap::get(self, outpoint).map(Cow::Borrowed))
    }
}
#[derive(Clone, Debug)]
pub struct Context {
    pub chain_id: Vec<u8>,
    pub block_height: u64,
    pub location: [u8; 2],
}
fn locked(lock: &[u8], height: u64) -> bool {
    let lock = &lock[lock.iter().position(|b| *b != 0).unwrap_or(lock.len())..];
    if lock.len() > 8 {
        return true;
    }
    let mut value = [0; 8];
    value[8 - lock.len()..].copy_from_slice(lock);
    u64::from_be_bytes(value) > height
}
fn zero_magnitude(value: &[u8]) -> bool {
    value.iter().all(|v| *v == 0)
}
/// Matches the reference input helper's checks and total within 4,096 inputs.
/// It intentionally does not claim transaction validity: cross-transaction spending,
/// signatures, output values/destinations, fees and fork rules need later stages.
pub fn check_input_values(
    transaction: &Transaction,
    view: &impl UtxoView,
    context: &Context,
) -> Result<u64, ReadError> {
    let Kind::Qi(tx) = transaction.kind() else {
        return Err("not a Qi transaction".into());
    };
    if context.location.iter().any(|v| *v > 15) {
        return Err("invalid zone location".into());
    }
    if tx.inputs.is_empty() {
        return Err("Qi transaction requires an input".into());
    }
    unique_inputs(transaction)?;
    let expected = &context.chain_id[context
        .chain_id
        .iter()
        .position(|v| *v != 0)
        .unwrap_or(context.chain_id.len())..];
    if tx.chain_id != expected {
        return Err("wrong chain ID".into());
    }
    if !matches!(tx.data.len(), 0 | 20 | 22) {
        return Err("invalid Qi data length".into());
    }
    if tx.data.len() == 20 && tx.data[1] & 0x80 != 0 {
        return Err("wrapping contract must be in Quai ledger".into());
    }
    if tx.data.len() == 22 && tx.data[3] & 0x80 == 0 {
        return Err("refund address must be in Qi ledger".into());
    }
    let mut total = 0u64;
    for input in &tx.inputs {
        let utxo = view
            .get(&OutPoint {
                hash: input.hash,
                index: input.index,
            })?
            .ok_or("missing UTXO")?;
        if locked(&utxo.lock, context.block_height) {
            return Err("locked UTXO".into());
        }
        // Transaction decoding stores every input key as a 65-byte uncompressed
        // SEC1 point; the address hashes the point without its tag byte.
        let Some((_, point)) = input.public_key.split_first() else {
            return Err("invalid input public key".into());
        };
        let address = crate::evm::word::low_address(keccak(point));
        if address[1] & 0x80 == 0 {
            return Err("UTXO owner is not in Qi ledger".into());
        }
        // go-quai compares with `common.BytesToAddress(utxo.Address)`: the
        // last 20 bytes of a longer address.
        if address != utxo.address.last20() {
            return Err("UTXO public-key ownership mismatch".into());
        }
        total = total
            .checked_add(
                *DENOMINATIONS
                    .get(utxo.denomination as usize)
                    .ok_or("invalid input denomination")?,
            )
            .ok_or("input value overflow")?;
    }
    for output in &tx.outputs {
        if output.denomination as usize >= DENOMINATIONS.len() {
            return Err("invalid output denomination".into());
        }
        if !zero_magnitude(&output.lock) {
            return Err("Qi output has nonzero lock".into());
        }
    }
    Ok(total)
}
/// Duplicate outpoint invariant now enforced by the upstream input helper.
/// A real block transition must also remove spends from its isolated state view
/// so subsequent transactions cannot spend them again.
pub fn unique_inputs(transaction: &Transaction) -> Result<Vec<OutPoint>, String> {
    let Kind::Qi(tx) = transaction.kind() else {
        return Err("not a Qi transaction".into());
    };
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::with_capacity(tx.inputs.len());
    for input in &tx.inputs {
        let point = OutPoint {
            hash: input.hash,
            index: input.index,
        };
        if !seen.insert(point) {
            return Err("duplicate Qi input".into());
        }
        ordered.push(point);
    }
    Ok(ordered)
}
#[cfg(test)]
mod tests {
    use super::*;
    /// A 20-byte address keeps the stored form of every record written
    /// before longer addresses; a longer one round-trips with its bytes.
    #[test]
    fn stored_utxo_form_keeps_twenty_byte_records() {
        let twenty = Utxo {
            denomination: 14,
            address: [7; 20].into(),
            lock: vec![1, 2],
        };
        let mut raw = vec![];
        encode_utxo_body(&twenty, &mut raw).unwrap();
        let mut old = vec![14];
        old.extend([7; 20]);
        old.extend([2, 1, 2]);
        assert_eq!(raw, old);
        assert_eq!(decode_utxo_body(&raw).unwrap(), twenty);
        for n in [21usize, 32, 64] {
            let bytes: Vec<u8> = (0..n as u8).collect();
            let long = Utxo {
                denomination: 3,
                address: UtxoAddress::new(&bytes).unwrap(),
                lock: vec![],
            };
            let mut raw = vec![];
            encode_utxo_body(&long, &mut raw).unwrap();
            assert_eq!(raw[0], 3 | 0x80);
            assert_eq!(raw[1..5], (n as u32).to_be_bytes());
            assert_eq!(raw.len(), 5 + n + 1);
            assert_eq!(decode_utxo_body(&raw).unwrap(), long);
            assert_eq!(long.address.first20()[..], bytes[..20]);
            assert_eq!(long.address.last20()[..], bytes[n - 20..]);
            // Truncated or trailing bytes are refused.
            assert!(decode_utxo_body(&raw[..raw.len() - 1]).is_err());
            raw.push(0);
            assert!(decode_utxo_body(&raw).is_err());
        }
        // A flagged record must hold more than 20 address bytes.
        let mut flagged = vec![0x80];
        flagged.extend(20u32.to_be_bytes());
        flagged.extend([7; 20]);
        flagged.push(0);
        assert!(decode_utxo_body(&flagged).is_err());
        assert!(UtxoAddress::new(&[1; 19]).is_none());
        assert!(UtxoAddress::new(&[]).is_none());
        assert_eq!(UtxoAddress::new(&[9; 20]), Some([9; 20].into()));
    }
}
