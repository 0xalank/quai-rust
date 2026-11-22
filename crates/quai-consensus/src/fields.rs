//! Header fields that the decoders guarantee present: `Header::decode`,
//! `WorkObject::decode` and `history::Record::decode` refuse a work object
//! without them. Reading them through these helpers turns an absent field
//! into an invalid block, never a panic.
use crate::Error;
use quai_primitives::schema::common::{ProtoAddress, ProtoHash, ProtoLocation};
use quai_primitives::wire::BytesValue;
use quai_primitives::Hash;

/// A message field that wraps bytes: the wire header's `BytesValue` and the
/// body header's `ProtoHash`, `ProtoAddress` and `ProtoLocation`.
pub(crate) trait Value {
    fn bytes(&self) -> &[u8];
}
impl Value for BytesValue {
    fn bytes(&self) -> &[u8] {
        &self.value
    }
}
impl Value for ProtoHash {
    fn bytes(&self) -> &[u8] {
        &self.value
    }
}
impl Value for ProtoAddress {
    fn bytes(&self) -> &[u8] {
        &self.value
    }
}
impl Value for ProtoLocation {
    fn bytes(&self) -> &[u8] {
        &self.value
    }
}

/// A present field, else `Error::Invalid(missing)`.
pub(crate) fn some<T>(v: Option<T>, missing: &'static str) -> Result<T, Error> {
    v.ok_or(Error::Invalid(missing))
}
/// The bytes of an integer or byte-string field.
pub(crate) fn bytes<'a>(v: &'a Option<Vec<u8>>, missing: &'static str) -> Result<&'a [u8], Error> {
    some(v.as_deref(), missing)
}
/// The bytes of a hash field.
pub(crate) fn hash<'a, T: Value>(
    v: &'a Option<T>,
    missing: &'static str,
) -> Result<&'a [u8], Error> {
    Ok(some(v.as_ref(), missing)?.bytes())
}
/// A hash field of 32 bytes.
pub(crate) fn hash32<T: Value>(v: &Option<T>, missing: &'static str) -> Result<Hash, Error> {
    hash(v, missing)?
        .try_into()
        .map_err(|_| Error::Invalid(missing))
}
/// The bytes of an address field.
pub(crate) fn address<'a, T: Value>(
    v: &'a Option<T>,
    missing: &'static str,
) -> Result<&'a [u8], Error> {
    Ok(some(v.as_ref(), missing)?.bytes())
}
/// The bytes of a location field.
pub(crate) fn location<T: Value>(v: &Option<T>) -> Result<&[u8], Error> {
    Ok(some(v.as_ref(), "missing location")?.bytes())
}
/// An address field of 20 bytes.
pub(crate) fn address20<T: Value>(v: &Option<T>, missing: &'static str) -> Result<[u8; 20], Error> {
    address(v, missing)?
        .try_into()
        .map_err(|_| Error::Invalid(missing))
}
