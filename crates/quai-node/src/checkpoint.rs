//! Replay checkpoint files: `checkpoint-<height>.qrc`.
//!
//! A checkpoint holds everything a replay needs to continue after `height`
//! except the execution state itself, which lives in `state-<height>.redb`
//! (bound here by its version id and commitment) and the redemption history
//! in `past.redb`. Layout (integers big-endian):
//!
//! ```text
//! magic "QUAI-REPLAY-CHECKPOINT-1\n"
//! body  environment, genesis hash, reference revision, height, zone hash,
//!       counters, context heads, ancestry windows (record bytes and append
//!       snapshots), prime views, orders, and for --state: version id,
//!       roots, digest, confirmation lists (with the region rollups in
//!       checkpoints without a prime section);
//!       then, in checkpoints of this version, the prime section: the prime
//!       controller state (head, exchange rates by prime number, the head's
//!       inbound ETXs, the token choice sets of the head and its parent) and
//!       the region rollups of recent region and prime blocks
//! sha256 of magic and body (32 bytes)
//! ```
//!
//! A checkpoint without the prime section (written before the prime
//! controllers were ported) is read as before; the replay rebuilds the
//! controller state from it only while its prime head is before
//! ControllerKickInBlock.
//!
//! A checkpoint of a shadow run (`quai-node run --shadow-bootstrap`, a test
//! tool) ends with one more section, flag 2: the zone block the bootstrap
//! trusted and its height. That block and its ancestry were not validated by
//! the node; every later checkpoint of the run keeps the section. A run
//! without the test flag and every replay refuse such a checkpoint, and
//! binaries older than the section refuse it as trailing bytes.
//!
//! A writer writes `<name>.tmp`, syncs it, renames it and syncs the
//! directory, so an interrupted write never replaces a complete checkpoint.
use crate::store::{fault, sync_dir};
use quai_primitives::{Hash, REFERENCE_REVISION};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const MAGIC: &[u8] = b"QUAI-REPLAY-CHECKPOINT-1\n";

fn bad(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub manifest: Option<Vec<Hash>>,
    pub interlinks: Option<[Hash; 4]>,
    /// `placement::Termini::encode()`
    pub termini: Option<Vec<u8>>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub heights: u64,
    pub records: [u64; 3],
    pub shares: u64,
    pub state_blocks: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateSection {
    pub version: Hash,
    pub evm: Hash,
    pub etx: Hash,
    pub utxo: Hash,
    pub accounts: u64,
    pub utxo_count: u64,
    pub digest: Hash,
    /// (block, confirmed ETXs, region rollup if the block was region or prime)
    pub confirmations: Vec<(Hash, Vec<u8>, Option<Vec<u8>>)>,
}
/// The prime controller state and the recent region rollups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrimeSection {
    pub head: Hash,
    pub number: u64,
    /// The prime number of `rates[0]`.
    pub first: u64,
    /// 32-byte big-endian exchange rates of the prime blocks `first..=number`.
    pub rates: Vec<Hash>,
    /// The head's inbound ETXs (`store::encode_tx_list`), when known.
    pub inbound: Option<Vec<u8>>,
    /// `choice_window::Window::encode` of the head's parent and of the head.
    pub parent_window: Option<Vec<u8>>,
    pub head_window: Vec<u8>,
    /// (block, region rollup), oldest first.
    pub rollups: Vec<(Hash, Vec<u8>)>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub environment: String,
    pub genesis: Hash,
    pub height: u64,
    pub block: Hash,
    pub counters: Counters,
    pub heads: [Option<(Hash, u64)>; 3],
    /// Per context, oldest first: record bytes and append snapshot.
    pub windows: [Vec<(Vec<u8>, Snapshot)>; 3],
    pub prime_views: Vec<Vec<u8>>,
    pub orders: Vec<(Hash, u8)>,
    pub state: Option<StateSection>,
    pub prime: Option<PrimeSection>,
    /// The zone block (hash, height) a shadow bootstrap trusted: it and its
    /// ancestry were not validated by this node. None: validated from the
    /// genesis or from a checkpoint that was.
    pub trusted: Option<(Hash, u64)>,
}

