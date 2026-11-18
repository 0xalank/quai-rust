//! Incremental local execution state on versioned secure tries. Native callers
//! supply completed, validated before/after states; this grants no admission.
use crate::{
    etx::EtxState,
    evm::{
        state::{Account, World, MAX_ACCOUNTS},
        word::{bytes, from_bytes},
    },
    lockup::{Key, Lockup},
    qi::{OutPoint, Utxo, UtxoView},
    state_image::Roots,
    utxo::{UtxoState, MAX_UTXOS},
};
use quai_primitives::{
    mpt,
    transaction::{keccak, Transaction},
    Hash,
};
use quai_store::state_db::{Snapshot, Workspace};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io,
    sync::{Arc, Mutex},
};
mod account_reader;
mod aux_reader;
mod utxo_reader;
const META_MAGIC: &[u8] = b"QUAI-KEYED-EXECUTION-2\n";
const MAX_LOAD_BYTES: usize = 256 << 20;
const TREES: [&[u8]; 10] = [
    b"accounts",
    b"account-data",
    b"storage-keys",
    b"code",
    b"etx",
    b"etx-data",
    b"etx-aux",
    b"lockups",
    b"utxos",
    b"state-meta",
];
fn bad(v: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, v.to_string())
}
fn need(ok: bool, v: &str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(bad(v))
    }
}
fn roots(world: &World, utxos: &UtxoState) -> io::Result<Roots> {
    Ok(Roots {
        evm: world.root().map_err(bad)?,
        etx: world.etxs().root().map_err(bad)?,
        utxo: utxos.root(),
        accounts: world.account_count(),
        utxo_count: utxos.consensus_size(),
    })
}
fn metadata(world: &World, utxos: &UtxoState) -> Vec<u8> {
    let mut out = META_MAGIC.to_vec();
    for n in [
        world.etxs().oldest(),
        world.etxs().newest(),
        world.account_count(),
        utxos.raw_len(),
        utxos.consensus_size(),
    ] {
        out.extend(n.to_be_bytes());
    }
    out.extend(utxos.accumulator());
    out
}
fn digest_roots(mut root: impl FnMut(&[u8]) -> Hash) -> Hash {
    let mut h = Sha256::new();
    h.update(META_MAGIC);
    for name in TREES {
        h.update([name.len() as u8]);
        h.update(name);
        h.update(root(name));
    }
    h.finalize().into()
}
pub fn snapshot_digest(snapshot: &Snapshot) -> io::Result<Hash> {
    need(
        snapshot
            .version()
            .roots
            .keys()
            .all(|k| TREES.contains(&k.as_slice())),
        "unknown execution tree",
    )?;
    Ok(digest_roots(|name| snapshot.root(name)))
}
fn change(
    w: &mut Workspace<'_, '_>,
    name: &[u8],
    key: &[u8],
    old: Option<Vec<u8>>,
    new: Option<Vec<u8>>,
) -> io::Result<()> {
    if old == new {
        return Ok(());
    }
    need(w.get(name, key)? == old, "execution delta parent value")?;
    w.put(name, key, new.as_deref().unwrap_or_default())
}
pub(crate) fn decode_slot(raw: &[u8]) -> io::Result<Hash> {
    need(!raw.is_empty(), "empty persisted storage value")?;
    let body = if raw[0] < 0x80 {
        need(raw.len() == 1, "storage trailing bytes")?;
        raw
    } else {
        let n = raw[0].checked_sub(0x80).ok_or_else(|| bad("storage RLP"))? as usize;
        need(n > 0 && n <= 32 && raw.len() == n + 1, "storage RLP width")?;
        let b = &raw[1..];
        need(n != 1 || b[0] >= 0x80, "noncanonical storage byte")?;
        b
    };
    need(
        body.len() <= 32 && body[0] != 0,
        "zero/noncanonical storage",
    )?;
    let mut out = [0; 32];
    out[32 - body.len()..].copy_from_slice(body);
    Ok(out)
}
fn account_record(address: [u8; 20], a: &Account, storage: Hash) -> Vec<u8> {
    let mut out = address.to_vec();
    out.extend(a.nonce.to_be_bytes());
    out.extend(bytes(a.balance));
    out.extend(bytes(a.storage_size));
    out.extend(keccak(&a.code));
    out.extend(storage);
    out.extend(a.storage_len().to_be_bytes());
    out
}
fn point_key(point: &OutPoint) -> Vec<u8> {
    let mut key = point.hash.to_vec();
    key.extend(point.index.to_be_bytes());
    key
}
fn utxo_record(p: &OutPoint, v: &Utxo) -> io::Result<Vec<u8>> {
    need(v.lock.len() <= 32, "UTXO lock width")?;
    let mut out = point_key(p);
    out.push(v.denomination);
    out.extend(v.address);
    out.push(v.lock.len() as u8);
    out.extend(&v.lock);
    Ok(out)
}
fn index_key(newest: bool) -> Vec<u8> {
    let mut key = vec![0; 4];
    key.extend([255; 28]);
    if !newest {
        key[31] = 254;
    }
    key
}
fn magnitude(n: u64) -> Vec<u8> {
    let b = n.to_be_bytes();
    b[b.iter().position(|v| *v != 0).unwrap_or(8)..].to_vec()
}
fn complete(world: &World) -> io::Result<()> {
    need(!world.in_block(), "unfinished block state cannot be stored")?;
    world.validate().map_err(bad)
}
#[derive(Clone, Debug)]
pub struct Commitment {
    pub roots: Roots,
    pub digest: Hash,
}
/// Write changed logical records, with storage tries and account roots in the
/// same transaction. The base must be the state restored for this parent.
pub fn apply_delta(
    w: &mut Workspace<'_, '_>,
    before: &World,
    before_utxos: &UtxoState,
    after: &World,
    after_utxos: &UtxoState,
) -> io::Result<Commitment> {
    complete(before)?;
    complete(after)?;
    let initial = roots(before, before_utxos)?;
    let final_roots = roots(after, after_utxos)?;
    need(
        w.root(b"accounts") == initial.evm && w.root(b"etx") == initial.etx,
        "execution delta parent roots",
    )?;
    let old_meta = w.get(b"state-meta", &[0])?;
    need(
        old_meta.as_deref() == Some(metadata(before, before_utxos).as_slice())
            || (old_meta.is_none()
                && before.account_count() == 0
                && before_utxos.raw_len() == 0
                && before_utxos.consensus_size() == 0
                && before.etxs().is_empty()
                && before.etxs().oldest() == 0
                && before.etxs().newest() == 0
                && before.etxs().is_pristine().map_err(bad)?
                && before.lockups_is_empty().map_err(bad)?),
        "execution delta parent metadata",
    )?;
    for address in before.changed_accounts(after).map_err(bad)? {
        let old = before.account(&address).map_err(bad)?;
        let new = after.account(&address).map_err(bad)?;
        if old
            .as_ref()
            .zip(new.as_ref())
            .is_some_and(|(a, b)| Arc::ptr_eq(a, b) || a == b)
        {
            continue;
        }
        let old_root = old
            .as_ref()
            .map(|a| a.storage_root())
            .transpose()
            .map_err(bad)?
            .unwrap_or_else(mpt::empty_root);
        let storage_root = if let Some(account) = &new {
            need(
                account.backing.is_some() || account.storage.values().all(|v| *v != [0; 32]),
                "noncanonical zero storage",
            )?;
            let trie = account.storage_trie().map_err(bad)?;
            let root = w.persist_trie(&trie)?;
            for (key, value) in &account.storage {
                if *value == [0; 32] {
                    continue;
                }
                if let Some(existing) = w.get(b"storage-keys", key)? {
                    need(existing == key, "immutable storage preimage")?;
                } else {
                    w.put(b"storage-keys", key, key)?;
                }
            }
            if !account.code.is_empty() {
                let hash = keccak(&account.code);
                if let Some(existing) = w.get(b"code", &hash)? {
                    need(existing == account.code, "immutable code conflict")?;
                } else {
                    w.put(b"code", &hash, &account.code)?;
                }
            }
            root
        } else {
            mpt::empty_root()
        };
        change(
            w,
            b"account-data",
            &address,
            old.as_ref().map(|a| account_record(address, a, old_root)),
            new.as_ref()
                .map(|a| account_record(address, a, storage_root)),
        )?;
        change(
            w,
            b"accounts",
            &address,
            old.as_ref()
                .map(|a| a.consensus_rlp())
                .transpose()
                .map_err(bad)?,
            new.as_ref()
                .map(|a| a.consensus_rlp())
                .transpose()
                .map_err(bad)?,
        )?;
    }
    for key in before
        .lockup_state()
        .changed_keys(after.lockup_state())
        .map_err(bad)?
    {
        let raw = key.raw();
        let record = |value: Lockup| {
            let mut out = raw.to_vec();
            out.extend(value.encode());
            out
        };
        change(
            w,
            b"lockups",
            &raw,
            before.lockup(&key).map_err(bad)?.map(record),
            after.lockup(&key).map_err(bad)?.map(record),
        )?;
    }
    for index in before.etxs().changed_slots(after.etxs()).map_err(bad)? {
        let a = before.etxs().entry(index).map_err(bad)?;
        let b = after.etxs().entry(index).map_err(bad)?;
        change(
            w,
            b"etx",
            &magnitude(index),
            a.as_ref().map(|v| v.envelope.clone()),
            b.as_ref().map(|v| v.envelope.clone()),
        )?;
        let encode = |v: &Arc<crate::etx::Entry>| {
            let mut out = index.to_be_bytes().to_vec();
            out.extend(v.transaction.encode());
            out
        };
        change(
            w,
            b"etx-data",
            &index.to_be_bytes(),
            a.as_ref().map(encode),
            b.as_ref().map(encode),
        )?;
    }
    for key in before.etxs().changed_auxiliary(after.etxs()).map_err(bad)? {
        let a = before.etxs().auxiliary_value(&key).map_err(bad)?;
        let b = after.etxs().auxiliary_value(&key).map_err(bad)?;
        change(w, b"etx", &key, a.clone(), b.clone())?;
        let encode = |value: Vec<u8>| {
            let mut out = key.clone();
            out.extend(value);
            out
        };
        change(w, b"etx-aux", &key, a.map(encode), b.map(encode))?;
    }
    for (key, a, b) in [
        (
            index_key(false),
            before.etxs().oldest(),
            after.etxs().oldest(),
        ),
        (
            index_key(true),
            before.etxs().newest(),
            after.etxs().newest(),
        ),
    ] {
        change(
            w,
            b"etx",
            &key,
            (a != 0).then(|| magnitude(a)),
            (b != 0).then(|| magnitude(b)),
        )?;
    }
    for point in before_utxos.changed_points(after_utxos).map_err(bad)? {
        let a = before_utxos
            .get(&point)
            .map_err(bad)?
            .map(|v| utxo_record(&point, &v))
            .transpose()?;
        let b = after_utxos
            .get(&point)
            .map_err(bad)?
            .map(|v| utxo_record(&point, &v))
            .transpose()?;
        change(w, b"utxos", &point_key(&point), a, b)?;
    }
    w.put(b"state-meta", &[0], &metadata(after, after_utxos))?;
    need(
        w.root(b"accounts") == final_roots.evm && w.root(b"etx") == final_roots.etx,
        "incremental execution roots",
    )?;
    Ok(Commitment {
        roots: final_roots,
        digest: digest_roots(|name| w.root(name)),
    })
}
struct Cursor<'a> {
    raw: &'a [u8],
    offset: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(n)
            .ok_or_else(|| bad("state record cursor"))?;
        let v = self
            .raw
            .get(self.offset..end)
            .ok_or_else(|| bad("truncated state record"))?;
        self.offset = end;
        Ok(v)
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn hash(&mut self) -> io::Result<Hash> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    fn done(&self) -> io::Result<()> {
        need(self.offset == self.raw.len(), "state record trailing bytes")
    }
}
fn preimage(key: Hash, raw: &[u8], n: usize) -> io::Result<&[u8]> {
    let p = raw
        .get(..n)
        .ok_or_else(|| bad("truncated state preimage"))?;
    need(keccak(p) == key, "state preimage/hash binding")?;
    Ok(p)
}
/// Materialize a local version with independent EVM/ETX roots and exact raw
/// Qi membership plus independent MuHash/count. This is not a peer importer.
pub fn restore(snapshot: &Snapshot, expected: &Commitment) -> io::Result<(World, UtxoState)> {
    restore_inner(snapshot, expected, None)
}
/// Accounts, contract slots and Qi membership load on demand from one pinned
/// transaction, including ETX queues, economic fields and raw lockups.
pub fn restore_sparse(
    snapshot: Arc<Mutex<Snapshot>>,
    expected: &Commitment,
) -> io::Result<(World, UtxoState)> {
    let view = snapshot.lock().map_err(bad)?;
    restore_inner(&view, expected, Some(snapshot.clone()))
}
/// Compatibility entry point; sparse restoration now includes accounts/slots.
pub fn restore_sparse_qi(
    snapshot: Arc<Mutex<Snapshot>>,
    expected: &Commitment,
) -> io::Result<(World, UtxoState)> {
    restore_sparse(snapshot, expected)
}
fn restore_inner(
    snapshot: &Snapshot,
    expected: &Commitment,
    lazy: Option<Arc<Mutex<Snapshot>>>,
) -> io::Result<(World, UtxoState)> {
    need(
        snapshot_digest(snapshot)? == expected.digest,
        "execution state root-set digest",
    )?;
    let raw = snapshot
        .get(b"state-meta", &[0])?
        .ok_or_else(|| bad("missing execution metadata"))?;
    let mut meta = Cursor {
        raw: &raw,
        offset: 0,
    };
    need(
        meta.take(META_MAGIC.len())? == META_MAGIC,
        "execution metadata format",
    )?;
    let oldest = meta.u64()?;
    let newest = meta.u64()?;
    let accounts = meta.u64()?;
    let raw_count = meta.u64()?;
    let count = meta.u64()?;
    let accumulator = meta.take(384)?.to_vec();
    meta.done()?;
    need(
        (lazy.is_some() || accounts <= MAX_ACCOUNTS as u64)
            && (lazy.is_some() || raw_count <= MAX_UTXOS as u64)
            && accounts == expected.roots.accounts
            && count == expected.roots.utxo_count,
        "execution materialization bounds/counts",
    )?;
    let remaining = std::cell::Cell::new(MAX_LOAD_BYTES);
    let account_bytes = std::cell::Cell::new(0usize);
    let scan = |name: &[u8],
                limit: usize,
                visit: &mut dyn FnMut(Hash, &[u8]) -> io::Result<()>|
     -> io::Result<()> {
        let stats = snapshot.scan(name, limit, remaining.get(), &mut |key, value| {
            visit(key, value)
        })?;
        remaining.set(
            remaining
                .get()
                .checked_sub(stats.bytes)
                .ok_or_else(|| bad("execution materialization byte limit"))?,
        );
        Ok(())
    };
    need(
        snapshot.root(b"accounts") == expected.roots.evm,
        "execution account root binding",
    )?;
    let mut world = if let Some(pinned) = &lazy {
        snapshot.check_root(b"accounts")?;
        snapshot.check_root(b"account-data")?;
        need(
            (accounts == 0) == (snapshot.root(b"accounts") == mpt::empty_root())
                && (accounts == 0) == (snapshot.root(b"account-data") == mpt::empty_root()),
            "account root/count",
        )?;
        World::from_reader(
            account_reader::Reader::new(pinned.clone(), expected.digest),
            expected.roots.evm,
            accounts,
        )
    } else {
        let mut world = World::default();
        scan(b"account-data", accounts as usize, &mut |key, value| {
            let address: [u8; 20] = preimage(key, value, 20)?.try_into().unwrap();
            let mut r = Cursor {
                raw: value,
                offset: 20,
            };
            let nonce = r.u64()?;
            let balance = from_bytes(r.take(32)?);
            let storage_size = from_bytes(r.take(32)?);
            let code_hash = r.hash()?;
            let storage_root = r.hash()?;
            let slots = r.u64()?;
            r.done()?;
            let code = if code_hash == keccak(&[]) {
                vec![]
            } else {
                snapshot
                    .get(b"code", &code_hash)?
                    .ok_or_else(|| bad("missing account code"))?
            };
            need(
                code.len() <= 32768 && keccak(&code) == code_hash,
                "account code hash/width",
            )?;
            let storage =
                account_reader::materialize_slots(snapshot, storage_root, 4096, remaining.get())?;
            need(storage.len() as u64 == slots, "account slot count")?;
            let extra = code.len() + storage.len() * 64 + 128;
            account_bytes.set(
                account_bytes
                    .get()
                    .checked_add(extra)
                    .ok_or_else(|| bad("account bytes"))?,
            );
            need(
                account_bytes.get() <= 32 << 20,
                "account materialization byte bound",
            )?;
            let account = Account {
                nonce,
                balance,
                storage_size,
                code,
                storage,
                ..Default::default()
            };
            need(
                account.storage_root().map_err(bad)? == storage_root,
                "restored account storage root",
            )?;
            need(
                snapshot.get(b"accounts", &address)? == Some(account.consensus_rlp().map_err(bad)?),
                "restored account consensus value",
            )?;
            need(
                world.account(&address).map_err(bad)?.is_none(),
                "duplicate account preimage",
            )?;
            world.insert(address, account).map_err(bad)
        })?;
        need(world.account_count() == accounts, "missing account index")?;
        world
    };
    if let Some(pinned) = &lazy {
        for name in [b"lockups".as_slice(), b"etx", b"etx-data", b"etx-aux"] {
            snapshot.check_root(name)?;
        }
        need(
            newest >= oldest
                && (newest == oldest) == (snapshot.root(b"etx-data") == mpt::empty_root()),
            "ETX queue interval/data root",
        )?;
        for (key, index) in [(index_key(false), oldest), (index_key(true), newest)] {
            need(
                snapshot.get(b"etx", &key)? == (index != 0).then(|| magnitude(index)),
                "ETX metadata index binding",
            )?;
        }
        let pristine = oldest == 0
            && newest == 0
            && snapshot.root(b"etx-data") == mpt::empty_root()
            && snapshot.root(b"etx-aux") == mpt::empty_root();
        need(
            (snapshot.root(b"etx") == mpt::empty_root()) == pristine,
            "ETX empty consensus/data roots",
        )?;
        let reader = aux_reader::Reader::new(pinned.clone(), expected.digest);
        world.set_lockup_reader(snapshot.root(b"lockups"), reader.clone());
        *world.etxs_mut() =
            EtxState::from_reader(oldest, newest, snapshot.root(b"etx"), reader).map_err(bad)?;
    } else {
        scan(b"lockups", 1_000_000, &mut |key, value| {
            let raw = preimage(key, value, 47)?;
            need(&raw[..2] == b"cl", "lockup key format")?;
            let k = Key {
                owner: raw[2..22].try_into().unwrap(),
                beneficiary: raw[22..42].try_into().unwrap(),
                lock: raw[42],
                epoch: u32::from_be_bytes(raw[43..47].try_into().unwrap()),
            };
            need(
                world.lockup(&k).map_err(bad)?.is_none(),
                "duplicate lockup key",
            )?;
            world
                .set_lockup(k, Some(Lockup::decode(&value[47..]).map_err(bad)?))
                .map_err(bad)?;
            Ok(())
        })?;
        let mut slots = BTreeMap::new();
        scan(
            b"etx-data",
            crate::etx::MAX_PENDING_ETXS,
            &mut |key, value| {
                let raw = preimage(key, value, 8)?;
                let index = u64::from_be_bytes(raw.try_into().unwrap());
                let tx = Transaction::decode(&value[8..]).map_err(bad)?;
                need(slots.insert(index, tx).is_none(), "duplicate queue slot")?;
                Ok(())
            },
        )?;
        let mut auxiliary = BTreeMap::new();
        scan(b"etx-aux", 4096, &mut |key, value| {
            let p = preimage(key, value, 32)?;
            need(
                auxiliary.insert(p.to_vec(), value[32..].to_vec()).is_none(),
                "duplicate queue auxiliary key",
            )?;
            Ok(())
        })?;
        *world.etxs_mut() =
            EtxState::from_image(oldest, newest, slots.into_iter().collect(), auxiliary)
                .map_err(bad)?;
    }
    let utxos = if let Some(shared) = lazy {
        snapshot.check_root(b"utxos")?;
        need(
            (snapshot.root(b"utxos") == mpt::empty_root()) == (raw_count == 0),
            "UTXO empty-root/count binding",
        )?;
        let reader = utxo_reader::Reader::new(shared, snapshot, raw_count, expected.digest);
        UtxoState::from_reader(reader, raw_count, &accumulator, count).map_err(bad)?
    } else {
        let mut entries = BTreeMap::new();
        scan(b"utxos", raw_count as usize, &mut |key, value| {
            let p = preimage(key, value, 34)?;
            let point = OutPoint {
                hash: p[..32].try_into().unwrap(),
                index: u16::from_be_bytes(p[32..].try_into().unwrap()),
            };
            let mut r = Cursor {
                raw: value,
                offset: 34,
            };
            let denomination = r.take(1)?[0];
            let address = r.take(20)?.try_into().unwrap();
            let len = r.take(1)?[0] as usize;
            need(len <= 32, "UTXO lock bound")?;
            let lock = r.take(len)?.to_vec();
            r.done()?;
            need(
                entries
                    .insert(
                        point,
                        Utxo {
                            denomination,
                            address,
                            lock,
                        },
                    )
                    .is_none(),
                "duplicate UTXO",
            )?;
            Ok(())
        })?;
        need(entries.len() as u64 == raw_count, "missing raw UTXO index")?;
        UtxoState::from_image(entries, &accumulator, count).map_err(bad)?
    };
    need(
        roots(&world, &utxos)? == expected.roots,
        "restored execution roots/counts",
    )?;
    need(
        snapshot.root(b"accounts") == expected.roots.evm
            && snapshot.root(b"etx") == expected.roots.etx,
        "execution consensus tree roots",
    )?;
    Ok((world, utxos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evm::word::Word;
    use quai_store::state_db::StateDatabase;
    fn equal(a: (World, UtxoState), b: (World, UtxoState)) {
        assert_eq!(a.0, b.0);
        assert_eq!(a.1.materialize().unwrap(), b.1.materialize().unwrap());
        assert_eq!(a.1.consensus_size(), b.1.consensus_size());
        assert_eq!(a.1.accumulator(), b.1.accumulator());
        assert_eq!(a.1.root(), b.1.root());
    }
    fn commit(
        db: &StateDatabase,
        parent: Option<Hash>,
        block: Hash,
        before: &World,
        before_u: &UtxoState,
        after: &World,
        after_u: &UtxoState,
        select: bool,
    ) -> (Hash, Commitment, usize) {
        let mut state = None;
        let committed = db
            .commit_prepared(db.head().unwrap(), parent, block, select, |w| {
                let c = apply_delta(w, before, before_u, after, after_u)?;
                let metadata = c.digest.to_vec();
                state = Some(c);
                Ok(metadata)
            })
            .unwrap();
        (
            committed.version.id,
            state.unwrap(),
            committed.new_nodes.nodes,
        )
    }
    #[test]
    fn sparse_qi_restoration_reads_touched_points_and_persists_legacy_ghosts() {
        let path = std::env::temp_dir().join(format!(
            "quai-sparse-qi-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        let db = StateDatabase::open(&path.join("state.redb"), [19; 32]).unwrap();
        let world = World::default();
        let none = UtxoState::default();
        let (genesis, _, _) = commit(&db, None, [0; 32], &world, &none, &world, &none, true);
        let point = |n: u64| OutPoint {
            hash: keccak(&n.to_be_bytes()),
            index: 0,
        };
        let mut memory = UtxoState::default();
        for n in 0..5000u64 {
            memory
                .insert(
                    point(n),
                    Utxo {
                        denomination: (n % 15) as u8,
                        address: [3; 20],
                        lock: vec![],
                    },
                )
                .unwrap();
        }
        let (parent, c, _) = commit(
            &db,
            Some(genesis),
            [1; 32],
            &world,
            &none,
            &world,
            &memory,
            true,
        );
        let (w, sparse) =
            restore_sparse_qi(Arc::new(Mutex::new(db.snapshot(Some(parent)).unwrap())), &c)
                .unwrap();
        assert!(sparse.is_lazy());
        assert_eq!(sparse.raw_len(), 5000);
        assert_eq!(sparse.loaded_entries(), 0);
        for n in 0..4u64 {
            assert_eq!(
                sparse.get(&point(n)).unwrap(),
                memory.get(&point(n)).unwrap()
            );
        }
        assert_eq!(sparse.loaded_entries(), 4);
        assert!(sparse.get(&point(99999)).unwrap().is_none());
        let mut changed = sparse.clone();
        let mut expected = memory.clone();
        changed.remove(&point(0)).unwrap();
        expected.remove(&point(0)).unwrap();
        let overwrite = Utxo {
            denomination: 14,
            address: [7; 20],
            lock: vec![1],
        };
        changed.create_raw(point(1), overwrite.clone()).unwrap();
        expected.create_raw(point(1), overwrite).unwrap();
        assert_ne!(changed.raw_len(), changed.consensus_size());
        let (child, c2, _) = commit(&db, Some(parent), [2; 32], &w, &sparse, &w, &changed, true);
        assert!(
            sparse.loaded_entries() <= 5,
            "delta enumerated unrelated membership"
        );
        let (_, recovered) =
            restore_sparse_qi(Arc::new(Mutex::new(db.snapshot(Some(child)).unwrap())), &c2)
                .unwrap();
        assert_eq!(recovered.loaded_entries(), 0);
        assert_eq!(recovered.raw_len(), expected.raw_len());
        assert_eq!(recovered.accumulator(), expected.accumulator());
        assert_eq!(recovered.consensus_size(), expected.consensus_size());
        assert_eq!(recovered.root(), expected.root());
        assert_eq!(
            recovered.materialize().unwrap(),
            expected.materialize().unwrap()
        );
        assert_eq!(
            sparse.get(&point(0)).unwrap(),
            memory.get(&point(0)).unwrap()
        );
        drop(recovered);
        drop(changed);
        drop(sparse);
        drop(db);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn sparse_qi_pinned_membership_survives_writer_close_reopen_and_child_commit() {
        let directory = std::env::temp_dir().join(format!(
            "quai-sparse-qi-lease-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("state.redb");
        let db = StateDatabase::open(&path, [21; 32]).unwrap();
        let world = World::default();
        let none = UtxoState::default();
        let mut memory = UtxoState::default();
        let point = |n| OutPoint {
            hash: [n; 32],
            index: 0,
        };
        for n in 1..=3 {
            memory
                .insert(
                    point(n),
                    Utxo {
                        denomination: n,
                        address: [3; 20],
                        lock: vec![],
                    },
                )
                .unwrap();
        }
        let (parent, c, _) = commit(&db, None, [0; 32], &world, &none, &world, &memory, true);
        let (world, sparse) =
            restore_sparse_qi(Arc::new(Mutex::new(db.snapshot(Some(parent)).unwrap())), &c)
                .unwrap();
        assert_eq!(sparse.loaded_entries(), 0);
        drop(db);
        // The old implementation returned unavailable/DatabaseClosed here.
        assert_eq!(
            sparse.get(&point(1)).unwrap(),
            memory.get(&point(1)).unwrap()
        );
        let db = StateDatabase::open(&path, [21; 32]).unwrap();
        let mut changed = sparse.clone();
        changed.remove(&point(1)).unwrap();
        let (child, next, _) = commit(
            &db,
            Some(parent),
            [1; 32],
            &world,
            &sparse,
            &world,
            &changed,
            true,
        );
        let (_, current) = restore_sparse_qi(
            Arc::new(Mutex::new(db.snapshot(Some(child)).unwrap())),
            &next,
        )
        .unwrap();
        drop(db);
        assert!(current.get(&point(1)).unwrap().is_none());
        assert_eq!(
            sparse.get(&point(1)).unwrap(),
            memory.get(&point(1)).unwrap()
        );
        // This point was never read or cached by either provider before close.
        assert_eq!(
            sparse.get(&point(3)).unwrap(),
            memory.get(&point(3)).unwrap()
        );
        assert_eq!(
            current.get(&point(3)).unwrap(),
            memory.get(&point(3)).unwrap()
        );
        assert!(sparse.get(&point(4)).unwrap().is_none());
        assert_eq!(sparse.root(), memory.root());
        assert_eq!(sparse.raw_len(), 3);
        assert_eq!(current.raw_len(), 2);
        drop(current);
        drop(changed);
        drop(sparse);
        let db = StateDatabase::open(&path, [21; 32]).unwrap();
        assert_eq!(db.head().unwrap(), Some(child));
        drop(db);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn malformed_sparse_qi_records_are_unavailable_and_never_missing_membership() {
        let path = std::env::temp_dir().join(format!(
            "quai-sparse-qi-bad-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        let db = StateDatabase::open(&path.join("state.redb"), [20; 32]).unwrap();
        let w = World::default();
        let none = UtxoState::default();
        let (g, _, _) = commit(&db, None, [0; 32], &w, &none, &w, &none, true);
        let point = OutPoint {
            hash: [11; 32],
            index: 4,
        };
        let value = Utxo {
            denomination: 1,
            address: [3; 20],
            lock: vec![],
        };
        let mut u = none.clone();
        u.insert(point, value.clone()).unwrap();
        let (p, c, _) = commit(&db, Some(g), [1; 32], &w, &none, &w, &u, true);
        let valid = utxo_record(&point, &value).unwrap();
        let mut bad_key = valid.clone();
        bad_key[0] ^= 1;
        let mut bad_denom = valid.clone();
        bad_denom[34] = 255;
        let mut bad_lock = valid.clone();
        bad_lock[55] = 33;
        let mut trailing = valid.clone();
        trailing.push(0);
        for (n, bad_value) in [vec![1], bad_key, bad_denom, bad_lock, trailing]
            .into_iter()
            .enumerate()
        {
            // Deliberately damaged local value; this version has no native
            // admission receipt. Supply its new local digest to test the codec.
            let version = db
                .commit(
                    Some(p),
                    Some(p),
                    [n as u8 + 2; 32],
                    b"damaged-record",
                    false,
                    |w| w.put(b"utxos", &point_key(&point), &bad_value),
                )
                .unwrap()
                .version
                .id;
            let snapshot = db.snapshot(Some(version)).unwrap();
            let damaged = Commitment {
                roots: c.roots.clone(),
                digest: snapshot_digest(&snapshot).unwrap(),
            };
            let (_, state) = restore_sparse_qi(Arc::new(Mutex::new(snapshot)), &damaged).unwrap();
            assert!(matches!(
                state.get(&point),
                Err(crate::qi::ReadError::Unavailable(_))
            ));
            assert_eq!(state.raw_len(), 1);
            assert_eq!(state.loaded_entries(), 0);
        }
        drop(db);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn missing_code_and_storage_preimages_never_restore_as_zero_state() {
        let path = std::env::temp_dir().join(format!(
            "quai-keyed-incomplete-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        let db = StateDatabase::open(&path.join("state.redb"), [9; 32]).unwrap();
        let empty = World::default();
        let none = UtxoState::default();
        let (g, _, _) = commit(&db, None, [0; 32], &empty, &none, &empty, &none, true);
        let (_, w, u) = crate::state_image::tests::sample();
        let (a, c, _) = commit(&db, Some(g), [1; 32], &empty, &none, &w, &u, true);
        let address = *w.accounts().unwrap().keys().next().unwrap();
        for (number, tree, key) in [
            (
                2,
                b"code".as_slice(),
                keccak(&w.account(&address).unwrap().unwrap().code).to_vec(),
            ),
            (3, b"storage-keys".as_slice(), [0; 32].to_vec()),
        ] {
            let v = db
                .commit(
                    Some(a),
                    Some(a),
                    [number; 32],
                    b"incomplete-local-state",
                    false,
                    |write| write.put(tree, &key, &[]),
                )
                .unwrap()
                .version
                .id;
            let snapshot = db.snapshot(Some(v)).unwrap();
            assert!(restore(&snapshot, &c).is_err());
            let mut forged = c.clone();
            forged.digest = snapshot_digest(&snapshot).unwrap();
            assert!(restore(&snapshot, &forged).is_err());
            assert_eq!(db.head().unwrap(), Some(a));
        }
        drop(db);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn keyed_execution_versions_preserve_code_slots_queue_lockups_and_legacy_qi_ghosts() {
        let path = std::env::temp_dir().join(format!(
            "quai-execution-keyed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        let file = path.join("state.redb");
        let db = StateDatabase::open(&file, [9; 32]).unwrap();
        let empty = World::default();
        let no_utxo = UtxoState::default();
        let (g, _, _) = commit(&db, None, [0; 32], &empty, &no_utxo, &empty, &no_utxo, true);
        let (_, world, utxos) = crate::state_image::tests::sample();
        let (a, ca, nodes) = commit(
            &db,
            Some(g),
            [1; 32],
            &empty,
            &no_utxo,
            &world,
            &utxos,
            true,
        );
        assert!(nodes > 0);
        let pinned = db.snapshot(Some(a)).unwrap();
        let (w, u) = restore(&pinned, &ca).unwrap();
        assert_eq!(w, world);
        equal((w.clone(), u), (world.clone(), utxos.clone()));
        assert_ne!(utxos.consensus_size(), utxos.raw_len());
        let mut next = world.clone();
        let mut changed_u = utxos.clone();
        let address = *world.accounts().unwrap().keys().next().unwrap();
        next.account_mut(&address)
            .unwrap()
            .unwrap()
            .put([0; 32], [0; 32])
            .unwrap();
        next.account_mut(&address)
            .unwrap()
            .unwrap()
            .put([2; 32], [3; 32])
            .unwrap();
        next.account_mut(&address).unwrap().unwrap().code = vec![0x60, 1, 0];
        next.account_mut(&address).unwrap().unwrap().balance = Word::from_u64(101);
        let key = *next.lockups().unwrap().keys().next().unwrap();
        next.set_lockup(key, None).unwrap();
        changed_u.remove_auxiliary_hash(world.lockups().unwrap()[&key].commitment(key));
        next.etxs_mut().pop().unwrap();
        let aux = next
            .etxs()
            .auxiliary()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        next.etxs_mut().set_auxiliary(aux, vec![4, 5]).unwrap();
        let point = *changed_u.materialize().unwrap().keys().next().unwrap();
        let v = changed_u.materialize().unwrap()[&point].clone();
        changed_u
            .create_raw(
                point,
                Utxo {
                    denomination: 9,
                    ..v
                },
            )
            .unwrap();
        let (b, cb, _) = commit(
            &db,
            Some(a),
            [2; 32],
            &world,
            &utxos,
            &next,
            &changed_u,
            true,
        );
        let (w, u) = restore(&db.snapshot(Some(b)).unwrap(), &cb).unwrap();
        assert_eq!(w, next);
        equal((w.clone(), u), (next.clone(), changed_u.clone()));
        equal(
            restore(&pinned, &ca).unwrap(),
            (world.clone(), utxos.clone()),
        );
        let mut fork = world.clone();
        fork.remove(&address).unwrap();
        let (c, cc, _) = commit(&db, Some(a), [3; 32], &world, &utxos, &fork, &utxos, false);
        assert_eq!(db.head().unwrap(), Some(b));
        equal(
            restore(&db.snapshot(Some(c)).unwrap(), &cc).unwrap(),
            (fork, utxos.clone()),
        );
        db.select(b, c).unwrap();
        equal(
            restore(&db.snapshot(Some(a)).unwrap(), &ca).unwrap(),
            (world.clone(), utxos.clone()),
        );
        let before = db.head().unwrap();
        let rejected = db.commit_prepared(before, Some(c), [4; 32], true, |w| {
            let _ = apply_delta(w, &world, &utxos, &next, &changed_u)?;
            Ok(vec![])
        });
        assert!(rejected.is_err());
        assert_eq!(db.head().unwrap(), before);
        assert!(db.version_for_block([4; 32]).unwrap().is_none());
        drop(pinned);
        drop(db);
        let reopened = StateDatabase::open(&file, [9; 32]).unwrap();
        assert_eq!(reopened.head().unwrap(), Some(c));
        equal(
            restore(&reopened.snapshot(Some(a)).unwrap(), &ca).unwrap(),
            (world, utxos),
        );
        drop(reopened);
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[cfg(test)]
mod lazy_tests;

#[cfg(test)]
mod aux_tests;
