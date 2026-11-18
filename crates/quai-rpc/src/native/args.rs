//! Positional arguments decoded as go-quai v0.57.0 decodes them, with the
//! same error texts: `rpc/json.go` (`parsePositionalArguments`),
//! `common/hexutil/json.go`, `rpc/types.go` (`BlockNumber`,
//! `BlockNumberOrHash`), `common/types.go` (`MixedcaseAddress`) and
//! `encoding/json` type errors.
use super::RpcError;
use quai_primitives::Hash;
use serde_json::Value;

pub type Address = [u8; 20];

/// The arguments of one call.
pub struct Args<'a> {
    values: &'a [Value],
}
impl<'a> Args<'a> {
    /// `max` is the number of parameters of the Go method.
    pub fn new(params: &'a Value, max: usize) -> Result<Self, RpcError> {
        let values = match params {
            Value::Null => &[][..],
            Value::Array(v) => v.as_slice(),
            _ => return Err(RpcError::params("non-array args")),
        };
        if values.len() > max {
            return Err(RpcError::params(format!(
                "too many arguments, want at most {max}"
            )));
        }
        Ok(Self { values })
    }
    /// A required argument: missing is an error, as for a Go value type.
    pub fn required(&self, i: usize) -> Result<&'a Value, RpcError> {
        self.values
            .get(i)
            .ok_or_else(|| RpcError::params(format!("missing value for required argument {i}")))
    }
    /// An optional argument (a Go pointer type): missing or null is None.
    pub fn optional(&self, i: usize) -> Option<&'a Value> {
        self.values.get(i).filter(|v| !v.is_null())
    }
    /// Decode argument `i` with `f`; a decode error becomes
    /// "invalid argument i: <error>".
    pub fn get<T>(&self, i: usize, f: impl Fn(&Value) -> Result<T, String>) -> Result<T, RpcError> {
        f(self.required(i)?).map_err(|e| invalid(i, e))
    }
    pub fn get_optional<T>(
        &self,
        i: usize,
        f: impl Fn(&Value) -> Result<T, String>,
    ) -> Result<Option<T>, RpcError> {
        self.optional(i)
            .map(|v| f(v).map_err(|e| invalid(i, e)))
            .transpose()
    }
}
pub fn invalid(i: usize, error: String) -> RpcError {
    RpcError::params(format!("invalid argument {i}: {error}"))
}

/// The JSON kind name in an `encoding/json` UnmarshalTypeError.
fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
/// "json: cannot unmarshal <what> into Go value of type <typ>"
pub fn type_error(what: &str, typ: &str) -> String {
    format!("json: cannot unmarshal {what} into Go value of type {typ}")
}

