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
            self.sets.remove(&old);
            self.order.retain(|k| *k != old);
        }
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
                self.order.push(key);
                self.insert(key, set);
            }
        }
        self.order.sort();
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
}
