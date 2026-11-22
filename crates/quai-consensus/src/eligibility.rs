//! Bounded coordinator ancestry/constraint/PCRC components on imported views.
//! Eligibility reports never establish validated history or canonical authority.
use crate::{
    fields,
    frontier::Frontier,
    history::Record,
    placement::{self, Profile, Termini},
    scoring::CandidateScore,
    verifier::Verifier,
    Error,
};
use quai_primitives::Hash;
use std::collections::{BTreeMap, BTreeSet};
pub type Constraints = BTreeMap<Vec<u8>, Hash>;
pub const MAX_SLICES: usize = 273;
pub const MAX_ENTRIES: usize = 4096;
pub const MAX_VIEW_BYTES: usize = 32 << 20;
pub const ANCESTOR_DISTANCE: usize = 10_000;
#[derive(Clone)]
struct Entry {
    record: Record,
    termini: Option<Termini>,
}
pub struct CoordinatorView {
    legacy: bool,
    profiles: BTreeMap<Vec<u8>, Profile>,
    entries: BTreeMap<(Vec<u8>, Hash), Entry>,
    bytes: usize,
}
#[derive(Debug)]
pub struct Eligibility {
    pub checked_edges: usize,
    pub imported_entries: usize,
    pub legacy_aliases: bool,
    pub fully_validated: bool,
    pub history_verified: bool,
}
impl CoordinatorView {
    pub fn new(legacy: bool) -> Self {
        Self {
            legacy,
            profiles: BTreeMap::new(),
            entries: BTreeMap::new(),
            bytes: 0,
        }
    }
    pub fn uses_legacy_aliases(&self) -> bool {
        self.legacy
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn tracked_bytes(&self) -> usize {
        self.bytes
    }
    fn key(&self, l: &[u8]) -> Result<Vec<u8>, Error> {
        if l.len() > 2 || l.iter().any(|n| *n > 15) {
            return Err(Error::Unsupported("coordinator location"));
        }
        let mut k = l.to_vec();
        if !k.is_empty() {
            if !self.legacy && k[0] > 3 {
                return Err(Error::Unsupported("ambiguous Go coordinator region names"));
            }
            if self.legacy && k[0] >= 3 {
                k[0] = 3
            }
        }
        Ok(k)
    }
    fn context_location(l: &[u8], order: u8) -> Result<Vec<u8>, Error> {
        if order > 2 || l.len() < order as usize {
            return Err(Error::Unsupported("coordinator context location"));
        }
        Ok(l[..order as usize].to_vec())
    }
    pub fn register(&mut self, p: Profile) -> Result<(), Error> {
        p.validate()?;
        let k = self.key(&p.location)?;
        if k != p.location {
            return Err(Error::Unsupported(
                "register canonical legacy slice coordinates",
            ));
        }
        if let Some(old) = self.profiles.get(&k) {
            if old.genesis != p.genesis
                || old.default_genesis != p.default_genesis
                || old.minimum_difficulty != p.minimum_difficulty
                || old.duration != p.duration
                || old.gas_ceiling != p.gas_ceiling
                || old.p2p_threshold != p.p2p_threshold
            {
                return Err(Error::Invalid("conflicting coordinator slice profile"));
            }
            return Ok(());
        }
        if self.profiles.len() == MAX_SLICES {
            return Err(Error::ResourceLimit("coordinator slices"));
        }
        self.profiles.insert(k, p);
        Ok(())
    }
    pub fn import_unverified(
        &mut self,
        location: &[u8],
        record: Record,
        termini: Option<Termini>,
    ) -> Result<(), Error> {
        let key = self.key(location)?;
        self.profile(&key)?;
        let id = record.id();
        if let Some(old) = self.entries.get(&(key.clone(), id)) {
            if old.record.bytes() != record.bytes() || old.termini != termini {
                return Err(Error::Invalid("conflicting coordinator entry"));
            }
            return Ok(());
        }
        let bytes = record.bytes().len() + key.len() + 32 + termini.as_ref().map_or(0, |_| 1024);
        if self.entries.len() == MAX_ENTRIES
            || self
                .bytes
                .checked_add(bytes)
                .is_none_or(|n| n > MAX_VIEW_BYTES)
        {
            return Err(Error::ResourceLimit("coordinator imported view"));
        }
        self.entries.insert((key, id), Entry { record, termini });
        self.bytes += bytes;
        Ok(())
    }
    fn profile(&self, location: &[u8]) -> Result<&Profile, Error> {
        self.profiles
            .get(&self.key(location)?)
            .ok_or(Error::Unavailable("coordinator slice profile"))
    }
    fn entry(&self, location: &[u8], id: Hash) -> Result<&Entry, Error> {
        self.profile(location)?;
        self.entries
            .get(&(self.key(location)?, id))
            .ok_or(Error::Unavailable("coordinator header"))
    }
    fn termini(&self, location: &[u8], id: Hash) -> Result<&Termini, Error> {
        self.entry(location, id)?
            .termini
            .as_ref()
            .ok_or(Error::Unavailable("coordinator termini"))
    }
    pub fn previous_coincident_matches(
        &self,
        location: &[u8],
        sub_parent: Hash,
        dominant: Hash,
    ) -> Result<bool, Error> {
        let Some(&last) = location.last() else {
            return Err(Error::Invalid("PCRC subordinate location"));
        };
        let l = self.key(location)?;
        let index = last as usize;
        Ok(self.termini(&l, sub_parent)?.dominant[index] == dominant)
    }
    /// Source IsAncestor equality and parent-chain semantics. Missing middle
    /// records are unavailable rather than silently treated as a negative vote.
    pub fn is_ancestor(
        &self,
        ancestor: Hash,
        mut header: Hash,
        header_location: &[u8],
        order: u8,
    ) -> Result<bool, Error> {
        let loc = Self::context_location(header_location, order)?;
        self.key(&loc)?;
        if ancestor == header {
            return Ok(true);
        }
        let mut seen = BTreeSet::new();
        for _ in 0..ANCESTOR_DISTANCE {
            if !seen.insert(header) {
                return Err(Error::Invalid("cyclic coordinator ancestry"));
            }
            let e = self.entry(&loc, header)?;
            let p = e.record.parents[order as usize];
            if p == ancestor {
                return Ok(true);
            }
            // Imported predecessors remain traversable across expansion anchors.
            // Only an absent predecessor terminates this explicitly bounded view.
            if self.profile(&loc)?.genesis.contains(&header)
                && !self.entries.contains_key(&(self.key(&loc)?, p))
            {
                return Ok(false);
            }
            header = p;
        }
        Ok(false)
    }
    pub fn normalize_constraints(&self, input: &Constraints) -> Result<Constraints, Error> {
        if input.len() > MAX_SLICES {
            return Err(Error::ResourceLimit("coordinator constraints"));
        }
        let mut out = Constraints::new();
        for (l, h) in input {
            let k = self.key(l)?;
            if let Some(old) = out.insert(k, *h) {
                if old != *h {
                    return Err(Error::Invalid("conflicting legacy constraints"));
                }
            }
        }
        Ok(out)
    }
    pub fn complete_constraints(
        &self,
        input: &Constraints,
        expansion: u8,
        default: Hash,
    ) -> Result<Constraints, Error> {
        let mut out = self.normalize_constraints(input)?;
        let active = crate::initialization::active_slices(expansion)?;
        let (r, _) = crate::initialization::hierarchy_size(expansion);
        out.entry(vec![]).or_insert(default);
        for i in 0..r as u8 {
            out.entry(self.key(&[i])?).or_insert(default);
        }
        for l in active {
            out.entry(self.key(&l)?).or_insert(default);
        }
        Ok(out)
    }
    pub fn check_frontier(&self, frontier: &Frontier) -> Result<Eligibility, Error> {
        if frontier.uses_legacy_aliases() != self.legacy {
            return Err(Error::Unsupported("frontier/view naming modes differ"));
        }
        let root = frontier
            .tips()
            .get(&vec![])
            .ok_or(Error::Unavailable("prime frontier tip"))?;
        let prime = self.termini(&[], root.id)?;
        let (r, z) = crate::initialization::hierarchy_size(frontier.expansion());
        let mut edges = 0;
        for i in 0..r as u8 {
            let rt = &frontier.tips()[&vec![i]];
            if !self.previous_coincident_matches(&[i], rt.id, prime.subordinate[i as usize])? {
                return Err(Error::Invalid("region frontier PCRC"));
            }
            edges += 1;
            let region = self.termini(&[i], rt.id)?;
            for j in 0..z as u8 {
                let zt = &frontier.tips()[&vec![i, j]];
                if !self.previous_coincident_matches(
                    &[i, j],
                    zt.id,
                    region.subordinate[j as usize],
                )? {
                    return Err(Error::Invalid("zone frontier PCRC"));
                }
                edges += 1;
            }
        }
        Ok(Eligibility {
            checked_edges: edges,
            imported_entries: self.entries.len(),
            legacy_aliases: self.legacy,
            fully_validated: false,
            history_verified: false,
        })
    }
    pub(crate) fn canonical_location(&self, l: &[u8]) -> Result<Vec<u8>, Error> {
        self.key(l)
    }
    pub fn tip_projection(
        &self,
        l: &[u8],
        id: Hash,
        entropy: crate::rules::Uint,
    ) -> Result<crate::frontier::Tip, Error> {
        let r = &self.entry(l, id)?.record;
        Ok(crate::frontier::Tip {
            id: r.id(),
            numbers: r.numbers,
            entropy,
        })
    }
    pub fn tip_native(
        &self,
        verifier: &mut Verifier,
        l: &[u8],
        id: Hash,
    ) -> Result<crate::frontier::Tip, Error> {
        let r = &self.entry(l, id)?.record;
        let p = self.profile(l)?;
        let entropy = if p.genesis.contains(&id) {
            crate::rules::Uint::ZERO
        } else {
            use quai_primitives::work::Context;
            let context = match l.len() {
                0 => Context::Prime,
                1 => Context::Region,
                _ => Context::Zone,
            };
            r.object()
                .verify_commitments(context)
                .map_err(|_| Error::Invalid("coordinator body commitments"))?;
            placement::verify_placement(verifier, r, p)?;
            let work = verifier.verify_header_proof(r.header())?;
            crate::scoring::project_entropy(r, work.hash(), l.len())?.total
        };
        self.tip_projection(l, id, entropy)
    }
    pub(crate) fn first_frontier_rejection(
        &self,
        map: &Constraints,
        expansion: u8,
    ) -> Result<Option<Hash>, Error> {
        let get = |l: &[u8]| {
            map.get(&self.key(l)?)
                .copied()
                .ok_or(Error::Unavailable("frontier constraint"))
        };
        let prime = self.termini(&[], get(&[])?)?;
        let (r, z) = crate::initialization::hierarchy_size(expansion);
        for i in 0..r as u8 {
            let id = get(&[i])?;
            if !self.previous_coincident_matches(&[i], id, prime.subordinate[i as usize])? {
                return Ok(Some(id));
            }
            let region = self.termini(&[i], id)?;
            for j in 0..z as u8 {
                let id = get(&[i, j])?;
                if !self.previous_coincident_matches(&[i, j], id, region.subordinate[j as usize])? {
                    return Ok(Some(id));
                }
            }
        }
        Ok(None)
    }
    pub fn trace_projection(
        &self,
        input: &Constraints,
        leader: Hash,
        leader_location: &[u8],
        first: bool,
        orders: &BTreeMap<Hash, u8>,
    ) -> Result<Constraints, Error> {
        self.trace(input, leader, leader_location, first, |_, r| {
            orders
                .get(&r.id())
                .copied()
                .filter(|o| *o <= 2)
                .ok_or(Error::Unavailable("controlled coordinator order"))
        })
    }
    pub fn trace_native(
        &self,
        verifier: &mut Verifier,
        input: &Constraints,
        leader: Hash,
        leader_location: &[u8],
        first: bool,
    ) -> Result<Constraints, Error> {
        self.trace(input, leader, leader_location, first, |l, r| {
            Ok(placement::verify_placement(verifier, r, self.profile(l)?)?.order)
        })
    }
    fn trace<F>(
        &self,
        input: &Constraints,
        leader: Hash,
        leader_location: &[u8],
        first: bool,
        mut order: F,
    ) -> Result<Constraints, Error>
    where
        F: FnMut(&[u8], &Record) -> Result<u8, Error>,
    {
        let mut map = self.normalize_constraints(input)?;
        let leader = &self.entry(leader_location, leader)?.record;
        let leader_loc = fields::hash(&leader.header().wire().location, "missing location")?;
        if self.profile(leader_loc)?.genesis.contains(&leader.id()) {
            return Ok(map);
        }
        if leader_loc.len() != 2 {
            return Err(Error::Unsupported("coordinator leader location"));
        }
        let mut current_order = order(leader_loc, leader)?;
        if current_order > 2 {
            return Err(Error::Invalid("coordinator order"));
        }
        map.insert(self.key(leader_loc)?, leader.id());
        let mut current = leader;
        let mut parent = leader;
        let mut parent_order = current_order;
        let mut finished = false;
        let mut seen = BTreeSet::new();
        for iteration in 0..ANCESTOR_DISTANCE {
            if !seen.insert((
                self.key(&Self::context_location(
                    fields::hash(&parent.header().wire().location, "missing location")?,
                    current_order,
                )?)?,
                parent.id(),
            )) {
                return Err(Error::Invalid("cyclic constraint ancestry"));
            }
            let pl = fields::hash(&parent.header().wire().location, "missing location")?;
            if parent_order < current_order || iteration == 0 {
                let key = self.key(&Self::context_location(pl, parent_order)?)?;
                let existing = map.get(&key).copied();
                match parent_order {
                    0 => {
                        self.termini(&[], parent.id())?;
                        let region = Self::context_location(pl, 1)?;
                        self.termini(&region, parent.id())?;
                        if let Some(t) = existing {
                            let h = &self.entry(&[], parent.id())?.record;
                            let ancestor = self.is_ancestor(
                                t,
                                parent.id(),
                                fields::hash(&h.header().wire().location, "missing location")?,
                                0,
                            )?;
                            let progeny = self.is_ancestor(parent.id(), t, &[], 0)?;
                            if !ancestor && !progeny {
                                return Err(Error::Invalid("zone not in prime constraint"));
                            }
                            if ancestor && !progeny {
                                map.insert(key, parent.id());
                            }
                            let rk = self.key(&region)?;
                            let update = match map.get(&rk) {
                                Some(rc) => !self.is_ancestor(parent.id(), *rc, &region, 1)?,
                                None => true,
                            };
                            if update {
                                map.insert(rk, parent.id());
                            }
                            if !first {
                                finished = true;
                            }
                        } else if parent.number(0)? == 0 {
                            map.insert(vec![], current.id());
                            map.insert(
                                self.key(&Self::context_location(
                                    fields::hash(
                                        &current.header().wire().location,
                                        "missing location",
                                    )?,
                                    1,
                                )?)?,
                                current.id(),
                            );
                            finished = true;
                        } else {
                            map.insert(vec![], parent.id());
                            if current_order != 1 {
                                map.insert(self.key(&region)?, parent.id());
                            }
                        }
                    }
                    1 => {
                        let region = Self::context_location(pl, 1)?;
                        self.termini(&region, parent.id())?;
                        if let Some(t) = existing {
                            let h = &self.entry(&region, parent.id())?.record;
                            let ancestor = self.is_ancestor(
                                t,
                                parent.id(),
                                fields::hash(&h.header().wire().location, "missing location")?,
                                1,
                            )?;
                            let progeny = self.is_ancestor(parent.id(), t, &region, 1)?;
                            if !ancestor && !progeny {
                                return Err(Error::Invalid("zone not in region constraint"));
                            }
                            if ancestor && !progeny {
                                map.insert(key, parent.id());
                            }
                        } else {
                            map.insert(key, parent.id());
                        }
                    }
                    2 => {
                        map.insert(self.key(pl)?, parent.id());
                    }
                    _ => return Err(Error::Invalid("coordinator order")),
                }
            }
            current = parent;
            current_order = parent_order.min(current_order);
            let cl = fields::hash(&current.header().wire().location, "missing location")?;
            let loc = Self::context_location(cl, current_order)?;
            let p = parent.parents[current_order as usize];
            let profile = self.profile(&loc)?;
            if profile.genesis.contains(&p) || profile.genesis.contains(&parent.id()) {
                return Ok(map);
            }
            parent = &self.entry(&loc, p)?.record;
            parent_order = order(&loc, parent)?;
            if current_order == 0 || finished {
                return Ok(map);
            }
        }
        Err(Error::ResourceLimit("coordinator constraint walk"))
    }
}
impl Frontier {
    /// Return a new frontier only after coincident parent and cross-context
    /// reference checks. The supplied views and original frontier are unchanged.
    pub fn updated_with_eligibility(
        &self,
        record: &Record,
        score: &CandidateScore,
        view: &CoordinatorView,
    ) -> Result<(Self, Eligibility), Error> {
        let next = self.updated_with_score(record, score)?;
        let report = view.check_frontier(&next)?;
        Ok((next, report))
    }
}

#[cfg(test)]
mod limits {
    use super::*;
    fn setup() -> (CoordinatorView, Record) {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/eligibility.json"))
                .unwrap();
        let record = Record::decode(
            &quai_primitives::decode_hex(fixture["native"]["root"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        let mut view = CoordinatorView::new(false);
        let p = Profile {
            location: vec![],
            genesis: [record.id()].into_iter().collect(),
            default_genesis: Some(record.id()),
            minimum_difficulty: crate::rules::Uint::ONE,
            duration: 5,
            gas_ceiling: 50_000_000,
            p2p_threshold: 7,
        };
        view.register(p.clone()).unwrap();
        view.register(Profile {
            location: vec![0],
            ..p
        })
        .unwrap();
        (view, record)
    }
    #[test]
    fn coordinator_byte_budget_rejects_new_views_atomically() {
        let (mut view, r) = setup();
        view.import_unverified(&[], r.clone(), None).unwrap();
        view.bytes = MAX_VIEW_BYTES;
        view.import_unverified(&[], r.clone(), None).unwrap();
        assert!(matches!(
            view.import_unverified(&[0], r, None),
            Err(Error::ResourceLimit(_))
        ));
        assert_eq!(view.len(), 1);
        assert_eq!(view.tracked_bytes(), MAX_VIEW_BYTES);
    }
    #[test]
    fn coordinator_entry_capacity_is_global_across_contexts() {
        let (mut view, r) = setup();
        // Simulate a full imported store without allocating 4096 distinct payloads.
        for i in 0..MAX_ENTRIES {
            let mut id = [0; 32];
            id[..8].copy_from_slice(&(i as u64).to_be_bytes());
            view.entries.insert(
                (vec![], id),
                Entry {
                    record: r.clone(),
                    termini: None,
                },
            );
        }
        assert!(matches!(
            view.import_unverified(&[0], r, None),
            Err(Error::ResourceLimit(_))
        ));
        assert_eq!(view.len(), MAX_ENTRIES);
        assert_eq!(view.tracked_bytes(), 0);
    }
}