const EMPTY_STRING: &str = "empty hex string";
const SYNTAX: &str = "invalid hex string";
const MISSING_PREFIX: &str = "hex string without 0x prefix";
const ODD_LENGTH: &str = "hex string of odd length";
const EMPTY_NUMBER: &str = "hex string \"0x\"";
const LEADING_ZERO: &str = "hex number with leading zero digits";
const UINT64_RANGE: &str = "hex number > 64 bits";
const BIG256_RANGE: &str = "hex number > 256 bits";

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}
fn has_prefix(s: &str) -> bool {
    s.len() >= 2 && (s.starts_with("0x") || s.starts_with("0X"))
}
/// `hexutil.checkText(input, true)`: Ok(None) for the empty string.
fn check_text(s: &str) -> Result<&str, &'static str> {
    if s.is_empty() {
        return Ok(s);
    }
    if !has_prefix(s) {
        return Err(MISSING_PREFIX);
    }
    let s = &s[2..];
    if s.len() % 2 != 0 {
        return Err(ODD_LENGTH);
    }
    Ok(s)
}
/// `hexutil.checkNumberText`.
fn check_number_text(s: &str) -> Result<&str, &'static str> {
    if s.is_empty() {
        return Ok(s);
    }
    if !has_prefix(s) {
        return Err(MISSING_PREFIX);
    }
    let s = &s[2..];
    if s.is_empty() {
        return Err(EMPTY_NUMBER);
    }
    if s.len() > 1 && s.starts_with('0') {
        return Err(LEADING_ZERO);
    }
    Ok(s)
}
fn decode_pairs(raw: &str) -> Result<Vec<u8>, &'static str> {
    raw.as_bytes()
        .chunks(2)
        .map(|p| Ok(nibble(p[0]).ok_or(SYNTAX)? << 4 | nibble(p[1]).ok_or(SYNTAX)?))
        .collect()
}
/// `hexutil.UnmarshalFixedJSON(typ, input, out)` for `N` bytes.
fn fixed<const N: usize>(v: &Value, typ: &str) -> Result<[u8; N], String> {
    let Value::String(s) = v else {
        return Err(type_error("non-string", typ));
    };
    let raw = check_text(s).map_err(|e| type_error(e, typ))?;
    if raw.len() / 2 != N {
        return Err(format!(
            "hex string has length {}, want {} for {typ}",
            raw.len(),
            N * 2
        ));
    }
    let bytes = decode_pairs(raw).map_err(|e| type_error(e, typ))?;
    let mut out = [0; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}
/// `common.Hash`.
pub fn hash(v: &Value) -> Result<Hash, String> {
    fixed::<32>(v, "common.Hash")
}
/// `common.Address` (and `common.InternalAddress` errors).
pub fn address(v: &Value) -> Result<Address, String> {
    fixed::<20>(v, "common.InternalAddress")
}
/// `common.MixedcaseAddress`: the address and the original string.
pub fn mixed_address(v: &Value) -> Result<(Address, String), String> {
    let a = address(v)?;
    Ok((a, v.as_str().unwrap_or_default().to_string()))
}
/// `common.AddressBytes`.
pub fn address_bytes(v: &Value) -> Result<Address, String> {
    fixed::<20>(v, "common.AddressBytes")
}
/// `hexutil.Uint64`.
pub fn uint64(v: &Value) -> Result<u64, String> {
    number_text(v, "hexutil.Uint64")
}
/// `hexutil.Uint` (64-bit platform).
pub fn uint(v: &Value) -> Result<u64, String> {
    number_text(v, "hexutil.Uint")
}
fn number_text(v: &Value, typ: &str) -> Result<u64, String> {
    let Value::String(s) = v else {
        return Err(type_error("non-string", typ));
    };
    let raw = check_number_text(s).map_err(|e| type_error(e, typ))?;
    if raw.len() > 16 {
        return Err(type_error(UINT64_RANGE, typ));
    }
    let mut n = 0u64;
    for c in raw.bytes() {
        n = n * 16 + nibble(c).ok_or_else(|| type_error(SYNTAX, typ))? as u64;
    }
    Ok(n)
}
/// `*hexutil.Big`, as a 256-bit big-endian value.
pub fn big(v: &Value) -> Result<[u8; 32], String> {
    let typ = "*hexutil.Big";
    let Value::String(s) = v else {
        return Err(type_error("non-string", typ));
    };
    let raw = check_number_text(s).map_err(|e| type_error(e, typ))?;
    if raw.len() > 64 {
        return Err(type_error(BIG256_RANGE, typ));
    }
    let mut out = [0u8; 32];
    for (i, c) in raw.bytes().rev().enumerate() {
        let n = nibble(c).ok_or_else(|| type_error(SYNTAX, typ))?;
        out[31 - i / 2] |= if i % 2 == 0 { n } else { n << 4 };
    }
    Ok(out)
}
/// `hexutil.Bytes`.
pub fn bytes(v: &Value) -> Result<Vec<u8>, String> {
    let typ = "hexutil.Bytes";
    let Value::String(s) = v else {
        return Err(type_error("non-string", typ));
    };
    let raw = check_text(s).map_err(|e| type_error(e, typ))?;
    decode_pairs(raw).map_err(|e| type_error(e, typ))
}
/// A Go `bool`.
pub fn boolean(v: &Value) -> Result<bool, String> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Null => Ok(false),
        other => Err(type_error(kind(other), "bool")),
    }
}
/// A Go `string`.
pub fn string(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Null => Ok(String::new()),
        other => Err(type_error(kind(other), "string")),
    }
}

/// `hexutil.DecodeUint64`.
fn decode_uint64(s: &str) -> Result<u64, String> {
    if s.is_empty() {
        return Err(EMPTY_STRING.into());
    }
    if !has_prefix(s) {
        return Err(MISSING_PREFIX.into());
    }
    let raw = &s[2..];
    if raw.is_empty() {
        return Err(EMPTY_NUMBER.into());
    }
    if raw.len() > 1 && raw.starts_with('0') {
        return Err(LEADING_ZERO.into());
    }
    let mut n = 0u64;
    for c in raw.bytes() {
        let d = nibble(c).ok_or(SYNTAX)? as u64;
        n = n
            .checked_mul(16)
            .and_then(|n| n.checked_add(d))
            .ok_or(UINT64_RANGE)?;
    }
    Ok(n)
}

/// `rpc.BlockNumber`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockNumber {
    Pending,
    Latest,
    Number(u64),
}
impl BlockNumber {
    /// The Go `int64` value (latest -1, pending -2, earliest 0).
    pub fn int64(self) -> i64 {
        match self {
            Self::Pending => -2,
            Self::Latest => -1,
            Self::Number(n) => n as i64,
        }
    }
}
fn block_number_text(input: &str, too_large: &str) -> Result<BlockNumber, String> {
    match input {
        "earliest" => Ok(BlockNumber::Number(0)),
        "latest" => Ok(BlockNumber::Latest),
        "pending" => Ok(BlockNumber::Pending),
        _ => {
            let n = decode_uint64(input)?;
            if n > i64::MAX as u64 {
                return Err(too_large.into());
            }
            Ok(BlockNumber::Number(n))
        }
    }
}
/// `rpc.BlockNumber.UnmarshalJSON`: the raw JSON text, unquoted.
pub fn block_number(v: &Value) -> Result<BlockNumber, String> {
    let text = match v {
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    };
    block_number_text(&text, "block number larger than int64")
}

