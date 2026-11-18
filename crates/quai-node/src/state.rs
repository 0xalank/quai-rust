//! Optional state replay (`replay --state`): execute each zone block from the
//! genesis with the existing execution and settlement code
//! (`zone_execution::native_components`) and compare the header roots: EVM
//! state, ETX set, UTXO set, receipts, outbound ETXs, gas and state counters.
//!
//! The inbound ETXs of a block are the ETXs its parent confirmed. The
//! confirmation follows `confirmation::Hierarchy::prepare_scoped` (which is
//! bound to the private chain profile): a prime block confirms the region
//! rollups in its manifest, a region block the zone outbound ETXs since the
//! last region block.
//!
//! The state is the lazy World, UTXO, ETX and lockup state of
//! `state_store::restore_sparse` over the replay's state database
//! (`store::StateStore`). Changes stay in an overlay until the next commit,
//! every `commit_every` heights, which writes them as a new version and
//! restarts the overlay from it. The ETX and lockup overlays refuse more than
//! 65,536 changes, so commits come long before that (about 18 ETX changes per
//! early mainnet block). The redemption and trim history is `store::PastDb`.
use crate::checkpoint::StateSection;
use crate::store::{self, PastDb, StateStore};
use quai_consensus::{
    confirmation::filter_to_sub,
    history::{History, Record},
    zone_execution::{self, Profile, State},
    Error,
};
use quai_execution::{qi::OutPoint, state_store::Commitment};
use quai_primitives::{
    transaction::{Kind, Transaction},
    Hash,
};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::Arc;

/// Rust limit of the private composition, kept as is.
const MAX_ETXS: usize = 8192;
/// Confirmation and rollup lists kept for recent blocks.
const KEEP: usize = 4096;

