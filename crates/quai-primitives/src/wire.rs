//! Protobuf field declarations matching go-quai core/types/proto_block.proto
//! and common/proto_common.proto. See NOTICE.md and the source license.
#[derive(Clone, PartialEq, prost::Message)]
pub struct BytesValue {
    #[prost(bytes = "vec", tag = "1")]
    pub value: Vec<u8>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct ShareCount {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub difficulty: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub count: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub uncled: Option<Vec<u8>>,
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct WorkHeader {
    #[prost(message, optional, tag = "1")]
    pub header_hash: Option<BytesValue>,
    #[prost(message, optional, tag = "2")]
    pub parent_hash: Option<BytesValue>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub number: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "4")]
    pub difficulty: Option<Vec<u8>>,
    #[prost(message, optional, tag = "5")]
    pub tx_hash: Option<BytesValue>,
    #[prost(uint64, optional, tag = "6")]
    pub nonce: Option<u64>,
    #[prost(message, optional, tag = "7")]
    pub location: Option<BytesValue>,
    #[prost(message, optional, tag = "8")]
    pub mix_hash: Option<BytesValue>,
    #[prost(uint64, optional, tag = "9")]
    pub time: Option<u64>,
    #[prost(bytes = "vec", optional, tag = "10")]
    pub prime_terminus_number: Option<Vec<u8>>,
    #[prost(uint32, optional, tag = "11")]
    pub lock: Option<u32>,
    #[prost(message, optional, tag = "12")]
    pub primary_coinbase: Option<BytesValue>,
    #[prost(bytes = "vec", tag = "13")]
    pub data: Vec<u8>,
    // Opaque wire field retained only to reject unsupported auxiliary forms.
    #[prost(bytes = "vec", optional, tag = "14")]
    pub aux_pow: Option<Vec<u8>>,
    #[prost(message, optional, tag = "15")]
    pub scrypt_diff_and_count: Option<ShareCount>,
    #[prost(message, optional, tag = "16")]
    pub sha_diff_and_count: Option<ShareCount>,
    #[prost(bytes = "vec", optional, tag = "17")]
    pub sha_share_target: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "18")]
    pub scrypt_share_target: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "19")]
    pub kawpow_difficulty: Option<Vec<u8>>,
}

pub mod tx;
