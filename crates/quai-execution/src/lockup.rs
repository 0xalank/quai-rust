//! Native lockup tranche encoding, commitments and wrapped-Qi accounting.
use crate::evm::{
    state::World,
    word::{bytes, from_bytes, Word},
    CapabilityError,
};
use quai_primitives::{
    transaction::{keccak, Address},
    trie, Hash,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    pub owner: Address,
    pub beneficiary: Address,
    pub lock: u8,
    pub epoch: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Lockup {
    pub balance: Word,
    pub unlock_height: u32,
    pub elements: u16,
    pub delegate: Address,
}
impl Key {
    pub fn raw(&self) -> [u8; 47] {
        let mut out = [0; 47];
        out[..2].copy_from_slice(b"cl");
        out[2..22].copy_from_slice(&self.owner);
        out[22..42].copy_from_slice(&self.beneficiary);
        out[42] = self.lock;
        out[43..].copy_from_slice(&self.epoch.to_be_bytes());
        out
    }
}
impl Lockup {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = bytes(self.balance).to_vec();
        b.extend(self.unlock_height.to_be_bytes());
        b.extend(self.elements.to_be_bytes());
        if self.delegate != [0; 20] {
            b.extend(self.delegate);
        }
        b
    }
    pub fn decode(b: &[u8]) -> Result<Self, String> {
        if !matches!(b.len(), 38 | 58) {
            return Err("lockup encoding".into());
        }
        Ok(Self {
            balance: from_bytes(&b[..32]),
            unlock_height: u32::from_be_bytes(crate::slice::array(&b[32..36])?),
            elements: u16::from_be_bytes(crate::slice::array(&b[36..38])?),
            delegate: if b.len() == 58 {
                crate::slice::array(&b[38..])?
            } else {
                [0; 20]
            },
        })
    }
    pub fn commitment(&self, k: Key) -> Hash {
        let b = bytes(self.balance);
        let balance = &b[b.iter().position(|b| *b != 0).unwrap_or(32)..];
        keccak(&trie::list([
            trie::bytes(&k.owner),
            trie::bytes(&k.beneficiary),
            trie::bytes(&self.delegate),
            trie::uint(k.lock as u64),
            trie::uint(k.epoch as u64),
            trie::bytes(balance),
            trie::uint(self.unlock_height as u64),
            trie::uint(self.elements as u64),
        ]))
    }
    pub fn abi(&self) -> Vec<u8> {
        let mut b = bytes(Word::from_u64(self.unlock_height as u64)).to_vec();
        b.extend(bytes(self.balance));
        b.extend(bytes(Word::from_u64(self.elements as u64)));
        b.extend([0; 12]);
        b.extend(self.delegate);
        b
    }
}
pub fn contract(location: [u8; 2]) -> Address {
    let mut a = [0; 20];
    a[0] = location[0] << 4 | location[1];
    a[19] = 10;
    a
}
pub fn deposit_key(owner: Address, beneficiary: Address) -> Hash {
    let mut k = [0; 32];
    k[..16].copy_from_slice(&owner[..16]);
    k[16..].copy_from_slice(&beneficiary[..16]);
    k
}
pub fn owner_key(owner: Address) -> Hash {
    let mut k = [0; 32];
    k[12..].copy_from_slice(&owner);
    k
}
/// Incoming wrapping updates storage and finalizes that account transaction.
pub fn wrap(
    world: &mut World,
    owner: Address,
    beneficiary: Address,
    value: Word,
    location: [u8; 2],
) -> Result<(), crate::state_error::ReadError> {
    if !crate::evm::calls::local(&owner, location)
        || !crate::evm::calls::local(&beneficiary, location)
    {
        return Err("wrapping address scope".into());
    }
    if value == Word::ZERO {
        return Ok(());
    }
    let a = contract(location);
    if world.account(&a)?.is_none() {
        world.insert(a, Default::default())?;
    }
    let deferred = world.in_block();
    let v = world.account_mut(&a)?.unwrap();
    let key = deposit_key(owner, beneficiary);
    let old = v.get(key)?;
    let before = from_bytes(&old);
    let next = before.wrapping_add(&value);
    v.put(key, bytes(next))?;
    if !deferred && old == [0; 32] {
        v.storage_size = v.storage_size.wrapping_add(&Word::ONE);
    }
    Ok(())
}
/// AddNewLock preserves epoch rounding, uint16 element wrapping and uint32
/// height truncation in the pinned implementation. Caller records both hashes.
pub fn add(
    world: &mut World,
    key: Key,
    delegate: Address,
    unlock_height: u64,
    value: Word,
) -> Result<(Option<Hash>, Hash), CapabilityError> {
    if value == Word::ZERO {
        return Err(CapabilityError::Context("nonpositive lockup value"));
    }
    let old = world.lockup(&key)?.unwrap_or_default();
    if old.unlock_height != 0 && (unlock_height < old.unlock_height as u64 || key.epoch == 0) {
        return Err(CapabilityError::Context("lockup tranche height/epoch"));
    }
    let existed = old.unlock_height != 0;
    let previous = existed.then(|| old.commitment(key));
    let mut next = old;
    if !existed {
        next.balance = Word::ZERO;
        next.elements = 0;
        next.unlock_height = (unlock_height - unlock_height % 50000) as u32;
    }
    next.balance = next.balance.wrapping_add(&value);
    if next.balance < value {
        return Err(CapabilityError::Context("lockup balance magnitude"));
    }
    next.elements = next.elements.wrapping_add(1);
    next.delegate = delegate;
    let hash = next.commitment(key);
    world.set_lockup(key, Some(next))?;
    Ok((previous, hash))
}
#[cfg(test)]
mod tests {
    use super::*;

    // go-quai v0.57.0 `core/rawdb/accessors_chain.go`: every lockup writer
    // appends the delegate only when `!delegate.Equal(common.Zero)`, and
    // `Address.Equal` compares the address bytes, so no writer stores a
    // 58-byte record with a zero delegate. `ReadCoinbaseLockup` reads
    // `data[38:]` only when the record has 58 bytes. A 58-byte record with a
    // zero delegate therefore reads as zero and writes back as 38 bytes.
    #[test]
    fn record_layout_matches_go() {
        let mut lockup = Lockup {
            balance: Word::from_u64(7),
            unlock_height: 1_000,
            elements: 3,
            delegate: [0; 20],
        };
        let short = lockup.encode();
        assert_eq!(short.len(), 38);
        assert_eq!(Lockup::decode(&short), Ok(lockup.clone()));

        lockup.delegate = [9; 20];
        let long = lockup.encode();
        assert_eq!(long.len(), 58);
        assert_eq!(&long[..38], &short[..]);
        assert_eq!(Lockup::decode(&long), Ok(lockup));

        let mut zero_delegate = short.clone();
        zero_delegate.extend([0; 20]);
        let read = Lockup::decode(&zero_delegate).expect("58-byte record");
        assert_eq!(read.delegate, [0; 20]);
        assert_eq!(read.encode(), short);

        // Go panics below 38 bytes and no Go writer makes another length.
        for len in [0, 37, 39, 57, 59] {
            assert!(Lockup::decode(&vec![0; len]).is_err(), "length {len}");
        }
    }
}
