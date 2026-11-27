//! Checkpoint state-trie, storage-trie, ETX-trie, and contract-bytecode syncer
//! over the `/quai/1.0.0` `GetTrieNode` / `GetTrieNodes` P2P protocol.
//!
//! Starting from a checkpoint block's `evm_root` and `etx_set_root` (typically
//! ~4,000 blocks behind the chain tip), [`TrieSyncer`] performs a breadth-first
//! download of all referenced MPT nodes (branch, extension, leaf), every
//! account's storage trie root, and every non-empty contract `codeHash`,
//! cryptographically verifying `keccak256(blob) == expected_hash` for every
//! single byte before persisting to a [`TrieSink`].
use quai_network::{public::PublicNode, MAX_TRIE_NODE_BATCH_SIZE};
use quai_primitives::{mpt, transaction::keccak, Hash};
use redb::{Database, ReadableDatabase, TableDefinition};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io;
use std::path::Path;
use std::sync::Arc;

/// Keccak-256 of empty bytecode (`0xc5d24601...`). Never needs to be fetched.
pub const EMPTY_CODE_HASH: Hash = [
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
];

const TRIE_NODES_TABLE: TableDefinition<&[u8; 32], &[u8]> =
    TableDefinition::new("checkpoint.trie_nodes.v1");

/// What kind of trie blob a queued hash represents so the BFS walker knows
/// whether to parse MPT children / account leaves or treat the blob as opaque bytecode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    AccountTrie,
    StorageTrie,
    EtxTrie,
    ContractCode,
}

/// Statistics produced by a checkpoint trie sync run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub account_nodes: u64,
    pub storage_nodes: u64,
    pub etx_nodes: u64,
    pub code_blobs: u64,
    pub total_bytes: u64,
    pub batches_requested: u64,
}

/// Persistence sink for verified trie nodes and contract bytecode.
pub trait TrieSink: Send + Sync {
    fn contains(&self, hash: &Hash) -> io::Result<bool>;
    fn put_batch(&mut self, entries: &[(Hash, Vec<u8>)]) -> io::Result<()>;
}

/// Durable `redb`-backed trie node store that also implements `serve::TrieNodeReader`
/// so synced nodes can immediately serve `GetTrieNode`/`GetTrieNodes` to peers.
pub struct RedbTrieStore {
    db: Arc<Database>,
}

impl RedbTrieStore {
    pub fn open(path: &Path) -> io::Result<Self> {
        let db = Database::create(path).map_err(|e| io::Error::other(e.to_string()))?;
        let tx = db
            .begin_write()
            .map_err(|e| io::Error::other(e.to_string()))?;
        {
            let _ = tx
                .open_table(TRIE_NODES_TABLE)
                .map_err(|e| io::Error::other(e.to_string()))?;
        }
        tx.commit().map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self { db: Arc::new(db) })
    }

    pub fn get(&self, hash: &Hash) -> io::Result<Option<Vec<u8>>> {
        let tx = self
            .db
            .begin_read()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let table = tx
            .open_table(TRIE_NODES_TABLE)
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(table
            .get(hash)
            .map_err(|e| io::Error::other(e.to_string()))?
            .map(|v| v.value().to_vec()))
    }
}

impl crate::serve::TrieNodeReader for RedbTrieStore {
    fn trie_node(&self, hash: &Hash) -> Option<Vec<u8>> {
        self.get(hash).ok().flatten()
    }
}

impl TrieSink for RedbTrieStore {
    fn contains(&self, hash: &Hash) -> io::Result<bool> {
        Ok(self.get(hash)?.is_some())
    }

    fn put_batch(&mut self, entries: &[(Hash, Vec<u8>)]) -> io::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let tx = self
            .db
            .begin_write()
            .map_err(|e| io::Error::other(e.to_string()))?;
        {
            let mut table = tx
                .open_table(TRIE_NODES_TABLE)
                .map_err(|e| io::Error::other(e.to_string()))?;
            for (h, blob) in entries {
                table
                    .insert(h, blob.as_slice())
                    .map_err(|e| io::Error::other(e.to_string()))?;
            }
        }
        tx.commit().map_err(|e| io::Error::other(e.to_string()))?;
        Ok(())
    }
}

