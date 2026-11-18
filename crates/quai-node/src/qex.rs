//! QEX1 reader: go-quai blocks exported in import order (docs/mainnet-export-format.md).
//! The reader checks the container only: manifest, segment hashes, record
//! framing and record order. Block contents are checked by the replay.
use quai_primitives::{decode_hex, Hash, REFERENCE_REVISION};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

pub const MAGIC: &[u8; 5] = b"QEX1\n";
pub const FORMAT: &str = "QEX1";
/// Zone heights in one segment; the last segment can hold fewer.
pub const SEGMENT_SPAN: u64 = 100_000;
pub const KIND_BLOCK: u8 = 1;
/// Fixed bytes of a record before the block bytes.
const RECORD_HEAD: u64 = 1 + 1 + 8 + 32 + 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportError(pub String);
impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ExportError {}
fn err<T>(message: impl Into<String>) -> Result<T, ExportError> {
    Err(ExportError(message.into()))
}
fn io(path: &Path, e: std::io::Error) -> ExportError {
    ExportError(format!("{}: {e}", path.display()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Context {
    Prime = 0,
    Region = 1,
    Zone = 2,
}
impl Context {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Prime),
            1 => Some(Self::Region),
            2 => Some(Self::Zone),
            _ => None,
        }
    }
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Prime => "prime",
            Self::Region => "region",
            Self::Zone => "zone",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentInfo {
    pub file: String,
    pub first_zone: u64,
    pub last_zone: u64,
    pub records: u64,
    pub sha256: Hash,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub environment: String,
    pub slice: [u8; 2],
    pub genesis_hash: Hash,
    pub receipts: bool,
    pub last_zone: u64,
    pub segments: Vec<SegmentInfo>,
    pub exported_utc: String,
}
#[derive(serde::Deserialize)]
struct RawManifest {
    format: String,
    reference_revision: String,
    environment: String,
    slice: Vec<u64>,
    genesis_hash: String,
    receipts: bool,
    last_zone: u64,
    segments: Vec<RawSegment>,
    exported_utc: String,
}
#[derive(serde::Deserialize)]
struct RawSegment {
    file: String,
    first_zone: u64,
    last_zone: u64,
    records: u64,
    sha256: String,
}
fn hash32(value: &str, what: &str) -> Result<Hash, ExportError> {
    let bytes = decode_hex(value).map_err(|e| ExportError(format!("{what}: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| ExportError(format!("{what}: expected 32 bytes")))
}
pub fn segment_name(first: u64, last: u64) -> String {
    format!("segments/{first:010}-{last:010}.qex")
}
impl Manifest {
    pub fn load(dir: &Path) -> Result<Self, ExportError> {
        let path = dir.join("manifest.json");
        let bytes = std::fs::read(&path).map_err(|e| io(&path, e))?;
        Self::parse(&bytes)
    }
    pub fn parse(bytes: &[u8]) -> Result<Self, ExportError> {
        let raw: RawManifest = serde_json::from_slice(bytes)
            .map_err(|e| ExportError(format!("manifest.json: {e}")))?;
        if raw.format != FORMAT {
            return err(format!(
                "manifest format is {:?}, want {FORMAT:?}",
                raw.format
            ));
        }
        if raw.reference_revision != REFERENCE_REVISION {
            return err(format!(
                "manifest reference_revision is {}, want {REFERENCE_REVISION}",
                raw.reference_revision
            ));
        }
        if raw.slice != [0, 0] {
            return err(format!("manifest slice is {:?}, want [0, 0]", raw.slice));
        }
        let genesis_hash = hash32(&raw.genesis_hash, "manifest genesis_hash")?;
        if raw.segments.is_empty() {
            return err("manifest lists no segments");
        }
        let count = raw.segments.len();
        let mut segments = Vec::with_capacity(count);
        let mut next = 0u64;
        for (i, s) in raw.segments.into_iter().enumerate() {
            if s.first_zone != next {
                return err(format!(
                    "segment {} starts at zone {}, want {next} (segments must be contiguous from 0)",
                    s.file, s.first_zone
                ));
            }
            if s.last_zone < s.first_zone {
                return err(format!("segment {} ends before it starts", s.file));
            }
            let span = s.last_zone - s.first_zone + 1;
            let last = i + 1 == count;
            if span > SEGMENT_SPAN || (!last && span != SEGMENT_SPAN) {
                return err(format!(
                    "segment {} spans {span} zone heights; a segment holds {SEGMENT_SPAN} (the last can hold fewer)",
                    s.file
                ));
            }
            if s.file != segment_name(s.first_zone, s.last_zone) {
                return err(format!(
                    "segment file {:?} does not match its range, want {:?}",
                    s.file,
                    segment_name(s.first_zone, s.last_zone)
                ));
            }
            if s.records < span {
                return err(format!(
                    "segment {} has {} records for {span} zone heights",
                    s.file, s.records
                ));
            }
            let sha256 = hash32(&format!("0x{}", s.sha256), "segment sha256")?;
            next = s.last_zone + 1;
            segments.push(SegmentInfo {
                file: s.file,
                first_zone: s.first_zone,
                last_zone: s.last_zone,
                records: s.records,
                sha256,
            });
        }
        if next - 1 != raw.last_zone {
            return err(format!(
                "manifest last_zone is {}, segments end at {}",
                raw.last_zone,
                next - 1
            ));
        }
        Ok(Self {
            environment: raw.environment,
            slice: [0, 0],
            genesis_hash,
            receipts: raw.receipts,
            last_zone: raw.last_zone,
            segments,
            exported_utc: raw.exported_utc,
        })
    }
}

pub fn file_sha256(path: &Path) -> Result<Hash, ExportError> {
    let mut file = File::open(path).map_err(|e| io(path, e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buffer).map_err(|e| io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize().into())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawRecord {
    pub context: Context,
    pub number: u64,
    pub hash: Hash,
    pub block: Vec<u8>,
    pub receipts: Vec<u8>,
}

/// Sequential record reader for one segment file.
pub struct SegmentReader {
    path: PathBuf,
    reader: BufReader<File>,
    remaining: u64,
    offset: u64,
}
impl SegmentReader {
    pub fn open(path: &Path) -> Result<Self, ExportError> {
        let file = File::open(path).map_err(|e| io(path, e))?;
        let size = file.metadata().map_err(|e| io(path, e))?.len();
        let mut s = Self {
            path: path.to_path_buf(),
            reader: BufReader::with_capacity(1 << 20, file),
            remaining: size,
            offset: 0,
        };
        let mut magic = [0u8; 5];
        s.take(&mut magic, "magic")?;
        if &magic != MAGIC {
            return err(format!("{}: missing QEX1 magic", path.display()));
        }
        Ok(s)
    }
    fn take(&mut self, out: &mut [u8], what: &str) -> Result<(), ExportError> {
        if (out.len() as u64) > self.remaining {
            return err(format!(
                "{}: truncated {what} at byte {}",
                self.path.display(),
                self.offset
            ));
        }
        self.reader.read_exact(out).map_err(|e| io(&self.path, e))?;
        self.remaining -= out.len() as u64;
        self.offset += out.len() as u64;
        Ok(())
    }
    fn bytes(&mut self, what: &str) -> Result<Vec<u8>, ExportError> {
        let mut len = [0u8; 4];
        self.take(&mut len, what)?;
        let len = u32::from_be_bytes(len) as u64;
        // The file size bounds every allocation.
        if len > self.remaining {
            return err(format!(
                "{}: {what} length {len} exceeds the {} bytes left at byte {}",
                self.path.display(),
                self.remaining,
                self.offset
            ));
        }
        let mut out = vec![0u8; len as usize];
        self.take(&mut out, what)?;
        Ok(out)
    }
    /// Byte offset of the next record, for error messages.
    pub fn offset(&self) -> u64 {
        self.offset
    }
    pub fn next_record(&mut self) -> Result<Option<RawRecord>, ExportError> {
        if self.remaining == 0 {
            return Ok(None);
        }
        if self.remaining < RECORD_HEAD {
            return err(format!(
                "{}: truncated record at byte {}",
                self.path.display(),
                self.offset
            ));
        }
        let at = self.offset;
        let mut head = [0u8; 42];
        self.take(&mut head, "record header")?;
        if head[0] != KIND_BLOCK {
            return err(format!(
                "{}: record at byte {at} has kind {}, want {KIND_BLOCK}",
                self.path.display(),
                head[0]
            ));
        }
        let context = Context::from_u8(head[1]).ok_or_else(|| {
            ExportError(format!(
                "{}: record at byte {at} has context {}",
                self.path.display(),
                head[1]
            ))
        })?;
        let number = u64::from_be_bytes(head[2..10].try_into().unwrap());
        let hash: Hash = head[10..42].try_into().unwrap();
        let block = self.bytes("block")?;
        let receipts = self.bytes("receipts")?;
        Ok(Some(RawRecord {
            context,
            number,
            hash,
            block,
            receipts,
        }))
    }
}

/// All records for one zone height: an optional prime record, an optional
/// region record and the zone record, as go-quai appends a coincident block.
#[derive(Clone, Debug)]
pub struct Height {
    pub zone_height: u64,
    pub records: [Option<RawRecord>; 3],
}
impl Height {
    pub fn zone(&self) -> &RawRecord {
        self.records[2].as_ref().unwrap()
    }
}

type SegmentHook = Box<dyn FnMut(&SegmentInfo) + Send>;
/// Streams zone heights from the segments of a verified manifest. Each
/// segment's SHA-256 is checked before any of its records is returned.
pub struct ExportReader {
    dir: PathBuf,
    manifest: Manifest,
    segment: usize,
    reader: Option<SegmentReader>,
    records_in_segment: u64,
    next_height: u64,
    stop_after: Option<u64>,
    on_segment: Option<SegmentHook>,
}
impl ExportReader {
    pub fn new(dir: &Path, manifest: Manifest, stop_after: Option<u64>) -> Self {
        Self {
            dir: dir.to_path_buf(),
            manifest,
            segment: 0,
            reader: None,
            records_in_segment: 0,
            next_height: 0,
            stop_after,
            on_segment: None,
        }
    }
    /// Called before a segment's hash is checked (for progress output).
    pub fn on_segment(&mut self, f: SegmentHook) {
        self.on_segment = Some(f);
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn open_segment(&mut self) -> Result<(), ExportError> {
        let info = self.manifest.segments[self.segment].clone();
        if let Some(f) = self.on_segment.as_mut() {
            f(&info);
        }
        let path = self.dir.join(&info.file);
        let actual = file_sha256(&path)?;
        if actual != info.sha256 {
            return err(format!(
                "{}: sha256 {} does not match manifest {}",
                info.file,
                hex(&actual),
                hex(&info.sha256)
            ));
        }
        self.reader = Some(SegmentReader::open(&path)?);
        self.records_in_segment = 0;
        Ok(())
    }
    fn finish_segment(&mut self) -> Result<(), ExportError> {
        let info = &self.manifest.segments[self.segment];
        if self.records_in_segment != info.records {
            return err(format!(
                "{}: holds {} records, manifest says {}",
                info.file, self.records_in_segment, info.records
            ));
        }
        if self.next_height != info.last_zone + 1 {
            return err(format!(
                "{}: ends at zone {}, manifest says {}",
                info.file,
                self.next_height.wrapping_sub(1),
                info.last_zone
            ));
        }
        self.reader = None;
        self.segment += 1;
        Ok(())
    }
    fn record(&mut self) -> Result<Option<RawRecord>, ExportError> {
        loop {
            if self.reader.is_none() {
                if self.segment == self.manifest.segments.len() {
                    return Ok(None);
                }
                self.open_segment()?;
            }
            match self.reader.as_mut().unwrap().next_record()? {
                Some(r) => {
                    self.records_in_segment += 1;
                    return Ok(Some(r));
                }
                None => self.finish_segment()?,
            }
        }
    }
    /// Next zone height, or None at the end of the export or after `stop_after`.
    pub fn next_height(&mut self) -> Result<Option<Height>, ExportError> {
        if self.stop_after.is_some_and(|to| self.next_height > to) {
            return Ok(None);
        }
        let expected = self.next_height;
        let mut records: [Option<RawRecord>; 3] = [None, None, None];
        let mut last: Option<Context> = None;
        loop {
            let Some(r) = self.record()? else {
                if last.is_some() {
                    return err(format!(
                        "export ends inside the records for zone height {expected}"
                    ));
                }
                if expected != self.manifest.last_zone + 1 {
                    return err(format!(
                        "export ends at zone height {}, manifest last_zone is {}",
                        expected.wrapping_sub(1),
                        self.manifest.last_zone
                    ));
                }
                return Ok(None);
            };
            if last.is_some_and(|c| r.context <= c) {
                return err(format!(
                    "zone height {expected}: {} record follows a {} record (want prime, region, zone order)",
                    r.context.name(),
                    last.unwrap().name()
                ));
            }
            if r.context == Context::Zone && last == Some(Context::Prime) {
                return err(format!(
                    "zone height {expected}: prime record without a region record"
                ));
            }
            if !self.manifest.receipts && !r.receipts.is_empty() {
                return err(format!(
                    "zone height {expected}: {} record has receipts but the manifest says \"receipts\": false",
                    r.context.name()
                ));
            }
            if r.context != Context::Zone && !r.receipts.is_empty() {
                return err(format!(
                    "zone height {expected}: {} record carries receipts",
                    r.context.name()
                ));
            }
            last = Some(r.context);
            let context = r.context;
            records[context.index()] = Some(r);
            if context == Context::Zone {
                break;
            }
        }
        let zone = records[2].as_ref().unwrap();
        if zone.number != expected {
            return err(format!(
                "zone record number {} where zone height {expected} was expected",
                zone.number
            ));
        }
        for c in [Context::Prime, Context::Region] {
            if let Some(r) = &records[c.index()] {
                if r.hash != zone.hash {
                    return err(format!(
                        "zone height {expected}: {} record hash {} differs from the zone record hash {}",
                        c.name(),
                        hex(&r.hash),
                        hex(&zone.hash)
                    ));
                }
            }
        }
        if expected == 0 && records.iter().any(Option::is_none) {
            return err("genesis must have prime, region and zone records");
        }
        self.next_height += 1;
        Ok(Some(Height {
            zone_height: expected,
            records,
        }))
    }
}
pub fn hex(bytes: &[u8]) -> String {
    quai_primitives::encode_hex(bytes)
}

/// Writes a QEX1 export. Used by tests and fixture tools; the Go exporter is
/// the reference producer.
pub struct ExportWriter {
    dir: PathBuf,
    environment: String,
    genesis_hash: Hash,
    receipts: bool,
    segments: Vec<SegmentInfo>,
    current: Option<(u64, Vec<u8>, u64)>,
    next_height: u64,
    span: u64,
}
impl ExportWriter {
    pub fn new(dir: &Path, environment: &str, genesis_hash: Hash, receipts: bool) -> Self {
        Self::with_span(dir, environment, genesis_hash, receipts, SEGMENT_SPAN)
    }
    /// A smaller span makes multi-segment test exports; the reader refuses it
    /// unless it equals `SEGMENT_SPAN`, so only use it for negative tests.
    pub fn with_span(
        dir: &Path,
        environment: &str,
        genesis_hash: Hash,
        receipts: bool,
        span: u64,
    ) -> Self {
        Self {
            dir: dir.to_path_buf(),
            environment: environment.into(),
            genesis_hash,
            receipts,
            segments: vec![],
            current: None,
            next_height: 0,
            span,
        }
    }
    /// Append the records of the next zone height, in prime, region, zone order.
    pub fn push(&mut self, records: &[RawRecord]) -> std::io::Result<()> {
        let height = self.next_height;
        let (first, buf, count) = self
            .current
            .get_or_insert_with(|| (height, MAGIC.to_vec(), 0));
        let _ = first;
        for r in records {
            buf.push(KIND_BLOCK);
            buf.push(r.context as u8);
            buf.extend_from_slice(&r.number.to_be_bytes());
            buf.extend_from_slice(&r.hash);
            buf.extend_from_slice(&(r.block.len() as u32).to_be_bytes());
            buf.extend_from_slice(&r.block);
            buf.extend_from_slice(&(r.receipts.len() as u32).to_be_bytes());
            buf.extend_from_slice(&r.receipts);
            *count += 1;
        }
        self.next_height += 1;
        if self.next_height - self.current.as_ref().unwrap().0 == self.span {
            self.flush()?;
        }
        Ok(())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let Some((first, buf, count)) = self.current.take() else {
            return Ok(());
        };
        let last = self.next_height - 1;
        let name = segment_name(first, last);
        std::fs::create_dir_all(self.dir.join("segments"))?;
        std::fs::write(self.dir.join(&name), &buf)?;
        self.segments.push(SegmentInfo {
            file: name,
            first_zone: first,
            last_zone: last,
            records: count,
            sha256: Sha256::digest(&buf).into(),
        });
        Ok(())
    }
    pub fn finish(mut self) -> std::io::Result<PathBuf> {
        self.flush()?;
        let segments: Vec<_> = self
            .segments
            .iter()
            .map(|s| {
                serde_json::json!({
                    "file": s.file,
                    "first_zone": s.first_zone,
                    "last_zone": s.last_zone,
                    "records": s.records,
                    "sha256": hex(&s.sha256)[2..],
                })
            })
            .collect();
        let manifest = serde_json::json!({
            "format": FORMAT,
            "reference_revision": REFERENCE_REVISION,
            "environment": self.environment,
            "slice": [0, 0],
            "genesis_hash": hex(&self.genesis_hash),
            "receipts": self.receipts,
            "last_zone": self.next_height.saturating_sub(1),
            "segments": segments,
            "exported_utc": "1970-01-01T00:00:00Z",
        });
        let path = self.dir.join("manifest.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&manifest)?)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest(segments: serde_json::Value, last: u64) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "format": "QEX1",
            "reference_revision": REFERENCE_REVISION,
            "environment": "colosseum",
            "slice": [0, 0],
            "genesis_hash": hex(&[7; 32]),
            "receipts": true,
            "last_zone": last,
            "segments": segments,
            "exported_utc": "2026-11-17T00:00:00Z",
        }))
        .unwrap()
    }
    fn seg(first: u64, last: u64) -> serde_json::Value {
        serde_json::json!({
            "file": segment_name(first, last),
            "first_zone": first,
            "last_zone": last,
            "records": last - first + 1,
            "sha256": "00".repeat(32),
        })
    }
    #[test]
    fn manifest_checks_contiguity_names_spans_and_revision() {
        let ok = manifest(
            serde_json::json!([seg(0, 99_999), seg(100_000, 100_004)]),
            100_004,
        );
        let m = Manifest::parse(&ok).unwrap();
        assert_eq!(m.segments.len(), 2);
        assert_eq!(m.genesis_hash, [7; 32]);
        // gap
        let bad = manifest(
            serde_json::json!([seg(0, 99_999), seg(100_001, 100_004)]),
            100_004,
        );
        assert!(Manifest::parse(&bad).unwrap_err().0.contains("contiguous"));
        // short non-last segment
        let bad = manifest(serde_json::json!([seg(0, 9), seg(10, 19)]), 19);
        assert!(Manifest::parse(&bad).unwrap_err().0.contains("spans"));
        // not from zero
        let bad = manifest(serde_json::json!([seg(1, 9)]), 9);
        assert!(Manifest::parse(&bad).is_err());
        // last_zone disagreement
        let bad = manifest(serde_json::json!([seg(0, 9)]), 10);
        assert!(Manifest::parse(&bad).unwrap_err().0.contains("last_zone"));
        // file name must match the range
        let mut s = seg(0, 9);
        s["file"] = "segments/../../etc/passwd".into();
        let bad = manifest(serde_json::json!([s]), 9);
        assert!(Manifest::parse(&bad)
            .unwrap_err()
            .0
            .contains("does not match"));
        // revision and format
        let mut v: serde_json::Value = serde_json::from_slice(&ok).unwrap();
        v["reference_revision"] = "0000".into();
        assert!(Manifest::parse(&serde_json::to_vec(&v).unwrap()).is_err());
        let mut v: serde_json::Value = serde_json::from_slice(&ok).unwrap();
        v["format"] = "QEX2".into();
        assert!(Manifest::parse(&serde_json::to_vec(&v).unwrap()).is_err());
        let mut v: serde_json::Value = serde_json::from_slice(&ok).unwrap();
        v["slice"] = serde_json::json!([0, 1]);
        assert!(Manifest::parse(&serde_json::to_vec(&v).unwrap()).is_err());
    }
}
