//! Transaction messages from pinned go-quai proto_block.proto; see NOTICE.md.
use super::BytesValue;
#[derive(Clone, PartialEq, prost::Message)]
pub struct Transaction {
    #[prost(uint64, optional, tag = "1")]
    pub r#type: Option<u64>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub to: Option<Vec<u8>>,
    #[prost(uint64, optional, tag = "3")]
    pub nonce: Option<u64>,
    #[prost(bytes = "vec", optional, tag = "4")]
    pub value: Option<Vec<u8>>,
    #[prost(uint64, optional, tag = "5")]
    pub gas: Option<u64>,
    #[prost(bytes = "vec", optional, tag = "6")]
    pub data: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "7")]
    pub chain_id: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "8")]
    pub gas_price: Option<Vec<u8>>,
    #[prost(message, optional, tag = "9")]
    pub access_list: Option<AccessList>,
    #[prost(bytes = "vec", optional, tag = "10")]
    pub v: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "11")]
    pub r: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "12")]
    pub s: Option<Vec<u8>>,
    #[prost(message, optional, tag = "13")]
    pub originating_tx_hash: Option<BytesValue>,
    #[prost(uint32, optional, tag = "14")]
    pub etx_index: Option<u32>,
    #[prost(message, optional, tag = "15")]
    pub tx_ins: Option<Inputs>,
    #[prost(message, optional, tag = "16")]
    pub tx_outs: Option<Outputs>,
    #[prost(bytes = "vec", optional, tag = "17")]
    pub signature: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "18")]
    pub etx_sender: Option<Vec<u8>>,
    #[prost(message, optional, tag = "19")]
    pub parent_hash: Option<BytesValue>,
    #[prost(message, optional, tag = "20")]
    pub mix_hash: Option<BytesValue>,
    #[prost(uint64, optional, tag = "21")]
    pub work_nonce: Option<u64>,
    #[prost(uint64, optional, tag = "22")]
    pub etx_type: Option<u64>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct AccessList {
    #[prost(message, repeated, tag = "1")]
    pub tuples: Vec<AccessTuple>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct AccessTuple {
    #[prost(bytes = "vec", tag = "1")]
    pub address: Vec<u8>,
    #[prost(message, repeated, tag = "2")]
    pub storage_keys: Vec<BytesValue>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct Inputs {
    #[prost(message, repeated, tag = "1")]
    pub inputs: Vec<Input>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct Input {
    #[prost(message, optional, tag = "1")]
    pub previous: Option<OutPoint>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub pub_key: Option<Vec<u8>>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct OutPoint {
    #[prost(message, optional, tag = "1")]
    pub hash: Option<BytesValue>,
    #[prost(uint32, optional, tag = "2")]
    pub index: Option<u32>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct Outputs {
    #[prost(message, repeated, tag = "1")]
    pub outputs: Vec<Output>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct Output {
    #[prost(uint32, optional, tag = "1")]
    pub denomination: Option<u32>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub address: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub lock: Option<Vec<u8>>,
}
