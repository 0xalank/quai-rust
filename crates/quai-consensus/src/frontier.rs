//! Bounded coordinator NodeSet projections and entropy-keyed pending candidates.
//! Imported tips, calculated scores and pending sets carry no canonical authority.
use crate::{fields, history::Record, initialization, rules::Uint, scoring::CandidateScore, Error};
use crypto_bigint::U512;
use quai_primitives::Hash;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
pub const MAX_PENDING: usize = 2000;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tip {
    pub id: Hash,
    pub numbers: [u64; 3],
    pub entropy: Uint,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frontier {
    expansion: u8,
    legacy_aliases: bool,
    tips: BTreeMap<Vec<u8>, Tip>,
}
impl Frontier {
    pub fn from_imported(expansion: u8, entries: Vec<(Vec<u8>, Tip)>) -> Result<Self, Error> {
        Self::build(expansion, entries, false)
    }
    /// Explicit compatibility projection for Go Location.Name: regions >=3
    /// share the "unknownregion" namespace. Aliases must agree on import.
    pub fn from_legacy_imported(
        expansion: u8,
        entries: Vec<(Vec<u8>, Tip)>,
    ) -> Result<Self, Error> {
        Self::build(expansion, entries, true)
    }
    fn build(
        expansion: u8,
        entries: Vec<(Vec<u8>, Tip)>,
        legacy_aliases: bool,
    ) -> Result<Self, Error> {
        let active = initialization::active_slices(expansion)?;
        let (regions, _) = initialization::hierarchy_size(expansion);
        if regions > 4 && !legacy_aliases {
            return Err(Error::Unsupported(
                "Go coordinator location names alias beyond four regions",
            ));
        }
        let wanted = 1 + regions as usize + active.len();
        if entries.len() != wanted {
            return Err(Error::Unavailable("incomplete candidate frontier"));
        }
        let mut tips = BTreeMap::new();
        for (location, tip) in entries {
            if tips.insert(location, tip).is_some() {
                return Err(Error::Invalid("duplicate frontier location"));
            }
        }
        if !tips.contains_key(&vec![])
            || (0..regions as u8).any(|r| !tips.contains_key(&vec![r]))
            || active.iter().any(|l| !tips.contains_key(l))
        {
            return Err(Error::Unavailable("candidate frontier locations"));
        }

        if legacy_aliases {
            let mut buckets: BTreeMap<Vec<u8>, &Tip> = BTreeMap::new();
            for (l, t) in &tips {
                let key = Self::legacy_key(l);
                if let Some(old) = buckets.insert(key, t) {
                    if old != t {
                        return Err(Error::Invalid("inconsistent legacy frontier aliases"));
                    }
                }
            }
        }
        Ok(Self {
            expansion,
            legacy_aliases,
            tips,
        })
    }
    fn legacy_key(location: &[u8]) -> Vec<u8> {
        let mut k = location.to_vec();
        if !k.is_empty() && k[0] >= 3 {
            k[0] = 3
        };
        k
    }
    pub fn uses_legacy_aliases(&self) -> bool {
        self.legacy_aliases
    }
    pub fn expansion(&self) -> u8 {
        self.expansion
    }
    pub fn tips(&self) -> &BTreeMap<Vec<u8>, Tip> {
        &self.tips
    }
    pub fn total(&self) -> U512 {
        self.tips
            .values()
            .fold(U512::ZERO, |s, t| s.wrapping_add(&t.entropy.resize()))
    }
    fn locations(&self, record: &Record, order: u8) -> Result<Vec<Vec<u8>>, Error> {
        if order > 2 {
            return Err(Error::Invalid("frontier order"));
        }
        let loc = fields::hash(&record.header().wire().location, "missing location")?;
        if loc.len() != 2 || loc.iter().any(|v| *v > 15) || !self.tips.contains_key(loc) {
            return Err(Error::Unsupported("frontier block location"));
        }
        let mut out = vec![];
        if order == 0 {
            out.push(vec![])
        }
        if order <= 1 {
            out.push(vec![loc[0]])
        }
        out.push(loc.to_vec());
        Ok(out)
    }
    pub fn extendable(&self, record: &Record, order: u8) -> Result<bool, Error> {
        for loc in self.locations(record, order)? {
            let tip = &self.tips[&loc];
            if record.parents[loc.len()] != tip.id {
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// Pure NodeSet.Update/Extendable projection with supplied entropy/order.
    pub fn updated_projection(
        &self,
        record: &Record,
        order: u8,
        entropy: Uint,
    ) -> Result<Self, Error> {
        let locations = self.locations(record, order)?;
        if !self.extendable(record, order)? {
            return Err(Error::Invalid("frontier parent mismatch"));
        }
        let mut next = self.clone();
        let tip = Tip {
            id: record.id(),
            numbers: record.numbers,
            entropy,
        };
        if self.legacy_aliases {
            let keys = locations
                .iter()
                .map(|l| Self::legacy_key(l))
                .collect::<Vec<_>>();
            for (l, t) in &mut next.tips {
                if keys.contains(&Self::legacy_key(l)) {
                    *t = tip.clone()
                }
            }
        } else {
            for loc in locations {
                next.tips.insert(loc, tip.clone());
            }
        }
        Ok(next)
    }
    /// Uses an immutable score computed by the native component path. The rest
    /// of this frontier is still imported and cross-context eligibility is open.
    pub fn updated_with_score(
        &self,
        record: &Record,
        score: &CandidateScore,
    ) -> Result<Self, Error> {
        if score.id() != record.id() || !score.work_checked() {
            return Err(Error::Invalid("frontier score binding"));
        }
        let m = score
            .metrics()
            .ok_or(Error::Invalid("missing score metrics"))?;
        if m.order as usize != score.context() {
            return Err(Error::Unsupported("frontier local score required"));
        }
        if fields::some(
            record.object().header().wire().expansion_number,
            "missing expansion number",
        )? as u8
            != self.expansion
        {
            return Err(Error::Unsupported("frontier expansion transition"));
        }
        self.updated_projection(record, m.order, score.total())
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct Offer {
    pub inserted: bool,
    pub evicted: Option<U512>,
    pub best_ever: U512,
}
#[derive(Clone)]
pub struct PendingFrontiers {
    capacity: usize,
    sets: BTreeMap<U512, Arc<Frontier>>,
    arrival: VecDeque<U512>,
    best_ever: U512,
    processing: Vec<U512>,
}
impl PendingFrontiers {
    pub fn new(capacity: usize) -> Result<Self, Error> {
        if capacity == 0 || capacity > MAX_PENDING {
            return Err(Error::ResourceLimit("pending frontier capacity"));
        }
        Ok(Self {
            capacity,
            sets: BTreeMap::new(),
            arrival: VecDeque::new(),
            best_ever: U512::ZERO,
            processing: vec![],
        })
    }
    pub fn len(&self) -> usize {
        self.sets.len()
    }
    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }
    pub fn best_ever(&self) -> U512 {
        self.best_ever
    }
    pub fn peek(&self, key: &U512) -> Option<&Frontier> {
        self.sets.get(key).map(Arc::as_ref)
    }
    pub fn descending(&self) -> impl Iterator<Item = (&U512, &Frontier)> {
        self.sets.iter().rev().map(|(k, v)| (k, v.as_ref()))
    }
    /// Mirrors Add/Get's no-touch lookup: first set at an entropy wins, and
    /// the oldest inserted key is evicted. The high-water mark never decreases.
    pub fn offer(&mut self, frontier: Frontier) -> Offer {
        let key = frontier.total();
        let inserted = !self.sets.contains_key(&key);
        let mut evicted = None;
        if inserted {
            // A full set has an oldest entry.
            if self.sets.len() == self.capacity {
                if let Some(old) = self.arrival.pop_front() {
                    self.sets.remove(&old);
                    self.processing.retain(|k| *k != old);
                    evicted = Some(old);
                }
            }
            self.sets.insert(key, Arc::new(frontier));
            self.arrival.push_back(key);
            self.processing.push(key);
        }
        self.best_ever = self.best_ever.max(key);
        Offer {
            inserted,
            evicted,
            best_ever: self.best_ever,
        }
    }
}

#[derive(Debug)]
pub struct ReapplicationReport {
    pub visited: usize,
    pub extended: usize,
    pub offers: Vec<Offer>,
    pub generation_ready: bool,
    pub fully_validated: bool,
    pub history_verified: bool,
}
impl PendingFrontiers {
    pub fn processing_order(&self) -> &[U512] {
        &self.processing
    }
    /// Pure source BuildPendingHeaders projection, including reverse processing
    /// order, the unconditional 21-visit bound, first-wins entropy and final sort.
    pub fn reapply_projection(
        &mut self,
        record: &Record,
        order: u8,
        entropy: Uint,
    ) -> Result<ReapplicationReport, Error> {
        self.reapply(|f| {
            if f.extendable(record, order)? {
                Ok(Some(f.updated_projection(record, order, entropy)?))
            } else {
                Ok(None)
            }
        })
    }
    /// Native score-bound reapplication additionally requires global PCRC gates.
    /// Any component/data error preserves the entire pending cache.
    pub fn reapply_native(
        &mut self,
        record: &Record,
        score: &CandidateScore,
        view: &crate::eligibility::CoordinatorView,
    ) -> Result<ReapplicationReport, Error> {
        if score.id() != record.id() || !score.work_checked() {
            return Err(Error::Invalid("reapplication score binding"));
        }
        self.reapply(|f| {
            if f.extendable(
                record,
                score
                    .metrics()
                    .ok_or(Error::Invalid("missing score metrics"))?
                    .order,
            )? {
                Ok(Some(f.updated_with_eligibility(record, score, view)?.0))
            } else {
                Ok(None)
            }
        })
    }
    fn reapply<F>(&mut self, mut update: F) -> Result<ReapplicationReport, Error>
    where
        F: FnMut(&Frontier) -> Result<Option<Frontier>, Error>,
    {
        let mut batch = vec![];
        let mut seen = std::collections::BTreeSet::new();
        let mut visited = 0;
        let mut extended = 0;
        for key in self.processing.iter().rev().take(21) {
            let f = self
                .peek(key)
                .ok_or(Error::Invalid("pending processing index"))?;
            visited += 1;
            if let Some(next) = update(f)? {
                extended += 1;
                if seen.insert(next.total()) {
                    batch.push(next)
                }
            }
        }
        let mut offers = vec![];
        for f in batch {
            offers.push(self.offer(f));
        }
        self.processing.sort();
        Ok(ReapplicationReport {
            visited,
            extended,
            offers,
            generation_ready: self.peek(&self.best_ever).is_some(),
            fully_validated: false,
            history_verified: false,
        })
    }
}
