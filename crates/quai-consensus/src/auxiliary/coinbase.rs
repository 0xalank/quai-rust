use crate::Error;
use quai_primitives::Hash;
use sha2::{Digest, Sha256};
fn invalid() -> Error {
    Error::Invalid("auxiliary coinbase encoding")
}
fn take<'a>(bytes: &'a [u8], cursor: &mut usize, length: usize) -> Result<&'a [u8], Error> {
    let end = cursor.checked_add(length).ok_or_else(invalid)?;
    let out = bytes.get(*cursor..end).ok_or_else(invalid)?;
    *cursor = end;
    Ok(out)
}
fn varint(bytes: &[u8], cursor: &mut usize) -> Result<u64, Error> {
    let c = take(bytes, cursor, 1)?[0];
    Ok(match c {
        253 => u16::from_le_bytes(take(bytes, cursor, 2)?.try_into().unwrap()) as u64,
        254 => u32::from_le_bytes(take(bytes, cursor, 4)?.try_into().unwrap()) as u64,
        255 => u64::from_le_bytes(take(bytes, cursor, 8)?.try_into().unwrap()),
        _ => c as u64,
    })
}
fn push<'a>(bytes: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], Error> {
    let n = take(bytes, cursor, 1)?[0];
    if n > 75 {
        return Err(invalid());
    }
    take(bytes, cursor, n as usize)
}
pub fn double_sha(bytes: &[u8]) -> Hash {
    Sha256::digest(Sha256::digest(bytes)).into()
}
#[derive(Clone, Debug)]
pub struct Coinbase {
    pub script: Vec<u8>,
    pub outputs: Vec<u8>,
    pub height: u32,
    pub signature_time: u32,
    pub commitment: Hash,
    pub merkle_size: u32,
    pub merkle_nonce: u32,
}
impl Coinbase {
    pub fn decode(tx: &[u8]) -> Result<Self, Error> {
        if tx.len() > 1 << 20 {
            return Err(Error::ResourceLimit("auxiliary coinbase"));
        }
        if tx.len() < 4 {
            return Err(invalid());
        }
        let mut cursor = 4;
        if varint(tx, &mut cursor)? != 1 {
            return Err(Error::Invalid("coinbase input count"));
        }
        if take(tx, &mut cursor, 32)?.iter().any(|b| *b != 0) {
            return Err(Error::Invalid("coinbase previous txid"));
        }
        if take(tx, &mut cursor, 4)? != [255; 4] {
            return Err(Error::Invalid("coinbase previous index"));
        }
        let length = usize::try_from(varint(tx, &mut cursor)?).map_err(|_| invalid())?;
        let script = take(tx, &mut cursor, length)?.to_vec();
        if take(tx, &mut cursor, 4)? != [255; 4] {
            return Err(Error::Invalid("coinbase sequence"));
        }
        let outputs = tx[cursor..].to_vec();
        // The pinned auxiliary stage signs the exact tail without independently
        // decoding outputs, witness data or locktime. Preserve that contract;
        // donor-chain transaction validity is not established by these helpers.
        let mut cursor = 0;
        let h = push(&script, &mut cursor)?;
        if h.len() > 5 {
            return Err(invalid());
        }
        let mut height = 0u32;
        for (i, b) in h.iter().take(4).enumerate() {
            height |= (*b as u32) << (8 * i);
        }
        let payload = push(&script, &mut cursor)?;
        if payload.len() != 44 || payload[..4] != [0xfa, 0xbe, 0x6d, 0x6d] {
            return Err(Error::Invalid("coinbase merged-mining commitment"));
        }
        let commitment = payload[4..36].try_into().unwrap();
        let merkle_size = u32::from_le_bytes(payload[36..40].try_into().unwrap());
        let merkle_nonce = u32::from_le_bytes(payload[40..].try_into().unwrap());
        push(&script, &mut cursor)?;
        let time = push(&script, &mut cursor)?;
        if time.len() != 4 {
            return Err(Error::Invalid("coinbase signature time"));
        }
        let signature_time = u32::from_le_bytes(time.try_into().unwrap());
        Ok(Self {
            script,
            outputs,
            height,
            signature_time,
            commitment,
            merkle_size,
            merkle_nonce,
        })
    }
}
pub fn merkle_root(tx: &[u8], branch: &[Vec<u8>]) -> Result<Hash, Error> {
    if branch.len() > 64 {
        return Err(Error::ResourceLimit("auxiliary branch depth"));
    }
    let mut h = double_sha(tx);
    for sibling in branch {
        let mut node = [0; 64];
        node[..32].copy_from_slice(&h);
        let n = sibling.len().min(32);
        node[32..32 + n].copy_from_slice(&sibling[..n]);
        h = double_sha(&node);
    }
    Ok(h)
}
pub fn scrypt_commitment(mut doge: Hash, mut seal: Hash) -> Hash {
    doge.reverse();
    seal.reverse();
    let mut bytes = [0; 64];
    bytes[..32].copy_from_slice(&doge);
    bytes[32..].copy_from_slice(&seal);
    let mut h = double_sha(&bytes);
    h.reverse();
    h
}
