//! Copy zone heights 0..=TO of a QEX1 export into a new export, after the
//! reader's own checks (manifest, segment hashes, record order):
//!
//!     cargo run --release -p quai-node --example qex-slice -- SRC DST TO
use quai_node::qex::{ExportReader, ExportWriter, Manifest, RawRecord};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        eprintln!("usage: qex-slice SRC DST TO");
        std::process::exit(2);
    }
    let (src, dst) = (Path::new(&args[0]), Path::new(&args[1]));
    let to: u64 = args[2].parse().expect("TO must be a zone height");
    if dst.join("manifest.json").exists() {
        eprintln!("{} already holds an export", dst.display());
        std::process::exit(2);
    }
    let manifest = Manifest::load(src).unwrap_or_else(|e| panic!("{e}"));
    let mut w = ExportWriter::new(
        dst,
        &manifest.environment,
        manifest.genesis_hash,
        manifest.receipts,
    );
    let mut reader = ExportReader::new(src, manifest, Some(to));
    while let Some(h) = reader.next_height().unwrap_or_else(|e| panic!("{e}")) {
        let records: Vec<RawRecord> = h.records.into_iter().flatten().collect();
        w.push(&records).unwrap();
    }
    println!("{}", w.finish().unwrap().display());
}
