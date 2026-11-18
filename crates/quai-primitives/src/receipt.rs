//! Consensus receipt/log RLP and Quai's 10,240-byte bloom; not EVM execution.
use crate::{
    transaction::{keccak, Transaction},
    trie, Hash,
};
pub const BLOOM_BYTES: usize = 10_240;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Log {
    pub address: [u8; 20],
    pub topics: Vec<Hash>,
    pub data: Vec<u8>,
}
impl Log {
    fn rlp(&self) -> Vec<u8> {
        trie::list([
            trie::bytes(&self.address),
            trie::list(self.topics.iter().map(|v| trie::bytes(v))),
            trie::bytes(&self.data),
        ])
    }
}
/// Matches the reference's large fixed bloom, including its 11-bit bit selection.
pub fn logs_bloom(logs: &[Log]) -> Vec<u8> {
    let mut bloom = vec![0; BLOOM_BYTES];
    for log in logs {
        for value in
            std::iter::once(log.address.as_slice()).chain(log.topics.iter().map(|t| t.as_slice()))
        {
            let hash = keccak(value);
            for pair in hash[..6].chunks_exact(2) {
                let bit = usize::from(u16::from_be_bytes([pair[0], pair[1]]) & 0x7ff);
                bloom[BLOOM_BYTES - 1 - (bit >> 3)] |= 1 << (bit & 7);
            }
        }
    }
    bloom
}
#[derive(Clone)]
pub struct Receipt {
    pub transaction_type: u8,
    pub post_state: Vec<u8>,
    pub status: u64,
    pub cumulative_gas_used: u64,
    pub bloom: Vec<u8>,
    pub logs: Vec<Log>,
    pub outbound_etxs: Vec<Transaction>,
}
impl Receipt {
    /// Receipts come from local execution, not from peers, and go-quai encodes
    /// them without any log, data or size cap (core/types/receipt.go). Gas
    /// bounds a transaction to at most 133,333 logs and 6,250,000 log data
    /// bytes at the 50,000,000 block gas limit.
    pub fn typed_rlp(&self) -> Result<Vec<u8>, String> {
        if self.transaction_type > 2 || self.bloom.len() != BLOOM_BYTES {
            return Err("unsupported receipt type/bloom length".into());
        }
        if self.post_state.len() > 32 {
            return Err("receipt post-state exceeds a root".into());
        }
        let outbound = self
            .outbound_etxs
            .iter()
            .map(Transaction::rlp_envelope)
            .collect::<Vec<_>>();
        let status = if !self.post_state.is_empty() {
            self.post_state.clone()
        } else if self.status == 0 {
            vec![]
        } else {
            vec![1]
        };
        let body = trie::list([
            trie::bytes(&status),
            trie::uint(self.cumulative_gas_used),
            trie::bytes(&self.bloom),
            trie::list(self.logs.iter().map(Log::rlp)),
            trie::list(outbound),
        ]);
        let mut bytes = vec![self.transaction_type];
        bytes.extend(body);
        Ok(bytes)
    }
}
/// No byte budget: every receipt carries a 10,240-byte bloom, and a block may
/// hold up to 13,157 transactions (3,800 intrinsic gas per minimal Qi
/// transaction at 50,000,000 gas), which is more than 64 MiB of receipts.
pub fn receipts_root(receipts: &[Receipt]) -> Result<Hash, String> {
    let values = receipts
        .iter()
        .map(Receipt::typed_rlp)
        .collect::<Result<Vec<_>, _>>()?;
    trie::ordered_root(&values)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipts_have_no_log_count_or_size_cap() {
        // More than the former 4,096 logs, 128 KiB per log and 3 MiB per receipt.
        let mut logs = vec![
            Log {
                address: [1; 20],
                topics: vec![],
                data: vec![2; 1],
            };
            5_000
        ];
        logs.push(Log {
            address: [1; 20],
            topics: vec![[3; 32]],
            data: vec![4; 3 << 20],
        });
        let receipt = Receipt {
            transaction_type: 0,
            post_state: vec![],
            status: 1,
            cumulative_gas_used: 21_000,
            bloom: logs_bloom(&logs),
            logs: logs.clone(),
            outbound_etxs: vec![],
        };
        let encoded = receipt.typed_rlp().unwrap();
        let body = trie::list([
            trie::bytes(&[1]),
            trie::uint(21_000),
            trie::bytes(&receipt.bloom),
            trie::list(logs.iter().map(Log::rlp)),
            trie::list([]),
        ]);
        assert_eq!(encoded, [vec![0], body].concat());
        assert!(encoded.len() > crate::schema::MAX_MESSAGE_BYTES);
        assert!(receipts_root(&[receipt]).is_ok());
    }
}
