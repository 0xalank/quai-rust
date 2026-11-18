//! protobuf-go's reading of the wire format (google.golang.org/protobuf
//! v1.36.6, `impl/decode.go`, `protowire`), for the transaction messages,
//! before prost decodes them.
//!
//! protobuf-go keeps a field whose number it does not know, or whose wire
//! type does not match the field it knows, as an unknown field; prost
//! refuses a known field with another wire type and field numbers above
//! 2^29 - 1. So `clean` re-encodes a message with only the fields protobuf-go
//! decodes, and fails where protobuf-go fails: a truncated or overlong
//! varint, a length past the end, a field number of 0 or above 2^29 - 1
//! (inside a skipped group: above 2^31 - 1), wire types 6 and 7, an
//! end-group tag without its start, nesting beyond protobuf-go's recursion
//! limit.
use prost::encoding::encode_varint;

#[derive(Clone, Copy)]
pub enum Kind {
    Varint,
    Bytes,
    Message(&'static [(u64, Kind)]),
}
use Kind::{Bytes, Message, Varint};

const HASH: &[(u64, Kind)] = &[(1, Bytes)];
const ACCESS_TUPLE: &[(u64, Kind)] = &[(1, Bytes), (2, Message(HASH))];
const ACCESS_LIST: &[(u64, Kind)] = &[(1, Message(ACCESS_TUPLE))];
const OUTPOINT: &[(u64, Kind)] = &[(1, Message(HASH)), (2, Varint)];
const TX_IN: &[(u64, Kind)] = &[(1, Message(OUTPOINT)), (2, Bytes)];
const TX_INS: &[(u64, Kind)] = &[(1, Message(TX_IN))];
const TX_OUT: &[(u64, Kind)] = &[(1, Varint), (2, Bytes), (3, Bytes)];
const TX_OUTS: &[(u64, Kind)] = &[(1, Message(TX_OUT))];
/// `ProtoTransaction` (core/types/proto_block.proto).
pub const TRANSACTION: &[(u64, Kind)] = &[
    (1, Varint),
    (2, Bytes),
    (3, Varint),
    (4, Bytes),
    (5, Varint),
    (6, Bytes),
    (7, Bytes),
    (8, Bytes),
    (9, Message(ACCESS_LIST)),
    (10, Bytes),
    (11, Bytes),
    (12, Bytes),
    (13, Message(HASH)),
    (14, Varint),
    (15, Message(TX_INS)),
    (16, Message(TX_OUTS)),
    (17, Bytes),
    (18, Bytes),
    (19, Message(HASH)),
    (20, Message(HASH)),
    (21, Varint),
    (22, Varint),
];
/// `protowire.MaxValidNumber`.
const MAX_VALID_NUMBER: u64 = (1 << 29) - 1;
/// protobuf-go's `DefaultRecursionLimit`.
const RECURSION_LIMIT: usize = 10_000;

/// `protowire.ConsumeVarint`: the value and its length.
fn varint(b: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..10 {
        let byte = *b.get(i)?;
        if i == 9 && byte > 1 {
            return None;
        }
        v |= ((byte & 0x7f) as u64) << (7 * i);
        if byte < 0x80 {
            return Some((v, i + 1));
        }
    }
    None
}
/// `protowire.ConsumeTag`: field number (at least 1, at most 2^31 - 1),
/// wire type and length.
fn tag(b: &[u8]) -> Option<(u64, u8, usize)> {
    let (v, n) = varint(b)?;
    let num = v >> 3;
    if num < 1 || num > i32::MAX as u64 {
        return None;
    }
    Some((num, (v & 7) as u8, n))
}
/// `protowire.ConsumeFieldValue`: the length of a field's value.
fn value_len(num: u64, wire: u8, b: &[u8], depth: usize) -> Option<usize> {
    match wire {
        0 => varint(b).map(|(_, n)| n),
        1 => (b.len() >= 8).then_some(8),
        5 => (b.len() >= 4).then_some(4),
        2 => {
            let (len, n) = varint(b)?;
            let len = usize::try_from(len).ok()?;
            (len <= b.len() - n).then_some(n + len)
        }
        3 => {
            if depth > RECURSION_LIMIT {
                return None;
            }
            let mut p = 0;
            loop {
                let (n2, w2, t) = tag(&b[p..])?;
                p += t;
                if w2 == 4 {
                    return (n2 == num).then_some(p);
                }
                p += value_len(n2, w2, &b[p..], depth + 1)?;
            }
        }
        _ => None,
    }
}

/// Re-encode `data` as the message `fields` with only the fields
/// protobuf-go decodes as known; None where protobuf-go fails.
pub fn clean(data: &[u8], fields: &[(u64, Kind)]) -> Option<Vec<u8>> {
    clean_at(data, fields, 0)
}
fn clean_at(data: &[u8], fields: &[(u64, Kind)], depth: usize) -> Option<Vec<u8>> {
    if depth > RECURSION_LIMIT {
        return None;
    }
    let mut out = Vec::with_capacity(data.len());
    let mut p = 0;
    while p < data.len() {
        let (num, wire, t) = tag(&data[p..])?;
        // unmarshalPointerEager: a field number above MaxValidNumber fails.
        if num > MAX_VALID_NUMBER {
            return None;
        }
        if wire == 4 {
            // An end-group tag outside a group.
            return None;
        }
        let start = p;
        p += t;
        let len = value_len(num, wire, &data[p..], depth)?;
        let value = &data[p..p + len];
        p += len;
        let known = fields.iter().find(|(n, _)| *n == num).map(|(_, k)| *k);
        match (known, wire) {
            (Some(Varint), 0) | (Some(Bytes), 2) => out.extend_from_slice(&data[start..p]),
            (Some(Message(inner)), 2) => {
                let (l, n) = varint(value)?;
                let body = clean_at(&value[n..n + l as usize], inner, depth + 1)?;
                encode_varint(num << 3 | 2, &mut out);
                encode_varint(body.len() as u64, &mut out);
                out.extend_from_slice(&body);
            }
            // Unknown to protobuf-go: kept as unknown bytes, never read.
            _ => {}
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_and_mistyped_fields_are_dropped() {
        // type = 0 (varint), field 19 as a varint (mistyped), field 99.
        let msg = [0x08, 0x00, 0x98, 0x01, 0x05, 0x98, 0x06, 0x01];
        assert_eq!(clean(&msg, TRANSACTION), Some(vec![0x08, 0x00]));
        // A mistyped type field: dropped.
        assert_eq!(clean(&[0x0a, 0x00], TRANSACTION), Some(vec![]));
        // Errors: truncation, wire type 7, field 0, a stray end group.
        assert_eq!(clean(&[0x08], TRANSACTION), None);
        assert_eq!(clean(&[0x0f], TRANSACTION), None);
        assert_eq!(clean(&[0x00, 0x00], TRANSACTION), None);
        assert_eq!(clean(&[0x0c], TRANSACTION), None);
        // Field number 2^29: invalid.
        assert_eq!(
            clean(&[0x80, 0x80, 0x80, 0x80, 0x10, 0x01], TRANSACTION),
            None
        );
        // A group (field 30) is skipped whole.
        assert_eq!(
            clean(&[0xf3, 0x01, 0x08, 0x01, 0xf4, 0x01], TRANSACTION),
            Some(vec![])
        );
        // A mismatched end group fails.
        assert_eq!(clean(&[0xf3, 0x01, 0xfc, 0x01], TRANSACTION), None);
    }
}
