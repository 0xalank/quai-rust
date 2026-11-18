//! Bounded, canonical local checkpoint codec for early private execution replay.
//! Decoding local bytes verifies structure and roots, never network consensus.
use crate::{
    early_replay::EarlyReplay,
    evm::{
        state::{Account, World, MAX_ACCOUNTS, MAX_STORAGE_SLOTS},
        word::{bytes, from_bytes},
    },
};
use quai_primitives::Hash;
const MAGIC: &[u8] = b"QUAI-EARLY-STATE-1\n";
const MAX: usize = 32 << 20;
struct Reader<'a> {
    input: &'a [u8],
    offset: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.offset.checked_add(n).ok_or("checkpoint offset")?;
        let b = self
            .input
            .get(self.offset..end)
            .ok_or("checkpoint truncated")?;
        self.offset = end;
        Ok(b)
    }
    fn n(&mut self) -> Result<usize, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()) as usize)
    }
    fn u(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
}
pub fn encode(state: &EarlyReplay) -> Result<Vec<u8>, String> {
    state.world().validate()?;
    let mut out = MAGIC.to_vec();
    out.extend(state.genesis());
    out.extend(state.head());
    out.extend(state.height().to_be_bytes());
    out.extend(state.timestamp().to_be_bytes());
    out.extend(state.world().root()?);
    out.extend((state.world().account_count() as u32).to_be_bytes());
    for (a, v) in state.world().accounts()?.iter() {
        out.extend(a);
        out.extend(v.nonce.to_be_bytes());
        out.extend(bytes(v.balance));
        out.extend(bytes(v.storage_size));
        out.extend((v.code.len() as u32).to_be_bytes());
        out.extend(&v.code);
        let storage = v.materialize_storage()?;
        out.extend((storage.len() as u32).to_be_bytes());
        for (k, v) in storage.iter() {
            out.extend(k);
            out.extend(v);
        }
        if out.len() > MAX {
            return Err("checkpoint size".into());
        }
    }
    Ok(out)
}
pub fn decode(input: &[u8], expected_genesis: Hash) -> Result<EarlyReplay, String> {
    if input.len() > MAX {
        return Err("checkpoint size".into());
    }
    let mut r = Reader { input, offset: 0 };
    if r.take(MAGIC.len())? != MAGIC {
        return Err("checkpoint format".into());
    }
    let genesis: Hash = r.take(32)?.try_into().unwrap();
    if genesis != expected_genesis {
        return Err("checkpoint genesis".into());
    }
    let head = r.take(32)?.try_into().unwrap();
    let height = r.u()?;
    let time = r.u()?;
    let root: Hash = r.take(32)?.try_into().unwrap();
    let count = r.n()?;
    if count > MAX_ACCOUNTS {
        return Err("checkpoint account limit".into());
    }
    let mut world = World::default();
    let mut last = None;
    for _ in 0..count {
        let a: [u8; 20] = r.take(20)?.try_into().unwrap();
        if last.is_some_and(|previous| previous >= a) {
            return Err("noncanonical checkpoint account order".into());
        }
        last = Some(a);
        let nonce = r.u()?;
        let balance = from_bytes(r.take(32)?);
        let storage_size = from_bytes(r.take(32)?);
        let n = r.n()?;
        if n > 32768 {
            return Err("checkpoint code limit".into());
        }
        let code = r.take(n)?.to_vec();
        let n = r.n()?;
        if n > MAX_STORAGE_SLOTS {
            return Err("checkpoint storage limit".into());
        }
        let mut storage = std::collections::BTreeMap::new();
        let mut previous = None;
        for _ in 0..n {
            let k: Hash = r.take(32)?.try_into().unwrap();
            let v: Hash = r.take(32)?.try_into().unwrap();
            if previous.is_some_and(|p| p >= k) || v == [0; 32] {
                return Err("noncanonical checkpoint storage".into());
            }
            previous = Some(k);
            storage.insert(k, v);
        }
        world.insert(
            a,
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
    if r.offset != input.len() || world.root()? != root {
        return Err("checkpoint state commitment".into());
    }
    let state = EarlyReplay::restore_local(world, genesis, head, height, time)
        .map_err(|e| e.to_string())?;
    if encode(&state)? != input {
        return Err("noncanonical checkpoint".into());
    }
    Ok(state)
}
