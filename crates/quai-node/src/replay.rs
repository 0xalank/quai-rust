//! `quai-node replay`: stream a QEX1 export through the chain processor
//! (`processor.rs`) and validate every block with `Verifier::validate_block`
//! against the replay's ancestry window, in import order. Proof-of-work runs
//! in parallel (see `pow.rs`) and enters through the verifier memo;
//! everything else runs on one thread, height by height.
//!
//! The export is one [`BlockSource`]; `run` feeds the same processor from
//! peers. Per zone height the processor decodes each record and compares its
//! hash and number with the values go-quai recorded (and the stored receipts
//! with the receipt root), then runs every consensus check and, with
//! `--state`, executes the zone block and compares every state root.
use crate::log::Log;
use crate::processor::{BlockSource, Next, Processor, Settings, SourceError};
use crate::qex::{self, ExportError, ExportReader, Manifest};
use crate::spec::Chain;
use quai_consensus::pow::SharedCaches;
use quai_primitives::encode_hex;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use crate::processor::{checkpoint_dir, describe, End, Failure, Summary, Timing};

#[derive(Clone, Debug)]
pub struct Options {
    pub export: PathBuf,
    pub to: Option<u64>,
    pub jobs: usize,
    pub progress: Duration,
    /// Also execute zone blocks from genesis and compare the state roots.
    pub state: bool,
    /// Where checkpoints go (`<datadir>/replay/...`). None writes none.
    pub datadir: Option<PathBuf>,
    /// Continue from the latest checkpoint, if there is one.
    pub resume: bool,
    /// Zone heights between checkpoints.
    pub checkpoint_every: u64,
    /// Zone heights between state commits (`--state`).
    pub commit_every: u64,
}
impl Options {
    pub fn new(export: PathBuf) -> Self {
        Self {
            export,
            to: None,
            jobs: 1,
            progress: Duration::from_secs(10),
            state: false,
            datadir: None,
            resume: false,
            checkpoint_every: 10_000,
            commit_every: 1_000,
        }
    }
}
fn hex(h: &[u8]) -> String {
    encode_hex(h)
}

/// A QEX1 export as a block source.
pub struct ExportSource {
    reader: ExportReader,
    receipts: bool,
    last_zone: u64,
}
impl ExportSource {
    pub fn new(reader: ExportReader) -> Self {
        let m = reader.manifest();
        Self {
            receipts: m.receipts,
            last_zone: m.last_zone,
            reader,
        }
    }
}
fn container(e: ExportError) -> SourceError {
    SourceError {
        context: "export".into(),
        check: "QEX1 container".into(),
        expected: "a well-formed export".into(),
        actual: e.to_string(),
    }
}
impl BlockSource for ExportSource {
    fn next(&mut self) -> Result<Next, SourceError> {
        match self.reader.next_height() {
            Ok(Some(h)) => Ok(Next::Height(Box::new(h))),
            Ok(None) => Ok(Next::End),
            Err(e) => Err(container(e)),
        }
    }
    fn target(&self) -> Option<u64> {
        Some(self.last_zone)
    }
    fn receipts(&self) -> bool {
        self.receipts
    }
}

pub fn run(opts: &Options, log: &Log) -> Result<Summary, String> {
    run_with(opts, log, None)
}
/// As `run`, with an epoch cache store that outlives the run (tests reuse it).
pub fn run_with(
    opts: &Options,
    log: &Log,
    caches: Option<Arc<SharedCaches>>,
) -> Result<Summary, String> {
    run_until(opts, log, caches, None)
}
/// As `run_with`; setting `stop` ends the run after the current height with
/// a final checkpoint ([`End::Stopped`]).
pub fn run_until(
    opts: &Options,
    log: &Log,
    caches: Option<Arc<SharedCaches>>,
    stop: Option<Arc<AtomicBool>>,
) -> Result<Summary, String> {
    let started = Instant::now();
    let manifest = Manifest::load(&opts.export).map_err(|e| e.to_string())?;
    let spec = Chain::for_environment(&manifest.environment).map_err(|e| {
        format!(
            "chain spec for environment {:?}: {}",
            manifest.environment,
            describe(&e)
        )
    })?;
    if manifest.genesis_hash != spec.genesis_hash() {
        return Err(format!(
            "manifest genesis_hash {} is not the {} genesis {}",
            hex(&manifest.genesis_hash),
            spec.spec().environment,
            hex(&spec.genesis_hash())
        ));
    }
    let settings = Settings {
        jobs: opts.jobs,
        progress: opts.progress,
        state: opts.state,
        datadir: opts.datadir.clone(),
        resume: opts.resume,
        checkpoint_every: opts.checkpoint_every,
        commit_every: opts.commit_every,
    };
    let mut processor = Processor::open(spec, settings, caches, started)?;
    if let Some(stop) = stop {
        processor.set_stop_flag(stop);
    }
    let start = processor.start();
    if start > manifest.last_zone {
        return Err(format!(
            "the export ends at zone {}, below checkpoint {start}",
            manifest.last_zone
        ));
    }
    let chain = processor.chain().clone();
    log.info(&format!(
        "replay {}: environment {} (chain {}), genesis {}, {} segments, zone heights {start}..={}{}, {} proof-of-work threads{}",
        opts.export.display(),
        chain.spec().environment,
        chain.spec().chain_id,
        hex(&chain.genesis_hash()),
        manifest.segments.len(),
        manifest.last_zone,
        opts.to.map_or(String::new(), |t| format!(" (stop after {t})")),
        opts.jobs,
        match processor.resumed_from() {
            Some(h) => format!(", resuming from checkpoint {h}"),
            None => String::new(),
        }
    ));
    let last_zone = manifest.last_zone;
    let environment = manifest.environment.clone();
    let mut reader = ExportReader::starting_at(&opts.export, manifest, start, opts.to);
    let seg_log = *log;
    reader.on_segment(Box::new(move |s: &qex::SegmentInfo| {
        seg_log.info(&format!(
            "segment {} (zone {}..={}, {} records): checking sha256",
            s.file, s.first_zone, s.last_zone, s.records
        ))
    }));
    let mut source = ExportSource::new(reader);
    processor.run(&mut source, log)?;
    let ended = processor.summary().end;
    let mut summary = processor.finish(log)?;
    summary.environment = environment;
    summary.last_zone = last_zone;
    if ended == End::Complete && opts.to.is_some_and(|to| to < last_zone) {
        summary.end = End::ReachedTo;
    }
    Ok(summary)
}
