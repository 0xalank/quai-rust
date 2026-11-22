//! Optional state replay (`replay --state`): execute each zone block from the
//! genesis with the existing execution and settlement code
//! (`zone_execution::native_components`) and compare the header roots: EVM
//! state, ETX set, UTXO set, receipts, outbound ETXs, gas and state counters.
//!
//! Execution is `Verifier::validate_block` with a parent state: the inbound
//! ETXs of a block are the ETXs its parent confirmed, and the shared
//! `quai_consensus::confirmation::confirm` (go-quai core/slice.go
//! CollectNewlyConfirmedEtxs) derives what the block confirms. This module
//! supplies what that reads besides the orders and region rollups the replay
//! keeps in both modes: the confirmations recorded here.
//!
//! The state is the lazy World, UTXO, ETX and lockup state of
//! `state_store::restore_sparse` over the replay's state database
//! (`store::StateStore`). Changes stay in an overlay until the next commit,
//! every `commit_every` heights, which writes them as a new version and
//! restarts the overlay from it. The redemption and trim history is
//! `store::PastDb`.
use crate::checkpoint::StateSection;
use crate::store::{self, PastDb, StateStore};
use quai_consensus::{
    block::ParentState,
    chain::Chain,
    confirmation::Known,
    history::Record,
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

/// Confirmation and rollup lists kept for recent blocks.
pub const KEEP: usize = 4096;
/// A committed state version: its id and commitment.
pub type Version = (Hash, Commitment);

pub struct StateReplay {
    pub profile: Profile,
    /// The state as of the last commit; `state` is derived from it.
    pub base: State,
    pub state: State,
    pub past: PastDb,
    pub store: StateStore,
    confirmed: HashMap<Hash, Arc<Vec<Transaction>>>,
    log: VecDeque<Hash>,
    pub executed: u64,
    /// Zone height of the last committed version.
    pub committed_height: u64,
}
/// The orders and region rollups of recent blocks and, with `--state`, this
/// replay's recorded confirmations: what `Verifier::validate_block` reads
/// through `confirmation::Known`.
pub struct KnownView<'a> {
    pub orders: &'a HashMap<Hash, u8>,
    pub region_rollups: &'a HashMap<Hash, Arc<Vec<Transaction>>>,
    pub maps: Option<Maps<'a>>,
}
/// The recorded confirmations of a state replay.
pub struct Maps<'a> {
    confirmed: &'a HashMap<Hash, Arc<Vec<Transaction>>>,
}
impl Known for KnownView<'_> {
    fn order(&self, id: &Hash) -> Result<u8, Error> {
        self.orders
            .get(id)
            .copied()
            .ok_or(Error::Unavailable("confirmed ancestry order"))
    }
    fn region_rollup(&self, id: &Hash) -> Result<Arc<Vec<Transaction>>, Error> {
        self.region_rollups
            .get(id)
            .cloned()
            .ok_or(Error::Unavailable("committed region rollup"))
    }
    fn confirmed(&self, id: &Hash) -> Result<Arc<Vec<Transaction>>, Error> {
        self.maps
            .as_ref()
            .and_then(|m| m.confirmed.get(id).cloned())
            .ok_or(Error::Unavailable("confirmed parent ETXs"))
    }
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
            log: VecDeque::new(),
            executed: 0,
            committed_height: 0,
        };
        s.confirmed.insert(genesis, Arc::new(vec![]));
        Ok(s)
    }
    /// Continue from a checkpoint: `store` already holds its version. The
    /// region rollups of a checkpoint without a prime section come back too.
    #[allow(clippy::type_complexity)]
    pub fn resume(
        profile: Profile,
        genesis: Hash,
        store: StateStore,
        past: PastDb,
        section: &StateSection,
        height: u64,
        executed: u64,
    ) -> io::Result<(Self, Vec<(Hash, Arc<Vec<Transaction>>)>)> {
        let mut s = Self::create(profile, genesis, store, past)?;
        s.committed_height = height;
        s.executed = executed;
        let mut rollups = vec![];
        for (id, confirmed, rollup) in &section.confirmations {
            s.confirmed
                .insert(*id, Arc::new(store::decode_tx_list(confirmed)?));
            if let Some(r) = rollup {
                rollups.push((*id, Arc::new(store::decode_tx_list(r)?)));
            }
            if *id != genesis {
                s.log.push_back(*id);
            }
        }
        Ok((s, rollups))
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
    /// The last committed version and its commitment.
    pub fn version_point(&self) -> Version {
        (self.store.version, self.store.commitment.clone())
    }
    /// Make an earlier committed version of this database the state again
    /// (a reorg to the block at `height`, or a compaction at the tip base).
    /// The changes since the last commit are dropped.
    pub fn restore_version(&mut self, height: u64, version: &Version) -> io::Result<()> {
        self.store.version = version.0;
        self.store.commitment = version.1.clone();
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
            // The region rollups live in the checkpoint's prime section.
            confirmations: self
                .confirmed
                .iter()
                .filter(|(id, _)| !in_log.contains(id))
                .map(|(id, list)| (*id, store::encode_tx_list(list), None))
                .chain(self.log.iter().filter_map(|id| {
                    Some((*id, store::encode_tx_list(self.confirmed.get(id)?), None))
                }))
                .collect(),
        }
    }
    /// The recorded confirmations and the execution parent state, borrowed
    /// together for one `Verifier::validate_block` call.
    pub fn split(&mut self) -> (Maps<'_>, ParentState<'_, PastDb>) {
        (
            Maps {
                confirmed: &self.confirmed,
            },
            ParentState {
                state: &mut self.state,
                past: &self.past,
                profile: &self.profile,
            },
        )
    }
    /// Record a block that executed and passed every check.
    pub fn record(
        &mut self,
        zone: &Record,
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
        self.confirmed.insert(id, Arc::new(confirmed));
        self.log.push_back(id);
        if self.log.len() > KEEP {
            if let Some(old) = self.log.pop_front() {
                self.confirmed.remove(&old);
            }
        }
        self.executed += 1;
        Ok(())
    }
}
/// The execution profile of a chain spec.
pub fn profile_for(chain: &Chain) -> Result<Profile, Error> {
    chain.execution_profile()
}
pub use zone_execution::{diagnose, native_components};
