//! Optional state replay (`replay --state`): execute each zone block from the
//! genesis with the existing execution and settlement code
//! (`zone_execution::native_components`) and compare the header roots: EVM
//! state, ETX set, UTXO set, receipts, outbound ETXs, gas and state counters.
//!
//! The inbound ETXs of a block are the ETXs its parent confirmed. The
//! confirmation follows `confirmation::Hierarchy::prepare_scoped` (which is
//! bound to the private chain profile): a prime block confirms the region
//! rollups in its manifest, a region block the zone outbound ETXs since the
//! last region block. State is kept in memory, so this suits early heights.
use quai_consensus::{
    confirmation::filter_to_sub,
    history::{History, Record},
    zone_execution::{self, Profile, State},
    Error,
};
use quai_execution::{historical, qi::OutPoint, settlement};
use quai_primitives::{
    transaction::{Kind, Transaction},
    Hash,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// Rust limit of the private composition, kept as is.
const MAX_ETXS: usize = 8192;
/// Confirmation and rollup lists kept for recent blocks.
const KEEP: usize = 4096;

#[derive(Default)]
pub struct Past {
    incoming: HashMap<u64, Vec<Transaction>>,
    created: HashMap<u64, Vec<(OutPoint, u8)>>,
}
impl historical::History for Past {
    fn incoming_at(&self, height: u64) -> Result<Vec<Transaction>, settlement::Error> {
        self.incoming
            .get(&height)
            .cloned()
            .ok_or(settlement::Error::Invalid(
                "missing replay redemption ancestry",
            ))
    }
    fn created_at(&self, height: u64) -> Result<Vec<(OutPoint, u8)>, settlement::Error> {
        self.created
            .get(&height)
            .cloned()
            .ok_or(settlement::Error::Invalid("missing replay trim ancestry"))
    }
}

pub struct StateReplay {
    pub profile: Profile,
    pub state: State,
    pub past: Past,
    confirmed: HashMap<Hash, Arc<Vec<Transaction>>>,
    region_rollups: HashMap<Hash, Arc<Vec<Transaction>>>,
    log: VecDeque<Hash>,
    pub executed: u64,
}
fn extend(out: &mut Vec<Transaction>, part: &[Transaction]) -> Result<(), Error> {
    if out.len() + part.len() > MAX_ETXS {
        return Err(Error::ResourceLimit("confirmed rollup transaction count"));
    }
    out.extend_from_slice(part);
    Ok(())
}
impl StateReplay {
    pub fn new(profile: Profile, genesis: Hash) -> Self {
        let mut s = Self {
            profile,
            state: State::default(),
            past: Past::default(),
            confirmed: HashMap::new(),
            region_rollups: HashMap::new(),
            log: VecDeque::new(),
            executed: 0,
        };
        s.confirmed.insert(genesis, Arc::new(vec![]));
        s.region_rollups.insert(genesis, Arc::new(vec![]));
        s
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
    /// The ETXs this block confirms for its zone children (prepare_scoped).
    pub fn confirm(
        &self,
        histories: &[History; 3],
        orders: &HashMap<Hash, u8>,
        records: &[Option<Arc<Record>>; 3],
        order: u8,
        accumulated: &[Transaction],
    ) -> Result<Vec<Transaction>, Error> {
        Ok(if order == 0 {
            let prime = records[0].as_ref().unwrap();
            let sub = self.sub_rollup(histories, prime, 0)?;
            filter_to_sub(
                &self.collect(histories, orders, prime, 0, order, &sub)?,
                [0, 0],
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
    pub fn commit(
        &mut self,
        zone: &Record,
        order: u8,
        cross: Vec<Transaction>,
        confirmed: Vec<Transaction>,
        created: Vec<(OutPoint, u8)>,
    ) -> Result<(), Error> {
        let id = zone.id();
        let height = zone.number(2)?;
        self.past.incoming.insert(
            height,
            zone.object()
                .transactions()
                .iter()
                .filter(|tx| matches!(tx.kind(), Kind::External(_)))
                .cloned()
                .collect(),
        );
        self.past.created.insert(height, created);
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
    pub fn execute_profile(&self) -> &Profile {
        &self.profile
    }
}
pub fn profile_for(environment: &str, chain_id: u64, genesis: Hash) -> Result<Profile, Error> {
    match environment {
        "colosseum" => Profile::official(
            quai_consensus::rules::Uint::from_u64(chain_id),
            [0, 0],
            [genesis].into(),
        ),
        "local" => Profile::reviewed_hierarchy(),
        _ => Err(Error::Unsupported("state replay environment")),
    }
}
pub use zone_execution::{diagnose, native_components};