/// In-memory trie sink for tests and ephemeral checkpoint verification.
#[derive(Default)]
pub struct MemoryTrieSink {
    pub nodes: BTreeMap<Hash, Vec<u8>>,
}

impl TrieSink for MemoryTrieSink {
    fn contains(&self, hash: &Hash) -> io::Result<bool> {
        Ok(self.nodes.contains_key(hash))
    }

    fn put_batch(&mut self, entries: &[(Hash, Vec<u8>)]) -> io::Result<()> {
        for (h, blob) in entries {
            self.nodes.insert(*h, blob.clone());
        }
        Ok(())
    }
}

/// Abstract P2P fetcher so [`TrieSyncer`] works over both live [`PublicNode`]
/// peers and deterministic in-memory test peers.
#[async_trait::async_trait]
pub trait TrieFetcher: Send + Sync {
    async fn fetch_batch(&self, hashes: Vec<Hash>) -> io::Result<Vec<(Hash, Vec<u8>)>>;
    async fn fetch_single(&self, hash: Hash) -> io::Result<Option<Vec<u8>>>;
}

/// Live P2P fetcher backed by [`PublicNode::get_trie_nodes`] and [`PublicNode::get_trie_node`].
pub struct P2pTrieFetcher {
    pub node: Arc<PublicNode>,
    pub location: Vec<u8>,
}

#[async_trait::async_trait]
impl TrieFetcher for P2pTrieFetcher {
    async fn fetch_batch(&self, hashes: Vec<Hash>) -> io::Result<Vec<(Hash, Vec<u8>)>> {
        self.node.get_trie_nodes(&self.location, hashes).await
    }

    async fn fetch_single(&self, hash: Hash) -> io::Result<Option<Vec<u8>>> {
        self.node.get_trie_node(&self.location, hash).await
    }
}

/// BFS checkpoint state-trie & bytecode downloader.
pub struct TrieSyncer {
    batch_size: usize,
    max_retries_per_batch: usize,
}

impl Default for TrieSyncer {
    fn default() -> Self {
        Self {
            batch_size: MAX_TRIE_NODE_BATCH_SIZE,
            max_retries_per_batch: 4,
        }
    }
}

