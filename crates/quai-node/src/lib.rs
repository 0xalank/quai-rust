//! Native Quai node shell. `replay` checks go-quai mainnet blocks exported in
//! the QEX1 format with the Rust consensus code and, with `--state`, executes
//! them and compares the state roots. `rpc` serves JSON-RPC from such an
//! export and the state of the latest replay checkpoint. It is not a
//! synchronizing node: it has no network and no live head.
pub mod checkpoint;
pub mod cli;
pub mod log;
pub mod pow;
pub mod qex;
pub mod replay;
pub mod rpc_call;
pub mod rpc_view;
pub mod spec;
pub mod state;
pub mod store;
pub mod window;
