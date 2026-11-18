//! Account and storage paths from a pinned, locally admitted root set.
use super::*;
use crate::evm::state::{AccountReader, NodeReader, ReadError, StorageBase};
use quai_primitives::mpt::ReadNodes;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicUsize, Ordering},
};
fn unavailable(v: impl std::fmt::Display) -> ReadError {
    ReadError::Unavailable(v.to_string())
}
struct Nodes {
    snapshot: Arc<Mutex<Snapshot>>,
    identity: Hash,
}
impl std::fmt::Debug for Nodes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateNodes")
            .field("identity", &self.identity)
            .finish()
    }
}
impl ReadNodes for Nodes {
    fn get_node(&self, hash: &Hash) -> Result<Option<Vec<u8>>, mpt::Error> {
        self.snapshot
            .lock()
            .map_err(|e| mpt::Error::Backend(e.to_string()))?
            .node(hash)
            .map_err(|e| mpt::Error::Backend(e.to_string()))
    }
}
impl NodeReader for Nodes {
    fn identity(&self) -> Hash {
        self.identity
    }
    fn slots(
        &self,
        root: Hash,
        max_entries: usize,
        max_bytes: usize,
    ) -> Result<BTreeMap<Hash, Hash>, ReadError> {
        let snapshot = self.snapshot.lock().map_err(unavailable)?;
        materialize_slots(&snapshot, root, max_entries, max_bytes).map_err(unavailable)
    }
}
pub(super) fn materialize_slots(
    snapshot: &Snapshot,
    root: Hash,
    max_entries: usize,
    max_bytes: usize,
) -> io::Result<BTreeMap<Hash, Hash>> {
    let mut slots = BTreeMap::new();
    snapshot.scan_at(root, max_entries, max_bytes, &mut |hashed, raw| {
        let key = snapshot
            .get_hashed(b"storage-keys", hashed)?
            .ok_or_else(|| bad("missing storage preimage"))?;
        need(
            key.len() == 32 && keccak(&key) == hashed,
            "storage preimage binding",
        )?;
        need(
            slots
                .insert(crate::slice::array(&key)?, decode_slot(raw)?)
                .is_none(),
            "duplicate storage preimage",
        )?;
        Ok(())
    })?;
    Ok(slots)
}
struct Cache {
    rows: BTreeMap<Address, Option<Arc<Account>>>,
    order: VecDeque<Address>,
    bytes: usize,
}
pub(super) struct Reader {
    nodes: Arc<Nodes>,
    cache: Mutex<Cache>,
    decoded: AtomicUsize,
}
impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountReader")
            .field("identity", &self.nodes.identity)
            .finish()
    }
}
use quai_primitives::transaction::Address;
impl Reader {
    pub fn new(snapshot: Arc<Mutex<Snapshot>>, digest: Hash) -> Arc<Self> {
        let mut h = Sha256::new();
        h.update(b"QUAI-PINNED-ACCOUNTS-2\n");
        h.update(digest);
        Arc::new(Self {
            nodes: Arc::new(Nodes {
                snapshot,
                identity: h.finalize().into(),
            }),
            cache: Mutex::new(Cache {
                rows: BTreeMap::new(),
                order: VecDeque::new(),
                bytes: 0,
            }),
            decoded: AtomicUsize::new(0),
        })
    }
    fn decode(&self, snapshot: &Snapshot, address: Address, raw: &[u8]) -> io::Result<Account> {
        need(
            raw.len() == 164 && raw[..20] == address,
            "account record key/width",
        )?;
        let mut r = Cursor { raw, offset: 20 };
        let nonce = r.u64()?;
        let balance = from_bytes(r.take(32)?);
        let storage_size = from_bytes(r.take(32)?);
        let code_hash = r.hash()?;
        let root = r.hash()?;
        let count = r.u64()?;
        r.done()?;
        need(
            (count == 0) == (root == mpt::empty_root()),
            "account storage root/count",
        )?;
        snapshot.check_at(root)?;
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
        let account = Account {
            nonce,
            balance,
            storage_size,
            code,
            storage: BTreeMap::new(),
            backing: Some(StorageBase {
                reader: self.nodes.clone(),
                root,
                count,
            }),
        };
        need(
            snapshot.get(b"accounts", &address)? == Some(account.consensus_rlp().map_err(bad)?),
            "account consensus binding",
        )?;
        Ok(account)
    }
}
impl ReadNodes for Reader {
    fn get_node(&self, hash: &Hash) -> Result<Option<Vec<u8>>, mpt::Error> {
        self.nodes.get_node(hash)
    }
}
impl AccountReader for Reader {
    fn identity(&self) -> Hash {
        self.nodes.identity
    }
    fn loaded(&self) -> usize {
        self.decoded.load(Ordering::Relaxed)
    }
    fn account(&self, address: &Address) -> Result<Option<Arc<Account>>, ReadError> {
        if let Some(value) = self.cache.lock().map_err(unavailable)?.rows.get(address) {
            return Ok(value.clone());
        }
        let snapshot = self.nodes.snapshot.lock().map_err(unavailable)?;
        let raw = snapshot
            .get(b"account-data", address)
            .map_err(unavailable)?;
        let value = if let Some(raw) = raw {
            Some(Arc::new(
                self.decode(&snapshot, *address, &raw)
                    .map_err(unavailable)?,
            ))
        } else {
            if snapshot
                .get(b"accounts", address)
                .map_err(unavailable)?
                .is_some()
            {
                return Err(unavailable("missing account metadata"));
            }
            None
        };
        drop(snapshot);
        self.decoded
            .fetch_add(usize::from(value.is_some()), Ordering::Relaxed);
        let mut cache = self.cache.lock().map_err(unavailable)?;
        if !cache.rows.contains_key(address) {
            let bytes = value.as_ref().map_or(0, |v| v.code.len());
            while cache.rows.len() >= 1024 || cache.bytes + bytes > 16 << 20 {
                let old = cache
                    .order
                    .pop_front()
                    .ok_or_else(|| unavailable("account cache accounting"))?;
                if let Some(Some(old)) = cache.rows.remove(&old) {
                    cache.bytes = cache
                        .bytes
                        .checked_sub(old.code.len())
                        .ok_or_else(|| unavailable("account cache bytes"))?;
                }
            }
            cache.bytes += bytes;
            cache.order.push_back(*address);
            cache.rows.insert(*address, value.clone());
        }
        Ok(value)
    }
    fn accounts(
        &self,
        max_entries: usize,
        max_bytes: usize,
    ) -> Result<BTreeMap<Address, Arc<Account>>, ReadError> {
        let snapshot = self.nodes.snapshot.lock().map_err(unavailable)?;
        let mut rows = BTreeMap::new();
        let mut bytes = 0usize;
        snapshot
            .scan(
                b"account-data",
                max_entries,
                max_bytes,
                &mut |hashed, raw| {
                    let address: Address = crate::slice::array(&preimage(hashed, raw, 20)?)?;
                    let account = self.decode(&snapshot, address, raw)?;
                    bytes = bytes
                        .checked_add(account.code.len() + raw.len())
                        .ok_or_else(|| bad("account export bytes"))?;
                    need(bytes <= max_bytes, "account export bytes")?;
                    need(
                        rows.insert(address, Arc::new(account)).is_none(),
                        "duplicate account preimage",
                    )?;
                    Ok(())
                },
            )
            .map_err(unavailable)?;
        self.decoded.fetch_add(rows.len(), Ordering::Relaxed);
        Ok(rows)
    }
}
