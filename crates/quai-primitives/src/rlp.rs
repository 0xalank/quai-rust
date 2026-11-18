//! Canonical RLP decoding for the access list of the ETX opcode.
//!
//! go-quai decodes that list with `rlp.DecodeBytes` into `types.AccessList`
//! (core/vm/instructions.go `opETX`). The Go decoder has no byte or item cap;
//! memory gas bounds the input. A valid access list nests three lists deep, and
//! Go rejects deeper input by its type, so the recursion depth bound here
//! changes no result.
use crate::transaction::AccessTuple;
const MAX_DEPTH: usize = 8;
#[derive(Debug, PartialEq, Eq)]
pub enum Item<'a> {
    Bytes(&'a [u8]),
    List(Vec<Item<'a>>),
}
fn length(bytes: &[u8]) -> Result<usize, String> {
    if bytes.is_empty() || bytes[0] == 0 || bytes.len() > std::mem::size_of::<usize>() {
        return Err("noncanonical/overflowing RLP length".into());
    }
    bytes.iter().try_fold(0usize, |a, b| {
        a.checked_mul(256)
            .and_then(|n| n.checked_add(*b as usize))
            .ok_or("RLP length overflow".into())
    })
}
fn take<'a>(input: &mut &'a [u8], depth: usize) -> Result<Item<'a>, String> {
    if depth > MAX_DEPTH {
        return Err("RLP nesting exceeds the access-list shape".into());
    }
    let prefix = *input.first().ok_or("truncated RLP")?;
    let (header, len, list) = match prefix {
        0..=0x7f => {
            let v = &input[..1];
            *input = &input[1..];
            return Ok(Item::Bytes(v));
        }
        0x80..=0xb7 => (1, (prefix - 0x80) as usize, false),
        0xb8..=0xbf => {
            let n = (prefix - 0xb7) as usize;
            let end = 1 + n;
            let len = length(input.get(1..end).ok_or("truncated RLP length")?)?;
            if len < 56 {
                return Err("noncanonical long RLP string".into());
            }
            (end, len, false)
        }
        0xc0..=0xf7 => (1, (prefix - 0xc0) as usize, true),
        _ => {
            let n = (prefix - 0xf7) as usize;
            let end = 1 + n;
            let len = length(input.get(1..end).ok_or("truncated RLP list length")?)?;
            if len < 56 {
                return Err("noncanonical long RLP list".into());
            }
            (end, len, true)
        }
    };
    let end = header.checked_add(len).ok_or("RLP length overflow")?;
    let bytes = input.get(header..end).ok_or("truncated RLP payload")?;
    *input = &input[end..];
    if !list {
        if len == 1 && bytes[0] < 0x80 {
            return Err("noncanonical single-byte RLP".into());
        }
        return Ok(Item::Bytes(bytes));
    }
    let mut remaining = bytes;
    let mut children = vec![];
    while !remaining.is_empty() {
        children.push(take(&mut remaining, depth + 1)?);
    }
    Ok(Item::List(children))
}
pub fn decode(input: &[u8]) -> Result<Item<'_>, String> {
    let mut remaining = input;
    let value = take(&mut remaining, 0)?;
    if !remaining.is_empty() {
        return Err("trailing RLP data".into());
    }
    Ok(value)
}
pub fn access_list(input: &[u8]) -> Result<Vec<AccessTuple>, String> {
    let Item::List(rows) = decode(input)? else {
        return Err("access list is not an RLP list".into());
    };
    let mut out = vec![];
    for row in rows {
        let Item::List(mut fields) = row else {
            return Err("access tuple is not a list".into());
        };
        if fields.len() != 2 {
            return Err("access tuple arity".into());
        }
        let (Some(Item::List(slots)), Some(Item::Bytes(address))) = (fields.pop(), fields.pop())
        else {
            return Err("access tuple is not [address bytes, key list]".into());
        };
        let mut parsed = vec![];
        for k in slots {
            let Item::Bytes(k) = k else {
                return Err("access key is not bytes".into());
            };
            parsed.push(k.try_into().map_err(|_| "access key length")?);
        }
        // go-quai `Address.DecodeRLP` accepts a byte string of any length and
        // applies `BytesToAddress`: keep the last 20 bytes, left-pad shorter
        // input with zeros.
        let mut normalized = [0; 20];
        let tail = &address[address.len().saturating_sub(20)..];
        normalized[20 - tail.len()..].copy_from_slice(tail);
        out.push(AccessTuple {
            address: normalized,
            keys: parsed,
        });
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::trie::{bytes, list};
    #[test]
    fn access_lists_have_no_byte_or_item_cap() {
        // 33,000 keys: above the former 1 MiB, 32,768-item and 4,096-key caps.
        let keys = (0..33_000u32)
            .map(|i| bytes(&[i.to_be_bytes().to_vec(), vec![0; 28]].concat()))
            .collect::<Vec<_>>();
        let tuples = (0..4_097u32).map(|i| {
            list([
                bytes(&[vec![1; 16], i.to_be_bytes().to_vec()].concat()),
                list([]),
            ])
        });
        let raw = list(std::iter::once(list([bytes(&[2; 20]), list(keys)])).chain(tuples));
        assert!(raw.len() > 1 << 20);
        let decoded = access_list(&raw).unwrap();
        assert_eq!(decoded.len(), 4_098);
        assert_eq!(decoded[0].keys.len(), 33_000);
        assert_eq!(decoded[0].keys[32_999][..4], 32_999u32.to_be_bytes());
    }
    #[test]
    fn addresses_follow_go_bytes_to_address() {
        let raw = list([
            list([bytes(&[7; 19]), list([])]),
            list([bytes(&[[9].as_slice(), &[8; 20]].concat()), list([])]),
            list([bytes(&[]), list([])]),
        ]);
        let decoded = access_list(&raw).unwrap();
        assert_eq!(decoded[0].address[0], 0);
        assert_eq!(decoded[0].address[1..], [7; 19]);
        assert_eq!(decoded[1].address, [8; 20]);
        assert_eq!(decoded[2].address, [0; 20]);
        // Keys stay exact 32-byte strings, as for Go's [32]byte decoding.
        assert!(access_list(&list([list([bytes(&[1; 20]), list([bytes(&[1; 31])])])])).is_err());
    }
}
