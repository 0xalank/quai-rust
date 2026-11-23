//! go-quai v0.57.0's head choice for slice [0 0].
//!
//! go-quai's hierarchical coordinator (`cmd/utils/hierarchical_coordinator.go`)
//! keeps entropy-keyed node sets: one node (hash, entropy) per location
//! (prime, region 0, zone 0-0 at expansion 0). Every appended block whose
//! order is the appending backend's context sends a `ChainEvent` with its
//! `TotalLogEntropy` in that context; `BuildPendingHeaders` extends each of
//! the (at most 21) highest node sets the block extends (`NodeSet.Extendable`),
//! updates the nodes of its order and below (`NodeSet.Update`), and keeps the
//! result keyed by its total (`NodeSet.Entropy`: the sum of the nodes) in a
//! 2,000-entry cache (`c_pendingHeaderSize`). The highest total ever seen
//! (`bestEntropy`) names the node set whose zone node becomes the head
//! (`GeneratePendingHeader` -> `HeaderChain.SetCurrentHeader`).
//!
//! `SetCurrentHeader` (core/headerchain.go:966) refuses a head whose common
//! ancestor with the current head is more than 60 zone blocks
//! (`c_zoneHorizonThreshold`) below it once the current head is above
//! MaxCodeSizeForkHeight (zone 3,490,000); `Slice.Append` refuses a block
//! more than 60 zone blocks above the current head (`ErrSubNotSyncedToDom`).
//!
//! Not ported: the asynchronous retries (`ReapplicationLoop` and the 5-second
//! `PendingHeadersMap` backup), which only matter when blocks arrive out of
//! order; the node feeds blocks after their ancestors.
use crypto_bigint::U256;
use quai_primitives::Hash;
use std::collections::{HashMap, VecDeque};

/// `c_pendingHeaderSize`.
pub const PENDING: usize = 2000;
/// The miss `threshold` of `BuildPendingHeaders`: at most 21 node sets are
/// tried per block.
pub const TRIES: usize = 21;
/// `c_zoneHorizonThreshold`.
pub const ZONE_HORIZON: u64 = 60;
/// `params.MaxCodeSizeForkHeight`: the reorg horizon applies above it.
pub const HORIZON_FROM: u64 = 3_490_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    pub hash: Hash,
    pub entropy: U256,
}
/// The prime, region and zone node of one candidate (expansion 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeSet {
    pub nodes: [Node; 3],
}
impl NodeSet {
    /// Every node at `hash` with `entropy` (the genesis, or a restart seed).
    pub fn at(hash: Hash, entropy: U256) -> Self {
        Self {
            nodes: [Node { hash, entropy }; 3],
        }
    }
    /// `NodeSet.Extendable`: the block's parents in its order's context and
    /// below are this set's nodes.
    pub fn extendable(&self, parents: &[Hash; 3], order: u8) -> bool {
        (order as usize..3).all(|c| parents[c] == self.nodes[c].hash)
    }
    /// `NodeSet.Update`: the block replaces the nodes of its order and below.
    pub fn update(&mut self, hash: Hash, entropy: U256, order: u8) {
        for c in order as usize..3 {
            self.nodes[c] = Node { hash, entropy };
        }
    }
    /// `NodeSet.Entropy` for one region and one zone.
    pub fn entropy(&self) -> U256 {
        self.nodes
            .iter()
            .fold(U256::ZERO, |a, n| a.wrapping_add(&n.entropy))
    }
    pub fn zone(&self) -> Hash {
        self.nodes[2].hash
    }
}

