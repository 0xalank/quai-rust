//! Three-context append and confirmed-ETX derivation (core/slice.go Append
//! and CollectNewlyConfirmedEtxs). `Hierarchy` runs it for a chain spec; the
//! reviewed private profiles keep their narrower qualification domains. No
//! admitted-block token, fork choice or canonical/RPC publication is implied.
use crate::{
    chain::Chain,
    fields,
    history::{History, Record, MAX_RECORDS},
    placement::{self, CandidateIndex, Prepared, Profile},
    prime_controller,
    private_profile::PrivateChain,
    share_rules,
    verifier::Verifier,
    Error,
};
use quai_primitives::{
    spec::{self, FORKS},
    transaction::{Kind, Transaction},
    work::transactions_root,
    Hash,
};
use std::{collections::BTreeMap, sync::Arc};
/// In-memory overlay budget of the reviewed private runtime. A chain spec has
/// no such limit: go-quai bounds rollups only by the blocks themselves.
const MAX_PRIVATE_ETX_BYTES: usize = 32 << 20;
fn external(tx: &Transaction) -> Result<&quai_primitives::transaction::External, Error> {
    if let Kind::External(e) = tx.kind() {
        Ok(e)
    } else {
        Err(Error::Invalid("non-ETX in committed rollup"))
    }
}
/// core/types/transaction.go Transactions.FilterToSub. Preserve reference
/// order and duplicates. Coinbase/conversion require prime confirmation;
/// region-local confirmation carries only standard ETXs.
pub fn filter_to_sub(
    input: &[Transaction],
    location: [u8; 2],
    context: usize,
    order: u8,
) -> Result<Vec<Transaction>, Error> {
    if context > 2 || order > 2 || location.iter().any(|v| *v > 15) {
        return Err(Error::Invalid("rollup filter scope"));
    }
    let mut out = vec![];
    for tx in input {
        let e = external(tx)?;
        let prefix = location[0] << 4 | location[1];
        let keep = match context {
            0 => e.to[0] >> 4 == location[0],
            1 => e.to[0] == prefix && (order == 0 || !matches!(e.etx_type, 1 | 2)),
            _ => false,
        };
        if keep {
            out.push(tx.clone());
        }
    }
    Ok(out)
}
/// The region's cross-prime rollup (core/slice.go Append): ETXs leaving the
/// node region, and every conversion and coinbase ETX.
pub fn cross_prime(input: &[Transaction], region: u8) -> Result<Vec<Transaction>, Error> {
    if region > 15 {
        return Err(Error::Invalid("cross-prime rollup region"));
    }
    let mut out = vec![];
    for tx in input {
        let e = external(tx)?;
        if e.to[0] >> 4 != region || matches!(e.etx_type, 1 | 2) {
            out.push(tx.clone());
        }
    }
    Ok(out)
}
/// What the confirmation derivation reads besides the context histories: the
/// computed order of earlier blocks, the cross-prime rollup of earlier region
/// blocks and the ETXs that earlier blocks confirmed for their zone.
pub trait Known {
    fn order(&self, id: &Hash) -> Result<u8, Error>;
    fn region_rollup(&self, id: &Hash) -> Result<Arc<Vec<Transaction>>, Error>;
    fn confirmed(&self, id: &Hash) -> Result<Arc<Vec<Transaction>>, Error>;
}
fn location_of(record: &Record) -> Result<[u8; 2], Error> {
    record
        .header()
        .wire()
        .location
        .as_ref()
        .ok_or(Error::Invalid("confirmation location"))?
        .value
        .as_slice()
        .try_into()
        .map_err(|_| Error::Unsupported("confirmation location"))
}
/// core/headerchain.go CollectSubRollup in a prime (context 0) or region
/// (context 1) node: the rollup of the subordinate blocks in the manifest.
pub fn sub_rollup(
    histories: &[History; 3],
    known: &dyn Known,
    record: &Record,
    context: usize,
) -> Result<Vec<Transaction>, Error> {
    let mut out = vec![];
    for id in record.object().manifest_hashes() {
        if context == 1 {
            out.extend_from_slice(histories[2].get(&id)?.object().outbound_etxs());
        } else if context == 0 {
            out.extend_from_slice(&known.region_rollup(&id)?);
        }
    }
    Ok(out)
}
/// core/slice.go CollectNewlyConfirmedEtxs for `block` in a prime (0) or
/// region (1) node, starting from the block's own filtered sub rollup.
pub fn newly_confirmed(
    histories: &[History; 3],
    known: &dyn Known,
    block: &Record,
    context: usize,
    order: u8,
    current: &[Transaction],
) -> Result<Vec<Transaction>, Error> {
    let location = location_of(block)?;
    let mut out = filter_to_sub(current, location, context, order)?;
    let mut cursor = crate::history::Cursor::from(block);
    loop {
        let parent = histories[context].get(&cursor.parents[context])?;
        if parent.number(context)?.checked_add(1) != Some(cursor.number(context)?) {
            return Err(Error::Invalid("confirmation ancestry height"));
        }
        // IsGenesisHash(ancHash).
        if parent.number(context)? == 0 {
            return Ok(out);
        }
        let parent_order = known.order(&parent.id())?;
        // Stop at a prime block that predates the slice's activation.
        let expansion = parent
            .object()
            .header()
            .wire()
            .expansion_number
            .unwrap_or(0) as u8;
        let (regions, zones) = spec::hierarchy_size(expansion);
        if parent_order == 0 && (location[0] as u64 > regions || location[1] as u64 > zones) {
            return Ok(out);
        }
        let pl = fields::hash(&parent.header().wire().location, "missing location")?;
        if pl.get(context) == Some(&location[context]) && parent_order as usize == context {
            return Ok(out);
        }
        if context == 1 && parent_order == 0 && pl != location {
            let list = known.confirmed(&parent.id())?;
            out.extend(filter_to_sub(&list, location, 1, 0)?);
        }
        let part = sub_rollup(histories, known, &parent, context)?;
        out.extend(filter_to_sub(&part, location, context, order)?);
        cursor = parent.into();
    }
}
/// core/slice.go CollectNewlyConfirmedEtxs for a prime block in the prime
/// context: the ETXs it confirms for its region, before the prime economic
/// branch of Append prices them (`prime_controller`).
pub fn prime_inbound(
    histories: &[History; 3],
    known: &dyn Known,
    prime: &Record,
) -> Result<Vec<Transaction>, Error> {
    let sub = sub_rollup(histories, known, prime, 0)?;
    newly_confirmed(histories, known, prime, 0, 0, &sub)
}
/// The ETXs a block confirms for its zone: a prime block filters the list its
/// prime context passes down (`dominant`: core/slice.go Append newInboundEtxs
/// after the prime economic branch, see `prime_controller::Step::dominant`) to
/// the zone (the region Append of a dominant block), a region block passes
/// its region collection, a zone-only block confirms nothing. `accumulated`
/// is the zone rollup of the region block's manifest.
pub fn confirm(
    histories: &[History; 3],
    known: &dyn Known,
    records: [Option<&Record>; 3],
    order: u8,
    accumulated: &[Transaction],
    location: [u8; 2],
    dominant: Option<&[Transaction]>,
) -> Result<Vec<Transaction>, Error> {
    match order {
        0 => filter_to_sub(
            dominant.ok_or(Error::Unavailable("prime dominant ETXs"))?,
            location,
            1,
            0,
        ),
        1 => {
            let region = records[1].ok_or(Error::Unavailable("coincident region record"))?;
            newly_confirmed(histories, known, region, 1, order, accumulated)
        }
        _ => Ok(vec![]),
    }
}
/// The zone outbound ETXs of the blocks in the zone parent's manifest.
pub fn accumulated(
    histories: &[History; 3],
    indexes: &[CandidateIndex; 3],
    zone: &Record,
) -> Result<Vec<Transaction>, Error> {
    let parent = indexes[2].get(&zone.parent(2)?)?;
    let mut out = vec![];
    for id in parent
        .manifest
        .as_ref()
        .ok_or(Error::Unavailable("zone rollup manifest"))?
    {
        out.extend_from_slice(histories[2].get(id)?.object().outbound_etxs());
    }
    Ok(out)
}
/// The uncled entropy a zone header must carry: the pre-KawPow share
/// entropy, or core/poem.go UncledLogEntropy after KawPow.
pub fn expected_uncled_entropy(
    v: &mut Verifier,
    history: &History,
    zone: &Record,
) -> Result<crate::rules::Uint, Error> {
    if zone.prime() < FORKS.kawpow {
        Ok(crate::prefork_shares::native_components(v, history, zone)?.uncled)
    } else {
        crate::validation::uncled_log_entropy(v, zone)
    }
}
/// core/slice.go Append, prime context: the prime controllers of a prime
/// block on the state after its parent. Returns the next controller state and
/// the list the prime context passes to the region.
pub fn prime_step(
    histories: &[History; 3],
    known: &dyn Known,
    state: &prime_controller::State,
    prime: &Record,
) -> Result<prime_controller::Step, prime_controller::Refusal> {
    let parent = histories[0].get(&prime.parent(0)?)?;
    let grandparent = if prime.number(0)? > FORKS.controller_kick_in {
        Some(histories[0].get(&parent.parent(0)?)?)
    } else {
        None
    };
    let inbound = prime_inbound(histories, known, prime)?;
    state.step(prime, &parent, grandparent.as_deref(), inbound)
}
pub(crate) mod delta;
pub use delta::LocalOutputs;
/// Authenticated local hierarchy outputs scoped to one immutable branch version.
pub trait Reader: Send + Sync {
    fn heads(&self) -> Result<[Hash; 3], Error>;
    fn record(&self, context: usize, id: &Hash) -> Result<Option<Arc<Record>>, Error>;
    fn snapshot(
        &self,
        context: usize,
        id: &Hash,
    ) -> Result<Option<Arc<placement::Snapshot>>, Error>;
    fn order(&self, id: &Hash) -> Result<Option<u8>, Error>;
    fn region_rollup(&self, id: &Hash) -> Result<Option<Arc<Vec<Transaction>>>, Error>;
    fn confirmed(&self, id: &Hash) -> Result<Option<Arc<Vec<Transaction>>>, Error>;
}
struct HistoryReader {
    source: Arc<dyn Reader>,
    context: usize,
}
impl crate::history::Reader for HistoryReader {
    fn read(&self, id: &Hash) -> Result<Option<Arc<Record>>, Error> {
        self.source.record(self.context, id)
    }
}
struct IndexReader {
    source: Arc<dyn Reader>,
    context: usize,
}
impl placement::Reader for IndexReader {
    fn read(&self, id: &Hash) -> Result<Option<Arc<placement::Snapshot>>, Error> {
        self.source.snapshot(self.context, id)
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum PreparationDomain {
    /// The go-quai rules of the chain spec, prime controllers included.
    /// Refusals are Go's; explicit stops remain only where go-quai panics.
    Spec,
    /// Reviewed private profile domains (qualification scopes of chain 1337).
    Early,
    ActivatedLocal,
    ActivatedHierarchy,
    ActivatedETX,
}
pub struct Hierarchy {
    chain: Arc<Chain>,
    source: Option<Arc<dyn Reader>>,
    profiles: [Profile; 3],
    histories: [History; 3],
    indexes: [CandidateIndex; 3],
    heads: [Hash; 3],
    orders: BTreeMap<Hash, u8>,
    region_rollups: BTreeMap<Hash, Arc<Vec<Transaction>>>,
    confirmed: BTreeMap<Hash, Arc<Vec<Transaction>>>,
    etx_bytes: usize,
    /// The prime controller state after the prime head (a chain spec built
    /// from its genesis); None over a reader, which does not supply it.
    controller: Option<prime_controller::State>,
}
/// Privately constructed component permit bound to the current three heads.
/// It never grants execution or full consensus authority by itself.
pub struct PreparedConfirmation {
    chain: Arc<Chain>,
    expected: [Hash; 3],
    records: [Option<Record>; 3],
    appends: [Option<Prepared>; 3],
    order: u8,
    region_rollup: Vec<Transaction>,
    confirmed: Vec<Transaction>,
    execution_inbound: Vec<Transaction>,
    charge: usize,
    controller: Option<prime_controller::State>,
}
impl PreparedConfirmation {
    pub(crate) fn chain_profile(&self) -> &Chain {
        &self.chain
    }
    pub fn order(&self) -> u8 {
        self.order
    }
    pub fn inbound_for_execution(&self) -> &[Transaction] {
        &self.execution_inbound
    }
    pub(crate) fn binds(&self, block: &Record) -> bool {
        self.records[2]
            .as_ref()
            .is_some_and(|record| record.bytes() == block.bytes())
    }
    pub fn newly_confirmed(&self) -> &[Transaction] {
        &self.confirmed
    }
    pub fn region_rollup(&self) -> &[Transaction] {
        &self.region_rollup
    }
    pub fn context_components(&self) -> usize {
        self.records.iter().filter(|v| v.is_some()).count()
    }
    pub fn snapshot(&self, context: usize) -> Option<&placement::Snapshot> {
        self.appends
            .get(context)
            .and_then(Option::as_ref)
            .map(|p| &p.next)
    }
    pub fn returned_dominant(&self, c: usize) -> Option<Hash> {
        self.appends
            .get(c)
            .and_then(Option::as_ref)
            .map(|p| p.previous_dominant)
    }
}
impl Known for Hierarchy {
    fn order(&self, id: &Hash) -> Result<u8, Error> {
        self.known_order(id)
    }
    fn region_rollup(&self, id: &Hash) -> Result<Arc<Vec<Transaction>>, Error> {
        self.region(id)
    }
    fn confirmed(&self, id: &Hash) -> Result<Arc<Vec<Transaction>>, Error> {
        self.confirmed_after(id)
    }
}
impl Hierarchy {
    /// The reviewed private hierarchy profile (chain 1337).
    pub fn new() -> Result<Self, Error> {
        Self::for_chain(Chain::private_hierarchy()?)
    }
    pub(crate) fn new_for_chain(p: Arc<PrivateChain>) -> Result<Self, Error> {
        Self::for_chain(Arc::new(Chain::from_private(p)))
    }
    /// An in-memory hierarchy at the genesis of any chain spec.
    pub fn for_chain(chain: Arc<Chain>) -> Result<Self, Error> {
        let id = chain.genesis_hash();
        let mut s = Self {
            profiles: std::array::from_fn(|c| chain.placement_profile(c)),
            source: None,
            histories: std::array::from_fn(|_| History::default()),
            indexes: std::array::from_fn(|_| CandidateIndex::default()),
            heads: [id; 3],
            orders: BTreeMap::from([(id, 0)]),
            region_rollups: BTreeMap::from([(id, Arc::new(vec![]))]),
            confirmed: BTreeMap::from([(id, Arc::new(vec![]))]),
            etx_bytes: 0,
            controller: Some(prime_controller::State::genesis(chain.genesis())?),
            chain,
        };
        for c in 0..3 {
            let profile = s.profile(c)?;
            let genesis = s.chain.genesis().clone();
            s.indexes[c]
                .insert_imported(id, placement::bootstrap_candidate(&genesis, &profile)?)?;
            s.histories[c].insert(genesis)?;
        }
        Ok(s)
    }
    pub fn from_reader(source: Arc<dyn Reader>) -> Result<Self, Error> {
        Self::from_reader_for(source, Chain::private_hierarchy()?)
    }
    pub(crate) fn from_reader_for_chain(
        source: Arc<dyn Reader>,
        p: Arc<PrivateChain>,
    ) -> Result<Self, Error> {
        Self::from_reader_for(source, Arc::new(Chain::from_private(p)))
    }
    /// A hierarchy over an authenticated reader of any chain spec. The reader
    /// supplies heads, records, snapshots, orders, rollups and confirmations.
    pub fn from_reader_for(source: Arc<dyn Reader>, chain: Arc<Chain>) -> Result<Self, Error> {
        let id = chain.genesis_hash();
        let heads = source.heads()?;
        Ok(Self {
            profiles: std::array::from_fn(|c| chain.placement_profile(c)),
            histories: std::array::from_fn(|context| {
                History::with_reader(Arc::new(HistoryReader {
                    source: source.clone(),
                    context,
                }))
            }),
            indexes: std::array::from_fn(|context| {
                CandidateIndex::with_reader(Arc::new(IndexReader {
                    source: source.clone(),
                    context,
                }))
            }),
            heads,
            orders: BTreeMap::from([(id, 0)]),
            region_rollups: BTreeMap::from([(id, Arc::new(vec![]))]),
            confirmed: BTreeMap::from([(id, Arc::new(vec![]))]),
            etx_bytes: 0,
            controller: None,
            source: Some(source),
            chain,
        })
    }
    pub fn chain(&self) -> &Arc<Chain> {
        &self.chain
    }
    pub(crate) fn overlay_records(&self) -> usize {
        self.histories[2].len()
    }
    pub fn indexed_history(&self) -> bool {
        self.source.is_some()
    }
    fn known_order(&self, id: &Hash) -> Result<u8, Error> {
        if let Some(order) = self.orders.get(id) {
            return Ok(*order);
        }
        self.source
            .as_ref()
            .ok_or(Error::Unavailable("confirmed ancestry order"))?
            .order(id)?
            .ok_or(Error::Unavailable("confirmed ancestry order"))
    }
    fn region(&self, id: &Hash) -> Result<Arc<Vec<Transaction>>, Error> {
        if let Some(list) = self.region_rollups.get(id) {
            return Ok(list.clone());
        }
        self.source
            .as_ref()
            .ok_or(Error::Unavailable("committed region rollup"))?
            .region_rollup(id)?
            .ok_or(Error::Unavailable("committed region rollup"))
    }
    fn profile(&self, c: usize) -> Result<Profile, Error> {
        self.profiles
            .get(c)
            .cloned()
            .ok_or(Error::Invalid("hierarchy context"))
    }
    pub fn heads(&self) -> [Hash; 3] {
        self.heads
    }
    pub fn confirmed_after(&self, id: &Hash) -> Result<Arc<Vec<Transaction>>, Error> {
        if let Some(list) = self.confirmed.get(id) {
            return Ok(list.clone());
        }
        self.source
            .as_ref()
            .ok_or(Error::Unavailable("confirmed parent ETXs"))?
            .confirmed(id)?
            .ok_or(Error::Unavailable("confirmed parent ETXs"))
    }
    /// The default domain: the go-quai rules for a chain spec, the early
    /// qualification scope for a reviewed private profile.
    fn default_domain(&self) -> PreparationDomain {
        if self.chain.private().is_some() {
            PreparationDomain::Early
        } else {
            PreparationDomain::Spec
        }
    }
    /// Runtime dispatch. A reviewed private profile uses only its embedded
    /// contract; a chain spec uses the go-quai rules.
    pub(crate) fn prepare_for_runtime(
        &self,
        v: &mut Verifier,
        zone: &Record,
        views: &[(u8, Vec<u8>)],
        now: u64,
    ) -> Result<PreparedConfirmation, Error> {
        let Some(p) = self.chain.private() else {
            return self.prepare_scoped(v, zone, views, now, PreparationDomain::Spec);
        };
        let domain = if p.activated_hierarchy && zone.number(2)? > FORKS.start_tx {
            if p.all_etx_hierarchy {
                PreparationDomain::ActivatedETX
            } else {
                PreparationDomain::ActivatedHierarchy
            }
        } else {
            PreparationDomain::Early
        };
        self.prepare_scoped(v, zone, views, now, domain)
    }
    /// Prepare one block in all its coincident contexts. `views` are the prime
    /// and region records of a coincident block.
    pub fn prepare(
        &self,
        v: &mut Verifier,
        zone: &Record,
        views: &[(u8, Vec<u8>)],
        now: u64,
    ) -> Result<PreparedConfirmation, Error> {
        self.prepare_scoped(v, zone, views, now, self.default_domain())
    }
    /// Activated local append components against an explicitly authenticated
    /// branch reader. This does not widen the private canonical runtime profile
    /// or authenticate a supplied checkpoint. Prime/region confirmation remains
    /// on the separately qualified early path.
    pub fn prepare_activated_local_components(
        &self,
        v: &mut Verifier,
        zone: &Record,
        now: u64,
    ) -> Result<PreparedConfirmation, Error> {
        self.prepare_scoped(v, zone, &[], now, PreparationDomain::ActivatedLocal)
    }
    /// Activated coincident append/rollup components before economic-controller
    /// activation. All parent records and confirmed outputs remain supplied by
    /// the authenticated branch reader. This grants no checkpoint or canonical
    /// authority and does not widen the early private runtime profile.
    pub fn prepare_activated_components(
        &self,
        v: &mut Verifier,
        zone: &Record,
        views: &[(u8, Vec<u8>)],
        now: u64,
    ) -> Result<PreparedConfirmation, Error> {
        self.prepare_scoped(v, zone, views, now, PreparationDomain::ActivatedHierarchy)
    }
    /// Composed activated hierarchy components for every currently defined
    /// external transaction kind. Parent state/history are authenticated inputs
    /// to this component interface, not a supplied canonical checkpoint. The
    /// reviewed runtime dispatch retains its narrower qualified contract.
    pub fn prepare_activated_etx_components(
        &self,
        v: &mut Verifier,
        zone: &Record,
        views: &[(u8, Vec<u8>)],
        now: u64,
    ) -> Result<PreparedConfirmation, Error> {
        self.prepare_scoped(v, zone, views, now, PreparationDomain::ActivatedETX)
    }
    /// The qualification scopes of the reviewed private profiles. A chain spec
    /// (`PreparationDomain::Spec`) has none of these refusals.
    fn private_scope(&self, zone: &Record, domain: PreparationDomain) -> Result<(), Error> {
        let activated = zone.number(2)? > FORKS.start_tx;
        let activated_domain = domain != PreparationDomain::Early;
        if activated != activated_domain || zone.prime >= FORKS.kawpow {
            return Err(Error::Unsupported("hierarchy transaction/fork profile"));
        }
        for tx in zone.object().transactions() {
            if activated_domain {
                if matches!(tx.kind(), Kind::External(e) if
                    if domain == PreparationDomain::ActivatedETX { e.etx_type > 6 }
                    else { !matches!(e.etx_type, 0 | 1) })
                {
                    return Err(Error::Unsupported("activated local ETX kind"));
                }
            } else if external(tx)?.etx_type != 1 {
                return Err(Error::Unsupported(
                    "mixed transaction/fork history unqualified",
                ));
            }
        }
        if matches!(
            domain,
            PreparationDomain::ActivatedHierarchy | PreparationDomain::ActivatedETX
        ) && zone.prime >= FORKS.controller_kick_in
        {
            return Err(Error::Unsupported(
                "activated economic controllers unqualified",
            ));
        }
        Ok(())
    }
    fn prepare_scoped(
        &self,
        v: &mut Verifier,
        zone: &Record,
        views: &[(u8, Vec<u8>)],
        now: u64,
        domain: PreparationDomain,
    ) -> Result<PreparedConfirmation, Error> {
        let spec_domain = domain == PreparationDomain::Spec;
        if spec_domain != self.chain.private().is_none() {
            return Err(Error::Invalid("preparation domain of another chain kind"));
        }
        if !spec_domain {
            self.private_scope(zone, domain)?;
        }
        let location = self.chain.location();
        let placement = placement::verify_placement(v, zone, &self.profile(2)?)?;
        if domain == PreparationDomain::ActivatedLocal && (placement.order != 2 || zone.prime != 0)
        {
            return Err(Error::Unsupported(
                "activated local prime-zero components required",
            ));
        }
        let order = placement.order;
        let mut records: [Option<Record>; 3] = std::array::from_fn(|_| None);
        records[2] = Some(zone.clone());
        if views.len() != 2usize.saturating_sub(order as usize) {
            return Err(Error::Unavailable("required higher-order views"));
        }
        for (c, bytes) in views {
            if *c > 1 || (*c as u8) < order || records[*c as usize].is_some() {
                return Err(Error::Invalid("hierarchy view roles"));
            }
            let r = Record::decode(bytes)?;
            if r.id() != zone.id()
                || r.header().encode() != zone.header().encode()
                || r.object().header().encode() != zone.object().header().encode()
            {
                return Err(Error::Invalid("coincident view/header binding"));
            }
            records[*c as usize] = Some(r);
        }
        let h = zone.object().header().wire();
        if !spec_domain {
            // Private profiles: every block keeps the genesis controller values.
            let gh = self.chain.genesis().object().header().wire();
            if h.exchange_rate != gh.exchange_rate
                || h.k_quai_discount != gh.k_quai_discount
                || h.conversion_flow_amount != gh.conversion_flow_amount
            {
                return Err(Error::Invalid("early hierarchy economic defaults"));
            }
        }
        let mut appends: [Option<Prepared>; 3] = std::array::from_fn(|_| None);
        let mut claim = [0; 32];
        for c in order as usize..3 {
            let r = records[c]
                .as_ref()
                .ok_or(Error::Unavailable("coincident context"))?;
            if r.parents[c] != self.heads[c] {
                return Err(Error::Invalid("hierarchy parent branch"));
            }
            let p = placement::prepare_append_components(
                v,
                &self.histories[c],
                &self.indexes[c],
                r,
                &self.profile(c)?,
                now,
                (order as usize) < c,
                claim,
            )?;
            claim = p.previous_dominant;
            appends[c] = Some(p);
        }
        let uncled = expected_uncled_entropy(v, &self.histories[2], zone)?;
        if share_rules::uint(fields::bytes(&h.uncled_entropy, "missing uncled entropy")?)? != uncled
        {
            return Err(Error::Invalid("hierarchy uncled entropy"));
        }
        let accumulated = accumulated(&self.histories, &self.indexes, zone)?;
        let cross = cross_prime(&accumulated, location[0])?;
        // core/slice.go Append checks EtxRollupHash in the region, so only for
        // region-coincident blocks. The private profiles check every block.
        if !spec_domain || order <= 1 {
            if transactions_root(&cross)
                .map_err(|_| Error::Unsupported("rollup root domain"))?
                .as_slice()
                != fields::hash(&h.etx_rollup_hash, "missing etx rollup hash")?
            {
                return Err(Error::Invalid("committed ETX rollup root"));
            }
        }
        if order <= 1 {
            let region = records[1]
                .as_ref()
                .ok_or(Error::Invalid("hierarchy region record"))?;
            let from_view = sub_rollup(&self.histories, self, region, 1)?;
            if from_view
                .iter()
                .map(Transaction::encode)
                .collect::<Vec<_>>()
                != accumulated
                    .iter()
                    .map(Transaction::encode)
                    .collect::<Vec<_>>()
            {
                return Err(Error::Invalid("region/zone rollup disagreement"));
            }
        }
        // core/slice.go Append, prime context: the prime controllers of a
        // chain spec, or the unpriced list of a private profile (its blocks
        // keep the genesis values and stop before the kick-in).
        let (controller, dominant) = match records[0].as_ref() {
            Some(prime) if spec_domain => {
                let state = self
                    .controller
                    .as_ref()
                    .ok_or(Error::Unavailable("prime controller state"))?;
                let step =
                    prime_step(&self.histories, self, state, prime).map_err(|r| r.error())?;
                let dominant = step.dominant().to_vec();
                (Some(step.into_state()), Some(dominant))
            }
            Some(prime) => (None, Some(prime_inbound(&self.histories, self, prime)?)),
            None => (None, None),
        };
        let confirmed = confirm(
            &self.histories,
            self,
            [
                records[0].as_ref(),
                records[1].as_ref(),
                records[2].as_ref(),
            ],
            order,
            &accumulated,
            location,
            dominant.as_deref(),
        )?;
        let execution_inbound = self.confirmed_after(&zone.parents[2])?.to_vec();
        let retained_cross = if order <= 1 { cross.as_slice() } else { &[] };
        let charge = retained_cross
            .iter()
            .chain(confirmed.iter())
            .try_fold(0usize, |n, t| {
                n.checked_add(t.encode().len())
                    .ok_or(Error::ResourceLimit("rollup size overflow"))
            })?;
        if !spec_domain
            && self
                .etx_bytes
                .checked_add(charge)
                .is_none_or(|n| n > MAX_PRIVATE_ETX_BYTES)
        {
            return Err(Error::ResourceLimit("hierarchy ETX history budget"));
        }
        Ok(PreparedConfirmation {
            chain: self.chain.clone(),
            expected: self.heads,
            records,
            appends,
            order,
            region_rollup: cross,
            confirmed,
            execution_inbound,
            charge,
            controller,
        })
    }
    /// Installs checked components only. The private branch caller must execute
    /// and validate zone state before persisting/choosing any execution head.
    pub fn commit(&mut self, mut p: PreparedConfirmation) -> Result<(), Error> {
        if self.chain.fingerprint() != p.chain.fingerprint() || self.heads != p.expected {
            return Err(Error::Invalid("stale hierarchy preparation"));
        }
        for c in p.order as usize..3 {
            let (Some(r), Some(a)) = (p.records[c].as_ref(), p.appends[c].as_ref()) else {
                return Err(Error::Invalid("prepared confirmation record"));
            };
            self.histories[c].check_insert(r)?;
            self.indexes[c].check_insert(&r.id(), &a.next)?;
        }
        if self.orders.len() >= MAX_RECORDS
            || (self.chain.private().is_some()
                && self
                    .etx_bytes
                    .checked_add(p.charge)
                    .is_none_or(|n| n > MAX_PRIVATE_ETX_BYTES))
        {
            return Err(Error::ResourceLimit("hierarchy history capacity"));
        }
        let id = p.records[2]
            .as_ref()
            .ok_or(Error::Invalid("prepared confirmation record"))?
            .id();
        // The loop above found every record and append present.
        for c in p.order as usize..3 {
            let (Some(r), Some(a)) = (p.records[c].take(), p.appends[c].take()) else {
                return Err(Error::Invalid("prepared confirmation record"));
            };
            self.heads[c] = r.id();
            self.histories[c].insert(r)?;
            self.indexes[c].insert_imported(id, a.next)?;
        }
        self.orders.insert(id, p.order);
        if p.order <= 1 {
            self.region_rollups.insert(id, Arc::new(p.region_rollup));
        }
        self.confirmed.insert(id, Arc::new(p.confirmed));
        self.etx_bytes += p.charge;
        if let Some(c) = p.controller.take() {
            self.controller = Some(c);
        }
        Ok(())
    }
}
