//! Immutable, path-copying secure Merkle Patricia trie. Persisted hash links
//! load through a caller-owned node reader; mutations never scan the full map.
use crate::{transaction::keccak, trie, Hash};
use std::{
    collections::BTreeSet,
    sync::{Arc, OnceLock},
};
pub const MAX_NODE_BYTES: usize = trie::MAX_VALUE_BYTES + 1024;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Missing(Hash),
    Invalid(&'static str),
    Backend(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
pub trait ReadNodes {
    fn get_node(&self, hash: &Hash) -> Result<Option<Vec<u8>>, Error>;
}
pub struct NoNodes;
impl ReadNodes for NoNodes {
    fn get_node(&self, _: &Hash) -> Result<Option<Vec<u8>>, Error> {
        Ok(None)
    }
}
/// A child reference. `repr(u64)` gives the tag the whole first word, so a
/// `Link` has no padding in its first 8 bytes.
///
/// macOS 26.1-26.3 (libmalloc 792.x) needs this. xzone malloc takes a tiny
/// block whose first word equals its free cookie (`zone cookie ^ address`)
/// for a possibly freed block and confirms with a freelist walk that compares
/// a byte offset with 16-byte granule offsets. For a block past index 1 of
/// its chunk the walk answers "free", and `free()` aborts with "pointer being
/// freed was not allocated". With a one-byte tag, the branch clone filled the
/// 7 padding bytes after it from stale registers and stack slots that still
/// held the `cookie ^ address` that malloc had just computed for the same
/// block. When the tag (2, `Node`) was equal to the low byte of that value,
/// in about 2 of 256 processes, the first word of a `Box<[Link; 16]>` was its
/// free cookie, and dropping that live branch aborted. libmalloc 812 (macOS
/// 26.4) compares granules.
#[derive(Clone, Debug)]
#[repr(u64)]
enum Link {
    Empty,
    Hash(Hash),
    Node(Arc<Node>),
}
#[derive(Debug)]
struct Node {
    data: Data,
    encoded: OnceLock<Vec<u8>>,
    hash: OnceLock<Hash>,
}
#[derive(Clone, Debug)]
enum Data {
    Leaf(Arc<[u8]>, Arc<[u8]>),
    Extension(Arc<[u8]>, Link),
    Branch(Box<[Link; 16]>),
}
#[derive(Clone, Debug)]
pub struct Trie {
    root: Link,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Batch {
    pub nodes: usize,
    pub bytes: usize,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    pub entries: usize,
    pub bytes: usize,
}
pub fn empty_root() -> Hash {
    keccak(&[0x80])
}
impl Default for Trie {
    fn default() -> Self {
        Self { root: Link::Empty }
    }
}
fn make(data: Data) -> Link {
    Link::Node(Arc::new(Node {
        data,
        encoded: OnceLock::new(),
        hash: OnceLock::new(),
    }))
}

fn empty_children() -> Box<[Link; 16]> {
    Box::new([const { Link::Empty }; 16])
}
fn compact(path: &[u8], leaf: bool) -> Vec<u8> {
    let flags = if leaf { 2 } else { 0 };
    let odd = path.len() % 2;
    let mut out = Vec::with_capacity(path.len() / 2 + 1);
    out.push(if odd == 1 {
        ((flags + 1) << 4) | path[0]
    } else {
        flags << 4
    });
    for p in path[odd..].chunks_exact(2) {
        out.push((p[0] << 4) | p[1]);
    }
    out
}
impl Node {
    fn encoded(&self) -> &[u8] {
        self.encoded.get_or_init(|| match &self.data {
            Data::Leaf(path, value) => {
                trie::list([trie::bytes(&compact(path, true)), trie::bytes(value)])
            }
            Data::Extension(path, child) => {
                trie::list([trie::bytes(&compact(path, false)), child.reference()])
            }
            Data::Branch(children) => {
                let mut out = children.iter().map(Link::reference).collect::<Vec<_>>();
                out.push(trie::bytes(&[]));
                trie::list(out)
            }
        })
    }
    fn hash(&self) -> Hash {
        *self.hash.get_or_init(|| keccak(self.encoded()))
    }
}
impl Link {
    fn reference(&self) -> Vec<u8> {
        match self {
            Self::Empty => trie::bytes(&[]),
            Self::Hash(h) => trie::bytes(h),
            Self::Node(n) => {
                let b = n.encoded();
                if b.len() < 32 {
                    b.to_vec()
                } else {
                    trie::bytes(&n.hash())
                }
            }
        }
    }
    fn hash(&self) -> Hash {
        match self {
            Self::Empty => empty_root(),
            Self::Hash(h) => *h,
            Self::Node(n) => n.hash(),
        }
    }
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Empty, Self::Empty) => true,
            (Self::Hash(a), Self::Hash(b)) => a == b,
            (Self::Node(a), Self::Node(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}
struct Rlp<'a> {
    raw: &'a [u8],
    body: &'a [u8],
    list: bool,
}
fn item(raw: &[u8]) -> Result<(Rlp<'_>, &[u8]), Error> {
    let first = *raw.first().ok_or(Error::Invalid("truncated RLP"))?;
    if first < 0x80 {
        return Ok((
            Rlp {
                raw: &raw[..1],
                body: &raw[..1],
                list: false,
            },
            &raw[1..],
        ));
    }
    let (list, short, long) = if first < 0xc0 {
        (false, 0x80, 0xb7)
    } else {
        (true, 0xc0, 0xf7)
    };
    let (offset, length) = if first <= long {
        (1, usize::from(first - short))
    } else {
        let width = usize::from(first - long);
        if width > std::mem::size_of::<usize>() {
            return Err(Error::Invalid("RLP length width"));
        }
        let bytes = raw
            .get(1..1 + width)
            .ok_or(Error::Invalid("truncated RLP length"))?;
        if bytes[0] == 0 {
            return Err(Error::Invalid("RLP leading zero length"));
        }
        let mut n = 0usize;
        for b in bytes {
            n = n
                .checked_mul(256)
                .and_then(|n| n.checked_add(*b as usize))
                .ok_or(Error::Invalid("RLP length overflow"))?;
        }
        if n < 56 {
            return Err(Error::Invalid("noncanonical long RLP"));
        }
        (1 + width, n)
    };
    let end = offset
        .checked_add(length)
        .ok_or(Error::Invalid("RLP range overflow"))?;
    let body = raw
        .get(offset..end)
        .ok_or(Error::Invalid("truncated RLP body"))?;
    if !list && length == 1 && body[0] < 0x80 {
        return Err(Error::Invalid("noncanonical single byte"));
    }
    Ok((
        Rlp {
            raw: &raw[..end],
            body,
            list,
        },
        &raw[end..],
    ))
}
fn path(bytes: &[u8]) -> Result<(Vec<u8>, bool), Error> {
    let head = *bytes
        .first()
        .ok_or(Error::Invalid("missing compact path"))?;
    let flag = head >> 4;
    if flag > 3 || flag & 1 == 0 && head & 15 != 0 {
        return Err(Error::Invalid("compact path flag/padding"));
    }
    let mut out = Vec::with_capacity(bytes.len() * 2);
    if flag & 1 != 0 {
        out.push(head & 15);
    }
    for b in &bytes[1..] {
        out.push(b >> 4);
        out.push(b & 15);
    }
    Ok((out, flag & 2 != 0))
}
fn child(r: Rlp<'_>, depth: usize) -> Result<Link, Error> {
    if r.list {
        if r.raw.len() >= 32 {
            return Err(Error::Invalid("oversized inline child"));
        }
        return decode(r.raw, depth);
    }
    match r.body.len() {
        0 => Ok(Link::Empty),
        32 => Ok(Link::Hash(
            r.body
                .try_into()
                .map_err(|_| Error::Invalid("child hash width"))?,
        )),
        _ => Err(Error::Invalid("child hash width")),
    }
}
fn decode(raw: &[u8], depth: usize) -> Result<Link, Error> {
    if raw.len() > MAX_NODE_BYTES || depth > 64 {
        return Err(Error::Invalid("node byte/depth limit"));
    }
    let (root, rest) = item(raw)?;
    if !rest.is_empty() || !root.list {
        return Err(Error::Invalid("node RLP envelope"));
    }
    let mut parts = vec![];
    let mut rest = root.body;
    while !rest.is_empty() {
        let (v, next) = item(rest)?;
        parts.push(v);
        rest = next;
        if parts.len() > 17 {
            return Err(Error::Invalid("node arity"));
        }
    }
    let data = match parts.len() {
        2 => {
            let (Some(second), Some(first)) = (parts.pop(), parts.pop()) else {
                return Err(Error::Invalid("node arity"));
            };
            if first.list {
                return Err(Error::Invalid("path is a list"));
            }
            let (p, leaf) = path(first.body)?;
            if leaf {
                if p.len() != 64 - depth
                    || second.list
                    || second.body.is_empty()
                    || second.body.len() > trie::MAX_VALUE_BYTES
                {
                    return Err(Error::Invalid("secure leaf path/value"));
                }
                Data::Leaf(p.into(), second.body.into())
            } else {
                if p.is_empty() || p.len() >= 64 - depth {
                    return Err(Error::Invalid("extension path"));
                }
                let c = child(second, depth + p.len())?;
                if matches!(c, Link::Empty) {
                    return Err(Error::Invalid("empty extension child"));
                }
                Data::Extension(p.into(), c)
            }
        }
        17 => {
            if depth >= 64 {
                return Err(Error::Invalid("secure branch depth"));
            }
            let Some(terminal) = parts.pop() else {
                return Err(Error::Invalid("node arity"));
            };
            if terminal.list || !terminal.body.is_empty() {
                return Err(Error::Invalid("secure branch terminal"));
            }
            let mut children = empty_children();
            for (i, r) in parts.into_iter().enumerate() {
                children[i] = child(r, depth + 1)?;
            }
            if children
                .iter()
                .filter(|c| !matches!(c, Link::Empty))
                .count()
                < 2
            {
                return Err(Error::Invalid("uncompressed branch"));
            }
            Data::Branch(children)
        }
        _ => return Err(Error::Invalid("node arity")),
    };
    let encoded = OnceLock::new();
    let _ = encoded.set(raw.to_vec());
    Ok(Link::Node(Arc::new(Node {
        data,
        encoded,
        hash: OnceLock::new(),
    })))
}
fn load<R: ReadNodes + ?Sized>(link: &Link, reader: &R, depth: usize) -> Result<Arc<Node>, Error> {
    match link {
        Link::Node(n) => Ok(n.clone()),
        Link::Empty => Err(Error::Invalid("empty node load")),
        Link::Hash(h) => {
            let raw = reader.get_node(h)?.ok_or(Error::Missing(*h))?;
            if raw.len() > MAX_NODE_BYTES || keccak(&raw) != *h {
                return Err(Error::Invalid("persisted node hash/bytes"));
            }
            if depth > 0 && raw.len() < 32 {
                return Err(Error::Invalid("hashed short child"));
            }
            let Link::Node(n) = decode(&raw, depth)? else {
                unreachable!()
            };
            let _ = n.hash.set(*h);
            Ok(n)
        }
    }
}
fn nibble_key(hash: Hash) -> [u8; 64] {
    let mut out = [0; 64];
    for (i, b) in hash.iter().enumerate() {
        out[2 * i] = b >> 4;
        out[2 * i + 1] = b & 15;
    }
    out
}
fn common(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(a, b)| a == b).count()
}
fn prepend<R: ReadNodes + ?Sized>(
    prefix: &[u8],
    link: Link,
    reader: &R,
    depth: usize,
) -> Result<Link, Error> {
    if matches!(link, Link::Empty) {
        return Ok(link);
    }
    if prefix.is_empty() {
        return Ok(link);
    }
    let n = load(&link, reader, depth + prefix.len())?;
    Ok(match &n.data {
        Data::Leaf(p, v) => {
            let mut path = prefix.to_vec();
            path.extend_from_slice(p);
            make(Data::Leaf(path.into(), v.clone()))
        }
        Data::Extension(p, c) => {
            let mut path = prefix.to_vec();
            path.extend_from_slice(p);
            make(Data::Extension(path.into(), c.clone()))
        }
        Data::Branch(_) => make(Data::Extension(prefix.into(), link)),
    })
}
fn mutate<R: ReadNodes + ?Sized>(
    old: &Link,
    reader: &R,
    depth: usize,
    key: &[u8; 64],
    value: Option<&[u8]>,
) -> Result<Link, Error> {
    if depth > 64 {
        return Err(Error::Invalid("mutation depth"));
    }
    if matches!(old, Link::Empty) {
        return Ok(value.map_or(Link::Empty, |v| {
            make(Data::Leaf(key[depth..].into(), v.into()))
        }));
    }
    let n = load(old, reader, depth)?;
    match &n.data {
        Data::Leaf(p, previous) => {
            let shared = common(p, &key[depth..]);
            if shared == p.len() {
                return Ok(match value {
                    Some(v) if previous.as_ref() == v => old.clone(),
                    Some(v) => make(Data::Leaf(p.clone(), v.into())),
                    None => Link::Empty,
                });
            }
            let Some(v) = value else {
                return Ok(old.clone());
            };
            let mut children = empty_children();
            children[p[shared] as usize] =
                make(Data::Leaf(p[shared + 1..].into(), previous.clone()));
            children[key[depth + shared] as usize] =
                make(Data::Leaf(key[depth + shared + 1..].into(), v.into()));
            let branch = make(Data::Branch(children));
            if shared == 0 {
                Ok(branch)
            } else {
                Ok(make(Data::Extension(p[..shared].into(), branch)))
            }
        }
        Data::Extension(p, c) => {
            let shared = common(p, &key[depth..]);
            if shared == p.len() {
                let next = mutate(c, reader, depth + p.len(), key, value)?;
                if next.same(c) {
                    return Ok(old.clone());
                }
                return prepend(p, next, reader, depth);
            }
            let Some(v) = value else {
                return Ok(old.clone());
            };
            let mut children = empty_children();
            children[p[shared] as usize] = if shared + 1 == p.len() {
                c.clone()
            } else {
                make(Data::Extension(p[shared + 1..].into(), c.clone()))
            };
            children[key[depth + shared] as usize] =
                make(Data::Leaf(key[depth + shared + 1..].into(), v.into()));
            let branch = make(Data::Branch(children));
            if shared == 0 {
                Ok(branch)
            } else {
                Ok(make(Data::Extension(p[..shared].into(), branch)))
            }
        }
        Data::Branch(children) => {
            if depth >= 64 {
                return Err(Error::Invalid("mutation branch depth"));
            }
            let mut children = children.clone();
            let i = key[depth] as usize;
            let next = mutate(&children[i], reader, depth + 1, key, value)?;
            if next.same(&children[i]) {
                return Ok(old.clone());
            }
            children[i] = next;
            let live = children
                .iter()
                .enumerate()
                .filter(|(_, c)| !matches!(c, Link::Empty))
                .map(|(i, _)| i)
                .collect::<Vec<_>>();
            match live.as_slice() {
                [] => Ok(Link::Empty),
                [i] => prepend(&[*i as u8], children[*i].clone(), reader, depth),
                _ => Ok(make(Data::Branch(children))),
            }
        }
    }
}
impl Trie {
    pub fn is_empty(&self) -> bool {
        matches!(self.root, Link::Empty)
    }
    /// Construct an initial secure snapshot in sorted-key order. Subsequent
    /// changes use path copying; bulk construction avoids repeated branch clones.
    /// Empty values and duplicate hashed keys are rejected, as in secure_root.
    pub fn from_secure_pairs(values: &[(Vec<u8>, Vec<u8>)]) -> Result<Self, Error> {
        if values.len() > trie::MAX_LIST_ITEMS {
            return Err(Error::Invalid("secure snapshot item limit"));
        }
        let total = values.iter().try_fold(0usize, |n, (k, v)| {
            if k.len() > 64 || v.is_empty() {
                return None;
            }
            n.checked_add(v.len())
        });
        if total.is_none_or(|n| n > trie::MAX_VALUE_BYTES) {
            return Err(Error::Invalid("secure snapshot key/value/byte limit"));
        }
        struct Entry<'a> {
            key: [u8; 64],
            value: &'a [u8],
        }
        fn build(entries: &[Entry<'_>], depth: usize) -> Link {
            if entries.is_empty() {
                return Link::Empty;
            }
            if entries.len() == 1 {
                return make(Data::Leaf(
                    entries[0].key[depth..].into(),
                    entries[0].value.into(),
                ));
            }
            let shared = common(
                &entries[0].key[depth..],
                &entries[entries.len() - 1].key[depth..],
            );
            if shared > 0 {
                return make(Data::Extension(
                    entries[0].key[depth..depth + shared].into(),
                    build(entries, depth + shared),
                ));
            }
            let mut children = empty_children();
            let mut start = 0;
            for (nibble, child) in children.iter_mut().enumerate() {
                let mut end = start;
                while end < entries.len() && entries[end].key[depth] as usize == nibble {
                    end += 1;
                }
                *child = build(&entries[start..end], depth + 1);
                start = end;
            }
            make(Data::Branch(children))
        }
        let mut entries = values
            .iter()
            .map(|(k, v)| Entry {
                key: nibble_key(keccak(k)),
                value: v,
            })
            .collect::<Vec<_>>();
        entries.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        if entries.windows(2).any(|p| p[0].key == p[1].key) {
            return Err(Error::Invalid("duplicate secure snapshot key"));
        }
        Ok(Self {
            root: build(&entries, 0),
        })
    }
    /// The root must come from trusted local version metadata. This does not
    /// authenticate a peer snapshot or turn a root into a block-admission permit.
    pub fn from_root(root: Hash) -> Self {
        Self {
            root: if root == empty_root() {
                Link::Empty
            } else {
                Link::Hash(root)
            },
        }
    }
    pub fn root(&self) -> Hash {
        self.root.hash()
    }
    pub fn get<R: ReadNodes + ?Sized>(
        &self,
        reader: &R,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, Error> {
        if key.len() > 64 {
            return Err(Error::Invalid("secure key length"));
        }
        self.get_hashed(reader, keccak(key))
    }
    pub fn get_hashed<R: ReadNodes + ?Sized>(
        &self,
        reader: &R,
        key: Hash,
    ) -> Result<Option<Vec<u8>>, Error> {
        let key = nibble_key(key);
        let mut current = self.root.clone();
        let mut depth = 0;
        loop {
            if matches!(current, Link::Empty) {
                return Ok(None);
            }
            let n = load(&current, reader, depth)?;
            match &n.data {
                Data::Leaf(p, v) => return Ok((p.as_ref() == &key[depth..]).then(|| v.to_vec())),
                Data::Extension(p, c) => {
                    if !key[depth..].starts_with(p) {
                        return Ok(None);
                    }
                    depth += p.len();
                    current = c.clone();
                }
                Data::Branch(c) => {
                    if depth >= 64 {
                        return Err(Error::Invalid("lookup depth"));
                    }
                    current = c[key[depth] as usize].clone();
                    depth += 1;
                }
            }
        }
    }
    pub fn update<R: ReadNodes + ?Sized>(
        &mut self,
        reader: &R,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), Error> {
        if key.len() > 64 {
            return Err(Error::Invalid("secure key length"));
        }
        self.update_hashed(reader, keccak(key), value)
    }
    pub fn update_hashed<R: ReadNodes + ?Sized>(
        &mut self,
        reader: &R,
        key: Hash,
        value: &[u8],
    ) -> Result<(), Error> {
        if value.len() > trie::MAX_VALUE_BYTES {
            return Err(Error::Invalid("secure value length"));
        }
        let next = mutate(
            &self.root,
            reader,
            0,
            &nibble_key(key),
            (!value.is_empty()).then_some(value),
        )?;
        self.root = next;
        Ok(())
    }
    /// Traverse verified leaves with explicit materialization bounds. The key
    /// is the fixed 32-byte hashed key, not its unavailable raw preimage.
    pub fn visit_leaves<R: ReadNodes + ?Sized>(
        &self,
        reader: &R,
        max_entries: usize,
        max_bytes: usize,
        visit: &mut impl FnMut(Hash, &[u8]) -> Result<(), Error>,
    ) -> Result<Scan, Error> {
        fn walk<R: ReadNodes + ?Sized>(
            link: &Link,
            reader: &R,
            prefix: &mut Vec<u8>,
            max_entries: usize,
            max_bytes: usize,
            visit: &mut impl FnMut(Hash, &[u8]) -> Result<(), Error>,
            stats: &mut Scan,
        ) -> Result<(), Error> {
            if matches!(link, Link::Empty) {
                return Ok(());
            }
            let node = load(link, reader, prefix.len())?;
            match &node.data {
                Data::Leaf(path, value) => {
                    if prefix.len() + path.len() != 64 {
                        return Err(Error::Invalid("leaf iteration depth"));
                    }
                    if stats.entries >= max_entries {
                        return Err(Error::Invalid("leaf iteration count"));
                    }
                    stats.bytes = stats
                        .bytes
                        .checked_add(value.len())
                        .ok_or(Error::Invalid("leaf iteration bytes"))?;
                    if stats.bytes > max_bytes {
                        return Err(Error::Invalid("leaf iteration byte bound"));
                    }
                    let mut full = prefix.clone();
                    full.extend_from_slice(path);
                    let mut key = [0; 32];
                    for (i, nibbles) in full.chunks_exact(2).enumerate() {
                        key[i] = (nibbles[0] << 4) | nibbles[1];
                    }
                    visit(key, value)?;
                    stats.entries += 1;
                }
                Data::Extension(path, child) => {
                    let n = prefix.len();
                    prefix.extend_from_slice(path);
                    walk(child, reader, prefix, max_entries, max_bytes, visit, stats)?;
                    prefix.truncate(n);
                }
                Data::Branch(children) => {
                    for (i, child) in children.iter().enumerate() {
                        prefix.push(i as u8);
                        walk(child, reader, prefix, max_entries, max_bytes, visit, stats)?;
                        prefix.pop();
                    }
                }
            }
            Ok(())
        }
        let mut stats = Scan::default();
        walk(
            &self.root,
            reader,
            &mut vec![],
            max_entries,
            max_bytes,
            visit,
            &mut stats,
        )?;
        Ok(stats)
    }
    pub fn check_root<R: ReadNodes + ?Sized>(&self, reader: &R) -> Result<(), Error> {
        if !matches!(self.root, Link::Empty) {
            load(&self.root, reader, 0)?;
        }
        Ok(())
    }
    /// Existing subtree references must already belong to the same backing
    /// store. This permits incremental persistence without copying other DBs.
    pub fn visit_hash_links(
        &self,
        check: &mut impl FnMut(Hash) -> Result<(), Error>,
    ) -> Result<(), Error> {
        fn walk(
            link: &Link,
            seen: &mut BTreeSet<Hash>,
            check: &mut impl FnMut(Hash) -> Result<(), Error>,
        ) -> Result<(), Error> {
            match link {
                Link::Empty => {}
                Link::Hash(h) => {
                    if seen.insert(*h) {
                        check(*h)?;
                    }
                }
                Link::Node(n) => match &n.data {
                    Data::Leaf(..) => {}
                    Data::Extension(_, c) => walk(c, seen, check)?,
                    Data::Branch(c) => {
                        for child in c.iter() {
                            walk(child, seen, check)?;
                        }
                    }
                },
            }
            Ok(())
        }
        walk(&self.root, &mut BTreeSet::new(), check)
    }
    /// Visit only newly materialized nodes. Existing hash links are already in
    /// the same caller-owned database. Inline children are encoded in their parent.
    pub fn visit_new_nodes(
        &self,
        write: &mut impl FnMut(Hash, &[u8]) -> Result<(), Error>,
    ) -> Result<Batch, Error> {
        fn walk(
            link: &Link,
            root: bool,
            seen: &mut BTreeSet<Hash>,
            write: &mut impl FnMut(Hash, &[u8]) -> Result<(), Error>,
            stats: &mut Batch,
        ) -> Result<(), Error> {
            let Link::Node(n) = link else { return Ok(()) };
            let hash = n.hash();
            if !seen.insert(hash) {
                return Ok(());
            }
            let raw = n.encoded();
            if root || raw.len() >= 32 {
                write(hash, raw)?;
                stats.nodes += 1;
                stats.bytes = stats
                    .bytes
                    .checked_add(raw.len())
                    .ok_or(Error::Invalid("node batch size"))?;
            }
            match &n.data {
                Data::Leaf(..) => {}
                Data::Extension(_, c) => walk(c, false, seen, write, stats)?,
                Data::Branch(c) => {
                    for child in c.iter() {
                        walk(child, false, seen, write, stats)?;
                    }
                }
            }
            Ok(())
        }
        let mut stats = Batch::default();
        walk(&self.root, true, &mut BTreeSet::new(), write, &mut stats)?;
        Ok(stats)
    }
}
