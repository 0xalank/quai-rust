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
/// Unlock height (8 bytes), address (20), flags (1), amount (32).
const RECORD: usize = 61;
/// The records after the 18-byte header (magic and record count). The build
/// checks the layout, so a malformed asset cannot reach a block.
const RECORDS: &[[u8; RECORD]] = {
    let (header, records) = ALLOCATIONS.split_at(18);
    let magic = b"QUAI-ALLOCS-1\n";
    let mut i = 0;
    while i < magic.len() {
        assert!(header[i] == magic[i], "allocations.bin magic");
        i += 1;
    }
    let n = u32::from_be_bytes([header[14], header[15], header[16], header[17]]) as usize;
    let (rows, rest) = records.as_chunks::<RECORD>();
    assert!(
        rows.len() == n && rest.is_empty(),
        "allocations.bin record count"
    );
    rows
};
fn allocation_height(row: &[u8; RECORD]) -> u64 {
    let [b0, b1, b2, b3, b4, b5, b6, b7, ..] = *row;
    u64::from_be_bytes([b0, b1, b2, b3, b4, b5, b6, b7])
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
    // The rows are sorted by unlock height (test `allocation_rows_are_sorted`).
    let first = RECORDS.partition_point(|row| allocation_height(row) < unlock);
    let mut staged = world.clone();
    let mut added = 0;
    for row in RECORDS.iter().skip(first) {
        if allocation_height(row) != unlock {
            break;
        }
        let flags = row[28];
        // Forfeiture from SingularityForkBlock, founder forfeiture from PolarisForkBlock.
        if prime >= FORKS.singularity && flags & 1 != 0 || prime >= FORKS.polaris && flags & 2 != 0
        {
            continue;
        }
        let address: Address = std::array::from_fn(|i| row[8 + i]);
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
/// Compact consensus-only representation of a single redeemable external
/// transaction (coinbase lockup `etx_type == 1` with 33-byte lockup payload or
/// conversion `etx_type == 2`, excluding Qi-prefix destinations).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledUnlock {
    pub to: Address,
    /// Minimal big-endian value bytes (<= 32 bytes; leading zeros stripped).
    pub value: Vec<u8>,
    /// `Some(lock_byte)` (`0..=3`) for `etx_type == 1`; `None` for `etx_type == 2`.
    pub lock: Option<u8>,
}

/// Extract the consensus-redeemable fields of an incoming external transaction,
/// returning `(lock_tier, ScheduledUnlock)` where `LOCK_PERIODS[lock_tier]` is
/// the exact unlock delay (`0` for 2-week coinbases and conversions, `1..=3`
/// for 3-month, 6-month, and 1-year coinbases). Non-redeemable ETXs return
/// `None`.
pub fn extract_redeemable(tx: &Transaction) -> Option<(u8, ScheduledUnlock)> {
    let Kind::External(etx) = tx.kind() else {
        return None;
    };
    if etx.to[1] & 128 != 0 {
        return None;
    }
    let trim_be = |raw: &[u8]| -> Vec<u8> {
        let low = &raw[raw.len().saturating_sub(32)..];
        let first = low.iter().position(|&b| b != 0).unwrap_or(low.len());
        low[first..].to_vec()
    };
    if etx.etx_type == 1 && etx.data.len() == 33 {
        let lock = etx.data[0];
        if (lock as usize) < LOCK_PERIODS.len() {
            return Some((
                lock,
                ScheduledUnlock {
                    to: etx.to,
                    value: trim_be(&etx.value),
                    lock: Some(lock),
                },
            ));
        }
        return None;
    }
    if etx.etx_type == 2 {
        return Some((
            0,
            ScheduledUnlock {
                to: etx.to,
                value: trim_be(&etx.value),
                lock: None,
            },
        ));
    }
    None
}

pub trait History {
    /// External transactions actually included at this height on the parent branch.
    fn incoming_at(&self, height: u64) -> Result<Vec<Transaction>, Error>;
    /// Created raw UTXO keys in denomination order, including historical duplicates.
    fn created_at(&self, height: u64) -> Result<Vec<(OutPoint, u8)>, Error>;
    /// Optional scheduled-unlock fast path: all redeemable entries scheduled to
    /// mature at `height`, ordered by lock tier (`0..=3`) and original tx order
    /// within each tier. Returning `Ok(None)` falls back to `incoming_at`.
    fn unlocks_at(&self, _height: u64) -> Result<Option<Vec<ScheduledUnlock>>, Error> {
        Ok(None)
    }
    /// Optional scheduled-trim fast path: all created UTXO outpoints of
    /// `denomination` (`0..=5`) scheduled for trimming at `height`, in original
    /// creation order. Returning `Ok(None)` falls back to `created_at`.
    fn trims_at(&self, _height: u64, _denomination: u8) -> Result<Option<Vec<OutPoint>>, Error> {
        Ok(None)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unlock {
    pub address: Address,
    pub amount: Word,
}
fn apply_unlock(
    staged: &mut World,
    unlocks: &mut Vec<Unlock>,
    to: Address,
    mut amount: Word,
    parent_state_size: Word,
    location: [u8; 2],
) -> Result<(), Error> {
    if to[0] != location[0] << 4 | location[1] {
        return Err(Error::Invalid("historical unlock scope"));
    }
    if staged.account(&to)?.is_none() {
        let creation = Word::from_u64(crate::evm::state_gas::scaled(
            parent_state_size,
            Word::ZERO,
            25000,
        ));
        if amount < creation {
            return Ok(());
        }
        amount = amount.wrapping_sub(&creation);
    }
    economy::add_balance(staged, to, amount)?;
    unlocks.push(Unlock {
        address: to,
        amount,
    });
    Ok(())
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
    if let Some(scheduled) = history.unlocks_at(height)? {
        for entry in scheduled {
            let amount = match entry.lock {
                Some(lock) => economy::reward(from_bytes(&entry.value), lock, height)?,
                None => from_bytes(&entry.value),
            };
            apply_unlock(
                &mut staged,
                &mut unlocks,
                entry.to,
                amount,
                parent_state_size,
                location,
            )?;
        }
        *world = staged;
        return Ok(unlocks);
    }
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
            apply_unlock(
                &mut staged,
                &mut unlocks,
                etx.to,
                amount,
                parent_state_size,
                location,
            )?;
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
        let points: Vec<OutPoint> = if let Some(fast) = history.trims_at(height, denomination)? {
            fast
        } else {
            let mut pts = vec![];
            for (point, stored_denomination) in history.created_at(height - depth)? {
                if stored_denomination > denomination {
                    break;
                }
                if stored_denomination == denomination {
                    pts.push(point);
                }
            }
            pts
        };
        for point in points {
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
#[cfg(test)]
mod allocation_tests {
    use super::*;
    use quai_primitives::transaction::External;
    use std::collections::HashMap;

    #[test]
    fn allocation_rows_are_sorted() {
        assert_eq!(RECORDS.len(), 54_752);
        assert!(RECORDS
            .windows(2)
            .all(|w| allocation_height(&w[0]) <= allocation_height(&w[1])));
    }

    struct LegacyOnly {
        incoming: HashMap<u64, Vec<Transaction>>,
        created: HashMap<u64, Vec<(OutPoint, u8)>>,
    }
    impl History for LegacyOnly {
        fn incoming_at(&self, height: u64) -> Result<Vec<Transaction>, Error> {
            Ok(self.incoming.get(&height).cloned().unwrap_or_default())
        }
        fn created_at(&self, height: u64) -> Result<Vec<(OutPoint, u8)>, Error> {
            Ok(self.created.get(&height).cloned().unwrap_or_default())
        }
    }

    struct ScheduledOnly {
        unlocks: HashMap<u64, Vec<ScheduledUnlock>>,
        trims: HashMap<(u64, u8), Vec<OutPoint>>,
    }
    impl History for ScheduledOnly {
        fn incoming_at(&self, _height: u64) -> Result<Vec<Transaction>, Error> {
            Err(Error::Invalid("legacy incoming_at should not be called"))
        }
        fn created_at(&self, _height: u64) -> Result<Vec<(OutPoint, u8)>, Error> {
            Err(Error::Invalid("legacy created_at should not be called"))
        }
        fn unlocks_at(&self, height: u64) -> Result<Option<Vec<ScheduledUnlock>>, Error> {
            Ok(Some(self.unlocks.get(&height).cloned().unwrap_or_default()))
        }
        fn trims_at(&self, height: u64, denomination: u8) -> Result<Option<Vec<OutPoint>>, Error> {
            Ok(Some(
                self.trims
                    .get(&(height, denomination))
                    .cloned()
                    .unwrap_or_default(),
            ))
        }
    }

    fn make_etx(etx_type: u64, to: Address, value: u128, data: Vec<u8>) -> Transaction {
        Transaction::external(External {
            originating_hash: [0u8; 32],
            index: 0,
            gas: 21_000,
            to,
            value: value.to_be_bytes().to_vec(),
            data,
            access_list: vec![],
            sender: [0u8; 20],
            etx_type,
        })
        .unwrap()
    }

    #[test]
    fn scheduled_unlocks_and_trims_match_legacy_bit_for_bit() {
        let exec_h = LOCK_PERIODS[3] + 500;
        let addr_a: Address = [
            0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0a,
        ];
        let addr_b: Address = [
            0x00, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0b,
        ];
        let qi_addr: Address = [
            0x00, 0x81, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0c,
        ];

        let mut incoming: HashMap<u64, Vec<Transaction>> = HashMap::new();
        // Tier 0 (2w): coinbase lock 0 + conversion + Qi coinbase (ignored) + regular ETX (ignored)
        let mut lock0 = vec![0u8; 33];
        lock0[0] = 0;
        let mut lock3 = vec![0u8; 33];
        lock3[0] = 3;
        incoming.insert(
            exec_h - LOCK_PERIODS[0],
            vec![
                make_etx(0, addr_a, 999_999, vec![1, 2, 3]),
                make_etx(1, qi_addr, 5_000_000_000_000_000, lock0.clone()),
                make_etx(1, addr_a, 10_000_000_000_000_000, lock0),
                make_etx(2, addr_b, 20_000_000_000_000_000, vec![]),
            ],
        );
        // Tier 3 (1y): coinbase lock 3 to same address addr_a
        incoming.insert(
            exec_h - LOCK_PERIODS[3],
            vec![make_etx(1, addr_a, 30_000_000_000_000_000, lock3)],
        );

        let pt0 = OutPoint {
            hash: [0x11; 32],
            index: 0,
        };
        let pt5 = OutPoint {
            hash: [0x55; 32],
            index: 2,
        };
        let pt7 = OutPoint {
            hash: [0x77; 32],
            index: 1,
        };
        let mut created: HashMap<u64, Vec<(OutPoint, u8)>> = HashMap::new();
        created.insert(exec_h - spec::TRIM_DEPTHS[0], vec![(pt0, 0), (pt7, 7)]);
        created.insert(exec_h - spec::TRIM_DEPTHS[5], vec![(pt5, 5)]);

        // Build scheduled view using extract_redeemable & denom<=5 filtering
        let mut unlocks_map: HashMap<u64, Vec<ScheduledUnlock>> = HashMap::new();
        for (tier, &depth) in LOCK_PERIODS.iter().enumerate() {
            if let Some(txs) = incoming.get(&(exec_h - depth)) {
                for tx in txs {
                    if let Some((t, u)) = extract_redeemable(tx) {
                        if t as usize == tier {
                            unlocks_map.entry(exec_h).or_default().push(u);
                        }
                    }
                }
            }
        }
        let mut trims_map: HashMap<(u64, u8), Vec<OutPoint>> = HashMap::new();
        for (d, &depth) in spec::TRIM_DEPTHS.iter().enumerate() {
            if let Some(list) = created.get(&(exec_h - depth)) {
                for &(pt, sd) in list {
                    if sd as usize == d {
                        trims_map.entry((exec_h, sd)).or_default().push(pt);
                    }
                }
            }
        }

        let legacy = LegacyOnly { incoming, created };
        let scheduled = ScheduledOnly {
            unlocks: unlocks_map,
            trims: trims_map,
        };

        let mut w_leg = World::default();
        let mut w_sch = World::default();
        let u_leg = redeem(&mut w_leg, &legacy, exec_h, Word::ZERO, [0, 0]).unwrap();
        let u_sch = redeem(&mut w_sch, &scheduled, exec_h, Word::ZERO, [0, 0]).unwrap();
        assert_eq!(u_leg, u_sch);
        assert_eq!(u_leg.len(), 3);
        assert_eq!(w_leg.root().unwrap(), w_sch.root().unwrap());

        let mut ustate = UtxoState::default();
        let uaddr = crate::qi::UtxoAddress::new(&qi_addr).unwrap();
        ustate
            .insert(
                pt0,
                Utxo {
                    denomination: 0,
                    address: uaddr.clone(),
                    lock: vec![],
                },
            )
            .unwrap();
        ustate
            .insert(
                pt5,
                Utxo {
                    denomination: 5,
                    address: uaddr.clone(),
                    lock: vec![],
                },
            )
            .unwrap();
        ustate
            .insert(
                pt7,
                Utxo {
                    denomination: 7,
                    address: uaddr,
                    lock: vec![],
                },
            )
            .unwrap();
        let base = ustate.clone();
        let mut s_leg = ustate.clone();
        let mut s_sch = ustate;
        let (t_leg, r_leg) = trim(&mut s_leg, &base, &legacy, exec_h, 0, &[]).unwrap();
        let (t_sch, r_sch) = trim(&mut s_sch, &base, &scheduled, exec_h, 0, &[]).unwrap();
        assert_eq!(t_leg.len(), 2);
        assert_eq!(t_leg.len(), t_sch.len());
        assert_eq!(r_leg, r_sch);
        assert_eq!(s_leg.root(), s_sch.root());
    }
}
