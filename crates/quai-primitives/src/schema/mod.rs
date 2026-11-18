//! Complete pinned Protobuf field declarations and allocation-bounded decoding.
//! Schema presence is not semantic/consensus validation. See NOTICE.md.
pub mod block;
pub mod common;
pub mod db;
mod descriptor;
pub mod peerdb;
pub mod quaiprotocol;
pub use descriptor::canonicalize;

pub const MAX_MESSAGE_BYTES: usize = 3 << 20;
pub const MAX_MESSAGE_FIELDS: usize = 65_536;
pub const MAX_MESSAGE_NODES: usize = 16_384;
pub const MAX_DEPTH: usize = 32;

fn varint(input: &mut &[u8]) -> Result<u64, String> {
    let mut out = 0u64;
    for shift in (0..70).step_by(7) {
        let (&byte, tail) = input.split_first().ok_or("truncated protobuf varint")?;
        *input = tail;
        if shift == 63 && byte > 1 {
            return Err("protobuf varint overflow".into());
        }
        out |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(out);
        }
    }
    Err("protobuf varint overflow".into())
}
fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], String> {
    if count > input.len() {
        return Err("truncated protobuf field".into());
    }
    let (value, tail) = input.split_at(count);
    *input = tail;
    Ok(value)
}
fn inspect(
    mut input: &[u8],
    kind: usize,
    depth: usize,
    budget: &mut [usize; 2],
) -> Result<(), String> {
    if depth > MAX_DEPTH || budget[1] == MAX_MESSAGE_NODES {
        return Err("protobuf nesting/message budget exceeded".into());
    }
    budget[1] += 1;
    while !input.is_empty() {
        if budget[0] == MAX_MESSAGE_FIELDS {
            return Err("protobuf field budget exceeded".into());
        }
        budget[0] += 1;
        let key = varint(&mut input)?;
        if key >> 3 == 0 || key >> 3 >= (1 << 29) {
            return Err("invalid protobuf field number".into());
        }
        let tag = (key >> 3) as u32;
        let wire = (key & 7) as u8;
        let field = descriptor::FIELDS[kind].iter().find(|f| f.0 == tag);
        if field.is_some_and(|f| f.1 != wire) {
            return Err("incorrect protobuf field wire type".into());
        }
        match wire {
            0 => {
                varint(&mut input)?;
            }
            1 => {
                take(&mut input, 8)?;
            }
            2 => {
                let length =
                    usize::try_from(varint(&mut input)?).map_err(|_| "protobuf length overflow")?;
                let value = take(&mut input, length)?;
                if let Some(child) = field.and_then(|f| f.2) {
                    inspect(value, child, depth + 1, budget)?;
                }
            }
            5 => {
                take(&mut input, 4)?;
            }
            _ => return Err("protobuf groups/unknown wire type outside supported domain".into()),
        }
    }
    Ok(())
}
/// Scan lengths, field types and aggregate nesting/node budgets before Prost
/// allocates nested message trees. Prototype budgets still need network qualification.
pub fn preflight(name: &str, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err("protobuf message exceeds 3 MiB".into());
    }
    let kind = descriptor::NAMES
        .iter()
        .position(|v| *v == name)
        .ok_or("unknown schema message")?;
    inspect(bytes, kind, 0, &mut [0, 0])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_amplification_rejected_before_decode() {
        let bytes = [0x0a, 0].repeat(MAX_MESSAGE_NODES + 1);
        assert!(preflight(".block.ProtoTransactions", &bytes).is_err());
        assert!(preflight(".block.ProtoWorkObject", &[0x0a, 0x80]).is_err());
        assert!(preflight(".block.ProtoWorkObject", &[0]).is_err());
        assert!(preflight(".common.ProtoHash", &[0x08, 1]).is_err());
    }
}