impl TrieSyncer {
    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size.clamp(1, MAX_TRIE_NODE_BATCH_SIZE);
        self
    }

    /// Download and cryptographically verify all nodes reachable from `evm_root`
    /// (including account storage tries and contract bytecode) and `etx_root`.
    pub async fn sync_checkpoint(
        &self,
        fetcher: &dyn TrieFetcher,
        sink: &mut dyn TrieSink,
        evm_root: Hash,
        etx_root: Hash,
    ) -> io::Result<SyncStats> {
        let empty_trie = mpt::empty_root();
        let mut queue: VecDeque<(Hash, NodeKind)> = VecDeque::new();
        let mut queued: HashSet<Hash> = HashSet::new();
        let mut kinds: HashMap<Hash, NodeKind> = HashMap::new();

        for (root, kind) in [
            (evm_root, NodeKind::AccountTrie),
            (etx_root, NodeKind::EtxTrie),
        ] {
            if root != Hash::default() && root != empty_trie && queued.insert(root) {
                kinds.insert(root, kind);
                queue.push_back((root, kind));
            }
        }

        let mut stats = SyncStats::default();
        let mut stall_rounds = 0usize;

        while !queue.is_empty() {
            let mut batch = Vec::with_capacity(self.batch_size.min(queue.len()));
            while batch.len() < self.batch_size {
                let Some((h, _)) = queue.pop_front() else {
                    break;
                };
                if sink.contains(&h)? {
                    continue;
                }
                batch.push(h);
            }
            if batch.is_empty() {
                break;
            }

            stats.batches_requested += 1;
            let fetched = if batch.len() == 1 {
                let target = batch[0];
                match fetcher.fetch_single(target).await? {
                    Some(blob) => vec![(keccak(&blob), blob)],
                    None => Vec::new(),
                }
            } else {
                let res = fetcher.fetch_batch(batch.clone()).await?;
                if res.is_empty() && batch.len() == 1 {
                    Vec::new()
                } else {
                    res
                }
            };

            let batch_set: HashSet<Hash> = batch.iter().copied().collect();
            let mut resolved: HashSet<Hash> = HashSet::with_capacity(fetched.len());
            let mut to_persist: Vec<(Hash, Vec<u8>)> = Vec::with_capacity(fetched.len());

            for (claimed_hash, blob) in fetched {
                if blob.is_empty() {
                    continue;
                }
                let actual = keccak(&blob);
                if actual != claimed_hash || !batch_set.contains(&actual) || !resolved.insert(actual)
                {
                    continue;
                }
                let kind = kinds.get(&actual).copied().unwrap_or(NodeKind::StorageTrie);
                match kind {
                    NodeKind::AccountTrie => stats.account_nodes += 1,
                    NodeKind::StorageTrie => stats.storage_nodes += 1,
                    NodeKind::EtxTrie => stats.etx_nodes += 1,
                    NodeKind::ContractCode => stats.code_blobs += 1,
                }
                stats.total_bytes = stats.total_bytes.saturating_add(blob.len() as u64);

                if kind != NodeKind::ContractCode {
                    let children = extract_mpt_children(&blob, kind)?;
                    for (child_hash, child_kind) in children {
                        if child_hash != Hash::default()
                            && child_hash != empty_trie
                            && child_hash != EMPTY_CODE_HASH
                            && queued.insert(child_hash)
                        {
                            kinds.insert(child_hash, child_kind);
                            queue.push_back((child_hash, child_kind));
                        }
                    }
                }
                to_persist.push((actual, blob));
            }

            if !to_persist.is_empty() {
                sink.put_batch(&to_persist)?;
                stall_rounds = 0;
            } else {
                stall_rounds += 1;
                if stall_rounds > self.max_retries_per_batch {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "checkpoint trie sync stalled: {} unresolved hashes remain",
                            batch.len() + queue.len()
                        ),
                    ));
                }
            }

            // Re-queue any hashes from this batch that the peer did not return
            // (e.g. truncated by the 2 MiB response byte cap or partial peer view).
            for h in batch {
                if !resolved.contains(&h) {
                    let kind = kinds.get(&h).copied().unwrap_or(NodeKind::StorageTrie);
                    queue.push_back((h, kind));
                }
            }
        }

        Ok(stats)
    }
}

/// Extract child 32-byte MPT node hashes (and for account-trie leaves, storage
/// trie roots and contract bytecode hashes) from an RLP-encoded MPT node.
pub fn extract_mpt_children(node_rlp: &[u8], kind: NodeKind) -> io::Result<Vec<(Hash, NodeKind)>> {
    let items = rlp_decode_list(node_rlp)?;
    let mut out = Vec::new();
    match items.len() {
        17 => {
            // Branch node: 16 child slots + 1 optional value slot
            for child in &items[..16] {
                collect_child_ref(child, kind, &mut out)?;
            }
            if kind == NodeKind::AccountTrie && !items[16].is_empty() {
                extract_account_leaf_refs(&items[16], &mut out);
            }
        }
        2 => {
            // Extension (compact flag bit 1 == 0) or Leaf (compact flag bit 1 == 1)
            let path = rlp_decode_bytes(&items[0])?;
            let first = *path
                .first()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty compact path"))?;
            let is_leaf = (first >> 4) & 2 != 0;
            if is_leaf {
                if kind == NodeKind::AccountTrie {
                    let val = rlp_decode_bytes(&items[1])?;
                    extract_account_leaf_refs(val, &mut out);
                }
            } else {
                collect_child_ref(&items[1], kind, &mut out)?;
            }
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected MPT RLP node item count: {}", items.len()),
            ));
        }
    }
    Ok(out)
}