/// Multi-slice node set for arbitrary hierarchy expansion numbers
/// (`quai_primitives::spec::hierarchy_size(expansion)` = `(regions, zones)`).
/// Keeps one prime node, `regions` region nodes, and `regions * zones` zone
/// nodes so coordinator fork choice works cleanly beyond expansion 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpandedNodeSet {
    pub expansion: u8,
    pub regions: usize,
    pub zones: usize,
    pub prime: Node,
    pub region_nodes: Vec<Node>,
    pub zone_nodes: Vec<Node>,
}
impl ExpandedNodeSet {
    pub fn at(expansion: u8, hash: Hash, entropy: U256) -> Self {
        let (r, z) = quai_primitives::spec::hierarchy_size(expansion);
        let regions = r as usize;
        let zones = z as usize;
        let node = Node { hash, entropy };
        Self {
            expansion,
            regions,
            zones,
            prime: node,
            region_nodes: vec![node; regions],
            zone_nodes: vec![node; regions * zones],
        }
    }
    pub fn zone_index(&self, slice: [u8; 2]) -> Option<usize> {
        let r = slice[0] as usize;
        let z = slice[1] as usize;
        if r < self.regions && z < self.zones {
            Some(r * self.zones + z)
        } else {
            None
        }
    }
    pub fn extendable(&self, slice: [u8; 2], parents: &[Hash; 3], order: u8) -> bool {
        let Some(zi) = self.zone_index(slice) else {
            return false;
        };
        let ri = slice[0] as usize;
        match order {
            0 => {
                parents[0] == self.prime.hash
                    && parents[1] == self.region_nodes[ri].hash
                    && parents[2] == self.zone_nodes[zi].hash
            }
            1 => parents[1] == self.region_nodes[ri].hash && parents[2] == self.zone_nodes[zi].hash,
            2 => parents[2] == self.zone_nodes[zi].hash,
            _ => false,
        }
    }
    pub fn update(&mut self, slice: [u8; 2], hash: Hash, entropy: U256, order: u8) -> bool {
        let Some(zi) = self.zone_index(slice) else {
            return false;
        };
        let ri = slice[0] as usize;
        let node = Node { hash, entropy };
        match order {
            0 => {
                self.prime = node;
                self.region_nodes[ri] = node;
                self.zone_nodes[zi] = node;
                true
            }
            1 => {
                self.region_nodes[ri] = node;
                self.zone_nodes[zi] = node;
                true
            }
            2 => {
                self.zone_nodes[zi] = node;
                true
            }
            _ => false,
        }
    }
    pub fn entropy(&self) -> U256 {
        let mut total = self.prime.entropy;
        for n in &self.region_nodes {
            total = total.wrapping_add(&n.entropy);
        }
        for n in &self.zone_nodes {
            total = total.wrapping_add(&n.entropy);
        }
        total
    }
    pub fn zone(&self, slice: [u8; 2]) -> Option<Hash> {
        self.zone_index(slice).map(|i| self.zone_nodes[i].hash)
    }
}

/// The entropy-keyed pending node sets and the best entropy.
pub struct Choice {
    sets: HashMap<U256, NodeSet>,
    /// Insertion order, for the cache eviction (go-quai only adds keys and
    /// reads with Peek, so its LRU evicts the oldest insertion).
    inserted: VecDeque<U256>,
    /// `pendingHeaders.order`, sorted after each block.
    order: Vec<U256>,
    best: U256,
}
impl Choice {
    /// Start with one node set keyed at its entropy.
    pub fn new(seed: NodeSet) -> Self {
        Self::keyed(seed, seed.entropy())
    }
    /// Start with one node set under `key`: go-quai keys the genesis set at
    /// 0 (`InitPendingHeaders`) and, after a restart, the set of the first
    /// block's parents at that block's entropy (`ChainEventLoop`).
    pub fn keyed(seed: NodeSet, key: U256) -> Self {
        let mut c = Self {
            sets: HashMap::new(),
            inserted: VecDeque::new(),
            order: vec![],
            best: U256::ZERO,
        };
        c.insert(key, seed);
        c.order.push(key);
        c.best = key;
        c
    }
    fn insert(&mut self, key: U256, set: NodeSet) {
        self.sets.insert(key, set);
        self.inserted.push_back(key);
        while self.sets.len() > PENDING {
            let Some(old) = self.inserted.pop_front() else {
                break;
            };
            // Never evict the current best node set; keep it at the back of
            // the eviction queue so it stays addressable across low-entropy bursts.
            if old == self.best && self.sets.len() > 1 {
                self.inserted.push_back(old);
                continue;
            }
            self.sets.remove(&old);
            if let Ok(pos) = self.order.binary_search(&old) {
                self.order.remove(pos);
            }
        }
    }
    /// Remove every candidate node set referencing `hash` (for example after
    /// state execution rejects a block that passed stateless checks), and
    /// recompute `best` from the remaining sets.
    pub fn remove_hash(&mut self, hash: &Hash) {
        let mut removed = Vec::new();
        self.sets.retain(|k, set| {
            let keep = !set.nodes.iter().any(|n| n.hash == *hash);
            if !keep {
                removed.push(*k);
            }
            keep
        });
        if removed.is_empty() {
            return;
        }
        self.inserted.retain(|k| self.sets.contains_key(k));
        self.order.retain(|k| self.sets.contains_key(k));
        self.best = self.order.last().copied().unwrap_or(U256::ZERO);
    }
    pub fn best_entropy(&self) -> U256 {
        self.best
    }
    /// The node set at the best entropy, if the cache still holds it.
    pub fn best(&self) -> Option<NodeSet> {
        self.sets.get(&self.best).copied()
    }
    pub fn len(&self) -> usize {
        self.sets.len()
    }
    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }
    /// `BuildPendingHeaders` for a block with `parents` (prime, region and
    /// zone parent hashes), `order` and `entropy` (`TotalLogEntropy` in the
    /// context of its order). Returns the best node set afterwards.
    pub fn build(
        &mut self,
        parents: [Hash; 3],
        hash: Hash,
        order: u8,
        entropy: U256,
    ) -> Option<NodeSet> {
        let mut fresh: Vec<(U256, NodeSet)> = vec![];
        for key in self.order.iter().rev().take(TRIES) {
            let Some(set) = self.sets.get(key) else {
                continue;
            };
            if set.extendable(&parents, order) {
                let mut next = *set;
                next.update(hash, entropy, order);
                let e = next.entropy();
                if !fresh.iter().any(|(k, _)| *k == e) {
                    fresh.push((e, next));
                }
                if self.best < e {
                    self.best = e;
                }
            }
        }
        for (key, set) in fresh {
            if !self.sets.contains_key(&key) {
                if let Err(pos) = self.order.binary_search(&key) {
                    self.order.insert(pos, key);
                }
                self.insert(key, set);
            }
        }
        self.best()
    }
}