struct W(Vec<u8>);
impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v)
    }
    fn u64(&mut self, v: u64) {
        self.0.extend(v.to_be_bytes())
    }
    fn hash(&mut self, v: &Hash) {
        self.0.extend(v)
    }
    fn bytes(&mut self, v: &[u8]) {
        self.0.extend((v.len() as u64).to_be_bytes());
        self.0.extend(v)
    }
    fn opt_bytes(&mut self, v: &Option<Vec<u8>>) {
        match v {
            Some(b) => {
                self.u8(1);
                self.bytes(b)
            }
            None => self.u8(0),
        }
    }
}
struct R<'a> {
    b: &'a [u8],
    at: usize,
}
impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(n)
            .filter(|e| *e <= self.b.len())
            .ok_or_else(|| bad("truncated checkpoint"))?;
        let v = &self.b[self.at..end];
        self.at = end;
        Ok(v)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u64(&mut self) -> io::Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes(
            b.try_into().map_err(|_| bad("checkpoint integer"))?,
        ))
    }
    fn len(&mut self) -> io::Result<usize> {
        let n = self.u64()?;
        if n > (self.b.len() - self.at) as u64 {
            return Err(bad("checkpoint length beyond file"));
        }
        Ok(n as usize)
    }
    fn hash(&mut self) -> io::Result<Hash> {
        self.take(32)?
            .try_into()
            .map_err(|_| bad("checkpoint hash"))
    }
    fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let n = self.len()?;
        Ok(self.take(n)?.to_vec())
    }
    fn opt_bytes(&mut self) -> io::Result<Option<Vec<u8>>> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.bytes()?)),
            _ => Err(bad("checkpoint option flag")),
        }
    }
    fn count(&mut self, item: usize) -> io::Result<usize> {
        let n = self.u64()?;
        if n.saturating_mul(item as u64) > (self.b.len() - self.at) as u64 {
            return Err(bad("checkpoint count beyond file"));
        }
        Ok(n as usize)
    }
}

