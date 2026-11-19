//! Native Quai node shell. `replay` checks go-quai mainnet blocks exported in
//! the QEX1 format with the Rust consensus code and, with `--state`, executes
//! them and compares the state roots. `run` feeds the same chain processor
//! from public go-quai peers and keeps a block store; `rpc` serves JSON-RPC
//! from the store (or an export) and the state of the latest checkpoint;
//! `observe` applies go-quai's head choice to live gossip. The Phase 2 gate
//! (two weeks at the live head without divergence from Go) is not passed.
pub mod blockstore;
pub mod checkpoint;
pub mod cli;
pub mod config;
pub mod forkchoice;
pub mod log;
pub mod metrics;
pub mod node;
pub mod peers;
pub mod pow;
pub mod processor;
pub mod qex;
pub mod replay;
pub mod rpc_call;
pub mod rpc_view;
pub mod spec;
pub mod state;
pub mod store;
pub mod tip;
pub mod txpool;
pub mod uncles;
pub mod window;
