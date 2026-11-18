//! Branch-bound prime token choices. This component accepts trusted inputs;
//! it cannot admit a checkpoint or confer canonical consensus authority.
use crate::{controllers, economics, history::Record, rules::Uint, share_rules, Error};
use crypto_bigint::{Encoding, U512};
use quai_primitives::spec::FORKS;
use quai_primitives::{
    transaction::{Kind, Transaction},
    Hash,
};
use quai_store::state_db::{Snapshot, Workspace};
use sha2::{Digest, Sha256};
use std::{io, sync::Arc};
const N: usize = controllers::WINDOW as usize;
const CHUNK: usize = 64;
const CHUNKS: usize = N.div_ceil(CHUNK);
const TREE: &[u8] = b"prime-token-choices-v1";
const MAGIC: &[u8] = b"QUAI-TOKEN-CHOICES-1\n";
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Choice {
    pub quai: u64,
    pub qi: u64,
    pub difficulty: Uint,
}
fn narrow(n: U512) -> Result<Uint, Error> {
    if n.bits_vartime() > 256 {
        return Err(Error::Unsupported("token choice magnitude"));
    }
    Ok(n.resize())
}
fn low64(n: Uint) -> u64 {
    u64::from_be_bytes(n.to_be_bytes()[24..].try_into().unwrap())
}
/// Normalize the actual ETXs using parent miner difficulty and reference uint64
/// wrap semantics. Signed corrections retain Go's two Euclidean divisions.
/// Negative final difficulty is outside this explicitly bounded component.
pub fn derive(
    parent: &Record,
    exchange: Uint,
    etxs: &[Transaction],
    actual: Uint,
    realized: Uint,
    miner: Uint,
) -> Result<Choice, Error> {
    if etxs.len() > 8192 {
        return Err(Error::ResourceLimit("token choice ETXs"));
    }
    let body = parent.object().header().wire();
    let quote = economics::reward_quote_at_difficulty(
        parent.header(),
        exchange,
        share_rules::uint(
            body.avg_tx_fees
                .as_deref()
                .ok_or(Error::Invalid("parent average fees"))?,
        )?,
        share_rules::uint(
            body.miner_difficulty
                .as_deref()
                .ok_or(Error::Invalid("parent miner difficulty"))?,
        )?,
    )?;
    let mut choice = Choice {
        quai: 0,
        qi: 0,
        difficulty: miner,
    };
    for tx in etxs {
        let Kind::External(e) = tx.kind() else {
            return Err(Error::Invalid("token choice non-ETX"));
        };
        let qi = e.to[1] & 128 != 0;
        let count = if e.etx_type == 1 {
            1
        } else if e.etx_type == 2 {
            let value = share_rules::uint(&e.value)?;
            low64(value.wrapping_div(&if qi { quote.qi } else { quote.conversion_quai }))
        } else {
            0
        };
        if qi {
            choice.qi = choice.qi.wrapping_add(count)
        } else {
            choice.quai = choice.quai.wrapping_add(count)
        }
    }
    if actual != Uint::ZERO && realized != Uint::ZERO {
        let negative = realized > actual;
        let delta = if negative {
            realized.wrapping_sub(&actual)
        } else {
            actual.wrapping_sub(&realized)
        };
        let product = delta.resize::<8>().wrapping_mul(&miner.resize::<8>());
        let mut correction = product.wrapping_div(&actual.resize());
        if negative && product.wrapping_rem(&actual.resize()) != U512::ZERO {
            correction = correction.wrapping_add(&U512::ONE)
        }
        let before = correction;
        correction = correction.wrapping_div(&U512::from_u64(5));
        if negative && before.wrapping_rem(&U512::from_u64(5)) != U512::ZERO {
            correction = correction.wrapping_add(&U512::ONE)
        }
        let subtract = (choice.qi > choice.quai) == negative;
        let wide = miner.resize::<8>();
        if subtract && correction > wide {
            return Err(Error::Unsupported("negative token choice difficulty"));
        }
        choice.difficulty = narrow(if subtract {
            wide.wrapping_sub(&correction)
        } else {
            wide.wrapping_add(&correction)
        })?;
    }
    if choice.difficulty == Uint::ZERO {
        choice.difficulty = Uint::ONE
    }
    Ok(choice)
}
impl Choice {
    pub(crate) fn encode(self) -> [u8; 48] {
        let mut out = [0; 48];
        out[..8].copy_from_slice(&self.quai.to_be_bytes());
        out[8..16].copy_from_slice(&self.qi.to_be_bytes());
        out[16..].copy_from_slice(&self.difficulty.to_be_bytes());
        out
    }
    pub(crate) fn decode(raw: &[u8]) -> io::Result<Self> {
        need(raw.len() == 48, "choice row bytes")?;
        Ok(Self {
            quai: u64::from_be_bytes(crate::slice::array(&raw[..8])?),
            qi: u64::from_be_bytes(crate::slice::array(&raw[8..16])?),
            difficulty: Uint::from_be_bytes(crate::slice::array(&raw[16..])?),
        })
    }
}
#[derive(Clone)]
pub struct Window {
    block: Hash,
    number: u64,
    start: usize,
    parent: Option<Hash>,
    chunks: [Arc<[Choice; CHUNK]>; CHUNKS],
    sum: U512,
    persisted_root: Option<Hash>,
    copied_rows: usize,
    persisted_row_writes: Option<usize>,
}
fn need(ok: bool, message: &'static str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::InvalidData, message))
    }
}
fn row_key(i: usize) -> [u8; 3] {
    let b = (i as u16).to_be_bytes();
    [1, b[0], b[1]]
}
impl Window {
    /// An explicit component seed. Its caller supplies the history; this never
    /// authenticates a checkpoint or creates a native admission receipt.
    pub fn empty_at(block: Hash, number: u64) -> Self {
        let zero = Arc::new([Choice::default(); CHUNK]);
        Self {
            block,
            number,
            start: 0,
            parent: None,
            chunks: std::array::from_fn(|_| zero.clone()),
            sum: U512::ZERO,
            persisted_root: None,
            copied_rows: 0,
            persisted_row_writes: None,
        }
    }
    pub fn block(&self) -> Hash {
        self.block
    }
    pub fn number(&self) -> u64 {
        self.number
    }
    pub fn difficulty_sum(&self) -> U512 {
        self.sum
    }
    /// Logical rows cloned for the most recent update, excluding Arc handles.
    /// This counter is not allocated bytes, heap usage or sampled RSS.
    pub fn copied_rows(&self) -> usize {
        self.copied_rows
    }
    /// Successful row puts in the current state transaction, excluding metadata.
    /// Publication still depends on that transaction committing successfully.
    pub fn persisted_row_writes(&self) -> Option<usize> {
        self.persisted_row_writes
    }
    pub fn choice(&self, i: usize) -> Option<Choice> {
        if i >= N {
            return None;
        }
        let slot = (self.start + i) % N;
        Some(self.chunks[slot / CHUNK][slot % CHUNK])
    }
    pub fn logical_digest(&self) -> Hash {
        let mut h = Sha256::new();
        for i in 0..N {
            h.update(self.choice(i).unwrap().encode())
        }
        h.finalize().into()
    }
    /// Consecutive parent binding prevents a memory window from crossing forks.
    pub fn advance(
        &self,
        block: Hash,
        parent: Hash,
        number: u64,
        choice: Choice,
    ) -> Result<Self, Error> {
        if parent != self.block || self.number.checked_add(1) != Some(number) {
            return Err(Error::Invalid("token choice parent/height"));
        }
        let mut next = if number <= FORKS.controller_kick_in + 1 {
            Self::empty_at(block, number)
        } else {
            self.clone()
        };
        next.block = block;
        next.number = number;
        next.parent = Some(parent);
        next.persisted_root = None;
        next.persisted_row_writes = None;
        next.copied_rows = 0;
        if number < FORKS.controller_kick_in {
            return Ok(next);
        }
        let slot = if number <= FORKS.controller_kick_in + 1 {
            0
        } else if number <= FORKS.controller_kick_in + controllers::WINDOW {
            (number - FORKS.controller_kick_in - 1) as usize
        } else {
            let old = next.start;
            next.start = (next.start + 1) % N;
            old
        };
        let old = next.chunks[slot / CHUNK][slot % CHUNK];
        next.sum = next
            .sum
            .wrapping_sub(&old.difficulty.resize())
            .wrapping_add(&choice.difficulty.resize());
        if old != choice {
            Arc::make_mut(&mut next.chunks[slot / CHUNK])[slot % CHUNK] = choice;
            next.copied_rows = CHUNK;
        }
        Ok(next)
    }
    fn metadata(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend(self.block);
        out.extend(self.number.to_be_bytes());
        out.extend((self.start as u16).to_be_bytes());
        out.extend(self.sum.to_be_bytes());
        out.push(u8::from(self.parent.is_some()));
        if let Some(p) = self.parent {
            out.extend(p)
        }
        out
    }
    /// Called inside the same native state transaction as execution. An aborted
    /// transaction cannot authorize the next write: its parent root must exist.
    pub fn persist(&self, ws: &mut Workspace<'_, '_>, previous: Option<&Self>) -> io::Result<Self> {
        if let Some(previous) = previous {
            need(
                previous.persisted_root == Some(ws.root(TREE)),
                "choice window parent root",
            )?;
            need(
                self.parent == Some(previous.block)
                    && previous.number.checked_add(1) == Some(self.number),
                "choice window parent binding",
            )?;
            need(
                ws.get(TREE, b"\0")?.as_deref() == Some(previous.metadata().as_slice()),
                "choice window parent metadata",
            )?;
        } else {
            need(
                self.parent.is_none() && ws.root(TREE) == quai_primitives::mpt::empty_root(),
                "choice window initial root",
            )?;
        }
        let mut rows_written = 0;
        for (c, chunk) in self.chunks.iter().enumerate() {
            if previous.is_some_and(|p| Arc::ptr_eq(chunk, &p.chunks[c])) {
                continue;
            }
            for (j, row) in chunk.iter().copied().enumerate() {
                let i = c * CHUNK + j;
                if i >= N {
                    break;
                }
                if previous.is_some_and(|p| p.chunks[c][j] == row) {
                    continue;
                }
                if row == Choice::default() {
                    if previous.is_some() {
                        ws.put(TREE, &row_key(i), &[])?;
                        rows_written += 1;
                    }
                } else {
                    ws.put(TREE, &row_key(i), &row.encode())?;
                    rows_written += 1;
                }
            }
        }
        ws.put(TREE, b"\0", &self.metadata())?;
        let mut next = self.clone();
        next.persisted_root = Some(ws.root(TREE));
        next.persisted_row_writes = Some(rows_written);
        Ok(next)
    }
    /// Full local recovery checks row encodings and the stored aggregate. No peer
    /// state constructor or fallback from missing/corrupt state is provided.
    pub fn load(snapshot: &Snapshot) -> io::Result<Self> {
        let raw = snapshot
            .get(TREE, b"\0")?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "choice window missing"))?;
        let fixed = MAGIC.len() + 32 + 8 + 2 + 64 + 1;
        need(
            raw.len() == fixed || raw.len() == fixed + 32,
            "choice window metadata size",
        )?;
        need(raw.starts_with(MAGIC), "choice window format")?;
        let offset = MAGIC.len();
        let block = crate::slice::array(&raw[offset..offset + 32])?;
        let mut out = Self::empty_at(
            block,
            u64::from_be_bytes(crate::slice::array(&raw[offset + 32..offset + 40])?),
        );
        out.start =
            u16::from_be_bytes(crate::slice::array(&raw[offset + 40..offset + 42])?) as usize;
        need(out.start < N, "choice window cursor")?;
        let expected = U512::from_be_bytes(crate::slice::array(&raw[offset + 42..offset + 106])?);
        let flag = raw[offset + 106];
        need(
            (flag == 0 && raw.len() == fixed) || (flag == 1 && raw.len() == fixed + 32),
            "choice window parent flag",
        )?;
        out.parent = if flag == 1 {
            Some(crate::slice::array(&raw[fixed..])?)
        } else {
            None
        };
        for i in 0..N {
            if let Some(raw) = snapshot.get(TREE, &row_key(i))? {
                let row = Choice::decode(&raw)?;
                need(row != Choice::default(), "choice window redundant zero")?;
                Arc::make_mut(&mut out.chunks[i / CHUNK])[i % CHUNK] = row;
                out.sum = out.sum.wrapping_add(&row.difficulty.resize());
            }
        }
        need(out.sum == expected, "choice window aggregate")?;
        out.persisted_root = Some(snapshot.root(TREE));
        Ok(out)
    }
    pub fn exchange_rate(&self, parent: Uint, miner: Uint) -> Result<Uint, Error> {
        controllers::exchange_rate_from_sum(self.number, parent, miner, self.sum)
    }
}