impl Checkpoint {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = W(MAGIC.to_vec());
        w.bytes(self.environment.as_bytes());
        w.hash(&self.genesis);
        w.bytes(REFERENCE_REVISION.as_bytes());
        w.u64(self.height);
        w.hash(&self.block);
        let c = &self.counters;
        for v in [
            c.heights,
            c.records[0],
            c.records[1],
            c.records[2],
            c.shares,
            c.state_blocks,
        ] {
            w.u64(v);
        }
        for head in &self.heads {
            match head {
                Some((h, n)) => {
                    w.u8(1);
                    w.hash(h);
                    w.u64(*n);
                }
                None => w.u8(0),
            }
        }
        for window in &self.windows {
            w.u64(window.len() as u64);
            for (record, s) in window {
                w.bytes(record);
                match &s.manifest {
                    Some(m) => {
                        w.u8(1);
                        w.u64(m.len() as u64);
                        for h in m {
                            w.hash(h);
                        }
                    }
                    None => w.u8(0),
                }
                match &s.interlinks {
                    Some(l) => {
                        w.u8(1);
                        for h in l {
                            w.hash(h);
                        }
                    }
                    None => w.u8(0),
                }
                w.opt_bytes(&s.termini);
            }
        }
        w.u64(self.prime_views.len() as u64);
        for v in &self.prime_views {
            w.bytes(v);
        }
        w.u64(self.orders.len() as u64);
        for (h, o) in &self.orders {
            w.hash(h);
            w.u8(*o);
        }
        match &self.state {
            Some(s) => {
                w.u8(1);
                w.hash(&s.version);
                w.hash(&s.evm);
                w.hash(&s.etx);
                w.hash(&s.utxo);
                w.u64(s.accounts);
                w.u64(s.utxo_count);
                w.hash(&s.digest);
                w.u64(s.confirmations.len() as u64);
                for (h, confirmed, rollup) in &s.confirmations {
                    w.hash(h);
                    w.bytes(confirmed);
                    w.opt_bytes(rollup);
                }
            }
            None => w.u8(0),
        }
        if let Some(p) = &self.prime {
            w.u8(1);
            w.hash(&p.head);
            w.u64(p.number);
            w.u64(p.first);
            w.u64(p.rates.len() as u64);
            for r in &p.rates {
                w.hash(r);
            }
            w.opt_bytes(&p.inbound);
            w.opt_bytes(&p.parent_window);
            w.bytes(&p.head_window);
            w.u64(p.rollups.len() as u64);
            for (h, r) in &p.rollups {
                w.hash(h);
                w.bytes(r);
            }
        }
        if let Some((hash, height)) = &self.trusted {
            w.u8(2);
            w.hash(hash);
            w.u64(*height);
        }
        let digest: Hash = Sha256::digest(&w.0).into();
        w.0.extend(digest);
        w.0
    }
    /// Decode and check the integrity hash. Content checks against the chain
    /// spec and the state are the caller's.
    pub fn decode(raw: &[u8]) -> io::Result<Self> {
        if raw.len() < MAGIC.len() + 32 || &raw[..MAGIC.len()] != MAGIC {
            return Err(bad("not a replay checkpoint"));
        }
        let (body, digest) = raw.split_at(raw.len() - 32);
        let actual: Hash = Sha256::digest(body).into();
        if actual.as_slice() != digest {
            return Err(bad("checkpoint integrity hash mismatch"));
        }
        let mut r = R {
            b: body,
            at: MAGIC.len(),
        };
        let environment =
            String::from_utf8(r.bytes()?).map_err(|_| bad("checkpoint environment"))?;
        let genesis = r.hash()?;
        if r.bytes()? != REFERENCE_REVISION.as_bytes() {
            return Err(bad("checkpoint reference revision"));
        }
        let height = r.u64()?;
        let block = r.hash()?;
        let counters = Counters {
            heights: r.u64()?,
            records: [r.u64()?, r.u64()?, r.u64()?],
            shares: r.u64()?,
            state_blocks: r.u64()?,
        };
        let mut heads = [None; 3];
        for head in &mut heads {
            *head = match r.u8()? {
                0 => None,
                1 => Some((r.hash()?, r.u64()?)),
                _ => return Err(bad("checkpoint head flag")),
            };
        }
        let mut windows: [Vec<(Vec<u8>, Snapshot)>; 3] = Default::default();
        for window in &mut windows {
            let n = r.count(8)?;
            for _ in 0..n {
                let record = r.bytes()?;
                let manifest = match r.u8()? {
                    0 => None,
                    1 => {
                        let m = r.count(32)?;
                        Some((0..m).map(|_| r.hash()).collect::<io::Result<Vec<_>>>()?)
                    }
                    _ => return Err(bad("checkpoint manifest flag")),
                };
                let interlinks = match r.u8()? {
                    0 => None,
                    1 => Some([r.hash()?, r.hash()?, r.hash()?, r.hash()?]),
                    _ => return Err(bad("checkpoint interlink flag")),
                };
                let termini = r.opt_bytes()?;
                window.push((
                    record,
                    Snapshot {
                        manifest,
                        interlinks,
                        termini,
                    },
                ));
            }
        }
        let n = r.count(8)?;
        let prime_views = (0..n).map(|_| r.bytes()).collect::<io::Result<Vec<_>>>()?;
        let n = r.count(33)?;
        let mut orders = Vec::with_capacity(n);
        for _ in 0..n {
            orders.push((r.hash()?, r.u8()?));
        }
        let state = match r.u8()? {
            0 => None,
            1 => {
                let version = r.hash()?;
                let evm = r.hash()?;
                let etx = r.hash()?;
                let utxo = r.hash()?;
                let accounts = r.u64()?;
                let utxo_count = r.u64()?;
                let digest = r.hash()?;
                let n = r.count(41)?;
                let mut confirmations = Vec::with_capacity(n);
                for _ in 0..n {
                    confirmations.push((r.hash()?, r.bytes()?, r.opt_bytes()?));
                }
                Some(StateSection {
                    version,
                    evm,
                    etx,
                    utxo,
                    accounts,
                    utxo_count,
                    digest,
                    confirmations,
                })
            }
            _ => return Err(bad("checkpoint state flag")),
        };
        let prime = if r.at == body.len() || body[r.at] == 2 {
            None
        } else {
            if r.u8()? != 1 {
                return Err(bad("checkpoint prime flag"));
            }
            let head = r.hash()?;
            let number = r.u64()?;
            let first = r.u64()?;
            let n = r.count(32)?;
            let rates = (0..n).map(|_| r.hash()).collect::<io::Result<Vec<_>>>()?;
            let inbound = r.opt_bytes()?;
            let parent_window = r.opt_bytes()?;
            let head_window = r.bytes()?;
            let n = r.count(40)?;
            let mut rollups = Vec::with_capacity(n);
            for _ in 0..n {
                rollups.push((r.hash()?, r.bytes()?));
            }
            Some(PrimeSection {
                head,
                number,
                first,
                rates,
                inbound,
                parent_window,
                head_window,
                rollups,
            })
        };
        let trusted = if r.at == body.len() {
            None
        } else {
            if r.u8()? != 2 {
                return Err(bad("checkpoint trusted-block flag"));
            }
            Some((r.hash()?, r.u64()?))
        };
        if r.at != body.len() {
            return Err(bad("checkpoint trailing bytes"));
        }
        Ok(Self {
            environment,
            genesis,
            height,
            block,
            counters,
            heads,
            windows,
            prime_views,
            orders,
            state,
            prime,
            trusted,
        })
    }
}

