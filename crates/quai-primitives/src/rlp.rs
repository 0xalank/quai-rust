//! Canonical bounded RLP decoding for embedded access lists and typed ingress.
use crate::transaction::AccessTuple;
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
fn take<'a>(input: &mut &'a [u8], depth: usize, budget: &mut usize) -> Result<Item<'a>, String> {
    if depth > 8 || *budget == 0 {
        return Err("RLP resource bound".into());
    }
    *budget -= 1;
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
        children.push(take(&mut remaining, depth + 1, budget)?);
    }
    Ok(Item::List(children))
}
pub fn decode(input: &[u8]) -> Result<Item<'_>, String> {
    if input.len() > 1 << 20 {
        return Err("RLP byte bound".into());
    }
    let mut remaining = input;
    let value = take(&mut remaining, 0, &mut 32768)?;
    if !remaining.is_empty() {
        return Err("trailing RLP data".into());
    }
    Ok(value)
}
pub fn access_list(input: &[u8]) -> Result<Vec<AccessTuple>, String> {
    let Item::List(rows) = decode(input)? else {
        return Err("access list is not an RLP list".into());
    };
    if rows.len() > 4096 {
        return Err("access-list address bound".into());
    }
    let mut out = vec![];
    let mut keys = 0;
    for row in rows {
        let Item::List(mut fields) = row else {
            return Err("access tuple is not a list".into());
        };
        if fields.len() != 2 {
            return Err("access tuple arity".into());
        }
        let Item::List(slots) = fields.pop().unwrap() else {
            return Err("access keys are not a list".into());
        };
        let Item::Bytes(address) = fields.pop().unwrap() else {
            return Err("access address is not bytes".into());
        };
        keys += slots.len();
        if keys > 4096 {
            return Err("access-list key bound".into());
        }
        let mut parsed = vec![];
        for k in slots {
            let Item::Bytes(k) = k else {
                return Err("access key is not bytes".into());
            };
            parsed.push(k.try_into().map_err(|_| "access key length")?);
        }
        out.push(AccessTuple {
            address: address.try_into().map_err(|_| "access address length")?,
            keys: parsed,
        });
    }
    Ok(out)
}
