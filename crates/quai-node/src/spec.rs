//! Chain specifications the replay checks against. The values live in
//! `quai_primitives::spec::ChainSpec` (one spec and one fork schedule for every
//! crate); `quai_consensus::chain::Chain` adds the rebuilt genesis and the
//! consensus profiles. `colosseum` is Quai mainnet (slice [0, 0]); `local` is
//! the reviewed private hierarchy profile (chain 1337) of the fixture tests.
pub use quai_consensus::chain::Chain;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_environments_map_to_chain_specs() {
        let c = Chain::for_environment("colosseum").unwrap();
        assert_eq!(c.spec().chain_id, 9);
        assert_eq!(c.location(), [0, 0]);
        assert_eq!(
            quai_primitives::encode_hex(&c.genesis_hash()),
            quai_primitives::spec::COLOSSEUM_GENESIS_HASH
        );
        let local = Chain::for_environment("local").unwrap();
        assert_eq!(local.spec().chain_id, 1337);
        assert_eq!(
            quai_primitives::encode_hex(&local.genesis_hash()),
            "0x53fc82ff80d79fb2f225ecc80fe9b5130dd9f1d248a6140f41096ce822fb10cd"
        );
        assert!(Chain::for_environment("garden").is_err());
    }
}