/// `SetCurrentHeader`'s horizon: whether the head may move from `current`
/// (zone number) to a block whose common ancestor with it is at `common`.
pub fn reorg_allowed(current: u64, common: u64) -> bool {
    !(current > HORIZON_FROM && current > common + ZONE_HORIZON)
}
/// `Slice.Append`'s horizon: whether a block at zone `number` may be
/// appended while the head is at `current`.
pub fn append_allowed(current: u64, number: u64) -> bool {
    number <= current + ZONE_HORIZON
}

#[cfg(test)]
mod tests {
    use super::*;
    use quai_consensus::history::Record;
    use quai_primitives::decode_hex;
    fn h(v: &serde_json::Value) -> Hash {
        decode_hex(v.as_str().unwrap()).unwrap().try_into().unwrap()
    }
    fn n(v: &serde_json::Value) -> U256 {
        let digits = v.as_str().unwrap();
        let mut out = U256::ZERO;
        for d in digits.bytes() {
            out = out
                .wrapping_mul(&U256::from_u64(10))
                .wrapping_add(&U256::from_u64((d - b'0') as u64));
        }
        out
    }
    /// go-quai's coordinator on the private fixture (reference/canonical):
    /// branch 1 first, then branch 0 from height 9 on; the second branch
    /// takes the head at its 15th block, and offering the tip again changes
    /// nothing.
    #[test]
    fn heads_and_scores_equal_go_quai() {
        let doc: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json"))
                .unwrap();
        let genesis = h(&doc["genesis_hash"]);
        let mut choice = Choice::keyed(NodeSet::at(genesis, U256::ZERO), U256::ZERO);
        let first = doc["first_branch"].as_u64().unwrap() as usize;
        let mut events = doc["canonical_events"].as_array().unwrap().iter();
        let mut selected = genesis;
        let offer = |choice: &mut Choice, branch: usize, i: usize| -> (Hash, U256) {
            let v = &doc["branches"][branch]["vectors"][i];
            let raw = decode_hex(v["work_object"].as_str().unwrap()).unwrap();
            let r = Record::decode(&raw).unwrap();
            let parents = [
                r.parent(0).unwrap(),
                r.parent(1).unwrap(),
                r.parent(2).unwrap(),
            ];
            let order = v["order"].as_u64().unwrap() as u8;
            let entropy = n(&v["poem"][2]["total"]);
            let best = choice.build(parents, r.id(), order, entropy).unwrap();
            (best.zone(), choice.best_entropy())
        };
        for (pass, branch) in [(0, first), (1, 1 - first)] {
            let start = if pass == 1 { 8 } else { 0 };
            for i in start..16 {
                let (head, best) = offer(&mut choice, branch, i);
                let e = events.next().unwrap();
                assert_eq!(
                    (
                        e["branch"].as_u64().unwrap() as usize,
                        e["index"].as_u64().unwrap() as usize
                    ),
                    (branch, i)
                );
                assert_eq!(head, h(&e["head"]), "branch {branch} block {i}");
                assert_eq!(best, n(&e["best_score"]), "branch {branch} block {i}");
                assert_eq!(head != selected, e["selected"].as_bool().unwrap());
                selected = head;
            }
        }
        let (head, best) = offer(&mut choice, 1 - first, 15);
        let e = events.next().unwrap();
        assert_eq!(e["duplicate"], true);
        assert_eq!((head, best), (h(&e["head"]), n(&e["best_score"])));
        assert!(events.next().is_none());
    }
    #[test]
    fn node_sets_follow_go_rules() {
        let a = NodeSet::at([1; 32], U256::from_u64(5));
        assert_eq!(a.entropy(), U256::from_u64(15));
        let mut b = a;
        b.update([2; 32], U256::from_u64(7), 2);
        assert_eq!(b.nodes[1].hash, [1; 32]);
        assert_eq!(b.entropy(), U256::from_u64(17));
        assert!(a.extendable(&[[9; 32], [9; 32], [1; 32]], 2));
        assert!(!a.extendable(&[[9; 32], [9; 32], [1; 32]], 1));
        assert!(a.extendable(&[[1; 32]; 3], 0));
        // The horizon applies only above MaxCodeSizeForkHeight.
        assert!(reorg_allowed(HORIZON_FROM, 0));
        assert!(reorg_allowed(HORIZON_FROM + 100, HORIZON_FROM + 40));
        assert!(!reorg_allowed(HORIZON_FROM + 100, HORIZON_FROM + 39));
        assert!(append_allowed(100, 160) && !append_allowed(100, 161));
    }
    #[test]
    fn the_cache_evicts_the_oldest_sets() {
        let mut c = Choice::new(NodeSet::at([0; 32], U256::ZERO));
        let mut parent = [0u8; 32];
        for i in 1..=(PENDING as u64 + 10) {
            let mut hash = [0u8; 32];
            hash[..8].copy_from_slice(&i.to_be_bytes());
            c.build([[0; 32], [0; 32], parent], hash, 2, U256::from_u64(i));
            parent = hash;
        }
        assert_eq!(c.len(), PENDING);
        assert_eq!(c.best().unwrap().zone(), parent);
    }
    /// go-quai's restart seed: the first block's parents keyed at the
    /// block's own entropy, so that the block's own set (three times that
    /// entropy when all nodes carry it) does not collide with the seed.
    #[test]
    fn a_restart_seed_lets_the_first_block_extend_it() {
        let e = U256::from_u64(7);
        let mut c = Choice::keyed(NodeSet::at([1; 32], e), e);
        let best = c.build([[1; 32]; 3], [2; 32], 2, e).unwrap();
        assert_eq!(best.zone(), [2; 32]);
        let best = c
            .build([[1; 32], [1; 32], [2; 32]], [3; 32], 2, U256::from_u64(8))
            .unwrap();
        assert_eq!(best.zone(), [3; 32]);
    }
    #[test]
    fn expanded_node_set_tracks_multi_slice_hierarchies() {
        let mut s1 = ExpandedNodeSet::at(1, [0; 32], U256::from_u64(1));
        assert_eq!((s1.regions, s1.zones), (1, 2));
        assert_eq!(s1.zone_index([0, 0]), Some(0));
        assert_eq!(s1.zone_index([0, 1]), Some(1));
        assert_eq!(s1.zone_index([1, 0]), None);
        assert!(s1.extendable([0, 1], &[[0; 32]; 3], 2));
        assert!(s1.update([0, 1], [0xab; 32], U256::from_u64(10), 2));
        assert_eq!(s1.zone_nodes[1].hash, [0xab; 32]);
        assert_eq!(s1.zone_nodes[0].hash, [0; 32]);

        let s2 = ExpandedNodeSet::at(2, [1; 32], U256::from_u64(2));
        assert_eq!((s2.regions, s2.zones), (2, 2));
        assert_eq!(s2.zone_nodes.len(), 4);
        assert_eq!(s2.zone_index([1, 1]), Some(3));
    }
}
