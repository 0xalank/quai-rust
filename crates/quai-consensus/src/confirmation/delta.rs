//! Immutable local output of native hierarchy preparation. This is not a peer
//! snapshot format and conveys no admission authority outside its local version.
use super::*;
const MAGIC: &[u8] = b"QUAI-HIERARCHY-DELTA-1\n";
const MAX_BYTES: usize = 32 << 20;
/// Decoded local hierarchy outputs. This format checks record/parent/role
/// bindings but supplies no consensus or ancestry authority. Persist it only in
/// a provenance-checked local branch version; never trust it as peer state.
pub struct LocalOutputs(Delta);
impl LocalOutputs {
    pub fn decode(raw: &[u8], zone: &Record) -> Result<Self, Error> {
        Delta::decode(raw, zone).map(Self)
    }
    pub fn heads(&self) -> [Hash; 3] {
        self.0.heads
    }
    pub fn order(&self) -> u8 {
        self.0.order
    }
    pub fn snapshot(&self, context: usize) -> Option<&placement::Snapshot> {
        self.0.snapshots.get(context).and_then(Option::as_ref)
    }
    pub fn region_rollup(&self) -> &[Transaction] {
        &self.0.region
    }
    pub fn confirmed(&self) -> &[Transaction] {
        &self.0.confirmed
    }
}
#[derive(Clone)]
pub(crate) struct Delta {
    pub heads: [Hash; 3],
    pub order: u8,
    pub snapshots: [Option<placement::Snapshot>; 3],
    pub region: Arc<Vec<Transaction>>,
    pub confirmed: Arc<Vec<Transaction>>,
}
fn invalid() -> Error {
    Error::Invalid("persisted hierarchy delta")
}
fn put(out: &mut Vec<u8>, v: &[u8]) -> Result<(), Error> {
    if out
        .len()
        .checked_add(v.len() + 4)
        .is_none_or(|n| n > MAX_BYTES)
    {
        return Err(Error::ResourceLimit("hierarchy delta bytes"));
    }
    out.extend((v.len() as u32).to_be_bytes());
    out.extend(v);
    Ok(())
}
fn list(out: &mut Vec<u8>, txs: &[Transaction]) -> Result<(), Error> {
    if txs.len() > MAX_ETXS {
        return Err(Error::ResourceLimit("hierarchy delta transactions"));
    }
    out.extend((txs.len() as u32).to_be_bytes());
    for tx in txs {
        put(out, &tx.encode())?;
    }
    Ok(())
}
impl PreparedConfirmation {
    /// Encode checked local outputs for the same branch version as execution
    /// state. These bytes cannot replace header validation or mint admission.
    pub fn encode_local_outputs(&self) -> Result<Vec<u8>, Error> {
        self.durable_delta()
    }
    pub(crate) fn durable_delta(&self) -> Result<Vec<u8>, Error> {
        let mut out = MAGIC.to_vec();
        for head in self.expected {
            out.extend(head);
        }
        out.extend(self.records[2].as_ref().ok_or_else(invalid)?.id());
        out.push(self.order);
        for append in &self.appends {
            match append {
                None => out.push(0),
                Some(p) => {
                    out.push(1);
                    let s = &p.next;
                    match &s.manifest {
                        None => out.push(0),
                        Some(v) => {
                            if v.len() > placement::MAX_MANIFEST {
                                return Err(invalid());
                            }
                            out.push(1);
                            out.extend((v.len() as u32).to_be_bytes());
                            for hash in v {
                                out.extend(hash);
                            }
                        }
                    }
                    match &s.interlinks {
                        None => out.push(0),
                        Some(v) => {
                            out.push(1);
                            for hash in v {
                                out.extend(hash);
                            }
                        }
                    }
                    match &s.termini {
                        None => out.push(0),
                        Some(t) => {
                            out.push(1);
                            put(&mut out, &t.encode())?;
                        }
                    }
                }
            }
        }
        list(
            &mut out,
            if self.order <= 1 {
                &self.region_rollup
            } else {
                &[]
            },
        )?;
        list(&mut out, &self.confirmed)?;
        Ok(out)
    }
}
struct Cursor<'a> {
    raw: &'a [u8],
    offset: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.offset.checked_add(n).ok_or_else(invalid)?;
        let v = self.raw.get(self.offset..end).ok_or_else(invalid)?;
        self.offset = end;
        Ok(v)
    }
    fn hash(&mut self) -> Result<Hash, Error> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn count(&mut self, max: usize) -> Result<usize, Error> {
        let n = u32::from_be_bytes(self.take(4)?.try_into().unwrap()) as usize;
        if n > max {
            return Err(invalid());
        }
        Ok(n)
    }
    fn blob(&mut self) -> Result<&'a [u8], Error> {
        let n = self.count(MAX_BYTES)?;
        self.take(n)
    }
    fn transactions(&mut self) -> Result<Arc<Vec<Transaction>>, Error> {
        let n = self.count(MAX_ETXS)?;
        // Each entry carries at least a 4-byte length, so the input bounds the reservation.
        let mut out = Vec::with_capacity(n.min((self.raw.len() - self.offset) / 4));
        for _ in 0..n {
            let tx = Transaction::decode(self.blob()?).map_err(|_| invalid())?;
            external(&tx)?;
            out.push(tx);
        }
        Ok(Arc::new(out))
    }
}
impl Delta {
    pub fn decode(raw: &[u8], zone: &Record) -> Result<Self, Error> {
        if raw.len() > MAX_BYTES {
            return Err(Error::ResourceLimit("hierarchy delta bytes"));
        }
        let mut r = Cursor { raw, offset: 0 };
        if r.take(MAGIC.len())? != MAGIC {
            return Err(invalid());
        }
        let expected = [r.hash()?, r.hash()?, r.hash()?];
        let id = r.hash()?;
        let order = r.byte()?;
        if order > 2 || id != zone.id() {
            return Err(invalid());
        }
        let mut snapshots = std::array::from_fn(|_| None);
        let mut heads = expected;
        for c in 0..3 {
            let flag = r.byte()?;
            if flag != u8::from(c >= order as usize) {
                return Err(invalid());
            }
            if flag == 0 {
                continue;
            }
            if zone.parent(c)? != expected[c] {
                return Err(invalid());
            }
            heads[c] = id;
            let manifest = match r.byte()? {
                0 => None,
                1 => {
                    let n = r.count(placement::MAX_MANIFEST)?;
                    let mut hashes = Vec::with_capacity(n);
                    for _ in 0..n {
                        hashes.push(r.hash()?);
                    }
                    Some(hashes)
                }
                _ => return Err(invalid()),
            };
            let interlinks = match r.byte()? {
                0 => None,
                1 => Some([r.hash()?, r.hash()?, r.hash()?, r.hash()?]),
                _ => return Err(invalid()),
            };
            let termini = match r.byte()? {
                0 => None,
                1 => Some(placement::Termini::decode(r.blob()?)?),
                _ => return Err(invalid()),
            };
            snapshots[c] = Some(placement::Snapshot {
                manifest,
                interlinks,
                termini,
            });
        }
        let region = r.transactions()?;
        let confirmed = r.transactions()?;
        if r.offset != raw.len() || (order == 2 && !region.is_empty()) {
            return Err(invalid());
        }
        Ok(Self {
            heads,
            order,
            snapshots,
            region,
            confirmed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_hierarchy_deltas_roundtrip_and_reject_wrong_branch_and_malformed_bytes() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../fixtures/protocol/confirmation.json"
        ))
        .unwrap();
        let now = corpus["now"].as_u64().unwrap();
        let mut graph = Hierarchy::new().unwrap();
        let mut verifier = Verifier::default();
        let bytes =
            |v: &serde_json::Value| quai_primitives::decode_hex(v.as_str().unwrap()).unwrap();
        let encode = |txs: &[Transaction]| txs.iter().map(Transaction::encode).collect::<Vec<_>>();
        let mut orders = [0; 3];
        for row in corpus["branches"][0]["vectors"].as_array().unwrap() {
            let zone = Record::decode(&bytes(&row["work_object"])).unwrap();
            let contexts = row["contexts"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| {
                    let c = v["context"].as_u64().unwrap() as u8;
                    (c < 2).then(|| (c, bytes(&v["work_object"])))
                })
                .collect::<Vec<_>>();
            let prepared = graph.prepare(&mut verifier, &zone, &contexts, now).unwrap();
            let raw = prepared.durable_delta().unwrap();
            let delta = Delta::decode(&raw, &zone).unwrap();
            orders[prepared.order() as usize] += 1;
            assert_eq!(delta.order, prepared.order());
            for c in 0..3 {
                assert_eq!(delta.snapshots[c].as_ref(), prepared.snapshot(c));
            }
            assert_eq!(encode(&delta.confirmed), encode(prepared.newly_confirmed()));
            let retained_region = if prepared.order() <= 1 {
                prepared.region_rollup()
            } else {
                &[]
            };
            assert_eq!(encode(&delta.region), encode(retained_region));
            for c in 0..3 {
                assert_eq!(
                    delta.heads[c],
                    if c >= prepared.order() as usize {
                        zone.id()
                    } else {
                        prepared.expected[c]
                    }
                );
            }
            for cut in [0, MAGIC.len() - 1, MAGIC.len() + 96, raw.len() - 1] {
                assert!(Delta::decode(&raw[..cut], &zone).is_err());
            }
            let mut trailing = raw.clone();
            trailing.push(0);
            assert!(Delta::decode(&trailing, &zone).is_err());
            for offset in [MAGIC.len() + 96, MAGIC.len() + 128, MAGIC.len() + 129] {
                let mut changed = raw.clone();
                changed[offset] ^= 0xff;
                assert!(Delta::decode(&changed, &zone).is_err());
            }
            // A stored active-parent head cannot be substituted across branches.
            let mut parent = raw.clone();
            parent[MAGIC.len() + prepared.order() as usize * 32] ^= 1;
            assert!(Delta::decode(&parent, &zone).is_err());
            graph.commit(prepared).unwrap();
            assert_eq!(delta.heads, graph.heads());
        }
        assert!(orders.iter().all(|n| *n > 0));
    }
}
