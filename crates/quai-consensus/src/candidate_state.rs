//! In-memory candidate branches. Neither fork choice nor durable canonical state.
use crate::{
    history::{History, Record},
    placement::{self, CandidateIndex, Prepared, Profile, Snapshot},
    verifier::Verifier,
    Error,
};
use quai_primitives::Hash;
pub struct CandidateState {
    history: History,
    index: CandidateIndex,
    profile: Profile,
}
impl CandidateState {
    pub fn new(profile: Profile) -> Result<Self, Error> {
        profile.validate()?;
        Ok(Self {
            history: History::default(),
            index: CandidateIndex::default(),
            profile,
        })
    }
    pub fn history(&self) -> &History {
        &self.history
    }
    pub fn index(&self) -> &CandidateIndex {
        &self.index
    }
    pub fn profile(&self) -> &Profile {
        &self.profile
    }
    /// Supplied ancestry remains unverified. Both stores are checked before
    /// either changes, including duplicate conflicts and separate budgets.
    pub fn import_unverified_ancestry(
        &mut self,
        record: Record,
        snapshot: Snapshot,
    ) -> Result<Hash, Error> {
        self.history.check_insert(&record)?;
        self.index.check_insert(&record.id(), &snapshot)?;
        let id = self.history.insert(record)?;
        self.index.insert_imported(id, snapshot)?;
        Ok(id)
    }
    pub fn seed_trusted(&mut self, record: Record) -> Result<Hash, Error> {
        let snapshot = placement::bootstrap_candidate(&record, &self.profile)?;
        self.import_unverified_ancestry(record, snapshot)
    }
    /// Recomputes placement and all supported components even for known IDs.
    /// A success stores candidate data only; no branch is selected or published.
    pub fn prepare_and_insert(
        &mut self,
        verifier: &mut Verifier,
        record: Record,
        now: u64,
        dominant_origin: bool,
        claimed_dominant: Hash,
    ) -> Result<Prepared, Error> {
        let prepared = placement::prepare_append_components(
            verifier,
            &self.history,
            &self.index,
            &record,
            &self.profile,
            now,
            dominant_origin,
            claimed_dominant,
        )?;
        self.import_unverified_ancestry(record, prepared.next.clone())?;
        Ok(prepared)
    }
    /// Score before either store changes; arithmetic/unsupported score failures
    /// cannot leave a candidate inserted without its score.
    pub fn score_and_insert(
        &mut self,
        verifier: &mut Verifier,
        record: Record,
        now: u64,
    ) -> Result<crate::scoring::CandidateScore, Error> {
        let score = crate::scoring::score_candidate_components(
            verifier,
            &self.history,
            &self.index,
            &record,
            &self.profile,
            now,
        )?;
        let snapshot = score
            .components()
            .ok_or(Error::Unsupported("seed anchors explicitly"))?
            .next
            .clone();
        self.import_unverified_ancestry(record, snapshot)?;
        Ok(score)
    }
}