pub fn path(dir: &Path, height: u64) -> PathBuf {
    dir.join(format!("checkpoint-{height:010}.qrc"))
}
/// Complete checkpoints in `dir`, highest first.
pub fn list(dir: &Path) -> io::Result<Vec<(u64, PathBuf)>> {
    let mut out = vec![];
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(digits) = name
            .strip_prefix("checkpoint-")
            .and_then(|n| n.strip_suffix(".qrc"))
        else {
            continue;
        };
        if digits.len() == 10 && digits.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(height) = digits.parse() {
                out.push((height, entry.path()));
            }
        }
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.0));
    Ok(out)
}
/// Write a checkpoint atomically: temporary file, sync, rename, sync directory.
pub fn write(dir: &Path, checkpoint: &Checkpoint) -> io::Result<PathBuf> {
    let target = path(dir, checkpoint.height);
    let tmp = target.with_extension("qrc.tmp");
    let raw = checkpoint.encode();
    {
        let mut f = fs::File::create(&tmp)?;
        let half = raw.len() / 2;
        f.write_all(&raw[..half])?;
        fault(dir, "checkpoint-half-written", checkpoint.height)?;
        f.write_all(&raw[half..])?;
        f.sync_all()?;
    }
    fault(dir, "checkpoint-before-rename", checkpoint.height)?;
    fs::rename(&tmp, &target)?;
    sync_dir(dir)?;
    Ok(target)
}
pub fn read(path: &Path) -> io::Result<Checkpoint> {
    let raw = fs::read(path)?;
    Checkpoint::decode(&raw).map_err(|e| bad(format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(state: bool, prime: bool, trusted: bool) -> Checkpoint {
        Checkpoint {
            environment: "colosseum".into(),
            genesis: [1; 32],
            height: 10_000,
            block: [2; 32],
            counters: Counters {
                heights: 10_001,
                records: [3, 4, 10_001],
                shares: 5,
                state_blocks: 10_000,
            },
            heads: [Some(([3; 32], 3)), None, Some(([2; 32], 10_000))],
            windows: [
                vec![(
                    vec![1, 2, 3],
                    Snapshot {
                        manifest: Some(vec![[4; 32]]),
                        interlinks: Some([[5; 32]; 4]),
                        termini: Some(vec![9; 40]),
                    },
                )],
                vec![],
                vec![(vec![7; 100], Snapshot::default())],
            ],
            prime_views: vec![vec![8; 20]],
            orders: vec![([6; 32], 2)],
            state: state.then(|| StateSection {
                version: [7; 32],
                evm: [8; 32],
                etx: [9; 32],
                utxo: [10; 32],
                accounts: 12_612,
                utxo_count: 0,
                digest: [11; 32],
                confirmations: vec![([12; 32], vec![0, 0, 0, 0], None)],
            }),
            prime: prime.then(|| PrimeSection {
                head: [3; 32],
                number: 3,
                first: 0,
                rates: vec![[13; 32]; 4],
                inbound: Some(vec![0, 0, 0, 0]),
                parent_window: None,
                head_window: vec![14; 41],
                rollups: vec![([15; 32], vec![0, 0, 0, 0])],
            }),
            trusted: trusted.then_some(([16; 32], 9_000)),
        }
    }
    #[test]
    fn encoding_round_trips_and_any_changed_byte_is_rejected() {
        for (state, prime, trusted) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (true, true, false),
            (false, true, true),
            (false, false, true),
        ] {
            let c = sample(state, prime, trusted);
            let raw = c.encode();
            assert_eq!(Checkpoint::decode(&raw).unwrap(), c);
            for i in [0, MAGIC.len(), raw.len() / 2, raw.len() - 1] {
                let mut bad = raw.clone();
                bad[i] ^= 1;
                assert!(Checkpoint::decode(&bad).is_err(), "byte {i}");
            }
            assert!(Checkpoint::decode(&raw[..raw.len() - 1]).is_err());
        }
        // A checkpoint without the trusted-block section is one with None:
        // the section only appends bytes.
        let plain = sample(false, true, false).encode();
        let marked = sample(false, true, true).encode();
        assert_eq!(plain[..plain.len() - 32], marked[..plain.len() - 32]);
    }
}
