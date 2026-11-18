//! Native execution preconditions, not a complete EVM or block state transition.
//! Compatibility sources and exclusions are recorded in NOTICE.md.
pub mod etx;
pub(crate) mod slice;
pub mod gas;
pub mod qi;

pub mod evm;

pub mod checkpoint;
pub mod early_replay;
pub mod economic_state;
pub mod lockup;
pub mod muhash;
pub mod qi_settlement;
pub mod settlement;
pub mod utxo;

pub mod incoming;

pub mod historical;

pub mod state_image;

pub mod state_store;

pub mod state_error;

#[cfg(test)]
extern crate self as quai_execution;

pub mod lockup_state;

#[cfg(test)]
mod fuzz_tests;
