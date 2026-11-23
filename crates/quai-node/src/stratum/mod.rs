//! Native Stratum v1 mining server (SHA-256d, Scrypt, KAWPOW) and HTTP/WS
//! pool dashboard API matching `go-quai/stratum/`.
pub mod api;
pub mod server;
pub mod stats;

pub use api::serve_dashboard;
pub use server::{Algo, StratumConfig, StratumHandle, StratumServer};
pub use stats::{
    AlgoStats, BlockFound, MinerStats, PoolOverview, PoolStats, ShareRecord, WorkerStats,
};
