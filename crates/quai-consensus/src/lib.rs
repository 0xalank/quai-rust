//! Native consensus components. No Go fallback, fake-proof mode or chain authority.
//! Bounded proof, auxiliary and ancestry components do not establish canonical
//! history, economic state validity or full block consensus.
pub mod bigdiv;
pub mod block;
pub mod chain;
pub mod donor;
pub mod pow;
pub mod rules;
pub(crate) mod slice;
pub mod verifier;
use quai_primitives::Hash;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    Unsupported(&'static str),
    ResourceLimit(&'static str),
    Unavailable(&'static str),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Debug)]
pub struct VerifiedPow {
    algorithm: pow::Algorithm,
    seal: Hash,
    height: u64,
    nonce: u64,
    hash: Hash,
}
impl VerifiedPow {
    pub fn hash(&self) -> Hash {
        self.hash
    }
    pub fn algorithm(&self) -> pow::Algorithm {
        self.algorithm
    }
    pub fn seal(&self) -> Hash {
        self.seal
    }
    pub fn height(&self) -> u64 {
        self.height
    }
    pub fn nonce(&self) -> u64 {
        self.nonce
    }
}

pub mod auxiliary;

pub mod share_rules;

pub mod history;

pub(crate) mod fields;
pub mod validation;

pub mod choice_window;
pub mod controllers;
pub mod cubic_discount;
pub mod economics;
pub mod prime_controller;
pub mod prime_economics;
pub mod prime_forward;
pub mod zone_header;

pub mod hierarchy_header;

pub mod placement;

pub mod initialization;

pub mod candidate_state;

pub mod frontier;
pub mod scoring;

pub mod eligibility;

pub mod coordinator;

pub mod private_profile;

pub mod payouts;

pub mod confirmation;
pub mod private_branch;
pub mod zone_admission;
pub mod zone_execution;

pub mod strong_uncles;

pub mod weak_float;

pub mod prefork_shares;

pub mod private_canonical;

#[cfg(test)]
extern crate self as quai_consensus;
#[cfg(test)]
mod fuzz_tests;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../examples/check-activation.rs"]
mod runtime_fixture;
#[cfg(test)]
mod runtime_profile_tests;