pub struct StateReplay {
    pub profile: Profile,
    /// The state as of the last commit; `state` is derived from it.
    pub base: State,
    pub state: State,
    pub past: PastDb,
    pub store: StateStore,
    confirmed: HashMap<Hash, Arc<Vec<Transaction>>>,
    region_rollups: HashMap<Hash, Arc<Vec<Transaction>>>,
    log: VecDeque<Hash>,
    pub executed: u64,
    /// Zone height of the last committed version.
    pub committed_height: u64,
}
fn extend(out: &mut Vec<Transaction>, part: &[Transaction]) -> Result<(), Error> {
    if out.len() + part.len() > MAX_ETXS {
        return Err(Error::ResourceLimit("confirmed rollup transaction count"));
    }
    out.extend_from_slice(part);
    Ok(())
}
impl StateReplay {
    /// A fresh replay state at genesis: an empty state database and history.
    pub fn create(
        profile: Profile,
        genesis: Hash,
        store: StateStore,
        past: PastDb,
    ) -> io::Result<Self> {
        let base = store.restore()?;
        let mut s = Self {
            profile,
            state: base.clone(),
            base,
            past,
            store,
            confirmed: HashMap::new(),
            region_rollups: HashMap::new(),
            log: VecDeque::new(),
            executed: 0,
            committed_height: 0,
        };
        s.confirmed.insert(genesis, Arc::new(vec![]));
        s.region_rollups.insert(genesis, Arc::new(vec![]));
        Ok(s)
    }
    /// Continue from a checkpoint: `store` already holds its version.
    pub fn resume(
        profile: Profile,
        genesis: Hash,
        store: StateStore,
        past: PastDb,
        section: &StateSection,
        height: u64,
        executed: u64,
    ) -> io::Result<Self> {
        let mut s = Self::create(profile, genesis, store, past)?;
        s.committed_height = height;
        s.executed = executed;
        for (id, confirmed, rollup) in &section.confirmations {
            s.confirmed
                .insert(*id, Arc::new(store::decode_tx_list(confirmed)?));
            if let Some(r) = rollup {
                s.region_rollups
                    .insert(*id, Arc::new(store::decode_tx_list(r)?));
            }
            if *id != genesis {
                s.log.push_back(*id);
            }
        }
        Ok(s)
    }
    /// Persist the overlay as the version of `block` and restart the overlay.
    pub fn commit_state(&mut self, height: u64, block: Hash) -> io::Result<()> {
        if height == self.committed_height {
            return Ok(());
        }
        self.store.commit(height, block, &self.base, &self.state)?;
        self.past.flush()?;
        self.committed_height = height;
        self.rebase()
    }
    /// Reload the lazy state from the last committed version.
    pub fn rebase(&mut self) -> io::Result<()> {
        let base = self.store.restore()?;
        self.state = base.clone();
        self.base = base;
        Ok(())
    }
    pub fn commitment(&self) -> &Commitment {
        &self.store.commitment
    }
    /// The checkpoint section for the last committed version.
    pub fn section(&self) -> StateSection {
        let c = &self.store.commitment;
        let in_log: std::collections::HashSet<&Hash> = self.log.iter().collect();
        StateSection {
            version: self.store.version,
            evm: c.roots.evm,
            etx: c.roots.etx,
            utxo: c.roots.utxo,
            accounts: c.roots.accounts,
            utxo_count: c.roots.utxo_count,
            digest: c.digest,
            confirmations: self
                .confirmed
                .iter()
                .filter(|(id, _)| !in_log.contains(id))
                .map(|(id, list)| {
                    (
                        *id,
                        store::encode_tx_list(list),
                        self.region_rollups
                            .get(id)
                            .map(|r| store::encode_tx_list(r)),
                    )
                })
                .chain(self.log.iter().filter_map(|id| {
                    Some((
                        *id,
                        store::encode_tx_list(self.confirmed.get(id)?),
                        self.region_rollups
                            .get(id)
                            .map(|r| store::encode_tx_list(r)),
                    ))
                }))
                .collect(),
        }
    }
    fn region(&self, id: &Hash) -> Result<Arc<Vec<Transaction>>, Error> {
        self.region_rollups
            .get(id)
            .cloned()
            .ok_or(Error::Unavailable("committed region rollup"))
    }
    pub fn confirmed_after(&self, id: &Hash) -> Result<Arc<Vec<Transaction>>, Error> {
        self.confirmed
            .get(id)
            .cloned()
            .ok_or(Error::Unavailable("confirmed parent ETXs"))
    }
    fn sub_rollup(
        &self,
        histories: &[History; 3],
        record: &Record,
        context: usize,
    ) -> Result<Vec<Transaction>, Error> {
        let mut out = vec![];
        for id in record.object().manifest_hashes() {
            if context == 1 {
                let zone = histories[2].get(&id)?;
                extend(&mut out, zone.object().outbound_etxs())?;
            } else if context == 0 {
                extend(&mut out, &self.region(&id)?)?;
            }
        }
        Ok(out)
    }
    fn collect(
        &self,
        histories: &[History; 3],
        orders: &HashMap<Hash, u8>,
        block: &Record,
        context: usize,
        order: u8,
        current: &[Transaction],
    ) -> Result<Vec<Transaction>, Error> {
        let location: [u8; 2] = block
            .header()
            .wire()
            .location
            .as_ref()
            .unwrap()
            .value
            .as_slice()
            .try_into()
            .map_err(|_| Error::Unsupported("confirmation location"))?;
        let mut out = filter_to_sub(current, location, context, order)?;
        let mut cursor: Arc<Record> = Arc::new(block.clone());
        loop {
            let parent = histories[context].get(&cursor.parent(context)?)?;
            if parent.number(context)?.checked_add(1) != Some(cursor.number(context)?) {
                return Err(Error::Invalid("confirmation ancestry height"));
            }
            if parent.number(context)? == 0 {
                return Ok(out);
            }
            let parent_order = *orders
                .get(&parent.id())
                .ok_or(Error::Unavailable("confirmed ancestry order"))?;
            let pl = &parent.header().wire().location.as_ref().unwrap().value;
            if pl.get(context) == Some(&location[context]) && parent_order as usize == context {
                return Ok(out);
            }
            if context == 1 && parent_order == 0 && pl.as_slice() != location {
                let list = self.confirmed_after(&parent.id())?;
                extend(&mut out, &filter_to_sub(&list, location, 1, 0)?)?;
            }
            let part = self.sub_rollup(histories, &parent, context)?;
            extend(&mut out, &filter_to_sub(&part, location, context, order)?)?;
            cursor = parent;
        }
    }
    /// The ETXs this block confirms for its zone children (prepare_scoped,
    /// which writes the zone location [0, 0] as a literal).
    #[allow(clippy::too_many_arguments)]
    pub fn confirm(
        &self,
        histories: &[History; 3],
        orders: &HashMap<Hash, u8>,
        records: &[Option<Arc<Record>>; 3],
        order: u8,
        accumulated: &[Transaction],
        location: [u8; 2],
    ) -> Result<Vec<Transaction>, Error> {
        Ok(if order == 0 {
            let prime = records[0].as_ref().unwrap();
            let sub = self.sub_rollup(histories, prime, 0)?;
            filter_to_sub(
                &self.collect(histories, orders, prime, 0, order, &sub)?,
                location,
                1,
                0,
            )?
        } else if order == 1 {
            self.collect(
                histories,
                orders,
                records[1].as_ref().unwrap(),
                1,
                order,
                accumulated,
            )?
        } else {
            vec![]
        })
    }
    /// Record a block that executed and passed every check.
    pub fn record(
        &mut self,
        zone: &Record,
        order: u8,
        cross: Vec<Transaction>,
        confirmed: Vec<Transaction>,
        created: Vec<(OutPoint, u8)>,
    ) -> Result<(), Error> {
        let id = zone.id();
        let height = zone.number(2)?;
        self.past.record(
            height,
            zone.object()
                .transactions()
                .iter()
                .filter(|tx| matches!(tx.kind(), Kind::External(_)))
                .cloned()
                .collect(),
            created,
        );
        if order <= 1 {
            self.region_rollups.insert(id, Arc::new(cross));
        }
        self.confirmed.insert(id, Arc::new(confirmed));
        self.log.push_back(id);
        if self.log.len() > KEEP {
            let old = self.log.pop_front().unwrap();
            self.region_rollups.remove(&old);
            self.confirmed.remove(&old);
        }
        self.executed += 1;
        Ok(())
    }
}
pub fn profile_for(
    environment: &str,
    chain_id: u64,
    location: [u8; 2],
    genesis: Hash,
) -> Result<Profile, Error> {
    match environment {
        "colosseum" => Profile::official(
            quai_consensus::rules::Uint::from_u64(chain_id),
            location,
            [genesis].into(),
        ),
        "local" => Profile::reviewed_hierarchy(),
        _ => Err(Error::Unsupported("state replay environment")),
    }
}
pub use zone_execution::{diagnose, native_components};
