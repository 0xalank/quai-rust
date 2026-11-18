//! Candidate append preparation: computed order, ancestry commitments and PCRC.
//! Candidate caches are unverified and never publish canonical chain state.
use crate::{
    history::{History, Record},
    rules::{self, Uint},
    share_rules,
    verifier::Verifier,
    Error,
};
use prost::Message;
use quai_primitives::{
    schema::{self, block::ProtoTermini, common::ProtoHash},
    Hash,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
/// Manifest bound of the private branch delta codec (`confirmation::delta`)
/// and of the candidate-index budget tests. Consensus has no manifest cap.
pub const MAX_MANIFEST: usize = 4096;
pub const MAX_AUX_RECORDS: usize = 4096;
pub const MAX_AUX_BYTES: usize = 32 << 20;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Termini {
    pub dominant: [Hash; 16],
    pub subordinate: [Hash; 16],
}
impl Termini {
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > 8192 {
            return Err(Error::ResourceLimit("termini encoding"));
        }
        schema::preflight(".block.ProtoTermini", bytes)
            .map_err(|_| Error::Invalid("termini protobuf"))?;
        let p = ProtoTermini::decode(bytes).map_err(|_| Error::Invalid("termini protobuf"))?;
        let read = |values: Vec<ProtoHash>| -> Result<[Hash; 16], Error> {
            if values.len() != 16 {
                return Err(Error::Invalid("termini width"));
            }
            let mut out = [[0; 32]; 16];
            for (i, v) in values.into_iter().enumerate() {
                if v.value.len() > 32 {
                    return Err(Error::Unsupported("termini hash width"));
                }
                out[i][32 - v.value.len()..].copy_from_slice(&v.value)
            }
            Ok(out)
        };
        Ok(Self {
            dominant: read(p.dom_termini)?,
            subordinate: read(p.sub_termini)?,
        })
    }
    pub fn encode(&self) -> Vec<u8> {
        ProtoTermini {
            dom_termini: self
                .dominant
                .iter()
                .map(|h| ProtoHash { value: h.to_vec() })
                .collect(),
            sub_termini: self
                .subordinate
                .iter()
                .map(|h| ProtoHash { value: h.to_vec() })
                .collect(),
        }
        .encode_to_vec()
    }
    pub fn genesis(id: Hash) -> Self {
        Self {
            dominant: [id; 16],
            subordinate: [id; 16],
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub manifest: Option<Vec<Hash>>,
    pub interlinks: Option<[Hash; 4]>,
    pub termini: Option<Termini>,
}
impl Snapshot {
    fn bytes(&self) -> Result<usize, Error> {
        let n = self.manifest.as_ref().map_or(0, Vec::len);
        Ok(n * 32
            + self.interlinks.as_ref().map_or(0, |_| 128)
            + self.termini.as_ref().map_or(0, |_| 1024)
            + 3)
    }
}
pub trait Reader: Send + Sync {
    fn read(&self, id: &Hash) -> Result<Option<Arc<Snapshot>>, Error>;
}
pub struct CandidateIndex {
    records: BTreeMap<Hash, Arc<Snapshot>>,
    bytes: usize,
    source: Option<Arc<dyn Reader>>,
    cache: crate::history::Cache<Snapshot>,
}
impl Default for CandidateIndex {
    fn default() -> Self {
        Self {
            records: Default::default(),
            bytes: 0,
            source: None,
            cache: crate::history::Cache::new(256, 8 << 20),
        }
    }
}
impl CandidateIndex {
    pub fn with_reader(reader: Arc<dyn Reader>) -> Self {
        Self {
            source: Some(reader),
            ..Self::default()
        }
    }
    fn find(&self, id: &Hash) -> Result<Option<Arc<Snapshot>>, Error> {
        if let Some(v) = self.records.get(id) {
            return Ok(Some(v.clone()));
        }
        let Some(source) = &self.source else {
            return Ok(None);
        };
        self.cache
            .get(id, || source.read(id), |v| v.bytes().unwrap_or(usize::MAX))
    }
    pub(crate) fn check_insert(&self, id: &Hash, snapshot: &Snapshot) -> Result<(), Error> {
        let bytes = snapshot.bytes()?;
        if let Some(old) = self.find(id)? {
            if old.as_ref() != snapshot {
                return Err(Error::Invalid("conflicting candidate ancestry"));
            }
            return Ok(());
        }
        if self.records.len() == MAX_AUX_RECORDS
            || self
                .bytes
                .checked_add(bytes)
                .is_none_or(|n| n > MAX_AUX_BYTES)
        {
            return Err(Error::ResourceLimit("candidate ancestry index"));
        }
        Ok(())
    }
    pub fn insert_imported(&mut self, id: Hash, snapshot: Snapshot) -> Result<(), Error> {
        self.check_insert(&id, &snapshot)?;
        if self.records.contains_key(&id) {
            return Ok(());
        }
        self.bytes += snapshot.bytes()?;
        self.records.insert(id, Arc::new(snapshot));
        Ok(())
    }
    pub fn get(&self, id: &Hash) -> Result<Arc<Snapshot>, Error> {
        self.find(id)?
            .ok_or(Error::Unavailable("candidate ancestry snapshot"))
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    pub fn tracked_payload_bytes(&self) -> usize {
        self.bytes
    }
}
#[derive(Clone, Debug)]
pub struct Profile {
    pub location: Vec<u8>,
    pub genesis: BTreeSet<Hash>,
    pub default_genesis: Option<Hash>,
    pub minimum_difficulty: Uint,
    pub duration: u64,
    pub gas_ceiling: u64,
    pub p2p_threshold: u16,
}
impl Profile {
    pub fn context(&self) -> usize {
        self.location.len()
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.location.len() > 2
            || self.location.iter().any(|n| *n > 15)
            || self.genesis.len() > 256
            || self.minimum_difficulty == Uint::ZERO
            || self.duration == 0
            || self.p2p_threshold == 0
            || self.p2p_threshold > 255
        {
            return Err(Error::Invalid("placement profile"));
        }
        Ok(())
    }
    fn zone(&self) -> crate::validation::Profile {
        crate::validation::Profile {
            location: self.location.clone(),
            genesis: self.genesis.clone(),
            default_genesis: self.default_genesis,
            minimum_difficulty: self.minimum_difficulty,
            duration: self.duration,
            gas_ceiling: self.gas_ceiling,
            p2p_threshold: self.p2p_threshold,
        }
    }
    fn hierarchy(&self) -> crate::hierarchy_header::Profile {
        crate::hierarchy_header::Profile {
            location: self.location.clone(),
            genesis: self.genesis.clone(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Local,
    Dominant,
    OtherContext,
}
pub fn route(context: usize, order: u8) -> Result<Route, Error> {
    if context > 2 || order > 2 {
        return Err(Error::Invalid("placement context/order"));
    }
    Ok(if order as usize == context {
        Route::Local
    } else if (order as usize) < context {
        Route::Dominant
    } else {
        Route::OtherContext
    })
}
/// Commitments for lists of fixed 32-byte hashes (RLP string prefix 0xa0).
/// go-quai has no manifest length limit: a region manifest grows with every
/// zone block until the next region block.
pub fn hash_list_root(values: &[Hash]) -> Result<Hash, Error> {
    let encoded = values
        .iter()
        .map(|h| {
            let mut v = Vec::with_capacity(33);
            v.push(0xa0);
            v.extend_from_slice(h);
            v
        })
        .collect::<Vec<_>>();
    quai_primitives::trie::ordered_root(&encoded)
        .map_err(|_| Error::Unsupported("candidate hash-list root"))
}
pub fn check_append_commitments(
    index: &CandidateIndex,
    header: &Record,
    context: usize,
) -> Result<(), Error> {
    if context > 2 {
        return Err(Error::Invalid("append context"));
    }
    let parent = index.get(&header.parents[context])?;
    let b = header.object.header().wire();
    if context > 0 {
        let manifest = parent
            .manifest
            .as_ref()
            .ok_or(Error::Unavailable("parent local manifest"))?;
        // Go rawdb decodes a stored empty manifest as nil; AppendHeader
        // treats that as unavailable in region/zone context.
        if manifest.is_empty() {
            return Err(Error::Unavailable("empty parent local manifest"));
        }
        if hash_list_root(manifest)?.as_slice() != b.manifest_hash[context].value {
            return Err(Error::Invalid("parent manifest commitment"));
        }
    } else {
        let links = parent
            .interlinks
            .as_ref()
            .ok_or(Error::Unavailable("parent interlinks"))?;
        if hash_list_root(links)?.as_slice() != b.interlink_root_hash.as_ref().unwrap().value {
            return Err(Error::Invalid("parent interlink commitment"));
        }
    }
    Ok(())
}
pub fn next_manifest(
    context: usize,
    order: u8,
    id: Hash,
    parent: Option<&[Hash]>,
) -> Result<Vec<Hash>, Error> {
    if context > 2 || order > 2 {
        return Err(Error::Invalid("manifest context/order"));
    }
    if context == 0 {
        return Ok(vec![]);
    }
    if (order as usize) < context {
        return Ok(vec![id]);
    }
    let parent = parent.ok_or(Error::Unavailable("parent local manifest"))?;
    let mut next = parent.to_vec();
    next.push(id);
    Ok(next)
}
pub fn next_interlinks(
    id: Hash,
    parent: Option<&[Hash; 4]>,
    rank: u8,
    trusted_genesis: bool,
) -> Result<[Hash; 4], Error> {
    if trusted_genesis {
        return Ok([id; 4]);
    }
    if rank > 4 {
        return Err(Error::Invalid("interlink rank"));
    }
    let mut next = *parent.ok_or(Error::Unavailable("parent interlinks"))?;
    next[..rank as usize].fill(id);
    Ok(next)
}
/// Exact previous-coincident-reference transition. It computes a candidate value;
/// callers cannot establish canonical authority by inserting its result.
pub fn pcrc(
    parent: &Termini,
    id: Hash,
    header_location: &[u8],
    node_location: &[u8],
    dominant_origin: bool,
    claimed_dominant: Hash,
    trusted_genesis: &BTreeSet<Hash>,
) -> Result<(Hash, Termini), Error> {
    if node_location.len() > 2
        || node_location.iter().any(|v| *v > 15)
        || header_location.len() != 2
        || header_location.iter().any(|v| *v > 15)
    {
        return Err(Error::Unsupported("PCRC location domain"));
    }
    let ctx = node_location.len();
    let old_dom_index = if ctx == 0 {
        0
    } else {
        node_location[ctx - 1] as usize
    };
    let new_dom_index = if ctx == 0 {
        0
    } else {
        header_location[ctx - 1] as usize
    };
    let old_dom = parent.dominant[old_dom_index];
    let mut next = parent.clone();
    let sub_index = if ctx < 2 {
        header_location[ctx] as usize
    } else {
        0
    };
    if ctx < 2 {
        next.subordinate[sub_index] = id
    }
    next.dominant[new_dom_index] = if ctx == 0 || dominant_origin {
        id
    } else {
        old_dom
    };
    if dominant_origin && !trusted_genesis.contains(&old_dom) && old_dom != claimed_dominant {
        return Err(Error::Invalid("cyclic coincident reference"));
    }
    Ok((
        if ctx == 2 {
            [0; 32]
        } else {
            parent.subordinate[sub_index]
        },
        next,
    ))
}
#[derive(Clone, Debug)]
pub struct OrderProjection {
    pub order: u8,
    pub rank: Option<u8>,
}
/// Controlled work-hash projection used only for differential arithmetic tests.
pub fn order_projection(
    header: &Record,
    hash: Hash,
    context: usize,
    trusted_genesis: bool,
) -> Result<OrderProjection, Error> {
    header.verify_binding()?;
    if context > 2 {
        return Err(Error::Invalid("placement context"));
    }
    if header.numbers[context] == 0 {
        if !trusted_genesis {
            return Err(Error::Unsupported("unqualified zero-number placement"));
        }
        return Ok(OrderProjection {
            order: 0,
            rank: Some(0),
        });
    }
    let b = header.object.header().wire();
    let mut pd = [Uint::ZERO; 3];
    for (i, v) in pd.iter_mut().enumerate() {
        *v = share_rules::uint(&b.parent_delta_entropy[i])?
    }
    let e = rules::entropy(&rules::EntropyInput {
        hash,
        difficulty: header.difficulty()?,
        expansion: b.expansion_number.unwrap() as u8,
        parent_entropy: [Uint::ZERO; 3],
        parent_delta: pd,
        parent_uncled_delta: [Uint::ZERO; 3],
        uncled_entropy: Uint::ZERO,
        postfork_share_count: None,
        node_context: 0,
    })?;
    Ok(OrderProjection {
        order: e.order,
        rank: e.rank,
    })
}
#[derive(Clone, Debug)]
pub struct Placement {
    header: Hash,
    pub order: u8,
    pub rank: Option<u8>,
    pub route: Route,
    pub candidate_work_checked: bool,
    pub candidate_order_checked: bool,
}
pub fn verify_placement(
    verifier: &mut Verifier,
    header: &Record,
    profile: &Profile,
) -> Result<Placement, Error> {
    profile.validate()?;
    header.verify_binding()?;
    let aux_id = header
        .header
        .wire()
        .aux_pow
        .as_ref()
        .map(|v| crate::auxiliary::Auxiliary::decode(v).map(|a| a.id()))
        .transpose()?;
    if !rules::pow_id_valid(header.prime, aux_id, false) {
        return Err(Error::Invalid("placement proof ID"));
    }
    let context = profile.context();
    let trusted = profile.genesis.contains(&header.id);
    let projection = if header.numbers[context] == 0 {
        order_projection(header, [0; 32], context, trusted)?
    } else {
        let work = verifier.verify_header_proof(&header.header)?;
        order_projection(header, work.hash(), context, trusted)?
    };
    Ok(Placement {
        header: header.id,
        order: projection.order,
        rank: projection.rank,
        route: route(context, projection.order)?,
        candidate_work_checked: header.numbers[context] != 0,
        candidate_order_checked: true,
    })
}
#[derive(Clone, Debug)]
pub struct Prepared {
    pub placement: Placement,
    pub previous_dominant: Hash,
    pub next: Snapshot,
    pub header_policy_checked: bool,
    pub ancestry_checked: bool,
    pub body_checked: bool,
    pub fully_validated: bool,
    pub history_verified: bool,
}
/// Compose proof/order, existing header/body policy, parent commitments and PCRC.
/// It never mutates the supplied history/index or publishes a canonical branch.
pub fn prepare_append_components(
    verifier: &mut Verifier,
    history: &History,
    index: &CandidateIndex,
    header: &Record,
    profile: &Profile,
    now: u64,
    dominant_origin: bool,
    claimed_dominant: Hash,
) -> Result<Prepared, Error> {
    profile.validate()?;
    let ctx = profile.context();
    let placement = verify_placement(verifier, header, profile)?;
    let eligible = matches!(
        (placement.route, dominant_origin),
        (Route::Local, false) | (Route::Dominant, true)
    );
    if !eligible {
        return Err(Error::Unsupported(
            "append path does not match computed order",
        ));
    }
    if placement.header != header.id {
        return Err(Error::Invalid("placement/header binding"));
    }
    if ctx == 2 {
        crate::zone_header::inspect_zone_candidate(
            verifier,
            history,
            header,
            &profile.zone(),
            now,
        )?;
    } else {
        crate::hierarchy_header::inspect_candidate(
            verifier,
            history,
            header,
            &profile.hierarchy(),
            now,
        )?;
    }
    check_append_commitments(index, header, ctx)?;
    let parent = index.get(&header.parents[ctx])?;
    let (previous_dominant, termini) = pcrc(
        parent
            .termini
            .as_ref()
            .ok_or(Error::Unavailable("parent termini"))?,
        header.id,
        &header.header.wire().location.as_ref().unwrap().value,
        &profile.location,
        dominant_origin,
        claimed_dominant,
        &profile.genesis,
    )?;
    let manifest = next_manifest(ctx, placement.order, header.id, parent.manifest.as_deref())?;
    let interlinks = if ctx == 0 {
        Some(next_interlinks(
            header.id,
            parent.interlinks.as_ref(),
            placement
                .rank
                .ok_or(Error::Invalid("prime interlink rank"))?,
            profile.genesis.contains(&header.id),
        )?)
    } else {
        None
    };
    Ok(Prepared {
        placement,
        previous_dominant,
        next: Snapshot {
            manifest: Some(manifest),
            interlinks,
            termini: Some(termini),
        },
        header_policy_checked: true,
        ancestry_checked: true,
        body_checked: true,
        fully_validated: false,
        history_verified: false,
    })
}
pub fn bootstrap_candidate(header: &Record, profile: &Profile) -> Result<Snapshot, Error> {
    profile.validate()?;
    if !profile.genesis.contains(&header.id) {
        return Err(Error::Unsupported("explicit trusted genesis required"));
    }
    Ok(Snapshot {
        // Slice initialization stores the genesis singleton in every context.
        manifest: Some(vec![header.id]),
        interlinks: if profile.context() == 0 {
            Some([header.id; 4])
        } else {
            None
        },
        termini: Some(Termini::genesis(header.id)),
    })
}