fn collect_child_ref(
    encoded_item: &[u8],
    kind: NodeKind,
    out: &mut Vec<(Hash, NodeKind)>,
) -> io::Result<()> {
    if encoded_item.is_empty() || encoded_item == [0x80] {
        return Ok(());
    }
    // Embedded inline child node (< 32 bytes) is an RLP list itself
    if encoded_item[0] >= 0xc0 {
        let nested = extract_mpt_children(encoded_item, kind)?;
        out.extend(nested);
        return Ok(());
    }
    let raw = rlp_decode_bytes(encoded_item)?;
    if raw.len() == 32 {
        let mut h = [0u8; 32];
        h.copy_from_slice(raw);
        out.push((h, kind));
    }
    Ok(())
}

/// Extract storage root (item 2) and code hash (item 3) from an RLP-encoded
/// Ethereum/Quai state account `[nonce, balance, storageRoot, codeHash, ...]`.
fn extract_account_leaf_refs(account_rlp: &[u8], out: &mut Vec<(Hash, NodeKind)>) {
    let Ok(fields) = rlp_decode_list(account_rlp) else {
        return;
    };
    if fields.len() < 4 {
        return;
    }
    if let Ok(storage_root) = rlp_decode_bytes(&fields[2]) {
        if storage_root.len() == 32 {
            let mut h = [0u8; 32];
            h.copy_from_slice(storage_root);
            out.push((h, NodeKind::StorageTrie));
        }
    }
    if let Ok(code_hash) = rlp_decode_bytes(&fields[3]) {
        if code_hash.len() == 32 {
            let mut h = [0u8; 32];
            h.copy_from_slice(code_hash);
            if h != EMPTY_CODE_HASH {
                out.push((h, NodeKind::ContractCode));
            }
        }
    }
}

fn rlp_read_len(data: &[u8], offset: usize, len_of_len: usize) -> io::Result<(usize, usize)> {
    if len_of_len == 0 || len_of_len > 8 || offset + len_of_len > data.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid RLP length"));
    }
    if data[offset] == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "non-canonical RLP length",
        ));
    }
    let mut len = 0usize;
    for &b in &data[offset..offset + len_of_len] {
        len = len
            .checked_shl(8)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "RLP length overflow"))?
            | usize::from(b);
    }
    Ok((offset + len_of_len, len))
}

fn rlp_next_item(data: &[u8]) -> io::Result<(&[u8], &[u8])> {
    let first = *data
        .first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty RLP item"))?;
    let total = match first {
        0x00..=0x7f => 1,
        0x80..=0xb7 => 1 + usize::from(first - 0x80),
        0xb8..=0xbf => {
            let (start, len) = rlp_read_len(data, 1, usize::from(first - 0xb7))?;
            start
                .checked_add(len)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "RLP overflow"))?
        }
        0xc0..=0xf7 => 1 + usize::from(first - 0xc0),
        0xf8..=0xff => {
            let (start, len) = rlp_read_len(data, 1, usize::from(first - 0xf7))?;
            start
                .checked_add(len)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "RLP overflow"))?
        }
    };
    if total > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated RLP item",
        ));
    }
    Ok(data.split_at(total))
}

fn rlp_decode_list(data: &[u8]) -> io::Result<Vec<Vec<u8>>> {
    let first = *data
        .first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty RLP list"))?;
    let (start, payload_len) = match first {
        0xc0..=0xf7 => (1usize, usize::from(first - 0xc0)),
        0xf8..=0xff => rlp_read_len(data, 1, usize::from(first - 0xf7))?,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected RLP list",
            ));
        }
    };
    let end = start
        .checked_add(payload_len)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "RLP list overflow"))?;
    if end != data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "RLP list length mismatch",
        ));
    }
    let mut cur = &data[start..end];
    let mut items = Vec::new();
    while !cur.is_empty() {
        let (item, rest) = rlp_next_item(cur)?;
        items.push(item.to_vec());
        cur = rest;
    }
    Ok(items)
}

