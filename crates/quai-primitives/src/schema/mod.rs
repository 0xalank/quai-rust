//! Complete pinned Protobuf field declarations and size-bounded decoding.
//! Schema presence is not semantic/consensus validation. See NOTICE.md.
pub mod block;
pub mod common;
pub mod db;
mod descriptor;
pub mod peerdb;
pub mod quaiprotocol;
pub use descriptor::canonicalize;

/// go-quai caps every P2P message at 3 MiB: gossip uses
/// `params.MaxGossipsubPacketSize` and streams use
/// `common.MaxStreamMessageSize`. A block that Go accepts from a peer fits in
/// that size. There is no field or sub-message budget, because a valid block
/// can carry tens of thousands of sub-messages (for example one per Qi input or
/// access-list key). The byte cap bounds them: each needs at least two bytes.
pub const MAX_MESSAGE_BYTES: usize = 3 << 20;
/// Only declared message fields recurse, and the pinned schema graph is acyclic
/// with at most 11 levels (`.quaiprotocol.QuaiMessage`), so this never triggers.
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
fn inspect(mut input: &[u8], kind: usize, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("protobuf nesting exceeds the pinned schemas".into());
    }
    while !input.is_empty() {
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
                    inspect(value, child, depth + 1)?;
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
/// Scan lengths, field types and nesting before Prost allocates nested message
/// trees. The message size is the only resource bound.
pub fn preflight(name: &str, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err("protobuf message exceeds 3 MiB".into());
    }
    let kind = descriptor::NAMES
        .iter()
        .position(|v| *v == name)
        .ok_or("unknown schema message")?;
    inspect(bytes, kind, 0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn message_size_is_the_only_resource_bound() {
        // More sub-messages than the former 16,384 budget fit in a valid block.
        let bytes = [0x0a, 0].repeat(20_000);
        assert!(preflight(".block.ProtoTransactions", &bytes).is_ok());
        let largest = [0x0a, 0].repeat(MAX_MESSAGE_BYTES / 2);
        assert!(preflight(".block.ProtoTransactions", &largest).is_ok());
        let over = [&largest[..], &[0x0a, 0][..]].concat();
        assert!(preflight(".block.ProtoTransactions", &over).is_err());
        assert!(preflight(".block.ProtoWorkObject", &[0x0a, 0x80]).is_err());
        assert!(preflight(".block.ProtoWorkObject", &[0]).is_err());
        assert!(preflight(".common.ProtoHash", &[0x08, 1]).is_err());
    }
}
