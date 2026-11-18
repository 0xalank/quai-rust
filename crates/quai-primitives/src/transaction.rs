//! Bounded native transaction codecs and signature primitives, not state validation.
//! Behavior follows pinned go-quai transaction.go, transaction_signing.go and utxo.go.
//! MuSig2 aggregation follows btcsuite's untweaked, unsorted key aggregation.
use crate::{
    wire::{tx as w, BytesValue},
    Hash,
};
use k256::{
    ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey},
    elliptic_curve::{sec1::ToEncodedPoint, PrimeField},
    ProjectivePoint, PublicKey, Scalar,
};
use prost::Message;
use sha3::{Digest, Keccak256};

pub type Address = [u8; 20];
const MAX_WIRE: usize = 1024 * 1024;
const MAX_ITEMS: usize = 4096;
const MAX_DATA: usize = 128 * 1024;
const ORDER: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,
    0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
];
const FIELD: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe, 0xff, 0xff, 0xfc, 0x2f,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessTuple {
    pub address: Address,
    pub keys: Vec<Hash>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Work {
    pub parent: Option<Hash>,
    pub mix: Option<Hash>,
    pub nonce: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quai {
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
pub struct External {
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
pub struct Input {
    pub hash: Hash,
    pub index: u16,
    pub public_key: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub denomination: u8,
    pub address: Option<Vec<u8>>,
    pub lock: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Qi {
    pub chain_id: Vec<u8>,
    pub inputs: Vec<Input>,
    pub outputs: Vec<Output>,
    pub signature: [u8; 64],
    pub data: Vec<u8>,
    pub work: Work,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Quai(Quai),
    External(External),
    Qi(Qi),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transaction {
    kind: Kind,
}

pub fn keccak(bytes: &[u8]) -> Hash {
    Keccak256::digest(bytes).into()
}
fn fixed<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut out = [0; N];
    let count = bytes.len().min(N);
    out[N - count..].copy_from_slice(&bytes[bytes.len() - count..]);
    out
}
fn magnitude(value: Option<Vec<u8>>) -> Result<Vec<u8>, String> {
    let v = value.ok_or("required integer missing")?;
    let start = v.iter().position(|b| *b != 0).unwrap_or(v.len());
    if v.len() - start > 32 {
        return Err("integer outside bounded 256-bit domain".into());
    }
    Ok(v[start..].to_vec())
}
fn data(v: Option<Vec<u8>>) -> Result<Vec<u8>, String> {
    let v = v.ok_or("required data missing")?;
    if v.len() > MAX_DATA {
        return Err("transaction data exceeds 128 KiB".into());
    }
    Ok(v)
}
fn work(p: &w::Transaction) -> Work {
    Work {
        parent: p.parent_hash.as_ref().map(|h| fixed(&h.value)),
        mix: p.mix_hash.as_ref().map(|h| fixed(&h.value)),
        nonce: p.work_nonce,
    }
}
fn access(v: Option<w::AccessList>) -> Result<Vec<AccessTuple>, String> {
    let list = v.ok_or("required access list missing")?;
    if list.tuples.len() > MAX_ITEMS
        || list
            .tuples
            .iter()
            .map(|v| v.storage_keys.len())
            .sum::<usize>()
            > MAX_ITEMS
    {
        return Err("access list exceeds bounded item count".into());
    }
    Ok(list
        .tuples
        .into_iter()
        .map(|v| AccessTuple {
            address: fixed(&v.address),
            keys: v.storage_keys.iter().map(|h| fixed(&h.value)).collect(),
        })
        .collect())
}
fn access_wire(v: &[AccessTuple]) -> Option<w::AccessList> {
    Some(w::AccessList {
        tuples: v
            .iter()
            .map(|v| w::AccessTuple {
                address: v.address.to_vec(),
                storage_keys: v
                    .keys
                    .iter()
                    .map(|h| BytesValue { value: h.to_vec() })
                    .collect(),
            })
            .collect(),
    })
}
fn hash_wire(v: Hash) -> BytesValue {
    BytesValue { value: v.to_vec() }
}
fn reduce_order(mut bytes: Hash) -> Hash {
    if bytes >= ORDER {
        let mut borrow = 0i16;
        for i in (0..32).rev() {
            let n = bytes[i] as i16 - ORDER[i] as i16 - borrow;
            bytes[i] = n as u8;
            borrow = i16::from(n < 0);
        }
    }
    bytes
}
fn ecdsa(v: &Quai) -> Result<EcdsaSignature, String> {
    let sig = EcdsaSignature::from_scalars(fixed::<32>(&v.r), fixed::<32>(&v.s))
        .map_err(|_| "invalid ECDSA scalars")?;
    if v.v.last().copied().unwrap_or(0) > 1 || sig.normalize_s().is_some() {
        return Err("invalid ECDSA recovery bit or high S".into());
    }
    Ok(sig)
}
impl Transaction {
    pub fn external(value: External) -> Result<Self, String> {
        let tx = Self {
            kind: Kind::External(value),
        };
        Self::decode(&tx.encode())
    }
    pub fn kind(&self) -> &Kind {
        &self.kind
    }
    pub fn type_id(&self) -> u8 {
        match self.kind {
            Kind::Quai(_) => 0,
            Kind::External(_) => 1,
            Kind::Qi(_) => 2,
        }
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_WIRE {
            return Err("transaction protobuf exceeds 1 MiB".into());
        }
        let p = w::Transaction::decode(bytes).map_err(|_| "malformed transaction protobuf")?;
        let work = work(&p);
        let kind = match p.r#type.ok_or("transaction type missing")? {
            0 => {
                let v = Quai {
                    chain_id: magnitude(p.chain_id)?,
                    nonce: p.nonce.ok_or("nonce missing")?,
                    gas_price: magnitude(p.gas_price)?,
                    gas: p.gas.ok_or("gas missing")?,
                    to: p.to.map(|v| fixed(&v)),
                    value: magnitude(p.value)?,
                    data: data(p.data)?,
                    access_list: access(p.access_list)?,
                    v: magnitude(p.v)?,
                    r: magnitude(p.r)?,
                    s: magnitude(p.s)?,
                    work,
                };
                if !v.v.is_empty() || !v.r.is_empty() || !v.s.is_empty() {
                    ecdsa(&v)?;
                }
                Kind::Quai(v)
            }
            1 => Kind::External(External {
                originating_hash: fixed(
                    &p.originating_tx_hash
                        .ok_or("originating hash missing")?
                        .value,
                ),
                index: p.etx_index.ok_or("ETX index missing")? as u16,
                gas: p.gas.ok_or("gas missing")?,
                to: fixed(&p.to.ok_or("ETX recipient missing")?),
                value: magnitude(p.value)?,
                data: data(p.data)?,
                access_list: access(p.access_list)?,
                sender: fixed(&p.etx_sender.unwrap_or_default()),
                etx_type: p.etx_type.ok_or("ETX type missing")?,
            }),
            2 => {
                let input = p.tx_ins.ok_or("inputs missing")?.inputs;
                let output = p.tx_outs.ok_or("outputs missing")?.outputs;
                if input.is_empty() || input.len() > MAX_ITEMS || output.len() > MAX_ITEMS {
                    return Err("invalid bounded input/output count".into());
                }
                let inputs = input
                    .into_iter()
                    .map(|v| {
                        let previous = v.previous.ok_or("outpoint missing")?;
                        let key =
                            PublicKey::from_sec1_bytes(&v.pub_key.ok_or("public key missing")?)
                                .map_err(|_| "invalid input public key")?;
                        Ok(Input {
                            hash: fixed(&previous.hash.ok_or("outpoint hash missing")?.value),
                            index: previous.index.ok_or("outpoint index missing")? as u16,
                            public_key: key.to_encoded_point(false).as_bytes().to_vec(),
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let outputs = output
                    .into_iter()
                    .map(|v| {
                        Ok(Output {
                            denomination: u8::try_from(
                                v.denomination.ok_or("denomination missing")?,
                            )
                            .map_err(|_| "denomination exceeds uint8")?,
                            address: v.address,
                            lock: magnitude(Some(v.lock.unwrap_or_default()))?,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let signature: [u8; 64] = p
                    .signature
                    .ok_or("signature missing")?
                    .try_into()
                    .map_err(|_| "Schnorr signature must be 64 bytes")?;
                if signature[..32] >= FIELD[..] {
                    return Err("Schnorr R exceeds field".into());
                }
                if signature[32..] >= ORDER[..] {
                    return Err("Schnorr S exceeds group order".into());
                }
                Kind::Qi(Qi {
                    chain_id: magnitude(p.chain_id)?,
                    inputs,
                    outputs,
                    signature,
                    data: data(p.data)?,
                    work,
                })
            }
            _ => return Err("unknown transaction type".into()),
        };
        Ok(Self { kind })
    }
    fn proto(&self, signing: bool) -> w::Transaction {
        let mut p = w::Transaction::default();
        match &self.kind {
            Kind::Quai(v) => {
                p.r#type = Some(0);
                p.to = v.to.map(|v| v.to_vec());
                p.nonce = Some(v.nonce);
                p.value = Some(v.value.clone());
                p.gas = Some(v.gas);
                p.data = Some(v.data.clone());
                p.chain_id = Some(v.chain_id.clone());
                p.gas_price = Some(v.gas_price.clone());
                p.access_list = access_wire(&v.access_list);
                if !signing {
                    p.v = Some(v.v.clone());
                    p.r = Some(v.r.clone());
                    p.s = Some(v.s.clone());
                    set_work(&mut p, &v.work);
                }
            }
            Kind::External(v) => {
                if !signing {
                    p.r#type = Some(1);
                    p.to = Some(v.to.to_vec());
                    p.value = Some(v.value.clone());
                    p.gas = Some(v.gas);
                    p.data = Some(v.data.clone());
                    p.access_list = access_wire(&v.access_list);
                    p.originating_tx_hash = Some(hash_wire(v.originating_hash));
                    p.etx_index = Some(v.index as u32);
                    p.etx_sender = Some(v.sender.to_vec());
                    p.etx_type = Some(v.etx_type);
                }
            }
            Kind::Qi(v) => {
                p.r#type = Some(2);
                p.chain_id = Some(v.chain_id.clone());
                p.data = Some(v.data.clone());
                p.tx_ins = Some(w::Inputs {
                    inputs: v
                        .inputs
                        .iter()
                        .map(|v| w::Input {
                            previous: Some(w::OutPoint {
                                hash: Some(hash_wire(v.hash)),
                                index: Some(v.index as u32),
                            }),
                            pub_key: Some(
                                PublicKey::from_sec1_bytes(&v.public_key)
                                    .expect("decoded key")
                                    .to_encoded_point(true)
                                    .as_bytes()
                                    .to_vec(),
                            ),
                        })
                        .collect(),
                });
                p.tx_outs = Some(w::Outputs {
                    outputs: v
                        .outputs
                        .iter()
                        .map(|v| w::Output {
                            denomination: Some(v.denomination as u32),
                            address: v.address.clone(),
                            lock: Some(v.lock.clone()),
                        })
                        .collect(),
                });
                if !signing {
                    p.signature = Some(v.signature.to_vec());
                    set_work(&mut p, &v.work);
                }
            }
        }
        p
    }
    pub fn encode(&self) -> Vec<u8> {
        self.proto(false).encode_to_vec()
    }
    pub fn signing_bytes(&self) -> Vec<u8> {
        self.proto(true).encode_to_vec()
    }
    /// ETXs have no signature; their reference signing message is the empty protobuf.
    pub fn signing_hash(&self) -> Hash {
        keccak(&self.signing_bytes())
    }
    pub fn sender(&self) -> Result<Address, String> {
        match &self.kind {
            Kind::External(v) => Ok(v.sender),
            Kind::Qi(_) => Err("Qi has no single ECDSA sender".into()),
            Kind::Quai(v) => {
                let sig = ecdsa(v)?;
                if v.v.len() > 1 {
                    return Err("recovery value exceeds sender domain".into());
                }
                let rec = RecoveryId::try_from(v.v.last().copied().unwrap_or(0))
                    .map_err(|_| "invalid recovery bit")?;
                let key = VerifyingKey::recover_from_prehash(&self.signing_hash(), &sig, rec)
                    .map_err(|_| "sender recovery failed")?;
                Ok(fixed(
                    &keccak(&key.to_encoded_point(false).as_bytes()[1..])[12..],
                ))
            }
        }
    }
    /// Explicit Quai origin is useful before sender recovery; it is not authentication.
    pub fn id(&self, origin: Option<[u8; 2]>) -> Result<Hash, String> {
        let mut h = keccak(&self.encode());
        match &self.kind {
            Kind::Quai(_) => {
                let route = match origin {
                    Some([r, z]) if r < 16 && z < 16 => r * 16 + z,
                    Some(_) => return Err("origin location outside nibble domain".into()),
                    None => self.sender()?[0],
                };
                h[0] = route;
                h[1] &= 0x7f;
                h[2] = route;
                h[3] &= 0x7f;
            }
            Kind::External(v) => {
                h[0] = v.originating_hash[2];
                h[1] = (h[1] & 0x7f) | (v.sender[1] & 0x80);
                h[2] = v.to[0];
                h[3] = (h[3] & 0x7f) | (v.to[1] & 0x80);
            }
            Kind::Qi(v) => {
                h[0] = v.inputs[0].hash[2];
                h[1] |= 0x80;
                h[2] = h[0];
                h[3] |= 0x80;
            }
        }
        Ok(h)
    }
    /// Signature mathematics only; does not check UTXO ownership, value, fees or locks.
    pub fn qi_aggregate_key(&self) -> Result<Vec<u8>, String> {
        let Kind::Qi(v) = &self.kind else {
            return Err("not a Qi transaction".into());
        };
        let keys = v
            .inputs
            .iter()
            .map(|v| PublicKey::from_sec1_bytes(&v.public_key).expect("decoded key"))
            .collect::<Vec<_>>();
        if keys.len() == 1 {
            return Ok(keys[0].to_encoded_point(true).as_bytes().to_vec());
        }
        let encoded = keys
            .iter()
            .map(|k| k.to_encoded_point(true).as_bytes().to_vec())
            .collect::<Vec<_>>();
        let fingerprint = tagged(b"KeyAgg list", &encoded.concat());
        let second = encoded.iter().find(|k| **k != encoded[0]);
        let mut sum = ProjectivePoint::IDENTITY;
        for (key, encoded) in keys.iter().zip(&encoded) {
            let coefficient = if second == Some(encoded) {
                Scalar::ONE
            } else {
                let mut message = fingerprint.to_vec();
                message.extend_from_slice(encoded);
                Option::<Scalar>::from(Scalar::from_repr(
                    reduce_order(tagged(b"KeyAgg coefficient", &message)).into(),
                ))
                .expect("reduced scalar")
            };
            sum += ProjectivePoint::from(*key.as_affine()) * coefficient;
        }
        let key =
            PublicKey::from_affine(sum.to_affine()).map_err(|_| "aggregate key at infinity")?;
        Ok(key.to_encoded_point(true).as_bytes().to_vec())
    }
    pub fn verify_qi_signature(&self) -> Result<bool, String> {
        let Kind::Qi(v) = &self.kind else {
            return Err("not a Qi transaction".into());
        };
        let key = self.qi_aggregate_key()?;
        let key = k256::schnorr::VerifyingKey::from_bytes(&key[1..])
            .map_err(|_| "invalid aggregate x-only key")?;
        let Ok(signature) = k256::schnorr::Signature::try_from(v.signature.as_slice()) else {
            return Ok(false);
        };
        Ok(key.verify_raw(&self.signing_hash(), &signature).is_ok())
    }
    /// RLP byte-string envelope used by stored transactions and nested receipts.
    pub fn rlp_envelope(&self) -> Vec<u8> {
        crate::trie::bytes(&self.typed_rlp())
    }
    /// Reference typed-RLP encoding. Qi's WireQiTx omits work fields.
    /// Decoding remains a separate ingestion gate.
    pub fn typed_rlp(&self) -> Vec<u8> {
        let fields = match &self.kind {
            Kind::Quai(v) => vec![
                rb(&v.chain_id),
                ru(v.nonce),
                rb(&v.gas_price),
                ru(v.gas),
                rb(v.to.as_ref().map(|v| v.as_slice()).unwrap_or_default()),
                rb(&v.value),
                rb(&v.data),
                ra(&v.access_list),
                rb(&v.v),
                rb(&v.r),
                rb(&v.s),
                rw(&v.work.parent),
                rw(&v.work.mix),
                rn(v.work.nonce),
            ],
            Kind::External(v) => vec![
                rb(&v.originating_hash),
                ru(v.index as u64),
                ru(v.gas),
                rb(&v.to),
                rb(&v.value),
                rb(&v.data),
                ra(&v.access_list),
                rb(&v.sender),
                ru(v.etx_type),
            ],
            Kind::Qi(v) => vec![
                rb(&v.chain_id),
                rl(v.inputs
                    .iter()
                    .map(|v| {
                        rl(vec![
                            rl(vec![rb(&v.hash), ru(v.index as u64)]),
                            rb(&v.public_key),
                        ])
                    })
                    .collect()),
                rl(v.outputs
                    .iter()
                    .map(|v| {
                        rl(vec![
                            ru(v.denomination as u64),
                            rb(v.address.as_deref().unwrap_or_default()),
                            rb(&v.lock),
                        ])
                    })
                    .collect()),
                rb(&v.signature),
                rb(&v.data),
            ],
        };
        let mut result = vec![self.type_id()];
        result.extend(rl(fields));
        result
    }
}
fn set_work(p: &mut w::Transaction, v: &Work) {
    p.parent_hash = v.parent.map(hash_wire);
    p.mix_hash = v.mix.map(hash_wire);
    p.work_nonce = v.nonce;
}
fn tagged(tag: &[u8], message: &[u8]) -> Hash {
    let tag = sha2::Sha256::digest(tag);
    let mut h = sha2::Sha256::new();
    h.update(tag);
    h.update(tag);
    h.update(message);
    h.finalize().into()
}
fn prefix(length: usize, short: u8, long: u8) -> Vec<u8> {
    if length < 56 {
        vec![short + length as u8]
    } else {
        let raw = length.to_be_bytes();
        let start = raw.iter().position(|b| *b != 0).unwrap();
        let mut out = vec![long + (raw.len() - start) as u8];
        out.extend_from_slice(&raw[start..]);
        out
    }
}
fn rb(v: &[u8]) -> Vec<u8> {
    if v.len() == 1 && v[0] < 128 {
        return v.to_vec();
    }
    let mut out = prefix(v.len(), 0x80, 0xb7);
    out.extend_from_slice(v);
    out
}
fn rl(values: Vec<Vec<u8>>) -> Vec<u8> {
    let mut out = prefix(values.iter().map(Vec::len).sum(), 0xc0, 0xf7);
    for v in values {
        out.extend(v);
    }
    out
}
fn ru(v: u64) -> Vec<u8> {
    let raw = v.to_be_bytes();
    let start = raw.iter().position(|b| *b != 0).unwrap_or(raw.len());
    rb(&raw[start..])
}
fn rw(v: &Option<Hash>) -> Vec<u8> {
    rb(v.as_ref().map(|v| v.as_slice()).unwrap_or_default())
}
fn rn(v: Option<u64>) -> Vec<u8> {
    match v {
        Some(v) => rb(&v.to_be_bytes()),
        None => rb(&[]),
    }
}
fn ra(v: &[AccessTuple]) -> Vec<u8> {
    rl(v.iter()
        .map(|v| {
            rl(vec![
                rb(&v.address),
                rl(v.keys.iter().map(|h| rb(h)).collect()),
            ])
        })
        .collect())
}
