//! The node's block store: `<checkpoint dir>/blocks.redb` (redb), next to the
//! checkpoints and the state it belongs to.
//!
//! The chain processor writes every zone block it accepts: the block bytes,
//! its order, receipts (go-quai's storage form) and uncle classes, the
//! number-to-hash index, the transaction index, the total entropy of each
//! block (from its child's header), and with `--state` the UTXOs each block
//! creates and an index of unspent outputs by address. Blocks enter in batches;
//! a batch is one redb transaction, durable when the state commits or a
//! checkpoint is written. On a resume from a checkpoint the store is rolled
//! back to the checkpoint height, so a crash never leaves it ahead of the
//! state; it is never behind, because the processor flushes it durably before
//! each state commit and checkpoint. A rollback removes the canonical entries
//! above a height and undoes their index changes; the block bytes stay, as
//! go-quai keeps blocks that leave the canonical chain.
//!
//! Tables (integers big-endian):
//! - `meta`: `head` (number, hash), `base` (lowest number), `utxos` (1 when
//!   the UTXO tables are kept);
//! - `canonical`: number -> hash;
//! - `blocks`: hash -> entry (number, order, uncle classes, receipts, bytes);
//! - `entropy`: hash -> `TotalLogEntropy` in the zone, 32 bytes;
//! - `txs`: transaction hash -> number, index (the newest block wins, as in
//!   go-quai's `WriteTxLookupEntries`);
//! - `created`: number -> the UTXOs the block created (outpoint,
//!   denomination and address as `qi::encode_denomination_address` writes
//!   them, lock);
//! - `spent`: number -> the address-index entries the block removed;
//! - `address`: address || outpoint -> (), the unspent outputs by address;
//! - `outpoint`: outpoint -> address, for removals.
//!
//! The index address of an output is the first 20 bytes of its address, as
//! go-quai's index keys it (`common.AddressBytes(utxo.Address)`); only a Qi
//! transaction before Polaris could create an address longer than 20 bytes.
use quai_execution::qi::{OutPoint, UtxoAddress};
use quai_primitives::{transaction::Address, Hash};
use redb::{
    Database, Durability, ReadTransaction, ReadableDatabase, ReadableTable, TableDefinition,
    WriteTransaction,
};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const META: TableDefinition<&str, &[u8]> = TableDefinition::new("blocks.meta.v1");
const CANON: TableDefinition<u64, &[u8]> = TableDefinition::new("blocks.canonical.v1");
const BLOCKS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blocks.objects.v1");
const ENTROPY: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blocks.entropy.v1");
const TXS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blocks.txs.v1");
const CREATED: TableDefinition<u64, &[u8]> = TableDefinition::new("blocks.created.v1");
const SPENT: TableDefinition<u64, &[u8]> = TableDefinition::new("blocks.spent.v1");
const ADDR: TableDefinition<&[u8], ()> = TableDefinition::new("blocks.address.v1");
const OUTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blocks.outpoint.v1");
/// Prime and region views by context and hash ([ctx] ++ hash), kept after a
/// reorg as zone blocks are.
const DOM_BLOCKS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blocks.dominant.v1");
/// The canonical prime and region hashes by context and number
/// ([ctx] ++ number).
const DOM_CANON: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("blocks.dominant.canonical.v1");
/// The dominant (context, number) pairs each canonical zone height added,
/// for rollbacks.
const DOM_AT: TableDefinition<u64, &[u8]> = TableDefinition::new("blocks.dominant.at.v1");
const ENTRY_VERSION: u8 = 1;

fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}
fn store_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("block store: {e}"))
}
pub fn path(checkpoint_dir: &Path) -> PathBuf {
    checkpoint_dir.join("blocks.redb")
}

/// A UTXO a block created, as it is after the block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    pub point: OutPoint,
    pub denomination: u8,
    /// Every byte of the address; the index uses its first 20.
    pub address: UtxoAddress,
    /// Big-endian lock height as the UTXO holds it.
    pub lock: Vec<u8>,
}
/// A zone block the processor accepted, with what the store keeps of it.
#[derive(Clone, Debug)]
pub struct Accepted {
    pub number: u64,
    pub hash: Hash,
    /// The zone parent.
    pub parent: Hash,
    pub order: u8,
    /// The zone work object bytes as the source sent them.
    pub block: Vec<u8>,
    /// go-quai's `ProtoReceiptsForStorage` bytes, when known.
    pub receipts: Option<Vec<u8>>,
    pub tx_hashes: Vec<Hash>,
    /// Uncle classes in body order: 0 block, 1 workshare, 2 other.
    pub uncle_kinds: Vec<u8>,
    /// The zone parent's `TotalLogEntropy`, as this block's header records it.
    pub parent_entropy: Option<Hash>,
    /// With state: the UTXOs the block created and the outpoints it removed.
    pub utxos: Option<UtxoChanges>,
    /// The prime and region views of a coincident block (go-quai keeps a
    /// block in each context it is coincident in).
    pub dominant: Vec<DominantView>,
}
/// A prime (0) or region (1) view of a zone block: its number in that
/// context and its work object bytes as the source sent them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DominantView {
    pub ctx: u8,
    pub number: u64,
    pub block: Vec<u8>,
}
fn dom_key(ctx: u8, hash: &Hash) -> [u8; 33] {
    let mut k = [0u8; 33];
    k[0] = ctx;
    k[1..].copy_from_slice(hash);
    k
}
fn dom_number_key(ctx: u8, number: u64) -> [u8; 9] {
    let mut k = [0u8; 9];
    k[0] = ctx;
    k[1..].copy_from_slice(&number.to_be_bytes());
    k
}
#[derive(Clone, Debug, Default)]
pub struct UtxoChanges {
    pub created: Vec<Created>,
    pub removed: Vec<OutPoint>,
}
/// A stored block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub number: u64,
    pub hash: Hash,
    pub order: u8,
    pub uncle_kinds: Vec<u8>,
    pub receipts: Option<Vec<u8>>,
    pub tx_hashes: Vec<Hash>,
    pub block: Vec<u8>,
}
/// What a rollback removed from the canonical chain, newest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    pub number: u64,
    pub hash: Hash,
}