fn rlp_decode_bytes(item: &[u8]) -> io::Result<&[u8]> {
    let first = *item
        .first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty RLP bytes"))?;
    match first {
        0x00..=0x7f => {
            if item.len() != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "trailing bytes in single-byte RLP",
                ));
            }
            Ok(item)
        }
        0x80..=0xb7 => {
            let len = usize::from(first - 0x80);
            if 1 + len != item.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "RLP short string length mismatch",
                ));
            }
            Ok(&item[1..])
        }
        0xb8..=0xbf => {
            let (start, len) = rlp_read_len(item, 1, usize::from(first - 0xb7))?;
            if start.checked_add(len) != Some(item.len()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "RLP long string length mismatch",
                ));
            }
            Ok(&item[start..])
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected RLP byte string",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quai_primitives::trie;

    struct MockPeer {
        blobs: HashMap<Hash, Vec<u8>>,
        max_per_response: usize,
    }

    #[async_trait::async_trait]
    impl TrieFetcher for MockPeer {
        async fn fetch_batch(&self, hashes: Vec<Hash>) -> io::Result<Vec<(Hash, Vec<u8>)>> {
            let mut out = Vec::new();
            for h in hashes {
                if out.len() >= self.max_per_response {
                    break;
                }
                if let Some(blob) = self.blobs.get(&h) {
                    out.push((h, blob.clone()));
                }
            }
            Ok(out)
        }

        async fn fetch_single(&self, hash: Hash) -> io::Result<Option<Vec<u8>>> {
            Ok(self.blobs.get(&hash).cloned())
        }
    }

    #[tokio::test]
    async fn syncs_account_storage_etx_tries_and_contract_code() {
        let mut blobs: HashMap<Hash, Vec<u8>> = HashMap::new();

        // 1. Contract bytecode
        let code = vec![0x60, 0x80, 0x60, 0x40, 0x52, 0xfe];
        let code_hash = keccak(&code);
        blobs.insert(code_hash, code.clone());

        // 2. Storage trie leaf (>32 bytes so it is hash-referenced)
        let storage_leaf = trie::list([
            trie::bytes(&[0x20, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]),
            trie::bytes(&[0x11; 32]),
        ]);
        let storage_root = keccak(&storage_leaf);
        blobs.insert(storage_root, storage_leaf.clone());

        // 3. Account RLP referencing storage_root + code_hash
        let account_rlp = trie::list([
            trie::uint(7),
            trie::uint(1_000_000),
            trie::bytes(&storage_root),
            trie::bytes(&code_hash),
        ]);
        let account_leaf = trie::list([
            trie::bytes(&[0x20, 0x01, 0x02, 0x03]),
            trie::bytes(&account_rlp),
        ]);
        let account_leaf_hash = keccak(&account_leaf);
        blobs.insert(account_leaf_hash, account_leaf.clone());

        // Branch root pointing to account_leaf_hash in slot 0
        let mut branch_items: Vec<Vec<u8>> = (0..17).map(|_| trie::bytes(&[])).collect();
        branch_items[0] = trie::bytes(&account_leaf_hash);
        let evm_root_node = trie::list(branch_items);
        let evm_root = keccak(&evm_root_node);
        blobs.insert(evm_root, evm_root_node.clone());

        // 4. ETX trie root
        let etx_leaf = trie::list([
            trie::bytes(&[0x20, 0x99, 0x88, 0x77, 0x66]),
            trie::bytes(&[0x42; 40]),
        ]);
        let etx_root = keccak(&etx_leaf);
        blobs.insert(etx_root, etx_leaf.clone());

        let peer = MockPeer {
            blobs: blobs.clone(),
            max_per_response: 2, // Force multi-round partial batch re-queueing!
        };
        let mut sink = MemoryTrieSink::default();
        let syncer = TrieSyncer::default().with_batch_size(4);

        let stats = syncer
            .sync_checkpoint(&peer, &mut sink, evm_root, etx_root)
            .await
            .unwrap();

        assert_eq!(stats.account_nodes, 2); // branch + leaf
        assert_eq!(stats.storage_nodes, 1);
        assert_eq!(stats.etx_nodes, 1);
        assert_eq!(stats.code_blobs, 1);
        assert_eq!(sink.nodes.len(), blobs.len());
        for (h, expected) in &blobs {
            assert_eq!(sink.nodes.get(h), Some(expected));
        }
    }
}
