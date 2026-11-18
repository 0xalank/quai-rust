//! Canonical body-header/work-object reconstruction and body commitments.
//! Bounded codecs and commitments only: not proof, state or hierarchy validity.
use crate::{
    schema::{self, block as b, common as c},
    transaction::Transaction,
    trie, Hash, Header, Schedule,
};
use prost::Message;
fn magnitude(value: &mut Option<Vec<u8>>) -> Result<(), String> {
    let v = value
        .as_mut()
        .ok_or("required body-header integer missing")?;
    let start = v.iter().position(|b| *b != 0).unwrap_or(v.len());
    if v.len() - start > 32 {
        return Err("body-header integer exceeds 256-bit scope".into());
    }
    v.drain(..start);
    Ok(())
}
fn fixed(value: &mut Vec<u8>) -> Result<(), String> {
    if value.len() > 32 {
        return Err("hash exceeds supported 32-byte domain".into());
    }
    let mut normalized = vec![0; 32];
    normalized[32 - value.len()..].copy_from_slice(value);
    *value = normalized;
    Ok(())
}
fn hash(value: &mut Option<c::ProtoHash>) -> Result<(), String> {
    fixed(
        &mut value
            .as_mut()
            .ok_or("required body-header hash missing")?
            .value,
    )
}
fn read_hash(value: &Option<c::ProtoHash>) -> Hash {
    value.as_ref().unwrap().value.as_slice().try_into().unwrap()
}
#[derive(Clone)]
pub struct BodyHeader {
    wire: b::ProtoHeader,
}
impl BodyHeader {
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        schema::preflight(".block.ProtoHeader", bytes)?;
        let mut wire = b::ProtoHeader::decode(bytes).map_err(|e| e.to_string())?;
        if wire.parent_hash.len() != 2
            || wire.manifest_hash.len() != 3
            || wire.number.len() != 2
            || wire.parent_entropy.len() != 3
            || wire.parent_delta_entropy.len() != 3
            || wire.parent_uncled_delta_entropy.len() != 3
        {
            return Err("invalid body-header hierarchy array length".into());
        }
        for h in wire.parent_hash.iter_mut().chain(&mut wire.manifest_hash) {
            fixed(&mut h.value)?;
        }
        for h in [
            &mut wire.uncle_hash,
            &mut wire.evm_root,
            &mut wire.tx_hash,
            &mut wire.outbound_etx_hash,
            &mut wire.etx_rollup_hash,
            &mut wire.receipt_hash,
            &mut wire.utxo_root,
            &mut wire.etx_set_root,
            &mut wire.etx_eligible_slices,
            &mut wire.prime_terminus_hash,
            &mut wire.interlink_root_hash,
            &mut wire.prime_state_root,
            &mut wire.region_state_root,
        ] {
            hash(h)?;
        }
        for v in [
            &mut wire.uncled_entropy,
            &mut wire.base_fee,
            &mut wire.quai_state_size,
            &mut wire.exchange_rate,
            &mut wire.avg_tx_fees,
            &mut wire.total_fees,
            &mut wire.k_quai_discount,
            &mut wire.conversion_flow_amount,
            &mut wire.miner_difficulty,
        ] {
            magnitude(v)?;
        }
        for group in [
            &mut wire.number,
            &mut wire.parent_entropy,
            &mut wire.parent_delta_entropy,
            &mut wire.parent_uncled_delta_entropy,
        ] {
            for value in group {
                let mut some = Some(std::mem::take(value));
                magnitude(&mut some)?;
                *value = some.unwrap();
            }
        }
        for value in [
            &mut wire.efficiency_score,
            &mut wire.threshold_count,
            &mut wire.expansion_number,
        ] {
            if value.is_none() {
                return Err("required body-header score/expansion missing".into());
            }
        }
        wire.efficiency_score = Some(wire.efficiency_score.unwrap() as u16 as u64);
        wire.threshold_count = Some(wire.threshold_count.unwrap() as u16 as u64);
        wire.expansion_number = Some(wire.expansion_number.unwrap() as u8 as u64);
        for value in [
            &mut wire.gas_limit,
            &mut wire.gas_used,
            &mut wire.state_limit,
            &mut wire.state_used,
        ] {
            *value = Some(value.unwrap_or(0));
        }
        // Declared legacy fields ignored by the reference body-header domain object.
        wire.difficulty = None;
        wire.location = None;
        wire.mix_hash = None;
        wire.nonce = None;
        if wire.extra.as_ref().is_some_and(|v| v.len() > 131_072) {
            return Err("body-header extra exceeds supported scope".into());
        }
        Ok(Self { wire })
    }
    pub fn encode(&self) -> Vec<u8> {
        self.wire.encode_to_vec()
    }
    pub fn hash(&self) -> Hash {
        *blake3::hash(&self.encode()).as_bytes()
    }
    pub fn wire(&self) -> &b::ProtoHeader {
        &self.wire
    }
}
fn canonical_transaction(value: &mut b::ProtoTransaction) -> Result<Transaction, String> {
    let transaction = Transaction::decode(&value.encode_to_vec())?;
    *value =
        b::ProtoTransaction::decode(transaction.encode().as_slice()).map_err(|e| e.to_string())?;
    Ok(transaction)
}
fn normalize_work_header(
    value: &mut b::ProtoWorkObjectHeader,
    schedule: Schedule,
    allow_auxiliary: bool,
) -> Result<(), String> {
    let decoded = if allow_auxiliary {
        Header::decode_auxiliary(&value.encode_to_vec(), schedule)?
    } else {
        Header::decode(&value.encode_to_vec(), schedule)?
    };
    *value =
        b::ProtoWorkObjectHeader::decode(decoded.encode().as_slice()).map_err(|e| e.to_string())?;
    Ok(())
}
fn hash_list(values: &[c::ProtoHash]) -> Result<Hash, String> {
    trie::ordered_root(
        &values
            .iter()
            .map(|v| trie::bytes(&v.value))
            .collect::<Vec<_>>(),
    )
}
pub fn transactions_root(values: &[Transaction]) -> Result<Hash, String> {
    if values.len() > trie::MAX_LIST_ITEMS {
        return Err("transaction-list item budget exceeded".into());
    }
    let mut encoded = Vec::with_capacity(values.len());
    let mut bytes = 0usize;
    for tx in values {
        let value = tx.typed_rlp();
        bytes = bytes
            .checked_add(value.len())
            .ok_or("transaction-list size overflow")?;
        if bytes > trie::MAX_VALUE_BYTES {
            return Err("transaction-list byte budget exceeded".into());
        }
        encoded.push(value);
    }
    trie::ordered_root(&encoded)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Prime,
    Region,
    Zone,
}
#[derive(Clone)]
pub struct WorkObject {
    wire: b::ProtoWorkObject,
    header: BodyHeader,
    transactions: Vec<Transaction>,
    outbound: Vec<Transaction>,
}
impl WorkObject {
    /// Reconstructs a complete BlockObject-domain view. Auxiliary semantic forms
    /// remain unsupported even though the complete raw schemas are available.
    pub fn decode(bytes: &[u8], schedule: Schedule) -> Result<Self, String> {
        Self::decode_profile(bytes, schedule, false)
    }
    pub fn decode_auxiliary(bytes: &[u8], schedule: Schedule) -> Result<Self, String> {
        Self::decode_profile(bytes, schedule, true)
    }
    fn decode_profile(
        bytes: &[u8],
        schedule: Schedule,
        allow_auxiliary: bool,
    ) -> Result<Self, String> {
        schema::preflight(".block.ProtoWorkObject", bytes)?;
        let mut wire = b::ProtoWorkObject::decode(bytes).map_err(|e| e.to_string())?;
        normalize_work_header(
            wire.wo_header.as_mut().ok_or("missing work header")?,
            schedule,
            allow_auxiliary,
        )?;
        let body = wire.wo_body.as_mut().ok_or("missing work body")?;
        let header = BodyHeader::decode(
            &body
                .header
                .as_ref()
                .ok_or("missing body header for complete view")?
                .encode_to_vec(),
        )?;
        body.header = Some(header.wire.clone());
        // No transaction-count cap: go-quai bounds the list only by block gas
        // (at least 3,800 gas per Qi transaction, so up to 13,157 entries at
        // 50,000,000 gas) and by the 3 MiB message size checked above.
        let decode_list =
            |value: &mut Option<b::ProtoTransactions>| -> Result<Vec<Transaction>, String> {
                let value = value.get_or_insert_default();
                value
                    .transactions
                    .iter_mut()
                    .map(canonical_transaction)
                    .collect()
            };
        let transactions = decode_list(&mut body.transactions)?;
        let outbound = decode_list(&mut body.outbound_etxs)?;
        let uncles = body.uncles.get_or_insert_default();
        if uncles.wo_headers.len() > 4096 {
            return Err("uncle count exceeds supported scope".into());
        }
        for uncle in &mut uncles.wo_headers {
            normalize_work_header(uncle, schedule, allow_auxiliary)?;
        }
        let manifest = body.manifest.get_or_insert_default();
        let interlink = body.interlink_hashes.get_or_insert_default();
        if manifest.manifest.len() > 4096 || interlink.hashes.len() > 4096 {
            return Err("hash-list exceeds supported scope".into());
        }
        for h in manifest.manifest.iter_mut().chain(&mut interlink.hashes) {
            fixed(&mut h.value)?;
        }
        if let Some(tx) = &mut wire.tx {
            canonical_transaction(tx)?;
        }
        Ok(Self {
            wire,
            header,
            transactions,
            outbound,
        })
    }
    pub fn encode(&self) -> Vec<u8> {
        self.wire.encode_to_vec()
    }
    pub fn auxiliary_work_header(&self, schedule: Schedule) -> Result<crate::Header, String> {
        Header::decode_auxiliary(
            &self.wire.wo_header.as_ref().unwrap().encode_to_vec(),
            schedule,
        )
    }
    pub fn work_header(&self, schedule: Schedule) -> Result<crate::Header, String> {
        crate::Header::decode(
            &self.wire.wo_header.as_ref().unwrap().encode_to_vec(),
            schedule,
        )
    }
    pub fn header(&self) -> &BodyHeader {
        &self.header
    }
    pub fn transactions(&self) -> &[Transaction] {
        &self.transactions
    }
    pub fn outbound_etxs(&self) -> &[Transaction] {
        &self.outbound
    }
    /// Canonically padded manifest hashes, in committed order.
    pub fn manifest_hashes(&self) -> Vec<Hash> {
        self.wire
            .wo_body
            .as_ref()
            .unwrap()
            .manifest
            .as_ref()
            .unwrap()
            .manifest
            .iter()
            .map(|h| h.value.as_slice().try_into().unwrap())
            .collect()
    }
    pub fn transaction_root(&self) -> Result<Hash, String> {
        transactions_root(&self.transactions)
    }
    pub fn outbound_root(&self) -> Result<Hash, String> {
        transactions_root(&self.outbound)
    }
    pub fn uncle_headers(&self, schedule: Schedule) -> Result<Vec<Header>, String> {
        self.wire
            .wo_body
            .as_ref()
            .unwrap()
            .uncles
            .as_ref()
            .unwrap()
            .wo_headers
            .iter()
            .map(|h| Header::decode_auxiliary(&h.encode_to_vec(), schedule))
            .collect()
    }
    pub fn uncle_root(&self) -> Hash {
        let uncles = self.wire.wo_body.as_ref().unwrap().uncles.as_ref().unwrap();
        if uncles.wo_headers.is_empty() {
            crate::transaction::keccak(&trie::list([]))
        } else {
            *blake3::hash(&uncles.encode_to_vec()).as_bytes()
        }
    }
    pub fn manifest_root(&self) -> Result<Hash, String> {
        hash_list(
            &self
                .wire
                .wo_body
                .as_ref()
                .unwrap()
                .manifest
                .as_ref()
                .unwrap()
                .manifest,
        )
    }
    pub fn interlink_root(&self) -> Result<Hash, String> {
        hash_list(
            &self
                .wire
                .wo_body
                .as_ref()
                .unwrap()
                .interlink_hashes
                .as_ref()
                .unwrap()
                .hashes,
        )
    }
    /// Body/header commitments only. Does not establish valid signatures, active
    /// destinations, ancestry, proof of work, execution/state roots or fork choice.
    pub fn verify_commitments(&self, context: Context) -> Result<(), String> {
        let header = &self.header.wire;
        let body = self.wire.wo_body.as_ref().unwrap();
        let outer = self.wire.wo_header.as_ref().unwrap();
        if outer.header_hash.as_ref().unwrap().value != self.header.hash() {
            return Err("body-header commitment mismatch".into());
        }
        match context {
            Context::Zone => {
                if !body.manifest.as_ref().unwrap().manifest.is_empty()
                    || !body.interlink_hashes.as_ref().unwrap().hashes.is_empty()
                {
                    return Err("zone manifest/interlink must be empty".into());
                }
                if self.transaction_root()? != read_hash(&header.tx_hash) {
                    return Err("transaction root mismatch".into());
                }
                if self.outbound_root()? != read_hash(&header.outbound_etx_hash) {
                    return Err("outbound ETX root mismatch".into());
                }
                if self.uncle_root() != read_hash(&header.uncle_hash) {
                    return Err("uncle root mismatch".into());
                }
            }
            Context::Prime | Context::Region => {
                if !self.transactions.is_empty()
                    || !self.outbound.is_empty()
                    || !body.uncles.as_ref().unwrap().wo_headers.is_empty()
                {
                    return Err("non-zone body has transactions/ETXs/uncles".into());
                }
                let index = if context == Context::Prime { 1 } else { 2 };
                let manifest = self.manifest_root()?;
                if manifest == trie::ordered_root(&[])?
                    || manifest.as_slice() != header.manifest_hash[index].value
                {
                    return Err("subordinate manifest commitment mismatch".into());
                }
                if context == Context::Prime
                    && self.interlink_root()? != read_hash(&header.interlink_root_hash)
                {
                    return Err("interlink commitment mismatch".into());
                }
            }
        }
        Ok(())
    }
}
