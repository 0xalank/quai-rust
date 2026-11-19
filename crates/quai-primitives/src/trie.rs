//! Ordered-list Merkle Patricia commitments. Pure Rust; no persistent trie database.
//! Matches go-quai DeriveSha / StackTrie. See NOTICE.md for compatibility sources.
use crate::{transaction::keccak, Hash};

pub const MAX_LIST_ITEMS: usize = 65_536;
pub const MAX_VALUE_BYTES: usize = 64 * 1024 * 1024;

fn prefix(length: usize, short: u8, long: u8) -> Vec<u8> {
    if length < 56 {
        return vec![short + length as u8];
    }
    let bytes = length.to_be_bytes();
    // Length is at least 56 here, so a nonzero byte exists.
    let bytes = &bytes[bytes.iter().position(|b| *b != 0).unwrap_or(0)..];
    let mut out = vec![long + bytes.len() as u8];
    out.extend_from_slice(bytes);
    out
}
pub fn bytes(value: &[u8]) -> Vec<u8> {
    if value.len() == 1 && value[0] < 128 {
        return value.to_vec();
    }
    let mut out = prefix(value.len(), 0x80, 0xb7);
    out.extend_from_slice(value);
    out
}
pub fn uint(value: u64) -> Vec<u8> {
    let encoded = value.to_be_bytes();
    bytes(&encoded[encoded.iter().position(|b| *b != 0).unwrap_or(8)..])
}
pub fn list(items: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
    // Whole-item copies into one buffer: the same bytes as flattening the
    // items, without a byte-by-byte iterator and its regrowing buffer.
    let items: Vec<Vec<u8>> = items.into_iter().collect();
    let length = items.iter().map(Vec::len).sum();
    let mut out = prefix(length, 0xc0, 0xf7);
    out.reserve(length);
    for item in &items {
        out.extend_from_slice(item);
    }
    out
}
fn compact(path: &[u8], leaf: bool) -> Vec<u8> {
    let flag = if leaf { 2 } else { 0 };
    let mut out = Vec::with_capacity(path.len() / 2 + 1);
    let start = if path.len() % 2 == 1 {
        out.push(((flag + 1) << 4) | path[0]);
        1
    } else {
        out.push(flag << 4);
        0
    };
    for pair in path[start..].chunks_exact(2) {
        out.push(pair[0] << 4 | pair[1]);
    }
    out
}
struct Entry<'a> {
    key: Vec<u8>,
    value: &'a [u8],
}
fn reference(node: Vec<u8>) -> Vec<u8> {
    if node.len() < 32 {
        node
    } else {
        bytes(&keccak(&node))
    }
}
fn node(entries: &[Entry<'_>], depth: usize) -> Vec<u8> {
    if entries.len() == 1 {
        return list([
            bytes(&compact(&entries[0].key[depth..], true)),
            bytes(entries[0].value),
        ]);
    }
    let first = &entries[0].key;
    let last = &entries[entries.len() - 1].key;
    let common = first[depth..]
        .iter()
        .zip(&last[depth..])
        .take_while(|(a, b)| a == b)
        .count();
    if common != 0 {
        return list([
            bytes(&compact(&first[depth..depth + common], false)),
            reference(node(entries, depth + common)),
        ]);
    }
    let mut children = Vec::with_capacity(17);
    let mut start = usize::from(first.len() == depth);
    for nibble in 0..16 {
        let mut end = start;
        while end < entries.len()
            && entries[end].key.len() > depth
            && entries[end].key[depth] == nibble
        {
            end += 1;
        }
        children.push(if end == start {
            bytes(&[])
        } else {
            reference(node(&entries[start..end], depth + 1))
        });
        start = end;
    }
    children.push(bytes(if first.len() == depth {
        entries[0].value
    } else {
        &[]
    }));
    list(children)
}
/// Values are exact list consensus encodings, e.g. type-prefixed transaction RLP.
/// The values are already in memory, so there is no byte budget: receipt lists
/// exceed 64 MiB at the block gas limit. The item limit is above any block:
/// a 50,000,000-gas block holds at most 13,157 transactions.
pub fn ordered_root(values: &[Vec<u8>]) -> Result<Hash, String> {
    if values.len() > MAX_LIST_ITEMS {
        return Err("commitment list item limit exceeded".into());
    }
    if values.is_empty() {
        return Ok(keccak(&bytes(&[])));
    }
    if values.iter().any(Vec::is_empty) {
        return Err("empty trie value is not a supported list entry".into());
    }
    let mut entries: Vec<_> = values
        .iter()
        .enumerate()
        .map(|(i, value)| Entry {
            key: uint(i as u64)
                .into_iter()
                .flat_map(|b| [b >> 4, b & 15])
                .collect(),
            value,
        })
        .collect();
    entries.sort_unstable_by(|a, b| a.key.cmp(&b.key));
    Ok(keccak(&node(&entries, 0)))
}
/// Commitment of a nonempty-value snapshot with already specified raw trie keys.
/// Empty values are deletions and must be absent from the supplied snapshot.
pub fn key_value_root(values: &[(Vec<u8>, Vec<u8>)]) -> Result<Hash, String> {
    if values.len() > MAX_LIST_ITEMS {
        return Err("trie snapshot item limit exceeded".into());
    }
    let total = values.iter().try_fold(0usize, |n, (k, v)| {
        n.checked_add(k.len()).and_then(|n| n.checked_add(v.len()))
    });
    if total.is_none_or(|n| n > MAX_VALUE_BYTES) {
        return Err("trie snapshot byte limit exceeded".into());
    }
    if values.iter().any(|(k, v)| k.len() > 64 || v.is_empty()) {
        return Err("trie snapshot key/value outside supported bounds".into());
    }
    if values.is_empty() {
        return Ok(keccak(&bytes(&[])));
    }
    let mut entries: Vec<_> = values
        .iter()
        .map(|(key, value)| Entry {
            key: key.iter().flat_map(|b| [b >> 4, b & 15]).collect(),
            value,
        })
        .collect();
    entries.sort_unstable_by(|a, b| a.key.cmp(&b.key));
    if entries.windows(2).any(|pair| pair[0].key == pair[1].key) {
        return Err("duplicate trie snapshot key".into());
    }
    Ok(keccak(&node(&entries, 0)))
}
/// SecureTrie hashes each raw key with Keccak before insertion. This pure
/// recomputation is not a persistent trie, proof generator or state database.
pub fn secure_root(values: &[(Vec<u8>, Vec<u8>)]) -> Result<Hash, String> {
    if values.len() > MAX_LIST_ITEMS || values.iter().any(|(k, _)| k.len() > 64) {
        return Err("secure snapshot outside supported bounds".into());
    }
    let total = values
        .iter()
        .try_fold(0usize, |n, (_, v)| n.checked_add(v.len()));
    if total.is_none_or(|n| n > MAX_VALUE_BYTES) {
        return Err("secure snapshot byte limit exceeded".into());
    }
    if values.iter().any(|(_, v)| v.is_empty()) {
        return Err("empty secure snapshot value".into());
    }
    if values.is_empty() {
        return Ok(keccak(&bytes(&[])));
    }
    let mut entries: Vec<_> = values
        .iter()
        .map(|(k, v)| Entry {
            key: keccak(k)
                .into_iter()
                .flat_map(|b| [b >> 4, b & 15])
                .collect(),
            value: v,
        })
        .collect();
    entries.sort_unstable_by(|a, b| a.key.cmp(&b.key));
    if entries.windows(2).any(|pair| pair[0].key == pair[1].key) {
        return Err("duplicate secure snapshot key".into());
    }
    Ok(keccak(&node(&entries, 0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_root_and_ordering() {
        assert_eq!(
            crate::encode_hex(&ordered_root(&[]).unwrap()),
            "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421"
        );
        assert_ne!(
            ordered_root(&[vec![1], vec![2]]),
            ordered_root(&[vec![2], vec![1]])
        );
        assert!(ordered_root(&[vec![]]).is_err());
    }
    #[test]
    fn list_equals_the_flattened_items_with_their_prefix() {
        let flat = |items: &[Vec<u8>]| {
            let body: Vec<u8> = items.iter().flatten().copied().collect();
            let mut out = prefix(body.len(), 0xc0, 0xf7);
            out.extend(body);
            out
        };
        for sizes in [
            vec![],
            vec![0],
            vec![1, 2, 3],
            vec![55],
            vec![56],
            vec![20, 36],
            vec![256, 0, 1000],
            vec![70_000, 3],
        ] {
            let items: Vec<Vec<u8>> = sizes
                .iter()
                .enumerate()
                .map(|(i, n)| (0..*n).map(|j| (i * 31 + j) as u8).collect())
                .collect();
            assert_eq!(list(items.clone()), flat(&items), "{sizes:?}");
        }
    }
    #[test]
    fn ordered_root_has_no_byte_budget() {
        // A block of minimal Qi transactions has more than 64 MiB of receipts.
        let values = vec![vec![1; 33 << 20], vec![2; 33 << 20]];
        assert!(values.iter().map(Vec::len).sum::<usize>() > MAX_VALUE_BYTES);
        assert!(ordered_root(&values).is_ok());
    }
}