fn outpoint_key(p: &OutPoint) -> [u8; 34] {
    let mut k = [0u8; 34];
    k[..32].copy_from_slice(&p.hash);
    k[32..].copy_from_slice(&p.index.to_be_bytes());
    k
}
fn outpoint_of(k: &[u8]) -> io::Result<OutPoint> {
    if k.len() != 34 {
        return Err(bad("outpoint key width"));
    }
    Ok(OutPoint {
        hash: k[..32].try_into().map_err(bad)?,
        index: u16::from_be_bytes([k[32], k[33]]),
    })
}
fn address_key(a: &Address, p: &OutPoint) -> [u8; 54] {
    let mut k = [0u8; 54];
    k[..20].copy_from_slice(a);
    k[20..].copy_from_slice(&outpoint_key(p));
    k
}
fn encode_entry(a: &Accepted) -> Vec<u8> {
    let mut out = Vec::with_capacity(a.block.len() + 64);
    out.push(ENTRY_VERSION);
    out.extend(a.number.to_be_bytes());
    out.push(a.order);
    out.push(u8::from(a.receipts.is_some()));
    out.extend((a.uncle_kinds.len() as u32).to_be_bytes());
    out.extend(&a.uncle_kinds);
    let receipts = a.receipts.as_deref().unwrap_or_default();
    out.extend((receipts.len() as u32).to_be_bytes());
    out.extend(receipts);
    out.extend((a.tx_hashes.len() as u32).to_be_bytes());
    for t in &a.tx_hashes {
        out.extend(t);
    }
    out.extend((a.block.len() as u32).to_be_bytes());
    out.extend(&a.block);
    out
}
struct Reader<'a> {
    b: &'a [u8],
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.b.len() < n {
            return Err(bad("truncated block store entry"));
        }
        let (a, rest) = self.b.split_at(n);
        self.b = rest;
        Ok(a)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> io::Result<usize> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().map_err(bad)?) as usize)
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().map_err(bad)?))
    }
    fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let n = self.u32()?;
        Ok(self.take(n)?.to_vec())
    }
}
fn decode_entry(hash: Hash, raw: &[u8]) -> io::Result<Stored> {
    let mut r = Reader { b: raw };
    if r.u8()? != ENTRY_VERSION {
        return Err(bad("block store entry version"));
    }
    let number = r.u64()?;
    let order = r.u8()?;
    let has_receipts = r.u8()? == 1;
    let uncle_kinds = r.bytes()?;
    let receipts = r.bytes()?;
    let n = r.u32()?;
    let tx_hashes = (0..n)
        .map(|_| hash_of(r.take(32)?))
        .collect::<io::Result<Vec<_>>>()?;
    let block = r.bytes()?;
    if !r.b.is_empty() {
        return Err(bad("block store entry trailing bytes"));
    }
    Ok(Stored {
        number,
        hash,
        order,
        uncle_kinds,
        receipts: has_receipts.then_some(receipts),
        tx_hashes,
        block,
    })
}
fn encode_created(list: &[Created]) -> Vec<u8> {
    let mut out = (list.len() as u32).to_be_bytes().to_vec();
    for c in list {
        out.extend(outpoint_key(&c.point));
        quai_execution::qi::encode_denomination_address(c.denomination, &c.address, &mut out);
        out.push(c.lock.len() as u8);
        out.extend(&c.lock);
    }
    out
}
fn decode_created(raw: &[u8]) -> io::Result<Vec<Created>> {
    let mut r = Reader { b: raw };
    let n = r.u32()?;
    let mut out = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        let point = outpoint_of(r.take(34)?)?;
        let (denomination, address, rest) =
            quai_execution::qi::decode_denomination_address(r.b).map_err(bad)?;
        r.b = rest;
        let len = r.u8()? as usize;
        let lock = r.take(len)?.to_vec();
        out.push(Created {
            point,
            denomination,
            address,
            lock,
        });
    }
    if !r.b.is_empty() {
        return Err(bad("created list trailing bytes"));
    }
    Ok(out)
}
fn encode_spent(list: &[(OutPoint, Address)]) -> Vec<u8> {
    let mut out = (list.len() as u32).to_be_bytes().to_vec();
    for (p, a) in list {
        out.extend(outpoint_key(p));
        out.extend(a);
    }
    out
}
fn decode_spent(raw: &[u8]) -> io::Result<Vec<(OutPoint, Address)>> {
    let mut r = Reader { b: raw };
    let n = r.u32()?;
    let mut out = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        let p = outpoint_of(r.take(34)?)?;
        let a: Address = r.take(20)?.try_into().map_err(bad)?;
        out.push((p, a));
    }
    if !r.b.is_empty() {
        return Err(bad("spent list trailing bytes"));
    }
    Ok(out)
}
fn head_value(number: u64, hash: &Hash) -> Vec<u8> {
    let mut v = number.to_be_bytes().to_vec();
    v.extend(hash);
    v
}
fn parse_head(v: &[u8]) -> io::Result<(u64, Hash)> {
    if v.len() != 40 {
        return Err(bad("block store head width"));
    }
    Ok((
        u64::from_be_bytes(v[..8].try_into().map_err(bad)?),
        v[8..].try_into().map_err(bad)?,
    ))
}
fn hash_of(v: &[u8]) -> io::Result<Hash> {
    v.try_into().map_err(|_| bad("block store hash width"))
}

