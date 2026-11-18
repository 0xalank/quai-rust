//! Historical economic effects derived from the candidate branch. A provider
//! must supply execution-validated ancestry; these helpers confer no authority.
use crate::qi::UtxoView;
use crate::{
    economic_state::{self as economy, CONVERSION_LOCK, LOCK_PERIODS},
    evm::{
        state::World,
        word::{from_bytes, Word},
    },
    qi::{OutPoint, Utxo, DENOMINATIONS},
    settlement::Error,
    utxo::{self, UtxoState},
};
use quai_primitives::{
    spec::{self, FORKS},
    transaction::{Address, Kind, Transaction},
    Hash,
};
use std::collections::BTreeSet;
const ALLOCATIONS: &[u8] = include_bytes!("../assets/allocations.bin");
const RECORD: usize = 61;
fn allocations() -> &'static [u8] {
    assert_eq!(&ALLOCATIONS[..14], b"QUAI-ALLOCS-1\n");
    let n = u32::from_be_bytes(ALLOCATIONS[14..18].try_into().unwrap()) as usize;
    assert_eq!(ALLOCATIONS.len(), 18 + n * RECORD);
    &ALLOCATIONS[18..]
}
fn allocation_height(b: &[u8]) -> u64 {
    u64::from_be_bytes(b[..8].try_into().unwrap())
}
/// Indexed by unlock height, so ordinary blocks examine no allocation records.
pub fn allocate(
    world: &mut World,
    height: u64,
    prime: u64,
    location: [u8; 2],
) -> Result<usize, Error> {
    // core/headerchain_validation.go Finalize unlocks only in zone [0, 0];
    // core/state/gen_allocs.go AddLockedBalances at height 1 and each month.
    if location != [0, 0] || height != 1 && height % economy::MONTH != 0 {
        return Ok(0);
    }
    let unlock = if height == 1 { 0 } else { height };
    let b = allocations();
    let count = b.len() / RECORD;
    let mut lo = 0;
    let mut hi = count;
    while lo < hi {
        let m = (lo + hi) / 2;
        if allocation_height(&b[m * RECORD..]) < unlock {
            lo = m + 1
        } else {
            hi = m
        }
    }
    let mut staged = world.clone();
    let mut added = 0;
    for row in b[lo * RECORD..].chunks_exact(RECORD) {
        if allocation_height(row) != unlock {
            break;
        }
        let flags = row[28];
        // Forfeiture from SingularityForkBlock, founder forfeiture from PolarisForkBlock.
        if prime >= FORKS.singularity && flags & 1 != 0 || prime >= FORKS.polaris && flags & 2 != 0
        {
            continue;
        }
        let address: Address = row[8..28].try_into().unwrap();
        if address[0] != 0 {
            return Err(Error::Invalid("allocation scope"));
        }
        economy::add_balance(&mut staged, address, from_bytes(&row[29..61]))?;
        added += 1;
    }
    staged
        .validate()
        .map_err(|_| Error::Invalid("allocation capacity"))?;
    *world = staged;
    Ok(added)
}
pub trait History {
    /// External transactions actually included at this height on the parent branch.
    fn incoming_at(&self, height: u64) -> Result<Vec<Transaction>, Error>;
    /// Created raw UTXO keys in denomination order, including historical duplicates.
    fn created_at(&self, height: u64) -> Result<Vec<(OutPoint, u8)>, Error>;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unlock {
    pub address: Address,
    pub amount: Word,
}
/// Redeem direct Quai coinbases and Qi-to-Quai conversions before this block's
/// transactions. Contract-owned coinbases require a later explicit claim.
pub fn redeem(
    world: &mut World,
    history: &impl History,
    height: u64,
    parent_state_size: Word,
    location: [u8; 2],
) -> Result<Vec<Unlock>, Error> {
    let mut staged = world.clone();
    let mut unlocks = vec![];
    for depth in LOCK_PERIODS {
        if height <= depth {
            continue;
        }
        for tx in history.incoming_at(height - depth)? {
            let Kind::External(etx) = tx.kind() else {
                return Err(Error::Invalid("historical incoming type"));
            };
            if etx.to[1] & 128 != 0 {
                continue;
            }
            let amount = if etx.etx_type == 1 && etx.data.len() == 33 {
                let lock = etx.data[0] as usize;
                if LOCK_PERIODS.get(lock) != Some(&depth) {
                    continue;
                }
                economy::reward(from_bytes(&etx.value), lock as u8, height)?
            } else if etx.etx_type == 2 && depth == CONVERSION_LOCK {
                from_bytes(&etx.value)
            } else {
                continue;
            };
            if etx.to[0] != location[0] << 4 | location[1] {
                return Err(Error::Invalid("historical unlock scope"));
            }
            let mut amount = amount;
            if staged.account(&etx.to)?.is_none() {
                let creation = Word::from_u64(crate::evm::state_gas::scaled(
                    parent_state_size,
                    Word::ZERO,
                    25000,
                ));
                if amount < creation {
                    continue;
                }
                amount = amount.wrapping_sub(&creation);
            }
            economy::add_balance(&mut staged, etx.to, amount)?;
            unlocks.push(Unlock {
                address: etx.to,
                amount,
            });
        }
    }
    *world = staged;
    Ok(unlocks)
}
#[derive(Clone, Debug)]
pub struct Trimmed {
    pub point: OutPoint,
    pub value: Utxo,
}
/// Trimming reads the pre-block raw store, matching Go's non-batch reads. The
/// post-fork set suppresses a second removal of outputs spent in this block.
pub fn trim(
    state: &mut UtxoState,
    base: &UtxoState,
    history: &impl History,
    height: u64,
    prime: u64,
    spent: &[Hash],
) -> Result<(Vec<Trimmed>, Word), Error> {
    let mut staged = state.clone();
    let mut deleted = spent.iter().copied().collect::<BTreeSet<_>>();
    let mut trimmed = vec![];
    let mut removed = Word::ZERO;
    for denomination in 0..=5u8 {
        let depth = spec::TRIM_DEPTHS[denomination as usize];
        if height <= depth {
            continue;
        }
        for (point, stored_denomination) in history.created_at(height - depth)? {
            if stored_denomination > denomination {
                break;
            }
            if stored_denomination != denomination {
                continue;
            }
            let Some(value) = base
                .get(&point)
                .map_err(|e| Error::utxo(e, "historical UTXO"))?
            else {
                continue;
            };
            if value.denomination != denomination || value.lock.iter().any(|v| *v != 0) {
                continue;
            }
            let hash = utxo::utxo_hash(point, &value);
            if prime >= FORKS.conversion_lock_change && !deleted.insert(hash) {
                continue;
            }
            staged
                .trim_raw(point, hash)
                .map_err(|e| Error::utxo(e, "historical UTXO trim"))?;
            removed = removed.wrapping_add(&Word::from_u64(DENOMINATIONS[denomination as usize]));
            trimmed.push(Trimmed {
                point,
                value: value.into_owned(),
            });
        }
    }
    *state = staged;
    Ok((trimmed, removed))
}
