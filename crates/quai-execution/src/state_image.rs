//! Canonical images for completed, trusted local execution checkpoints. A known
//! payload digest is mandatory: roots alone do not prove raw UTXO membership or
//! legacy ghost/count metadata. This codec never admits a network snapshot.
use crate::{
    etx::EtxState,
    evm::{
        state::{Account, World, MAX_ACCOUNTS, MAX_STORAGE_SLOTS},
        word::{bytes, from_bytes},
    },
    lockup::{Key, Lockup},
    qi::{OutPoint, Utxo},
    utxo::{UtxoState, MAX_UTXOS},
};
use quai_primitives::{transaction::Transaction, Hash};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
const MAGIC: &[u8] = b"QUAI-STATE-IMAGE-1\n";
const MAX_BYTES: usize = 256 << 20;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub profile: Hash,
    pub block: Hash,
    pub parent: Hash,
    pub height: u64,
    pub timestamp: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roots {
    pub evm: Hash,
    pub etx: Hash,
    pub utxo: Hash,
    pub accounts: u64,
    pub utxo_count: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub digest: Hash,
    pub anchor: Anchor,
    pub roots: Roots,
}
pub struct Restored {
    pub world: World,
    pub utxos: UtxoState,
}
fn hash(v: &[u8]) -> Hash {
    Sha256::digest(v).into()
}
fn roots(world: &World, state: &UtxoState) -> Result<Roots, String> {
    Ok(Roots {
        evm: world.root()?,
        etx: world.etxs().root()?,
        utxo: state.root(),
        accounts: world.account_count(),
        utxo_count: state.consensus_size(),
    })
}
fn n(out: &mut Vec<u8>, value: usize) -> Result<(), String> {
    let value = u32::try_from(value).map_err(|_| "image length width")?;
    out.extend(value.to_be_bytes());
    Ok(())
}
fn blob(out: &mut Vec<u8>, value: &[u8]) -> Result<(), String> {
    n(out, value.len())?;
    out.extend(value);
    if out.len() > MAX_BYTES - 32 {
        return Err("image byte budget".into());
    }
    Ok(())
}
fn header(out: &mut Vec<u8>, anchor: &Anchor, r: &Roots) {
    out.extend(anchor.profile);
    out.extend(anchor.block);
    out.extend(anchor.parent);
    out.extend(anchor.height.to_be_bytes());
    out.extend(anchor.timestamp.to_be_bytes());
    out.extend(r.evm);
    out.extend(r.etx);
    out.extend(r.utxo);
    out.extend(r.accounts.to_be_bytes());
    out.extend(r.utxo_count.to_be_bytes());
}
/// Encode a completed local state; derived account/root caches are omitted.
pub fn encode(
    anchor: Anchor,
    world: &World,
    utxos: &UtxoState,
) -> Result<(Vec<u8>, Reference), String> {
    if world.lockups()?.len() > 1_000_000 {
        return Err("state image lockup count".into());
    }
    if world.in_block() {
        return Err("unfinished block state cannot be checkpointed".into());
    }
    world.validate()?;
    let r = roots(world, utxos)?;
    let mut out = MAGIC.to_vec();
    header(&mut out, &anchor, &r);
    n(&mut out, world.account_count() as usize)?;
    for (address, a) in world.accounts()?.iter() {
        out.extend(address);
        out.extend(a.nonce.to_be_bytes());
        out.extend(bytes(a.balance));
        out.extend(bytes(a.storage_size));
        blob(&mut out, &a.code)?;
        let storage = a.materialize_storage()?;
        n(&mut out, storage.len())?;
        for (k, v) in storage.iter() {
            if *v == [0; 32] {
                return Err("noncanonical zero storage".into());
            }
            out.extend(k);
            out.extend(v);
        }
    }
    n(&mut out, world.lockups()?.len())?;
    for (k, v) in world.lockups()?.iter() {
        out.extend(k.owner);
        out.extend(k.beneficiary);
        out.push(k.lock);
        out.extend(k.epoch.to_be_bytes());
        let value = v.encode();
        blob(&mut out, &value)?;
    }
    let queue = world.etxs();
    out.extend(queue.oldest().to_be_bytes());
    out.extend(queue.newest().to_be_bytes());
    n(&mut out, queue.len())?;
    for (index, tx) in queue.entries()? {
        out.extend(index.to_be_bytes());
        blob(&mut out, &tx.encode())?;
    }
    n(&mut out, queue.auxiliary()?.len())?;
    for (k, v) in queue.auxiliary()?.iter() {
        out.extend(k);
        blob(&mut out, v)?;
    }
    let entries = utxos.materialize()?;
    n(&mut out, entries.len())?;
    for (point, v) in entries.iter() {
        out.extend(point.hash);
        out.extend(point.index.to_be_bytes());
        out.push(v.denomination);
        out.extend(v.address);
        blob(&mut out, &v.lock)?;
        if out.len() > MAX_BYTES - 416 {
            return Err("image byte budget".into());
        }
    }
    out.extend(utxos.accumulator());
    let digest = hash(&out);
    out.extend(digest);
    if out.len() > MAX_BYTES {
        return Err("image byte budget".into());
    }
    Ok((
        out,
        Reference {
            digest,
            anchor,
            roots: r,
        },
    ))
}
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.offset.checked_add(n).ok_or("image offset")?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or("truncated state image")?;
        self.offset = end;
        Ok(value)
    }
    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], String> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(self.fixed()?))
    }
    fn count(&mut self, max: usize) -> Result<usize, String> {
        let n = u32::from_be_bytes(self.fixed()?) as usize;
        if n > max {
            return Err("image count limit".into());
        }
        Ok(n)
    }
    fn blob(&mut self, max: usize) -> Result<Vec<u8>, String> {
        let n = self.count(max)?;
        Ok(self.take(n)?.to_vec())
    }
}
/// `expected` must come from a previously completed local commit, never a peer's
/// claimed roots/digest. Structural/root checks are additional integrity checks.
pub fn decode(input: &[u8], expected: &Reference) -> Result<Restored, String> {
    if input.len() > MAX_BYTES || input.len() < MAGIC.len() + 32 {
        return Err("state image size".into());
    }
    let end = input.len() - 32;
    if hash(&input[..end]) != expected.digest || input[end..] != expected.digest {
        return Err("state image payload digest".into());
    }
    let mut reader = Reader {
        bytes: &input[..end],
        offset: 0,
    };
    if reader.take(MAGIC.len())? != MAGIC {
        return Err("state image version".into());
    }
    let anchor = Anchor {
        profile: reader.fixed()?,
        block: reader.fixed()?,
        parent: reader.fixed()?,
        height: reader.u64()?,
        timestamp: reader.u64()?,
    };
    let r = Roots {
        evm: reader.fixed()?,
        etx: reader.fixed()?,
        utxo: reader.fixed()?,
        accounts: reader.u64()?,
        utxo_count: reader.u64()?,
    };
    if anchor != expected.anchor || r != expected.roots {
        return Err("state image anchor/roots".into());
    }
    let count = reader.count(MAX_ACCOUNTS)?;
    let mut world = World::default();
    let mut previous = None;
    for _ in 0..count {
        let address = reader.fixed::<20>()?;
        if previous.is_some_and(|v| v >= address) {
            return Err("state image account order".into());
        }
        previous = Some(address);
        let nonce = reader.u64()?;
        let balance = from_bytes(reader.take(32)?);
        let storage_size = from_bytes(reader.take(32)?);
        let code = reader.blob(32768)?;
        let slots = reader.count(MAX_STORAGE_SLOTS)?;
        let mut storage = BTreeMap::new();
        let mut last = None;
        for _ in 0..slots {
            let key = reader.fixed::<32>()?;
            let value = reader.fixed::<32>()?;
            if value == [0; 32] || last.is_some_and(|v| v >= key) {
                return Err("state image storage order/value".into());
            }
            last = Some(key);
            storage.insert(key, value);
        }
        world.insert(
            address,
            Account {
                nonce,
                balance,
                storage_size,
                code,
                storage,
                backing: None,
            },
        )?;
    }
    let count = reader.count(1_000_000)?;
    let mut previous = None;
    for _ in 0..count {
        let key = Key {
            owner: reader.fixed()?,
            beneficiary: reader.fixed()?,
            lock: reader.take(1)?[0],
            epoch: u32::from_be_bytes(reader.fixed()?),
        };
        if previous.is_some_and(|v| v >= key) {
            return Err("state image lockup order".into());
        }
        previous = Some(key);
        let encoded = reader.blob(58)?;
        let value = Lockup::decode(&encoded)?;
        if value.encode() != encoded {
            return Err("state image lockup encoding".into());
        }
        world.set_lockup(key, Some(value))?;
    }
    let oldest = reader.u64()?;
    let newest = reader.u64()?;
    let count = reader.count(crate::etx::MAX_PENDING_ETXS)?;
    let mut slots = vec![];
    for _ in 0..count {
        let index = reader.u64()?;
        let encoded = reader.blob(1 << 20)?;
        let tx = Transaction::decode(&encoded)?;
        if tx.encode() != encoded {
            return Err("state image ETX encoding".into());
        }
        slots.push((index, tx));
    }
    let count = reader.count(4096)?;
    let mut auxiliary = BTreeMap::new();
    let mut previous = None;
    for _ in 0..count {
        let key = reader.fixed::<32>()?;
        if previous.is_some_and(|v| v >= key) {
            return Err("state image metadata order".into());
        }
        previous = Some(key);
        let value = reader.blob(65536)?;
        if value.is_empty() {
            return Err("state image empty metadata".into());
        }
        auxiliary.insert(key.to_vec(), value);
    }
    *world.etxs_mut() = EtxState::from_image(oldest, newest, slots, auxiliary)?;
    let count = reader.count(MAX_UTXOS)?;
    let mut entries = BTreeMap::new();
    let mut previous = None;
    for _ in 0..count {
        let point = OutPoint {
            hash: reader.fixed()?,
            index: u16::from_be_bytes(reader.fixed()?),
        };
        if previous.is_some_and(|v| v >= point) {
            return Err("state image UTXO order".into());
        }
        previous = Some(point);
        let denomination = reader.take(1)?[0];
        let address = reader.fixed()?;
        let lock = reader.blob(32)?;
        entries.insert(
            point,
            Utxo {
                denomination,
                address,
                lock,
            },
        );
    }
    let accumulator = reader.take(384)?;
    let state = UtxoState::from_image(entries, accumulator, r.utxo_count)?;
    if reader.offset != end || roots(&world, &state)? != r {
        return Err("state image trailing data/commitments".into());
    }
    Ok(Restored {
        world,
        utxos: state,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::evm::word::Word;
    use quai_primitives::transaction::External;
    pub(crate) fn sample() -> (Anchor, World, UtxoState) {
        let mut world = World::default();
        let mut address = [0; 20];
        address[19] = 42;
        world
            .insert(
                address,
                Account {
                    nonce: 1,
                    balance: Word::from_u64(99),
                    storage: BTreeMap::from([([0; 32], [1; 32])]),
                    storage_size: Word::ONE,
                    code: vec![0],
                    backing: None,
                },
            )
            .unwrap();
        let key = Key {
            owner: address,
            beneficiary: address,
            lock: 2,
            epoch: 7,
        };
        let value = Lockup {
            balance: Word::from_u64(123),
            unlock_height: 999,
            elements: 8,
            delegate: [0; 20],
        };
        world.set_lockup(key, Some(value.clone())).unwrap();
        let mut state = UtxoState::default();
        let point = OutPoint {
            hash: [1; 32],
            index: 2,
        };
        let other = OutPoint {
            hash: [2; 32],
            index: 3,
        };
        let mut qi = address;
        qi[1] = 128;
        let value_qi = Utxo {
            denomination: 6,
            address: qi,
            lock: vec![1],
        };
        state.insert(point, value_qi.clone()).unwrap();
        state.insert(other, value_qi.clone()).unwrap();
        state.remove(&other).unwrap();
        state
            .create_raw(
                point,
                Utxo {
                    denomination: 8,
                    ..value_qi
                },
            )
            .unwrap();
        state.add_auxiliary_hash(value.commitment(key));
        for index in 0..3 {
            let tx = Transaction::external(External {
                originating_hash: [index; 32],
                index: 0,
                gas: 21000,
                to: address,
                value: vec![1],
                data: vec![],
                access_list: vec![],
                sender: address,
                etx_type: 0,
            })
            .unwrap();
            world.etxs_mut().push(&[tx]).unwrap();
        }
        world.etxs_mut().pop().unwrap();
        world.etxs_mut().pop().unwrap();
        let mut aux = vec![255; 32];
        aux[..4].fill(0);
        aux[31] = 250;
        world.etxs_mut().set_auxiliary(aux, vec![1, 2, 3]).unwrap();
        (
            Anchor {
                profile: [3; 32],
                block: [4; 32],
                parent: [5; 32],
                height: 8,
                timestamp: 9,
            },
            world,
            state,
        )
    }
    #[test]
    fn exact_images_preserve_raw_lockups_queue_metadata_and_legacy_muhash_ghosts() {
        let (a, w, u) = sample();
        let (bytes, reference) = encode(a.clone(), &w, &u).unwrap();
        let restored = decode(&bytes, &reference).unwrap();
        assert_eq!(restored.world, w);
        assert_eq!(
            restored.utxos.materialize().unwrap(),
            u.materialize().unwrap()
        );
        assert_eq!(restored.utxos.accumulator(), u.accumulator());
        assert_eq!(restored.utxos.consensus_size(), 3);
        assert_eq!(restored.utxos.materialize().unwrap().len(), 1);
        assert_eq!(restored.world.etxs().oldest(), 2);
        assert_eq!(restored.world.etxs().newest(), 3);
        assert_eq!(
            encode(a, &restored.world, &restored.utxos).unwrap().0,
            bytes
        );
    }
    #[test]
    fn digest_anchor_version_and_count_mutations_are_rejected() {
        let (a, w, u) = sample();
        let (bytes, reference) = encode(a, &w, &u).unwrap();
        for offset in [0, 32, 128, bytes.len() / 2, bytes.len() - 1] {
            let mut bad = bytes.clone();
            bad[offset] ^= 1;
            assert!(decode(&bad, &reference).is_err());
        }
        let mut wrong = reference.clone();
        wrong.anchor.profile[0] ^= 1;
        assert!(decode(&bytes, &wrong).is_err());
        let mut bad = bytes.clone();
        let offset = MAGIC.len() + 224;
        bad[offset..offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        let end = bad.len() - 32;
        let digest = hash(&bad[..end]);
        bad[end..].copy_from_slice(&digest);
        let mut own = reference;
        own.digest = digest;
        assert!(decode(&bad, &own).is_err());
    }
    #[test]
    fn incomplete_block_state_is_never_serialized_as_a_checkpoint() {
        let (a, mut w, u) = sample();
        w.begin_block();
        assert!(encode(a, &w, &u).is_err());
    }
}
