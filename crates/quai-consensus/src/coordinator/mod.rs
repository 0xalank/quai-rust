//! Bounded recent-candidate selection, coordinator search and retry projections.
//! Results remain imported, unverified candidates and carry no canonical authority.
mod sort;
use crate::{
    eligibility::{Constraints, CoordinatorView, Eligibility},
    frontier::{Frontier, Tip},
    initialization,
    rules::Uint,
    verifier::Verifier,
    Error,
};
use quai_primitives::Hash;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
pub const MAX_RECENT_PER_SLICE: usize = 1000;
pub const MAX_RECENT_TOTAL: usize = 4096;
pub const MAX_SEARCH_ATTEMPTS: usize = 3;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub location: Vec<u8>,
    pub tip: Tip,
}
#[derive(Clone)]
struct Cache {
    nodes: BTreeMap<Hash, Candidate>,
    arrival: VecDeque<Hash>,
}
#[derive(Clone)]
pub struct RecentCandidates {
    expansion: u8,
    legacy: bool,
    capacity: usize,
    caches: BTreeMap<Vec<u8>, Cache>,
    len: usize,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Admission {
    pub accepted: bool,
    pub evicted: Option<Hash>,
}
impl RecentCandidates {
    pub fn new(expansion: u8, legacy: bool, capacity: usize) -> Result<Self, Error> {
        if capacity == 0 || capacity > MAX_RECENT_PER_SLICE {
            return Err(Error::ResourceLimit("recent per-slice capacity"));
        }
        let (r, _) = initialization::hierarchy_size(expansion);
        initialization::active_slices(expansion)?;
        if r > 4 && !legacy {
            return Err(Error::Unsupported("ambiguous Go coordinator region names"));
        }
        Ok(Self {
            expansion,
            legacy,
            capacity,
            caches: BTreeMap::new(),
            len: 0,
        })
    }
    fn key(&self, l: &[u8]) -> Result<Vec<u8>, Error> {
        if l.len() != 2 || !initialization::active_slices(self.expansion)?.contains(&l.to_vec()) {
            return Err(Error::Unsupported("inactive recent candidate location"));
        }
        let mut k = l.to_vec();
        if self.legacy && k[0] >= 3 {
            k[0] = 3
        }
        Ok(k)
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn expansion(&self) -> u8 {
        self.expansion
    }
    pub fn uses_legacy_aliases(&self) -> bool {
        self.legacy
    }
    pub fn at(&self, l: &[u8]) -> Result<Vec<Candidate>, Error> {
        let k = self.key(l)?;
        Ok(self.caches.get(&k).map_or_else(Vec::new, |c| {
            c.arrival.iter().map(|h| c.nodes[h].clone()).collect()
        }))
    }
    /// Import a declared recent-cache seed, preserving FIFO arrival. This is not admission.
    pub fn import_unverified(&mut self, l: &[u8], tip: Tip) -> Result<Admission, Error> {
        self.insert(l, tip, false)
    }
    /// Actual ComputeMapPending rule: compare the new entropy against the oldest,
    /// including before capacity is reached. An admitted duplicate moves to newest.
    pub fn observe_projection(&mut self, l: &[u8], tip: Tip) -> Result<Admission, Error> {
        self.insert(l, tip, true)
    }
    pub fn observe_native(
        &mut self,
        view: &CoordinatorView,
        verifier: &mut Verifier,
        l: &[u8],
        id: Hash,
    ) -> Result<Admission, Error> {
        if self.legacy != view.uses_legacy_aliases() {
            return Err(Error::Unsupported("recent/view naming modes differ"));
        }
        let t = view.tip_native(verifier, l, id)?;
        self.observe_projection(l, t)
    }
    fn insert(&mut self, l: &[u8], tip: Tip, gate: bool) -> Result<Admission, Error> {
        let k = self.key(l)?;
        let old = self.caches.get(&k);
        if let Some(c) = old {
            if let Some(t) = c.nodes.get(&tip.id) {
                if t.tip != tip || t.location != l {
                    return Err(Error::Invalid("conflicting recent candidate"));
                }
            }
            if gate && c.nodes[&c.arrival[0]].tip.entropy >= tip.entropy {
                return Ok(Admission {
                    accepted: false,
                    evicted: None,
                });
            }
        }
        let exists = old.is_some_and(|c| c.nodes.contains_key(&tip.id));
        let full = old.is_some_and(|c| c.nodes.len() == self.capacity);
        if !exists && !full && self.len == MAX_RECENT_TOTAL {
            return Err(Error::ResourceLimit("global recent candidate capacity"));
        }
        let c = self.caches.entry(k).or_insert_with(|| Cache {
            nodes: BTreeMap::new(),
            arrival: VecDeque::new(),
        });
        let mut evicted = None;
        if exists {
            c.arrival.retain(|id| *id != tip.id)
        } else if full {
            let id = c.arrival.pop_front().unwrap();
            c.nodes.remove(&id);
            evicted = Some(id)
        } else {
            self.len += 1
        }
        c.arrival.push_back(tip.id);
        c.nodes.insert(
            tip.id,
            Candidate {
                location: l.to_vec(),
                tip,
            },
        );
        Ok(Admission {
            accepted: true,
            evicted,
        })
    }
    pub fn candidates(&self, l: &[u8], bad: &BTreeSet<Hash>) -> Result<Vec<Candidate>, Error> {
        let mut v = self
            .at(l)?
            .into_iter()
            .filter(|n| !bad.contains(&n.tip.id))
            .collect::<Vec<_>>();
        sort::sort(&mut v, |a, b| a.tip.entropy > b.tip.entropy);
        Ok(v)
    }
    pub fn leaders(&self, bad: &BTreeSet<Hash>) -> Result<Vec<Candidate>, Error> {
        let mut out = vec![];
        for l in initialization::active_slices(self.expansion)? {
            let nodes = self.at(&l)?;
            if nodes.is_empty() {
                continue;
            }
            let mut best: Option<Candidate> = None;
            for n in nodes {
                if bad.contains(&n.tip.id) {
                    continue;
                }
                if best.as_ref().is_none_or(|b| b.tip.entropy < n.tip.entropy) {
                    best = Some(n)
                }
            }
            out.push(best.ok_or(Error::Unavailable("all recent candidates excluded"))?)
        }
        sort::sort(&mut out, |a, b| a.tip.entropy > b.tip.entropy);
        Ok(out)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceAttempt {
    pub location: Vec<u8>,
    pub id: Hash,
    pub first: bool,
    pub accepted: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchAttempt {
    pub leaders: Vec<Candidate>,
    pub traces: Vec<TraceAttempt>,
    pub constraints: Constraints,
    pub rejected: Option<Hash>,
}
pub struct SearchReport {
    pub frontier: Option<Frontier>,
    pub attempts: Vec<SearchAttempt>,
    pub eligibility: Option<Eligibility>,
    pub fully_validated: bool,
    pub history_verified: bool,
}
impl RecentCandidates {
    pub fn search_projection(
        &mut self,
        view: &CoordinatorView,
        default: Hash,
        orders: &BTreeMap<Hash, u8>,
        entropy: &BTreeMap<(Vec<u8>, Hash), Uint>,
    ) -> Result<SearchReport, Error> {
        self.search(
            view,
            default,
            |m, id, l, first| view.trace_projection(m, id, l, first, orders),
            |l, id| {
                view.tip_projection(
                    l,
                    id,
                    *entropy
                        .get(&(view.canonical_location(l)?, id))
                        .ok_or(Error::Unavailable("controlled node entropy"))?,
                )
            },
        )
    }
    pub fn search_native(
        &mut self,
        view: &CoordinatorView,
        verifier: &mut Verifier,
        default: Hash,
    ) -> Result<SearchReport, Error> {
        for cache in self.caches.values() {
            for candidate in cache.nodes.values() {
                if view.tip_native(verifier, &candidate.location, candidate.tip.id)?
                    != candidate.tip
                {
                    return Err(Error::Invalid("native recent candidate binding"));
                }
            }
        }
        let verifier = std::cell::RefCell::new(verifier);
        self.search(
            view,
            default,
            |m, id, l, first| view.trace_native(&mut verifier.borrow_mut(), m, id, l, first),
            |l, id| view.tip_native(&mut verifier.borrow_mut(), l, id),
        )
    }
    fn search<T, N>(
        &mut self,
        view: &CoordinatorView,
        default: Hash,
        mut trace: T,
        mut node: N,
    ) -> Result<SearchReport, Error>
    where
        T: FnMut(&Constraints, Hash, &[u8], bool) -> Result<Constraints, Error>,
        N: FnMut(&[u8], Hash) -> Result<Tip, Error>,
    {
        if self.legacy != view.uses_legacy_aliases() {
            return Err(Error::Unsupported("recent/view naming modes differ"));
        }
        let mut seeded = self.clone();
        for l in initialization::active_slices(self.expansion)? {
            if seeded.at(&l)?.is_empty() {
                let mut t = node(&l, default)?;
                t.entropy = Uint::ZERO;
                seeded.import_unverified(&l, t)?;
            }
        }
        let mut attempts = vec![];
        for _ in 0..MAX_SEARCH_ATTEMPTS {
            let leaders = seeded.leaders(&BTreeSet::new())?;
            let mut map = Constraints::new();
            let mut first = true;
            let mut traces = vec![];
            for leader in &leaders {
                for n in seeded.candidates(&leader.location, &BTreeSet::new())? {
                    let result = trace(&map, n.tip.id, &n.location, first);
                    traces.push(TraceAttempt {
                        location: n.location,
                        id: n.tip.id,
                        first,
                        accepted: result.is_ok(),
                    });
                    first = false;
                    if let Ok(next) = result {
                        map = next;
                        break;
                    }
                }
            }
            let complete = view.complete_constraints(&map, self.expansion, default)?;
            // Match the source's first failing edge, including physical legacy indices.
            let rejected = view.first_frontier_rejection(&complete, self.expansion)?;
            attempts.push(SearchAttempt {
                leaders,
                traces,
                constraints: complete.clone(),
                rejected,
            });
            if rejected.is_some() {
                continue;
            }
            let mut entries = vec![];
            let (r, _) = initialization::hierarchy_size(self.expansion);
            let mut locations = vec![vec![]];
            for i in 0..r as u8 {
                locations.push(vec![i]);
            }
            locations.extend(initialization::active_slices(self.expansion)?);
            for l in locations {
                let id = complete[&view.canonical_location(&l)?];
                entries.push((l.clone(), node(&l, id)?));
            }
            let frontier = if self.legacy {
                Frontier::from_legacy_imported(self.expansion, entries)?
            } else {
                Frontier::from_imported(self.expansion, entries)?
            };
            let eligibility = view.check_frontier(&frontier)?;
            *self = seeded;
            return Ok(SearchReport {
                frontier: Some(frontier),
                attempts,
                eligibility: Some(eligibility),
                fully_validated: false,
                history_verified: false,
            });
        }
        *self = seeded;
        Ok(SearchReport {
            frontier: None,
            attempts,
            eligibility: None,
            fully_validated: false,
            history_verified: false,
        })
    }
}
/// Source ReapplicationLoop attempts at t=0,1,3,7,15,31,63 seconds.
/// Scheduling is explicit; this library never sleeps or starts generation workers.
pub struct ReapplicationSchedule {
    start: u64,
    next: usize,
}
impl ReapplicationSchedule {
    pub fn new(start: u64) -> Result<Self, Error> {
        start
            .checked_add(127)
            .ok_or(Error::Invalid("reapplication time overflow"))?;
        Ok(Self { start, next: 0 })
    }
    pub fn due(&self) -> Option<u64> {
        [0, 1, 3, 7, 15, 31, 63]
            .get(self.next)
            .map(|n| self.start + n)
    }
    pub fn take_due(&mut self, now: u64) -> bool {
        if self.due().is_some_and(|t| now >= t) {
            self.next += 1;
            true
        } else {
            false
        }
    }
    pub fn finished_at(&self) -> u64 {
        self.start + 127
    }
    pub fn cancel(&mut self) {
        self.next = 7
    }
}