/// The writer. One per store; readers take [`StoreReader`] snapshots.
pub struct BlockStore {
    path: PathBuf,
    db: Arc<Database>,
    pending: Vec<Accepted>,
    head: Option<(u64, Hash)>,
    base: Option<u64>,
    /// The UTXO tables are kept (state mode).
    utxos: bool,
}
impl BlockStore {
    /// Open or create the store. `utxos`: keep the UTXO tables (state mode);
    /// a store keeps the mode it was created with.
    pub fn open(path: &Path, utxos: bool) -> io::Result<Self> {
        let db = Database::create(path).map_err(store_err)?;
        let mut w = db.begin_write().map_err(store_err)?;
        w.set_durability(Durability::Immediate).map_err(store_err)?;
        let (head, base, kept) = {
            let mut meta = w.open_table(META).map_err(store_err)?;
            for t in [CANON, CREATED, SPENT] {
                w.open_table(t).map_err(store_err)?;
            }
            for t in [BLOCKS, ENTROPY, TXS, OUTS, DOM_BLOCKS, DOM_CANON] {
                w.open_table(t).map_err(store_err)?;
            }
            w.open_table(DOM_AT).map_err(store_err)?;
            w.open_table(ADDR).map_err(store_err)?;
            let head = meta
                .get("head")
                .map_err(store_err)?
                .map(|v| parse_head(v.value()))
                .transpose()?;
            let base = meta
                .get("base")
                .map_err(store_err)?
                .map(|v| -> io::Result<u64> {
                    Ok(u64::from_be_bytes(v.value().try_into().map_err(bad)?))
                })
                .transpose()?;
            let existing = meta
                .get("utxos")
                .map_err(store_err)?
                .map(|v| v.value() == [1]);
            let kept = match existing {
                Some(v) => v,
                None => {
                    meta.insert("utxos", [u8::from(utxos)].as_slice())
                        .map_err(store_err)?;
                    utxos
                }
            };
            (head, base, kept)
        };
        w.commit().map_err(store_err)?;
        if kept != utxos {
            return Err(bad(format!(
                "{}: the store was created {} the UTXO tables; it belongs to the other state mode",
                path.display(),
                if kept { "with" } else { "without" }
            )));
        }
        Ok(Self {
            path: path.to_path_buf(),
            db: Arc::new(db),
            pending: vec![],
            head,
            base,
            utxos,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// The newest block, staged blocks included.
    pub fn head(&self) -> Option<(u64, Hash)> {
        self.head
    }
    /// The lowest stored number.
    pub fn base(&self) -> Option<u64> {
        self.base
    }
    pub fn keeps_utxos(&self) -> bool {
        self.utxos
    }
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
    /// Add the next canonical block. It must extend the head.
    pub fn stage(&mut self, a: Accepted) -> io::Result<()> {
        if let Some((n, h)) = self.head {
            if a.number != n + 1 || a.parent != h {
                return Err(bad(format!(
                    "block store: block {} at {} does not extend the head {} at {n}",
                    quai_primitives::encode_hex(&a.hash),
                    a.number,
                    quai_primitives::encode_hex(&h)
                )));
            }
        }
        if self.base.is_none() {
            self.base = Some(a.number);
        }
        self.head = Some((a.number, a.hash));
        self.pending.push(a);
        Ok(())
    }
    /// Write the staged blocks in one transaction; `durable` waits for the disk.
    pub fn flush(&mut self, durable: bool) -> io::Result<()> {
        if self.pending.is_empty() && !durable {
            return Ok(());
        }
        let mut w = self.db.begin_write().map_err(store_err)?;
        w.set_durability(if durable {
            Durability::Immediate
        } else {
            Durability::None
        })
        .map_err(store_err)?;
        for a in std::mem::take(&mut self.pending) {
            self.write_block(&w, &a)?;
        }
        {
            let mut meta = w.open_table(META).map_err(store_err)?;
            if let Some((n, h)) = self.head {
                meta.insert("head", head_value(n, &h).as_slice())
                    .map_err(store_err)?;
            }
            if let Some(b) = self.base {
                meta.insert("base", b.to_be_bytes().as_slice())
                    .map_err(store_err)?;
            }
        }
        w.commit().map_err(store_err)
    }
    fn write_block(&self, w: &WriteTransaction, a: &Accepted) -> io::Result<()> {
        w.open_table(BLOCKS)
            .map_err(store_err)?
            .insert(a.hash.as_slice(), encode_entry(a).as_slice())
            .map_err(store_err)?;
        w.open_table(CANON)
            .map_err(store_err)?
            .insert(a.number, a.hash.as_slice())
            .map_err(store_err)?;
        if !a.dominant.is_empty() {
            let mut blocks = w.open_table(DOM_BLOCKS).map_err(store_err)?;
            let mut canon = w.open_table(DOM_CANON).map_err(store_err)?;
            let mut at = vec![];
            for d in &a.dominant {
                blocks
                    .insert(dom_key(d.ctx, &a.hash).as_slice(), d.block.as_slice())
                    .map_err(store_err)?;
                canon
                    .insert(
                        dom_number_key(d.ctx, d.number).as_slice(),
                        a.hash.as_slice(),
                    )
                    .map_err(store_err)?;
                at.extend(dom_number_key(d.ctx, d.number));
            }
            w.open_table(DOM_AT)
                .map_err(store_err)?
                .insert(a.number, at.as_slice())
                .map_err(store_err)?;
        }
        if let Some(e) = &a.parent_entropy {
            w.open_table(ENTROPY)
                .map_err(store_err)?
                .insert(a.parent.as_slice(), e.as_slice())
                .map_err(store_err)?;
        }
        {
            let mut txs = w.open_table(TXS).map_err(store_err)?;
            for (i, t) in a.tx_hashes.iter().enumerate() {
                let mut v = a.number.to_be_bytes().to_vec();
                v.extend((i as u32).to_be_bytes());
                txs.insert(t.as_slice(), v.as_slice()).map_err(store_err)?;
            }
        }
        if let (true, Some(u)) = (self.utxos, &a.utxos) {
            let mut addr = w.open_table(ADDR).map_err(store_err)?;
            let mut outs = w.open_table(OUTS).map_err(store_err)?;
            let mut spent = vec![];
            for p in &u.removed {
                let key = outpoint_key(p);
                let owner = outs
                    .remove(key.as_slice())
                    .map_err(store_err)?
                    .map(|v| v.value().to_vec());
                if let Some(owner) = owner {
                    let a: Address = owner.as_slice().try_into().map_err(bad)?;
                    addr.remove(address_key(&a, p).as_slice())
                        .map_err(store_err)?;
                    spent.push((*p, a));
                }
            }
            for c in &u.created {
                addr.insert(address_key(&c.address.first20(), &c.point).as_slice(), ())
                    .map_err(store_err)?;
                outs.insert(
                    outpoint_key(&c.point).as_slice(),
                    c.address.first20().as_slice(),
                )
                .map_err(store_err)?;
            }
            w.open_table(CREATED)
                .map_err(store_err)?
                .insert(a.number, encode_created(&u.created).as_slice())
                .map_err(store_err)?;
            w.open_table(SPENT)
                .map_err(store_err)?
                .insert(a.number, encode_spent(&spent).as_slice())
                .map_err(store_err)?;
        }
        Ok(())
    }
    /// Remove the canonical blocks above `height` and undo their index
    /// changes, durably. Returns the removed blocks, newest first.
    pub fn rollback_to(&mut self, height: u64) -> io::Result<Vec<Removed>> {
        self.flush(false)?;
        let Some((top, _)) = self.head else {
            return Ok(vec![]);
        };
        if top <= height {
            return Ok(vec![]);
        }
        let mut w = self.db.begin_write().map_err(store_err)?;
        w.set_durability(Durability::Immediate).map_err(store_err)?;
        let mut removed = vec![];
        {
            let mut canon = w.open_table(CANON).map_err(store_err)?;
            let blocks = w.open_table(BLOCKS).map_err(store_err)?;
            let mut txs = w.open_table(TXS).map_err(store_err)?;
            let mut created = w.open_table(CREATED).map_err(store_err)?;
            let mut spent = w.open_table(SPENT).map_err(store_err)?;
            let mut addr = w.open_table(ADDR).map_err(store_err)?;
            let mut outs = w.open_table(OUTS).map_err(store_err)?;
            let mut dom_canon = w.open_table(DOM_CANON).map_err(store_err)?;
            let mut dom_at = w.open_table(DOM_AT).map_err(store_err)?;
            let mut n = top;
            while n > height {
                let Some(hash) = canon
                    .remove(n)
                    .map_err(store_err)?
                    .map(|v| v.value().to_vec())
                else {
                    n -= 1;
                    continue;
                };
                let hash = hash_of(&hash)?;
                // The prime and region numbers this block held leave the
                // canonical chains with it.
                if let Some(raw) = dom_at
                    .remove(n)
                    .map_err(store_err)?
                    .map(|v| v.value().to_vec())
                {
                    for key in raw.chunks_exact(9) {
                        let held = dom_canon
                            .get(key)
                            .map_err(store_err)?
                            .is_some_and(|v| v.value() == hash.as_slice());
                        if held {
                            dom_canon.remove(key).map_err(store_err)?;
                        }
                    }
                }
                if let Some(raw) = blocks.get(hash.as_slice()).map_err(store_err)? {
                    let entry = decode_entry(hash, raw.value())?;
                    for id in entry.tx_hashes {
                        let points_here = txs
                            .get(id.as_slice())
                            .map_err(store_err)?
                            .is_some_and(|v| v.value().get(..8) == Some(&n.to_be_bytes()[..]));
                        if points_here {
                            txs.remove(id.as_slice()).map_err(store_err)?;
                        }
                    }
                }
                if let Some(raw) = created
                    .remove(n)
                    .map_err(store_err)?
                    .map(|v| v.value().to_vec())
                {
                    for c in decode_created(&raw)? {
                        addr.remove(address_key(&c.address.first20(), &c.point).as_slice())
                            .map_err(store_err)?;
                        outs.remove(outpoint_key(&c.point).as_slice())
                            .map_err(store_err)?;
                    }
                }
                if let Some(raw) = spent
                    .remove(n)
                    .map_err(store_err)?
                    .map(|v| v.value().to_vec())
                {
                    for (p, a) in decode_spent(&raw)? {
                        addr.insert(address_key(&a, &p).as_slice(), ())
                            .map_err(store_err)?;
                        outs.insert(outpoint_key(&p).as_slice(), a.as_slice())
                            .map_err(store_err)?;
                    }
                }
                removed.push(Removed { number: n, hash });
                n -= 1;
            }
            let new_head = canon
                .get(height)
                .map_err(store_err)?
                .map(|v| hash_of(v.value()))
                .transpose()?;
            let mut meta = w.open_table(META).map_err(store_err)?;
            match new_head {
                Some(h) => {
                    meta.insert("head", head_value(height, &h).as_slice())
                        .map_err(store_err)?;
                    self.head = Some((height, h));
                }
                None => {
                    meta.remove("head").map_err(store_err)?;
                    meta.remove("base").map_err(store_err)?;
                    self.head = None;
                    self.base = None;
                }
            }
        }
        w.commit().map_err(store_err)?;
        Ok(removed)
    }
    /// Index unspent outputs that exist before the store's first block (a
    /// store that starts at a checkpoint), durably.
    pub fn seed_utxos(&mut self, list: &[Created]) -> io::Result<()> {
        if !self.utxos {
            return Ok(());
        }
        let mut w = self.db.begin_write().map_err(store_err)?;
        w.set_durability(Durability::Immediate).map_err(store_err)?;
        {
            let mut addr = w.open_table(ADDR).map_err(store_err)?;
            let mut outs = w.open_table(OUTS).map_err(store_err)?;
            for c in list {
                addr.insert(address_key(&c.address.first20(), &c.point).as_slice(), ())
                    .map_err(store_err)?;
                outs.insert(
                    outpoint_key(&c.point).as_slice(),
                    c.address.first20().as_slice(),
                )
                .map_err(store_err)?;
            }
        }
        w.commit().map_err(store_err)
    }
    /// A read snapshot of everything flushed so far.
    pub fn reader(&self) -> io::Result<StoreReader> {
        StoreReader::new(self.db.clone())
    }
    pub fn database(&self) -> Arc<Database> {
        self.db.clone()
    }
}

/// A consistent read snapshot of the store.
pub struct StoreReader {
    txn: ReadTransaction,
    /// The database stays open while a snapshot of it is in use.
    _db: Arc<Database>,
    head: Option<(u64, Hash)>,
    base: Option<u64>,
    utxos: bool,
}
impl StoreReader {
    pub fn new(database: Arc<Database>) -> io::Result<Self> {
        let txn = database.begin_read().map_err(store_err)?;
        let (head, base, utxos) = {
            let meta = txn.open_table(META).map_err(store_err)?;
            let head = meta
                .get("head")
                .map_err(store_err)?
                .map(|v| parse_head(v.value()))
                .transpose()?;
            let base = meta
                .get("base")
                .map_err(store_err)?
                .map(|v| -> io::Result<u64> {
                    Ok(u64::from_be_bytes(v.value().try_into().map_err(bad)?))
                })
                .transpose()?;
            let utxos = meta
                .get("utxos")
                .map_err(store_err)?
                .is_some_and(|v| v.value() == [1]);
            (head, base, utxos)
        };
        Ok(Self {
            txn,
            _db: database,
            head,
            base,
            utxos,
        })
    }
    pub fn head(&self) -> Option<(u64, Hash)> {
        self.head
    }
    pub fn base(&self) -> Option<u64> {
        self.base
    }
    pub fn keeps_utxos(&self) -> bool {
        self.utxos
    }
    /// The canonical hash at `number`.
    pub fn hash_at(&self, number: u64) -> io::Result<Option<Hash>> {
        let t = self.txn.open_table(CANON).map_err(store_err)?;
        t.get(number)
            .map_err(store_err)?
            .map(|v| hash_of(v.value()))
            .transpose()
    }
    /// The canonical hash at `number` of the prime (0) or region (1) chain.
    pub fn dominant_hash_at(&self, ctx: u8, number: u64) -> io::Result<Option<Hash>> {
        let t = match self.txn.open_table(DOM_CANON) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(store_err(e)),
        };
        t.get(dom_number_key(ctx, number).as_slice())
            .map_err(store_err)?
            .map(|v| hash_of(v.value()))
            .transpose()
    }
    /// The work object bytes of a prime (0) or region (1) view, canonical or
    /// not.
    pub fn dominant(&self, ctx: u8, hash: &Hash) -> io::Result<Option<Vec<u8>>> {
        let t = match self.txn.open_table(DOM_BLOCKS) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(store_err(e)),
        };
        Ok(t.get(dom_key(ctx, hash).as_slice())
            .map_err(store_err)?
            .map(|v| v.value().to_vec()))
    }
    /// A stored block, canonical or not.
    pub fn block(&self, hash: &Hash) -> io::Result<Option<Stored>> {
        let t = self.txn.open_table(BLOCKS).map_err(store_err)?;
        t.get(hash.as_slice())
            .map_err(store_err)?
            .map(|v| decode_entry(*hash, v.value()))
            .transpose()
    }
    /// The canonical number of a hash, if the block is canonical.
    pub fn canonical_number(&self, hash: &Hash) -> io::Result<Option<u64>> {
        let Some(b) = self.block(hash)? else {
            return Ok(None);
        };
        Ok((self.hash_at(b.number)? == Some(*hash)).then_some(b.number))
    }
    pub fn entropy(&self, hash: &Hash) -> io::Result<Option<Hash>> {
        let t = self.txn.open_table(ENTROPY).map_err(store_err)?;
        t.get(hash.as_slice())
            .map_err(store_err)?
            .map(|v| hash_of(v.value()))
            .transpose()
    }
    /// (number, index) of a transaction in the canonical chain.
    pub fn transaction(&self, hash: &Hash) -> io::Result<Option<(u64, u32)>> {
        let t = self.txn.open_table(TXS).map_err(store_err)?;
        let Some(v) = t.get(hash.as_slice()).map_err(store_err)? else {
            return Ok(None);
        };
        let v = v.value();
        if v.len() != 12 {
            return Err(bad("transaction index width"));
        }
        Ok(Some((
            u64::from_be_bytes(v[..8].try_into().map_err(bad)?),
            u32::from_be_bytes(v[8..].try_into().map_err(bad)?),
        )))
    }
    /// The UTXOs block `number` created (state mode).
    pub fn created(&self, number: u64) -> io::Result<Option<Vec<Created>>> {
        let t = self.txn.open_table(CREATED).map_err(store_err)?;
        t.get(number)
            .map_err(store_err)?
            .map(|v| decode_created(v.value()))
            .transpose()
    }
    /// The address-index entries block `number` removed (state mode), in
    /// the order the processor reported them: trimmed outputs, then the
    /// inputs of its Qi transactions.
    pub fn spent(&self, number: u64) -> io::Result<Option<Vec<(OutPoint, Address)>>> {
        let t = self.txn.open_table(SPENT).map_err(store_err)?;
        t.get(number)
            .map_err(store_err)?
            .map(|v| decode_spent(v.value()))
            .transpose()
    }
    /// The indexed unspent outputs of an address (state mode).
    pub fn outpoints(&self, address: &Address) -> io::Result<Vec<OutPoint>> {
        let t = self.txn.open_table(ADDR).map_err(store_err)?;
        let mut start = [0u8; 54];
        start[..20].copy_from_slice(address);
        let mut end = [0xffu8; 54];
        end[..20].copy_from_slice(address);
        let mut out = vec![];
        for item in t
            .range(start.as_slice()..=end.as_slice())
            .map_err(store_err)?
        {
            let (k, _) = item.map_err(store_err)?;
            out.push(outpoint_of(&k.value()[20..])?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "quai-node-blockstore-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("blocks.redb")
    }
    fn block(n: u64, parent: Hash, txs: Vec<Hash>, utxos: Option<UtxoChanges>) -> Accepted {
        Accepted {
            dominant: vec![],
            number: n,
            hash: [n as u8 + 1; 32],
            parent,
            order: 2,
            block: vec![],
            receipts: None,
            tx_hashes: txs,
            uncle_kinds: vec![1, 2],
            parent_entropy: Some([n as u8; 32]),
            utxos,
        }
    }
    fn point(i: u8) -> OutPoint {
        OutPoint {
            hash: [i; 32],
            index: i as u16,
        }
    }
    fn created(i: u8, owner: u8) -> Created {
        Created {
            point: point(i),
            denomination: 3,
            address: [owner; 20].into(),
            lock: vec![],
        }
    }
    /// A 20-byte address keeps the record of the stores written before
    /// longer addresses, byte for byte; a longer one round-trips, and the
    /// index keys it by its first 20 bytes, as go-quai's index does.
    #[test]
    fn created_records_keep_their_layout_and_long_addresses() {
        let mut c = created(1, 7);
        c.lock = vec![1, 2];
        let mut old = 1u32.to_be_bytes().to_vec();
        old.extend(outpoint_key(&c.point));
        old.push(3);
        old.extend([7; 20]);
        old.extend([2, 1, 2]);
        assert_eq!(encode_created(std::slice::from_ref(&c)), old);
        assert_eq!(decode_created(&old).unwrap(), vec![c]);
        let mut long = [7u8; 32];
        long[20..].copy_from_slice(&[9; 12]);
        let l = Created {
            point: point(2),
            denomination: 14,
            address: UtxoAddress::new(&long).unwrap(),
            lock: vec![],
        };
        let raw = encode_created(std::slice::from_ref(&l));
        assert_eq!(raw[4 + 34], 14 | 0x80);
        assert_eq!(decode_created(&raw).unwrap(), vec![l.clone()]);
        let path = tmp("long");
        let mut s = BlockStore::open(&path, true).unwrap();
        let b0 = block(0, [0; 32], vec![], None);
        s.stage(b0.clone()).unwrap();
        let b1 = block(
            1,
            b0.hash,
            vec![[9; 32]],
            Some(UtxoChanges {
                created: vec![l.clone()],
                removed: vec![],
            }),
        );
        s.stage(b1.clone()).unwrap();
        let b2 = block(
            2,
            b1.hash,
            vec![],
            Some(UtxoChanges {
                created: vec![],
                removed: vec![point(2)],
            }),
        );
        s.stage(b2).unwrap();
        s.flush(true).unwrap();
        let r = s.reader().unwrap();
        assert_eq!(r.created(1).unwrap(), Some(vec![l.clone()]));
        assert!(r.outpoints(&[7; 20]).unwrap().is_empty());
        assert_eq!(r.spent(2).unwrap(), Some(vec![(point(2), [7; 20])]));
        drop(r);
        s.rollback_to(1).unwrap();
        let r = s.reader().unwrap();
        assert_eq!(r.outpoints(&[7; 20]).unwrap(), vec![point(2)]);
        assert!(r.outpoints(&l.address.last20()).unwrap().is_empty());
    }
    #[test]
    fn indexes_follow_appends_and_rollbacks() {
        let path = tmp("index");
        let mut s = BlockStore::open(&path, true).unwrap();
        let b0 = block(0, [0; 32], vec![], None);
        s.stage(b0.clone()).unwrap();
        let b1 = block(
            1,
            b0.hash,
            vec![[9; 32]],
            Some(UtxoChanges {
                created: vec![created(1, 7), created(2, 7)],
                removed: vec![],
            }),
        );
        s.stage(b1.clone()).unwrap();
        // A block that does not extend the head is refused.
        assert!(s.stage(block(5, [3; 32], vec![], None)).is_err());
        s.flush(true).unwrap();
        let b2 = block(
            2,
            b1.hash,
            vec![[9; 32], [8; 32]],
            Some(UtxoChanges {
                created: vec![created(3, 8)],
                removed: vec![point(1)],
            }),
        );
        s.stage(b2.clone()).unwrap();
        s.flush(false).unwrap();
        let r = s.reader().unwrap();
        assert_eq!(r.head(), Some((2, b2.hash)));
        assert_eq!(r.base(), Some(0));
        assert_eq!(r.hash_at(1).unwrap(), Some(b1.hash));
        assert_eq!(r.transaction(&[9; 32]).unwrap(), Some((2, 0)));
        assert_eq!(r.outpoints(&[7; 20]).unwrap(), vec![point(2)]);
        assert_eq!(r.outpoints(&[8; 20]).unwrap(), vec![point(3)]);
        assert_eq!(r.spent(2).unwrap(), Some(vec![(point(1), [7; 20])]));
        assert_eq!(r.spent(1).unwrap(), Some(vec![]));
        assert_eq!(r.entropy(&b1.hash).unwrap(), Some([2; 32]));
        assert_eq!(r.block(&b1.hash).unwrap().unwrap().uncle_kinds, vec![1, 2]);
        assert_eq!(r.canonical_number(&b2.hash).unwrap(), Some(2));
        drop(r);
        // Roll back block 2: its index changes are undone.
        let removed = s.rollback_to(1).unwrap();
        assert_eq!(
            removed,
            vec![Removed {
                number: 2,
                hash: b2.hash
            }]
        );
        let r = s.reader().unwrap();
        assert_eq!(r.head(), Some((1, b1.hash)));
        assert_eq!(r.hash_at(2).unwrap(), None);
        // The lookup entry pointed at block 2 and is deleted, as in go-quai.
        assert_eq!(r.transaction(&[9; 32]).unwrap(), None);
        assert_eq!(r.transaction(&[8; 32]).unwrap(), None);
        let mut seven = r.outpoints(&[7; 20]).unwrap();
        seven.sort_by_key(|p| p.index);
        assert_eq!(seven, vec![point(1), point(2)]);
        assert!(r.outpoints(&[8; 20]).unwrap().is_empty());
        // The block bytes stay, no longer canonical.
        assert!(r.block(&b2.hash).unwrap().is_some());
        assert_eq!(r.canonical_number(&b2.hash).unwrap(), None);
        drop(r);
        drop(s);
        // Reopened: the head and base survive; the mode must match.
        assert!(BlockStore::open(&path, false).is_err());
        let s = BlockStore::open(&path, true).unwrap();
        assert_eq!(s.head(), Some((1, b1.hash)));
        assert_eq!(s.base(), Some(0));
    }

    /// Prime and region views follow the zone chain: by number while their
    /// zone block is canonical, by hash after a reorg removed it (as zone
    /// blocks), and a number another branch reuses points to that branch.
    #[test]
    fn dominant_views_follow_appends_and_rollbacks() {
        let path = tmp("dominant");
        let mut s = BlockStore::open(&path, false).unwrap();
        let view = |ctx: u8, number: u64, tag: u8| DominantView {
            ctx,
            number,
            block: vec![tag; 3],
        };
        let b0 = block(0, [0; 32], vec![], None);
        s.stage(b0.clone()).unwrap();
        // Height 1 is a prime block (prime 1, region 1), height 2 a zone
        // block, height 3 a region block (region 2).
        let mut b1 = block(1, b0.hash, vec![], None);
        b1.dominant = vec![view(0, 1, 0xa1), view(1, 1, 0xb1)];
        let b2 = block(2, b1.hash, vec![], None);
        let mut b3 = block(3, b2.hash, vec![], None);
        b3.dominant = vec![view(1, 2, 0xb2)];
        for b in [&b1, &b2, &b3] {
            s.stage(b.clone()).unwrap();
        }
        s.flush(true).unwrap();
        let r = s.reader().unwrap();
        assert_eq!(r.dominant_hash_at(0, 1).unwrap(), Some(b1.hash));
        assert_eq!(r.dominant_hash_at(1, 1).unwrap(), Some(b1.hash));
        assert_eq!(r.dominant_hash_at(1, 2).unwrap(), Some(b3.hash));
        assert_eq!(r.dominant_hash_at(0, 2).unwrap(), None);
        assert_eq!(r.dominant(0, &b1.hash).unwrap(), Some(vec![0xa1; 3]));
        assert_eq!(r.dominant(1, &b3.hash).unwrap(), Some(vec![0xb2; 3]));
        assert_eq!(r.dominant(0, &b3.hash).unwrap(), None);
        drop(r);
        // A reorg to height 2 takes region 2 out of the canonical chain;
        // its bytes stay readable by hash.
        s.rollback_to(2).unwrap();
        let r = s.reader().unwrap();
        assert_eq!(r.dominant_hash_at(1, 2).unwrap(), None);
        assert_eq!(r.dominant_hash_at(1, 1).unwrap(), Some(b1.hash));
        assert_eq!(r.dominant(1, &b3.hash).unwrap(), Some(vec![0xb2; 3]));
        drop(r);
        // The other branch: its height 3 is region 2 as well.
        let mut c3 = block(3, b2.hash, vec![], None);
        c3.hash = [0xc3; 32];
        c3.dominant = vec![view(1, 2, 0xc2)];
        s.stage(c3.clone()).unwrap();
        s.flush(true).unwrap();
        // A rollback below both removes the prime number too.
        let r = s.reader().unwrap();
        assert_eq!(r.dominant_hash_at(1, 2).unwrap(), Some(c3.hash));
        drop(r);
        s.rollback_to(0).unwrap();
        let r = s.reader().unwrap();
        for (ctx, n) in [(0, 1), (1, 1), (1, 2)] {
            assert_eq!(r.dominant_hash_at(ctx, n).unwrap(), None, "{ctx} {n}");
        }
        assert_eq!(r.dominant(0, &b1.hash).unwrap(), Some(vec![0xa1; 3]));
        drop(r);
        drop(s);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
