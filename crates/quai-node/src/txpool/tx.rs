//! A transaction as go-quai v0.57.0's pool sees it: `Transaction.ProtoDecode`
//! with Go's error texts, the canonical re-encoding that `tx.Hash()` and the
//! signature cover, the sender that `tx.Hash()` recovers and caches, and
//! `tx.Size()`.
//!
//! go-quai decodes integers into `*big.Int` of any length, so values here
//! are minimal big-endian magnitudes without a length limit: the pool, not
//! the decoder, refuses a gas price above 2^256. Public keys of Qi inputs
//! are kept uncompressed and re-encoded compressed, as `TxIn.ProtoDecode`
//! and `TxIn.ProtoEncode` do, so a key sent uncompressed gives the same hash.
use super::big;
use k256::elliptic_curve::sec1::ToEncodedPoint;
use prost::Message;
use quai_primitives::{
    transaction::{keccak, AccessTuple, Transaction as Primitive},
    wire::{tx as w, BytesValue},
    Hash,
};
use std::sync::atomic::{AtomicU64, Ordering};

pub type Address = [u8; 20];

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Work {
    pub parent: Option<Hash>,
    pub mix: Option<Hash>,
    pub nonce: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuaiBody {
    pub chain_id: Vec<u8>,
    pub nonce: u64,
    pub gas_price: Vec<u8>,
    pub gas: u64,
    pub to: Option<Address>,
    pub value: Vec<u8>,
    pub data: Vec<u8>,
    pub access_list: Vec<AccessTuple>,
    pub v: Vec<u8>,
    pub r: Vec<u8>,
    pub s: Vec<u8>,
    pub work: Work,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalBody {
    pub originating_hash: Hash,
    pub index: u16,
    pub gas: u64,
    pub to: Address,
    pub value: Vec<u8>,
    pub data: Vec<u8>,
    pub access_list: Vec<AccessTuple>,
    pub sender: Address,
    pub etx_type: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QiInput {
    pub hash: Hash,
    pub index: u16,
    /// 65-byte uncompressed SEC1 key.
    pub public_key: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QiOutput {
    pub denomination: u8,
    /// `None` when the field is absent; any length otherwise.
    pub address: Option<Vec<u8>>,
    pub lock: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QiBody {
    pub chain_id: Vec<u8>,
    pub inputs: Vec<QiInput>,
    pub outputs: Vec<QiOutput>,
    pub signature: [u8; 64],
    pub data: Vec<u8>,
    pub work: Work,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Quai(QuaiBody),
    External(ExternalBody),
    Qi(QiBody),
}

/// A decoded transaction with go-quai's derived values.
#[derive(Debug)]
pub struct PoolTx {
    pub body: Body,
    /// `tx.Hash()`: zero for a Quai transaction whose sender cannot be
    /// recovered.
    pub hash: Hash,
    /// The sender `tx.Hash()` recovers with the transaction's own chain ID
    /// and caches in the transaction (Quai only).
    pub sender: Option<Address>,
    /// `tx.Size()` of a Quai transaction.
    pub size: u64,
    /// `tx.Time()` in nanoseconds of the pool clock. Go sets it when it
    /// decodes the transaction and again when a reorg reinjects it.
    time: AtomicU64,
    /// The canonical protobuf (`ProtoEncode`).
    pub proto: Vec<u8>,
}
impl Clone for PoolTx {
    fn clone(&self) -> Self {
        Self {
            body: self.body.clone(),
            hash: self.hash,
            sender: self.sender,
            size: self.size,
            time: AtomicU64::new(self.time()),
            proto: self.proto.clone(),
        }
    }
}

/// protobuf-go's error for malformed wire data.
pub const WIRE_ERROR: &str = "proto: cannot parse invalid wire-format data";
/// `types.ErrInvalidSig`.
pub const INVALID_SIG: &str = "invalid transaction v, r, s values";

fn missing(field: &str) -> String {
    format!("missing required field '{field}' in ProtoTransaction")
}
/// `common.BytesToAddress`/`BytesToHash` bytes: keep the last N, left-pad.
fn fixed<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut out = [0; N];
    let n = bytes.len().min(N);
    out[N - n..].copy_from_slice(&bytes[bytes.len() - n..]);
    out
}
fn access(list: &w::AccessList) -> Vec<AccessTuple> {
    list.tuples
        .iter()
        .map(|t| AccessTuple {
            address: fixed(&t.address),
            keys: t.storage_keys.iter().map(|k| fixed(&k.value)).collect(),
        })
        .collect()
}
fn access_wire(list: &[AccessTuple]) -> w::AccessList {
    w::AccessList {
        tuples: list
            .iter()
            .map(|t| w::AccessTuple {
                address: t.address.to_vec(),
                storage_keys: t
                    .keys
                    .iter()
                    .map(|k| BytesValue { value: k.to_vec() })
                    .collect(),
            })
            .collect(),
    }
}
fn hash_wire(h: &Hash) -> BytesValue {
    BytesValue { value: h.to_vec() }
}
fn work_of(p: &w::Transaction) -> Work {
    Work {
        parent: p.parent_hash.as_ref().map(|h| fixed(&h.value)),
        mix: p.mix_hash.as_ref().map(|h| fixed(&h.value)),
        nonce: p.work_nonce,
    }
}
fn set_work(p: &mut w::Transaction, work: &Work) {
    p.parent_hash = work.parent.as_ref().map(hash_wire);
    p.mix_hash = work.mix.as_ref().map(hash_wire);
    p.work_nonce = work.nonce;
}

/// `crypto.ValidateSignatureValues(v, r, s)` with Homestead's low S.
pub fn valid_signature_values(v: u8, r: &[u8], s: &[u8]) -> bool {
    const N: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36,
        0x41, 0x41,
    ];
    const HALF_N: [u8; 32] = [
        0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0x5d, 0x57, 0x6e, 0x73, 0x57, 0xa4, 0x50, 0x1d, 0xdf, 0xe9, 0x2f, 0x46, 0x68, 0x1b,
        0x20, 0xa0,
    ];
    if big::is_zero(r) || big::is_zero(s) {
        return false;
    }
    if big::cmp(s, &HALF_N).is_gt() {
        return false;
    }
    big::cmp(r, &N).is_lt() && big::cmp(s, &N).is_lt() && (v == 0 || v == 1)
}

/// The low byte of `v.Uint64()`.
fn low_byte(v: &[u8]) -> u8 {
    v.last().copied().unwrap_or(0)
}

/// Decompress or check an input key as `decompressPubKeyIfNeeded`
/// (go-quai built with cgo, as its release binaries are).
fn decompress(key: &[u8]) -> Result<Vec<u8>, String> {
    match key.len() {
        33 => match k256::PublicKey::from_sec1_bytes(key) {
            Ok(k) => Ok(k.to_encoded_point(false).as_bytes().to_vec()),
            Err(_) => Err("invalid public key".into()),
        },
        65 => {
            if key[0] != 4 {
                return Err("invalid secp256k1 public key".into());
            }
            match k256::PublicKey::from_sec1_bytes(key) {
                Ok(_) => Ok(key.to_vec()),
                Err(_) => Err("invalid secp256k1 public key".into()),
            }
        }
        _ => Err("invalid public key length".into()),
    }
}
fn compress(key: &[u8]) -> Vec<u8> {
    k256::PublicKey::from_sec1_bytes(key)
        .map(|k| k.to_encoded_point(true).as_bytes().to_vec())
        .unwrap_or_default()
}

/// `schnorr.ParseSignature` of btcec v2.3.2.
fn parse_schnorr(sig: &[u8]) -> Result<[u8; 64], String> {
    const P: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe, 0xff, 0xff,
        0xfc, 0x2f,
    ];
    const N: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36,
        0x41, 0x41,
    ];
    if sig.len() < 64 {
        return Err(format!(
            "malformed signature: too short: {} < 64",
            sig.len()
        ));
    }
    if sig.len() > 64 {
        return Err(format!("malformed signature: too long: {} > 64", sig.len()));
    }
    if sig[..32] >= P[..] {
        return Err("invalid signature: r >= field prime".into());
    }
    if sig[32..] >= N[..] {
        return Err("invalid signature: s >= group order".into());
    }
    let mut out = [0; 64];
    out.copy_from_slice(sig);
    Ok(out)
}

impl PoolTx {
    /// `proto.Unmarshal` and `Transaction.ProtoDecode` of an RPC or gossip
    /// transaction; `time` is `tx.Time()`.
    pub fn decode(raw: &[u8], time: u64) -> Result<Self, String> {
        // Fields protobuf-go would keep as unknown are dropped first.
        let raw = super::wire::clean(raw, super::wire::TRANSACTION).ok_or(WIRE_ERROR)?;
        let p = w::Transaction::decode(raw.as_slice()).map_err(|_| WIRE_ERROR.to_string())?;
        Self::from_wire(&p, time)
    }
    /// A block transaction (already decoded by the block code).
    pub fn from_primitive(tx: &Primitive, time: u64) -> Result<Self, String> {
        Self::decode(&tx.encode(), time)
    }
    fn from_wire(p: &w::Transaction, time: u64) -> Result<Self, String> {
        let kind = p.r#type.ok_or_else(|| missing("Type"))?;
        let body = match kind {
            0 => {
                let nonce = p.nonce.ok_or_else(|| missing("Nonce"))?;
                let gas = p.gas.ok_or_else(|| missing("Gas"))?;
                let list = p
                    .access_list
                    .as_ref()
                    .ok_or_else(|| missing("AccessList"))?;
                let value = p.value.as_ref().ok_or_else(|| missing("Value"))?;
                let gas_price = p.gas_price.as_ref().ok_or_else(|| missing("GasPrice"))?;
                let data = p.data.as_ref().ok_or_else(|| missing("Data"))?;
                let chain_id = p.chain_id.as_ref().ok_or_else(|| missing("ChainId"))?;
                let v = p.v.as_ref().ok_or("missing required field 'V' in QuaiTx")?;
                let r = p.r.as_ref().ok_or("missing required field 'R' in QuaiTx")?;
                let s = p.s.as_ref().ok_or("missing required field 'S' in QuaiTx")?;
                let (v, r, s) = (big::trim(v), big::trim(r), big::trim(s));
                let signed = !v.is_empty() || !r.is_empty() || !s.is_empty();
                if signed && !valid_signature_values(low_byte(&v), &r, &s) {
                    return Err(INVALID_SIG.into());
                }
                Body::Quai(QuaiBody {
                    chain_id: big::trim(chain_id),
                    nonce,
                    gas_price: big::trim(gas_price),
                    gas,
                    to: p.to.as_ref().map(|t| fixed(t)),
                    value: big::trim(value),
                    data: data.clone(),
                    access_list: access(list),
                    v,
                    r,
                    s,
                    work: work_of(p),
                })
            }
            1 => {
                let gas = p.gas.ok_or_else(|| missing("Gas"))?;
                let list = p
                    .access_list
                    .as_ref()
                    .ok_or_else(|| missing("AccessList"))?;
                let value = p.value.as_ref().ok_or_else(|| missing("Value"))?;
                let data = p.data.as_ref().ok_or_else(|| missing("Data"))?;
                let to = p.to.as_ref().ok_or_else(|| missing("To"))?;
                let origin = p
                    .originating_tx_hash
                    .as_ref()
                    .ok_or_else(|| missing("OriginatingTxHash"))?;
                let index = p.etx_index.ok_or_else(|| missing("EtxIndex"))?;
                let etx_type = p.etx_type.ok_or_else(|| missing("EtxType"))?;
                Body::External(ExternalBody {
                    originating_hash: fixed(&origin.value),
                    index: index as u16,
                    gas,
                    to: fixed(to),
                    value: big::trim(value),
                    data: data.clone(),
                    access_list: access(list),
                    sender: fixed(p.etx_sender.as_deref().unwrap_or_default()),
                    etx_type,
                })
            }
            2 => {
                let ins = p.tx_ins.as_ref().ok_or_else(|| missing("TxIns"))?;
                let outs = p.tx_outs.as_ref().ok_or_else(|| missing("TxOuts"))?;
                let sig = p.signature.as_ref().ok_or_else(|| missing("Signature"))?;
                let chain_id = p.chain_id.as_ref().ok_or_else(|| missing("ChainId"))?;
                let data = p.data.as_ref().ok_or_else(|| missing("Data"))?;
                let mut inputs = Vec::with_capacity(ins.inputs.len());
                for i in &ins.inputs {
                    let point = i.previous.as_ref().ok_or("missing outpoint")?;
                    let hash = point.hash.as_ref().ok_or("missing outpoint hash")?;
                    let index = point.index.ok_or("missing outpoint index")?;
                    inputs.push(QiInput {
                        hash: fixed(&hash.value),
                        index: index as u16,
                        public_key: decompress(i.pub_key.as_deref().unwrap_or_default())?,
                    });
                }
                if inputs.is_empty() {
                    return Err("QiTx must have at least one input".into());
                }
                let mut outputs = Vec::with_capacity(outs.outputs.len());
                for o in &outs.outputs {
                    let d = o.denomination.ok_or("protoTxOut.Denomination is nil")?;
                    if d > 255 {
                        return Err("protoTxOut.Denomination is above the max uint8 value".into());
                    }
                    outputs.push(QiOutput {
                        denomination: d as u8,
                        address: o.address.clone(),
                        lock: big::trim(o.lock.as_deref().unwrap_or_default()),
                    });
                }
                Body::Qi(QiBody {
                    chain_id: big::trim(chain_id),
                    inputs,
                    outputs,
                    signature: parse_schnorr(sig)?,
                    data: data.clone(),
                    work: work_of(p),
                })
            }
            _ => return Err("invalid transaction type".into()),
        };
        Ok(Self::new(body, time))
    }

    pub fn new(body: Body, time: u64) -> Self {
        let proto = canonical(&body, false).encode_to_vec();
        let mut hash = keccak(&proto);
        let mut sender = None;
        match &body {
            Body::Quai(_) => match recover(&body) {
                Ok(from) => {
                    let origin = from[0];
                    hash[0] = origin;
                    hash[1] &= 0x7f;
                    hash[2] = origin;
                    hash[3] &= 0x7f;
                    sender = Some(from);
                }
                Err(_) => hash = [0; 32],
            },
            Body::External(e) => {
                hash[0] = e.originating_hash[2];
                hash[1] = (hash[1] & 0x7f) | (e.sender[1] & 0x80);
                hash[2] = e.to[0];
                hash[3] = (hash[3] & 0x7f) | (e.to[1] & 0x80);
            }
            Body::Qi(q) => {
                hash[0] = q.inputs[0].hash[2];
                hash[1] |= 0x80;
                hash[2] = hash[0];
                hash[3] |= 0x80;
            }
        }
        let size = match &body {
            Body::Quai(q) => quai_rlp_size(q),
            _ => 0,
        };
        Self {
            body,
            hash,
            sender,
            size,
            time: AtomicU64::new(time),
            proto,
        }
    }
    pub fn time(&self) -> u64 {
        self.time.load(Ordering::Relaxed)
    }
    pub fn set_time(&self, t: u64) {
        self.time.store(t, Ordering::Relaxed)
    }
    /// The error of `types.Sender(signer, tx)` for the node's chain ID when
    /// the sender cannot be had.
    pub fn sender_error(&self, chain_id: u64) -> String {
        if let Body::Quai(q) = &self.body {
            if big::cmp(&q.chain_id, &big::from_u64(chain_id)).is_ne() {
                return "invalid chain id for signer".into();
            }
        }
        recover(&self.body).err().unwrap_or_default()
    }

    pub fn type_id(&self) -> u8 {
        match self.body {
            Body::Quai(_) => 0,
            Body::External(_) => 1,
            Body::Qi(_) => 2,
        }
    }
    pub fn quai(&self) -> Option<&QuaiBody> {
        match &self.body {
            Body::Quai(q) => Some(q),
            _ => None,
        }
    }
    pub fn qi(&self) -> Option<&QiBody> {
        match &self.body {
            Body::Qi(q) => Some(q),
            _ => None,
        }
    }
    /// `tx.Nonce()` (0 for other types, which never reach the nonce lists).
    pub fn nonce(&self) -> u64 {
        self.quai().map_or(0, |q| q.nonce)
    }
    pub fn gas(&self) -> u64 {
        match &self.body {
            Body::Quai(q) => q.gas,
            Body::External(e) => e.gas,
            Body::Qi(_) => 0,
        }
    }
    /// `tx.GasPrice()` (zero for an ETX).
    pub fn gas_price(&self) -> &[u8] {
        self.quai().map_or(&[], |q| &q.gas_price)
    }
    /// `tx.Cost()`: gas * gasPrice + value.
    pub fn cost(&self) -> Vec<u8> {
        match &self.body {
            Body::Quai(q) => big::add(&big::mul_u64(&q.gas_price, q.gas), &q.value),
            _ => vec![],
        }
    }
    /// The sender of `types.Sender(pool.signer, tx)` for the pool's chain ID.
    pub fn pool_sender(&self, chain_id: u64) -> Result<Address, &'static str> {
        let Some(q) = self.quai() else {
            return Err("invalid sender");
        };
        if big::cmp(&q.chain_id, &big::from_u64(chain_id)).is_ne() {
            return Err("invalid chain id for signer");
        }
        self.sender.ok_or("invalid sender")
    }
    /// The block-code transaction (for the RPC JSON). Pool transactions
    /// always convert: the pool refuses the values the block decoder does
    /// not represent.
    pub fn primitive(&self) -> Option<Primitive> {
        Primitive::decode(&self.proto).ok()
    }
    /// The signing hash (`signer.Hash`): the protobuf without signature and
    /// work fields.
    pub fn signing_hash(&self) -> Hash {
        keccak(&canonical(&self.body, true).encode_to_vec())
    }
}

