//! Native Quai block production, AuxPoW merged-mining templates, workshare
//! assembly/validation and mining control (`go-quai/core/worker.go`,
//! `core/slice.go`, `core/types/auxpow*.go`).
pub mod auxpow;
pub mod relay;
pub mod template;
pub mod worker;

pub use auxpow::{
    build_coinbase_tx, build_donor_header, compact_to_target, difficulty_to_target,
    encode_bip34_height, extract_coinb1_coinb2, kawpow_seed_hash, parse_submitted_donor_block,
    progpow_seed_hash, sign_aux_template, target_to_hex, word_swap_prev_hash, ParsedDonorBlock,
    POW_KAWPOW, POW_PROGPOW, POW_SCRYPT, POW_SHA_BCH, POW_SHA_BTC,
};
pub use relay::{verify_builder_template, BuilderRelay, VerifiedBuilderTemplate};
pub use template::{marshal_aux_pow_template, BlockTemplateParams};
pub use worker::{
    BuilderSubmitHook, Miner, MinerConfig, MinerMode, StateBlockBuilder, SubmitOutcome,
    WorkshareLruDump,
};