/// `rpc.BlockNumberOrHash`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockRef {
    Number(BlockNumber),
    Hash {
        hash: Hash,
        canonical: bool,
    },
    /// Neither field set (`{}` or null).
    Neither,
}
pub fn block_ref(v: &Value) -> Result<BlockRef, String> {
    match v {
        Value::Object(map) => {
            let number = map
                .get("blockNumber")
                .filter(|v| !v.is_null())
                .map(block_number)
                .transpose()?;
            let hash = map
                .get("blockHash")
                .filter(|v| !v.is_null())
                .map(self::hash)
                .transpose()?;
            let canonical = map
                .get("requireCanonical")
                .filter(|v| !v.is_null())
                .map(boolean)
                .transpose()?
                .unwrap_or(false);
            match (number, hash) {
                (Some(_), Some(_)) => Err(
                    "cannot specify both BlockHash and BlockNumber, choose one or the other".into(),
                ),
                (Some(n), None) => Ok(BlockRef::Number(n)),
                (None, Some(hash)) => Ok(BlockRef::Hash { hash, canonical }),
                (None, None) => Ok(BlockRef::Neither),
            }
        }
        Value::Null => Ok(BlockRef::Neither),
        Value::String(s) => match s.as_str() {
            "earliest" => Ok(BlockRef::Number(BlockNumber::Number(0))),
            "latest" => Ok(BlockRef::Number(BlockNumber::Latest)),
            "pending" => Ok(BlockRef::Number(BlockNumber::Pending)),
            s if s.len() == 66 => {
                // `common.Hash.UnmarshalText`: unwrapped hexutil errors.
                let raw = check_text(s).map_err(str::to_string)?;
                let bytes = decode_pairs(raw).map_err(str::to_string)?;
                Ok(BlockRef::Hash {
                    hash: bytes.try_into().map_err(|_| SYNTAX.to_string())?,
                    canonical: false,
                })
            }
            s => match block_number_text(s, "blocknumber too high")? {
                n => Ok(BlockRef::Number(n)),
            },
        },
        other => Err(type_error(kind(other), "string")),
    }
}

/// `common.FromHex` + `common.BytesToHash`: the lenient key parse of
/// `GetStorageAt`.
pub fn lenient_hash(s: &str) -> Hash {
    let mut s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s)
        .to_string();
    if s.len() % 2 == 1 {
        s.insert(0, '0');
    }
    // hex.DecodeString returns the bytes before the first bad pair.
    let mut bytes = vec![];
    for p in s.as_bytes().chunks(2) {
        match (nibble(p[0]), nibble(p[1])) {
            (Some(a), Some(b)) => bytes.push(a << 4 | b),
            _ => break,
        }
    }
    let mut out = [0u8; 32];
    let take = bytes.len().min(32);
    out[32 - take..].copy_from_slice(&bytes[bytes.len() - take..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn go_error_texts() {
        assert_eq!(
            block_number(&json!("zz")).unwrap_err(),
            "hex string without 0x prefix"
        );
        assert_eq!(block_number(&json!("0x")).unwrap_err(), "hex string \"0x\"");
        assert_eq!(
            block_number(&json!("0x01")).unwrap_err(),
            "hex number with leading zero digits"
        );
        assert_eq!(block_number(&json!("latest")).unwrap(), BlockNumber::Latest);
        assert_eq!(
            block_number(&json!("0x10")).unwrap(),
            BlockNumber::Number(16)
        );
        assert_eq!(
            hash(&json!("0x12")).unwrap_err(),
            "hex string has length 2, want 64 for common.Hash"
        );
        assert_eq!(
            hash(&json!(5)).unwrap_err(),
            "json: cannot unmarshal non-string into Go value of type common.Hash"
        );
        assert_eq!(
            hash(&json!("12")).unwrap_err(),
            "json: cannot unmarshal hex string without 0x prefix into Go value of type common.Hash"
        );
        assert_eq!(
            boolean(&json!("true")).unwrap_err(),
            "json: cannot unmarshal string into Go value of type bool"
        );
        assert_eq!(big(&json!("0x1f")).unwrap()[31], 0x1f);
        assert_eq!(big(&json!("0x100")).unwrap()[30..], [1, 0]);
        assert_eq!(uint64(&json!("0x10")).unwrap(), 16);
        assert_eq!(lenient_hash("0x1")[31], 1);
        assert_eq!(lenient_hash("0x1zz")[31], 1);
    }
}
