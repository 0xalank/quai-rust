//! go-quai `vm.AccessListTracer` (core/vm/access_list_tracer.go): the
//! accounts and storage slots that a call touches, for `quai_createAccessList`
//! and `quai_estimateGas`. It runs only in the RPC call mode
//! (`evm::message`); consensus execution never has a tracer.
//!
//! Go keeps the list in maps and returns it in map order, which is random.
//! Here the list is sorted by address and key. The order changes no result:
//! intrinsic gas and the warm set count entries, not positions.
use super::word::{address, bytes, Word};
use quai_primitives::{
    transaction::{AccessTuple, Address},
    Hash,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessListTracer {
    exclude: BTreeSet<Address>,
    list: BTreeMap<Address, BTreeSet<Hash>>,
}

impl AccessListTracer {
    /// `NewAccessListTracer(acl, from, to, precompiles)`. An excluded address
    /// of `seed` stays in the list only when it has storage keys: Go's
    /// `addSlot` does not check the exclusions.
    pub fn new(seed: &[AccessTuple], from: Address, to: Address, precompiles: &[Address]) -> Self {
        let mut exclude: BTreeSet<Address> = [from, to].into_iter().collect();
        exclude.extend(precompiles.iter().copied());
        let mut tracer = Self {
            exclude,
            list: BTreeMap::new(),
        };
        for tuple in seed {
            if !tracer.exclude.contains(&tuple.address) {
                tracer.list.entry(tuple.address).or_default();
            }
            for key in &tuple.keys {
                tracer.list.entry(tuple.address).or_default().insert(*key);
            }
        }
        tracer
    }
    /// `CaptureState` for operation `op` of the frame whose storage is
    /// `contract`, with `stack` (top last) before the operation runs.
    pub(crate) fn capture(&mut self, op: u8, stack: &[Word], contract: Address) {
        let top = |n: usize| {
            stack
                .len()
                .checked_sub(n + 1)
                .and_then(|i| stack.get(i))
                .copied()
        };
        match op {
            // SLOAD, SSTORE
            0x54 | 0x55 => {
                if let Some(slot) = top(0) {
                    self.list.entry(contract).or_default().insert(bytes(slot));
                }
            }
            // EXTCODECOPY, EXTCODEHASH, EXTCODESIZE, BALANCE, SELFDESTRUCT
            0x3c | 0x3f | 0x3b | 0x31 | 0xff => {
                if let Some(a) = top(0) {
                    self.add_unexcluded(address(a));
                }
            }
            // DELEGATECALL, CALL, STATICCALL, CALLCODE: Go reads the address
            // when the stack has at least 5 items, also on a stack underflow.
            0xf4 | 0xf1 | 0xfa | 0xf2 if stack.len() >= 5 => {
                if let Some(a) = top(1) {
                    self.add_unexcluded(address(a));
                }
            }
            _ => {}
        }
    }
    fn add_unexcluded(&mut self, a: Address) {
        if !self.exclude.contains(&a) {
            self.list.entry(a).or_default();
        }
    }
    /// The `EVM.create` hook (core/vm/evm.go): `tracer.list.addAddress`,
    /// without the exclusions.
    pub(crate) fn add_address(&mut self, a: Address) {
        self.list.entry(a).or_default();
    }
    /// `AccessListTracer.AccessList`.
    pub fn access_list(&self) -> Vec<AccessTuple> {
        self.list
            .iter()
            .map(|(address, keys)| AccessTuple {
                address: *address,
                keys: keys.iter().copied().collect(),
            })
            .collect()
    }
    /// `AccessListTracer.Equal`: the same addresses and the same slots.
    pub fn same_list(&self, other: &Self) -> bool {
        self.list == other.list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn w(n: u64) -> Word {
        Word::from_u64(n)
    }
    #[test]
    fn exclusions_apply_to_addresses_but_not_to_slots() {
        let from = [1; 20];
        let to = [2; 20];
        let seed = vec![
            AccessTuple {
                address: to,
                keys: vec![[7; 32]],
            },
            AccessTuple {
                address: from,
                keys: vec![],
            },
            AccessTuple {
                address: [3; 20],
                keys: vec![],
            },
        ];
        let mut t = AccessListTracer::new(&seed, from, to, &[[9; 20]]);
        let list = t.access_list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].address, to);
        assert_eq!(list[1].address, [3; 20]);
        // SLOAD in the frame of an excluded address still adds the slot.
        t.capture(0x54, &[w(5)], from);
        assert!(t.access_list().iter().any(|a| a.address == from));
        // BALANCE of a precompile is excluded; of another account it is not.
        let mut p = [0u8; 32];
        p[12..].copy_from_slice(&[9; 20]);
        t.capture(0x31, &[crate::evm::word::from_bytes(&p)], to);
        assert!(!t.access_list().iter().any(|a| a.address == [9; 20]));
        // A CALL with 5 stack items (an underflow in Go) still names its address.
        let mut q = [0u8; 32];
        q[12..].copy_from_slice(&[4; 20]);
        let target = crate::evm::word::from_bytes(&q);
        t.capture(0xf1, &[w(0), w(0), w(0), target, w(1)], to);
        assert!(t.access_list().iter().any(|a| a.address == [4; 20]));
        let before = t.clone();
        t.capture(0xf1, &[target, w(1)], to);
        assert!(t.same_list(&before));
    }
}