/// `Transaction.ProtoEncode` (and `ProtoEncodeTxSigningData` when
/// `signing`).
fn canonical(body: &Body, signing: bool) -> w::Transaction {
    let mut p = w::Transaction::default();
    match body {
        Body::Quai(q) => {
            p.r#type = Some(0);
            p.to = q.to.map(|t| t.to_vec());
            p.nonce = Some(q.nonce);
            p.value = Some(q.value.clone());
            p.gas = Some(q.gas);
            p.data = Some(q.data.clone());
            p.chain_id = Some(q.chain_id.clone());
            p.gas_price = Some(q.gas_price.clone());
            p.access_list = Some(access_wire(&q.access_list));
            if !signing {
                p.v = Some(q.v.clone());
                p.r = Some(q.r.clone());
                p.s = Some(q.s.clone());
                set_work(&mut p, &q.work);
            }
        }
        Body::External(e) => {
            if !signing {
                p.r#type = Some(1);
                p.gas = Some(e.gas);
                p.access_list = Some(access_wire(&e.access_list));
                p.value = Some(e.value.clone());
                p.data = Some(e.data.clone());
                p.to = Some(e.to.to_vec());
                p.originating_tx_hash = Some(hash_wire(&e.originating_hash));
                p.etx_index = Some(e.index as u32);
                p.etx_sender = Some(e.sender.to_vec());
                p.etx_type = Some(e.etx_type);
            }
        }
        Body::Qi(q) => {
            p.r#type = Some(2);
            p.tx_ins = Some(w::Inputs {
                inputs: q
                    .inputs
                    .iter()
                    .map(|i| w::Input {
                        previous: Some(w::OutPoint {
                            hash: Some(hash_wire(&i.hash)),
                            index: Some(i.index as u32),
                        }),
                        pub_key: Some(compress(&i.public_key)),
                    })
                    .collect(),
            });
            p.tx_outs = Some(w::Outputs {
                outputs: q
                    .outputs
                    .iter()
                    .map(|o| w::Output {
                        denomination: Some(o.denomination as u32),
                        address: o.address.clone(),
                        lock: Some(o.lock.clone()),
                    })
                    .collect(),
            });
            p.chain_id = Some(q.chain_id.clone());
            p.data = Some(q.data.clone());
            if !signing {
                p.signature = Some(q.signature.to_vec());
                set_work(&mut p, &q.work);
            }
        }
    }
    p
}

