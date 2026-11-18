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
    pub fn typed_rlp(&self) -> Result<Vec<u8>, String> {
        if self.transaction_type > 2 || self.bloom.len() != BLOOM_BYTES {
            return Err("unsupported receipt type/bloom length".into());
        }
        if self.logs.len() > 4096 || self.outbound_etxs.len() > 4096 || self.post_state.len() > 32 {
            return Err("receipt exceeds supported bounds".into());
        }
        if self
            .logs
            .iter()
            .any(|l| l.topics.len() > 4096 || l.data.len() > 131_072)
        {
            return Err("receipt log exceeds supported bounds".into());
        }
        let mut budget = self.bloom.len() + 256;
        for log in &self.logs {
            budget = budget
                .checked_add(log.data.len())
                .and_then(|n| n.checked_add(log.topics.len() * 33 + 64))
                .ok_or("receipt size overflow")?;
        }
        if budget > crate::schema::MAX_MESSAGE_BYTES {
            return Err("receipt exceeds bounded byte budget".into());
        }
        let mut outbound = Vec::with_capacity(self.outbound_etxs.len());
        for tx in &self.outbound_etxs {
            let encoded = tx.rlp_envelope();
            budget = budget
                .checked_add(encoded.len())
                .ok_or("receipt size overflow")?;
            if budget > crate::schema::MAX_MESSAGE_BYTES {
                return Err("receipt exceeds bounded byte budget".into());
            }
            outbound.push(encoded);
        }
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
pub fn receipts_root(receipts: &[Receipt]) -> Result<Hash, String> {
    if receipts.len() > trie::MAX_LIST_ITEMS {
        return Err("receipt count limit exceeded".into());
    }
    let mut values = Vec::with_capacity(receipts.len());
    let mut bytes = 0usize;
    for receipt in receipts {
        let value = receipt.typed_rlp()?;
        bytes = bytes
            .checked_add(value.len())
            .ok_or("receipt-list size overflow")?;
        if bytes > trie::MAX_VALUE_BYTES {
            return Err("receipt-list byte budget exceeded".into());
        }
        values.push(value);
    }
    trie::ordered_root(&values)
}
