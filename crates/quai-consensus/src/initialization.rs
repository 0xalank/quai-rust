//! Bounded genesis construction and hierarchy initialization. No canonical commit.
use crate::{history::Record, rules::Uint, share_rules, Error};
use crypto_bigint::{Encoding, U512};
use prost::Message;
use quai_primitives::{
    schema::{block as b, common as c},
    work::BodyHeader,
    Hash,
};
pub const MAX_GENESIS_EXTRA: usize = 4096;
#[derive(Clone, Debug)]
pub struct GenesisSpec {
    pub context: usize,
    pub nonce: u64,
    pub timestamp: u64,
    pub extra: Vec<u8>,
    pub allocation_hash: Hash,
    pub gas_limit: u64,
    pub difficulty: Uint,
    pub starting_expansion: u64,
}
fn magnitude(n: Uint) -> Vec<u8> {
    let bytes = n.to_be_bytes();
    bytes[bytes.iter().position(|v| *v != 0).unwrap_or(32)..].to_vec()
}
fn ph(h: Hash) -> c::ProtoHash {
    c::ProtoHash { value: h.to_vec() }
}
fn constant(s: &str) -> Hash {
    quai_primitives::decode_hex(s).unwrap().try_into().unwrap()
}
/// Reconstructs pinned Genesis.ToBlock fields, including the empty placeholder
/// transaction. Allocation files/state are not executed, trusted or committed.
pub fn build_genesis(spec: &GenesisSpec) -> Result<Record, Error> {
    if spec.context > 2 {
        return Err(Error::Invalid("genesis context"));
    }
    if spec.extra.len() > MAX_GENESIS_EXTRA {
        return Err(Error::ResourceLimit("genesis extra"));
    }
    let root =
        quai_primitives::trie::ordered_root(&[]).map_err(|_| Error::Invalid("empty root"))?;
    let mu = constant("0x544eb3142c000f0ad2c76ac41f4222abbababed830eeafee4b6dc56b52d5cac0");
    let uncles = constant("0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347");
    let mut extra = spec.extra.clone();
    extra.extend_from_slice(&spec.allocation_hash);
    let h = b::ProtoHeader {
        parent_hash: vec![ph([0; 32]); 2],
        uncle_hash: Some(ph(uncles)),
        evm_root: Some(ph(root)),
        tx_hash: Some(ph(root)),
        outbound_etx_hash: Some(ph(root)),
        etx_rollup_hash: Some(ph(root)),
        manifest_hash: vec![ph(root); 3],
        receipt_hash: Some(ph(root)),
        parent_entropy: vec![vec![]; 3],
        parent_delta_entropy: vec![vec![]; 3],
        parent_uncled_delta_entropy: vec![vec![]; 3],
        uncled_entropy: Some(vec![]),
        number: vec![vec![]; 2],
        gas_limit: Some(if spec.gas_limit == 0 {
            12_000_000
        } else {
            spec.gas_limit
        }),
        gas_used: Some(0),
        base_fee: Some(vec![1]),
        extra: Some(extra),
        utxo_root: Some(ph(mu)),
        etx_set_root: Some(ph(root)),
        efficiency_score: Some(0),
        threshold_count: Some(0),
        expansion_number: Some(spec.starting_expansion as u8 as u64),
        etx_eligible_slices: Some(ph(if spec.starting_expansion == 0 {
            [0; 32]
        } else {
            [255; 32]
        })),
        prime_terminus_hash: Some(ph(root)),
        interlink_root_hash: Some(ph(root)),
        state_limit: Some(5_000_000),
        state_used: Some(0),
        quai_state_size: Some(vec![]),
        exchange_rate: Some(magnitude(Uint::from_u64(221077819000000000))),
        avg_tx_fees: Some(vec![]),
        total_fees: Some(vec![]),
        k_quai_discount: Some(vec![100]),
        conversion_flow_amount: Some(magnitude(
            Uint::from_u64(10000).wrapping_mul(&Uint::from_u64(1_000_000_000_000_000_000)),
        )),
        miner_difficulty: Some(magnitude(spec.difficulty)),
        prime_state_root: Some(ph(root)),
        region_state_root: Some(ph(root)),
        ..Default::default()
    };
    let header = BodyHeader::decode(&h.encode_to_vec())
        .map_err(|_| Error::Unsupported("genesis body header"))?;
    let zero = b::ProtoPowShareDiffAndCount {
        difficulty: Some(vec![]),
        count: Some(vec![]),
        uncled: Some(vec![]),
    };
    let wh = b::ProtoWorkObjectHeader {
        header_hash: Some(ph(header.hash())),
        parent_hash: Some(ph([0; 32])),
        number: Some(vec![]),
        difficulty: Some(magnitude(spec.difficulty)),
        tx_hash: Some(ph(root)),
        nonce: Some(spec.nonce),
        location: Some(c::ProtoLocation { value: vec![] }),
        mix_hash: Some(ph([0; 32])),
        time: Some(spec.timestamp),
        prime_terminus_number: Some(vec![]),
        lock: Some(0),
        primary_coinbase: Some(c::ProtoAddress { value: vec![0; 20] }),
        sha_diff_and_count: Some(zero.clone()),
        scrypt_diff_and_count: Some(zero),
        sha_share_target: Some(vec![]),
        scrypt_share_target: Some(vec![]),
        kawpow_difficulty: Some(vec![]),
        ..Default::default()
    };
    let tx = b::ProtoTransaction {
        r#type: Some(0),
        to: Some(vec![0; 20]),
        nonce: Some(0),
        value: Some(vec![]),
        gas: Some(0),
        data: Some(vec![]),
        chain_id: Some(vec![]),
        gas_price: Some(vec![]),
        access_list: Some(b::ProtoAccessList {
            access_tuples: vec![b::ProtoAccessTuple {
                address: vec![0; 20],
                storage_key: vec![],
            }],
        }),
        v: Some(vec![]),
        r: Some(vec![]),
        s: Some(vec![]),
        parent_hash: Some(ph([0; 32])),
        mix_hash: Some(ph([0; 32])),
        work_nonce: Some(0),
        ..Default::default()
    };
    let wire = b::ProtoWorkObject {
        wo_header: Some(wh),
        wo_body: Some(b::ProtoWorkObjectBody {
            header: Some(h),
            transactions: Some(Default::default()),
            uncles: Some(Default::default()),
            outbound_etxs: Some(Default::default()),
            manifest: Some(Default::default()),
            interlink_hashes: Some(Default::default()),
        }),
        tx: Some(tx),
    };
    Record::decode(&wire.encode_to_vec())
}
/// Raw arithmetic geometry, including source values outside 4-bit addresses.
pub fn hierarchy_size(expansion: u8) -> (u16, u16) {
    (expansion as u16 / 2 + 1, (expansion as u16 + 1) / 2 + 1)
}
fn supported_size(expansion: u8) -> Result<(u8, u8), Error> {
    let (r, z) = hierarchy_size(expansion);
    if r > 16 || z > 16 {
        return Err(Error::Unsupported("hierarchy exceeds location domain"));
    }
    Ok((r as u8, z as u8))
}
pub fn active_slices(expansion: u8) -> Result<Vec<Vec<u8>>, Error> {
    let (r, z) = supported_size(expansion)?;
    Ok((0..r)
        .flat_map(|i| (0..z).map(move |j| vec![i, j]))
        .collect())
}
pub fn new_chains(expansion: u8) -> Result<Vec<Vec<u8>>, Error> {
    if expansion == 0 {
        return Err(Error::Unsupported("no expansion before genesis"));
    }
    let (r, z) = supported_size(expansion)?;
    let (old_r, _) = supported_size(expansion - 1)?;
    Ok(if r > old_r {
        std::iter::once(vec![r - 1])
            .chain((0..z).map(|j| vec![r - 1, j]))
            .collect()
    } else {
        (0..old_r).map(|i| vec![i, z - 1]).collect()
    })
}
/// Exact trusted-genesis CalcDifficulty branch. A 257-bit Go result is retained
/// here; callers with 256-bit headers must reject it instead of truncating.
pub fn genesis_difficulty(
    parent: &Record,
    expansion: u8,
    default_genesis: Option<Hash>,
) -> Result<U512, Error> {
    let h = parent.object().header().wire();
    if expansion == 0
        && parent
            .header()
            .wire()
            .location
            .as_ref()
            .unwrap()
            .value
            .is_empty()
        || h.expansion_number.unwrap() > 0 && default_genesis == Some(parent.id())
    {
        return Ok(parent.difficulty()?.resize());
    }
    if share_rules::uint(&h.parent_entropy[0])? != Uint::ZERO {
        return Err(Error::Invalid("genesis entropy is below parent entropy"));
    }
    // TotalLogEntropy(trusted genesis) is zero. The prime target division
    // preserves zero and EntropyBigBitsToDifficultyBits(0) returns 2^256.
    Ok(U512::ONE.shl_vartime(256))
}