/// `types.Sender` with the transaction's own chain ID (`recoverPlain`).
fn recover(body: &Body) -> Result<Address, String> {
    let Body::Quai(q) = body else {
        return Err("not a Quai transaction".into());
    };
    // V + 27 must fit a byte; V itself 0 or 1.
    if q.v.len() > 1 || q.v.first().is_some_and(|v| *v > 228) {
        return Err(INVALID_SIG.into());
    }
    let v = low_byte(&q.v);
    if !valid_signature_values(v, &q.r, &q.s) {
        return Err(INVALID_SIG.into());
    }
    let hash = keccak(&canonical(body, true).encode_to_vec());
    let key = quai_primitives::recover::recover_pubkey(&hash, &fixed(&q.r), &fixed(&q.s), v)
        .ok_or("recovery failed")?;
    Ok(fixed(&keccak(&key)[12..]))
}

// --- RLP size of the inner QuaiTx (`tx.Size()`) -------------------------------

fn rlp_len_prefix(len: usize) -> usize {
    if len < 56 {
        1
    } else {
        1 + (usize::BITS as usize / 8 - len.leading_zeros() as usize / 8)
    }
}
fn rlp_bytes(b: &[u8]) -> usize {
    if b.len() == 1 && b[0] < 0x80 {
        1
    } else {
        rlp_len_prefix(b.len()) + b.len()
    }
}
fn rlp_u64(v: u64) -> usize {
    let raw = v.to_be_bytes();
    let start = raw.iter().position(|b| *b != 0).unwrap_or(8);
    rlp_bytes(&raw[start..])
}
fn rlp_list(payload: usize) -> usize {
    rlp_len_prefix(payload) + payload
}
fn quai_rlp_size(q: &QuaiBody) -> u64 {
    let access: usize = q
        .access_list
        .iter()
        .map(|t| {
            let keys = rlp_list(t.keys.len() * 33);
            rlp_list(21 + keys)
        })
        .sum();
    let payload = rlp_bytes(&q.chain_id)
        + rlp_u64(q.nonce)
        + rlp_bytes(&q.gas_price)
        + rlp_u64(q.gas)
        + q.to.map_or(1, |_| 21)
        + rlp_bytes(&q.value)
        + rlp_bytes(&q.data)
        + rlp_list(access)
        + rlp_bytes(&q.v)
        + rlp_bytes(&q.r)
        + rlp_bytes(&q.s)
        + q.work.parent.map_or(1, |_| 33)
        + q.work.mix.map_or(1, |_| 33)
        + q.work.nonce.map_or(1, |_| 9);
    rlp_list(payload) as u64
}
