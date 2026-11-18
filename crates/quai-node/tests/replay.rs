//! Replay over QEX1 exports built from Go-encoded work objects in
//! fixtures/protocol/canonical.json (go-quai v0.57.0, reviewed private
//! hierarchy chain 1337): genesis plus 16 linked heights with coincident
//! prime/region blocks, workshares and ETXs, and Go-rejected variants.
use quai_consensus::pow::SharedCaches;
use quai_node::log::{Level, Log};
use quai_node::qex::{Context, ExportWriter, RawRecord};
use quai_node::replay::{self, End, Options, Summary};
use quai_primitives::{decode_hex, Hash};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

fn corpus() -> &'static serde_json::Value {
    static C: OnceLock<serde_json::Value> = OnceLock::new();
    C.get_or_init(|| {
        serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json")).unwrap()
    })
}
/// One ProgPoW epoch cache for every replay in this test binary.
fn caches() -> Arc<SharedCaches> {
    static C: OnceLock<Arc<SharedCaches>> = OnceLock::new();
    C.get_or_init(|| Arc::new(SharedCaches::new(2).unwrap()))
        .clone()
}
fn hash(v: &serde_json::Value) -> Hash {
    decode_hex(v.as_str().unwrap()).unwrap().try_into().unwrap()
}
fn bytes(v: &serde_json::Value) -> Vec<u8> {
    decode_hex(v.as_str().unwrap()).unwrap()
}
fn context(n: u64) -> Context {
    Context::from_u8(n as u8).unwrap()
}
/// Go's canonical block hashes (`wo.Hash()`) for a branch, heights 0..=16.
fn go_hashes(branch: u64) -> Vec<Hash> {
    let event = corpus()["canonical_events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["branch"] == branch && e["selected"] == true && e["head_number"] == 16)
        .unwrap();
    event["canonical_hashes"]
        .as_array()
        .unwrap()
        .iter()
        .map(hash)
        .collect()
}
/// Records per zone height for branch 0, with Go's recorded hashes.
fn heights() -> Vec<Vec<RawRecord>> {
    let branch = &corpus()["branches"][0];
    let genesis = hash(&branch["genesis_hash"]);
    let go = go_hashes(0);
    assert_eq!(go[0], genesis);
    let mut out = vec![[Context::Prime, Context::Region, Context::Zone]
        .into_iter()
        .map(|c| RawRecord {
            context: c,
            number: 0,
            hash: genesis,
            block: bytes(&branch["genesis"]),
            receipts: vec![],
        })
        .collect::<Vec<_>>()];
    let mut numbers = [0u64; 3];
    for (i, v) in branch["vectors"].as_array().unwrap().iter().enumerate() {
        out.push(records(v, &mut numbers, go[i + 1]));
    }
    out
}
/// Rust's hash of a work object, for variants Go never stored.
fn rust_hash(v: &serde_json::Value) -> Hash {
    quai_consensus::history::Record::decode(&bytes(&v["contexts"][0]["work_object"]))
        .unwrap()
        .id()
}
fn records(v: &serde_json::Value, numbers: &mut [u64; 3], hash: Hash) -> Vec<RawRecord> {
    v["contexts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let ctx = context(c["context"].as_u64().unwrap());
            numbers[ctx.index()] += 1;
            RawRecord {
                context: ctx,
                number: numbers[ctx.index()],
                hash,
                block: bytes(&c["work_object"]),
                receipts: vec![],
            }
        })
        .collect()
}
fn write(dir: &Path, heights: &[Vec<RawRecord>]) -> PathBuf {
    let genesis = heights[0][0].hash;
    let mut w = ExportWriter::new(dir, "local", genesis, false);
    for h in heights {
        w.push(h).unwrap();
    }
    w.finish().unwrap();
    dir.to_path_buf()
}
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quai-node-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
fn replay(dir: &Path, to: Option<u64>) -> Summary {
    run(dir, to, false)
}
fn run(dir: &Path, to: Option<u64>, state: bool) -> Summary {
    let opts = Options {
        export: dir.to_path_buf(),
        to,
        jobs: 2,
        progress: Duration::from_secs(3600),
        state,
    };
    replay::run_with(&opts, &Log::new(Level::Warn), Some(caches())).unwrap()
}

#[test]
fn fixture_chain_passes_every_stateless_check() {
    let dir = scratch("pass");
    let s = replay(&write(&dir, &heights()), None);
    assert_eq!(s.failure, None, "{}", s.line());
    assert_eq!(s.end, End::Complete);
    assert_eq!(s.highest_passed, Some(16));
    assert_eq!(s.heights, 17);
    // Prime at heights 0, 5, 10, 15; region also at 3, 8, 12.
    assert_eq!(s.records, [4, 7, 17]);
    assert_eq!(s.shares, 14);
    assert!(s
        .line()
        .starts_with("replay PASS: highest zone height with all checks passed: 16"));
    // --to stops early and says so.
    let s = replay(&dir, Some(4));
    assert_eq!(s.end, End::ReachedTo);
    assert_eq!(s.highest_passed, Some(4));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn go_rejected_variants_fail_at_their_height_with_the_matching_check() {
    let branch = &corpus()["branches"][0];
    let base = heights();
    let expected = [
        ("duplicate", "zone rules: duplicate uncle"),
        ("ancestor", "zone rules: uncle is ancestor"),
        ("sibling", "zone rules: dangling uncle"),
        ("previously-included", "zone rules: duplicate uncle"),
        ("wrong-number", "zone rules: uncle number"),
        ("bad-mix", "zone rules: mix hash"),
        ("uncle-root", "body: uncle root"),
        ("uncled-entropy", "uncled entropy"),
        ("forged-rollup-with-valid-work", "ETX rollup root"),
    ];
    for n in branch["negatives"].as_array().unwrap() {
        let id = n["id"].as_str().unwrap();
        let at = n["at_height"].as_u64().unwrap() as usize;
        let mut chain: Vec<_> = base[..at].to_vec();
        let mut numbers = [0u64; 3];
        for h in &chain[1..] {
            for r in h {
                numbers[r.context.index()] += 1;
            }
        }
        let id_hash = rust_hash(n);
        chain.push(records(n, &mut numbers, id_hash));
        let dir = scratch(id);
        let s = replay(&write(&dir, &chain), None);
        let f = s
            .failure
            .as_ref()
            .unwrap_or_else(|| panic!("{id} passed: {}", s.line()));
        assert_eq!(s.end, End::Failed, "{id}");
        assert_eq!(s.highest_passed, Some(at as u64 - 1), "{id}: {}", s.line());
        assert_eq!(f.zone_height, Some(at as u64), "{id}");
        assert_eq!(f.hash, Some(id_hash), "{id}");
        let want = expected.iter().find(|(i, _)| *i == id).unwrap().1;
        assert_eq!(f.check, want, "{id}: {}", s.line());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn container_and_link_faults_are_reported_with_the_last_good_height() {
    let base = heights();
    // A record hash that is not the block's hash.
    let mut chain = base[..4].to_vec();
    chain[3][1].hash[0] ^= 1;
    chain[3][0].hash[0] ^= 1;
    let dir = scratch("hash");
    let s = replay(&write(&dir, &chain), None);
    let f = s.failure.unwrap();
    assert_eq!(
        (f.check.as_str(), f.context.as_str()),
        ("block hash", "region")
    );
    assert_eq!(s.highest_passed, Some(2));
    std::fs::remove_dir_all(dir).unwrap();

    // A block the codec refuses: the failure carries the codec's reason.
    let mut chain = base[..3].to_vec();
    let zone = chain[2]
        .iter_mut()
        .find(|r| r.context == Context::Zone)
        .unwrap();
    zone.block.truncate(zone.block.len() - 7);
    let dir = scratch("decode");
    let s = replay(&write(&dir, &chain), None);
    let f = s.failure.unwrap();
    assert_eq!((f.check.as_str(), f.zone_height), ("decode", Some(2)));
    assert!(f.actual.contains('('), "{}", f.actual);
    assert_eq!(s.highest_passed, Some(1));
    std::fs::remove_dir_all(dir).unwrap();

    // Branch 1 shares heights 1..=8 and forks at 9: its height 10 does not
    // extend branch 0's height 9.
    let mut chain = base[..10].to_vec();
    let mut numbers = [0u64; 3];
    for r in chain[1..].iter().flatten() {
        numbers[r.context.index()] += 1;
    }
    chain.push(records(
        &corpus()["branches"][1]["vectors"][9],
        &mut numbers,
        go_hashes(1)[10],
    ));
    let dir = scratch("fork");
    let s = replay(&write(&dir, &chain), None);
    let f = s.failure.unwrap();
    assert_eq!(
        (f.check.as_str(), f.context.as_str()),
        ("parent hash", "zone")
    );
    assert_eq!(s.highest_passed, Some(9));
    std::fs::remove_dir_all(dir).unwrap();

    // A coincident prime block exported without its prime record.
    let mut chain = base[..6].to_vec();
    chain[5].retain(|r| r.context != Context::Prime);
    let dir = scratch("order");
    let s = replay(&write(&dir, &chain), None);
    assert_eq!(s.failure.unwrap().check, "coincident order");
    assert_eq!(s.highest_passed, Some(4));
    std::fs::remove_dir_all(dir).unwrap();

    // A segment whose bytes no longer match the manifest hash.
    let dir = scratch("sha");
    write(&dir, &base[..3]);
    let seg = dir.join(quai_node::qex::segment_name(0, 2));
    let mut b = std::fs::read(&seg).unwrap();
    let last = b.len() - 1;
    b[last] ^= 1;
    std::fs::write(&seg, b).unwrap();
    let s = replay(&dir, None);
    let f = s.failure.unwrap();
    assert_eq!(f.context, "export");
    assert!(f.actual.contains("sha256"), "{}", f.actual);
    assert_eq!(s.highest_passed, None);
    std::fs::remove_dir_all(dir).unwrap();

    // The manifest must name the genesis of a known chain specification.
    let dir = scratch("genesis");
    let mut w = ExportWriter::new(&dir, "local", [9; 32], false);
    for h in &base[..2] {
        w.push(h).unwrap();
    }
    w.finish().unwrap();
    let opts = Options {
        export: dir.clone(),
        to: None,
        jobs: 1,
        progress: Duration::from_secs(3600),
        state: false,
    };
    let e = replay::run_with(&opts, &Log::new(Level::Warn), Some(caches())).unwrap_err();
    assert!(e.contains("genesis"), "{e}");
    std::fs::remove_dir_all(dir).unwrap();
}

/// Real mainnet heights 0..=8 from the go-quai v0.57.0 exporter
/// (fixtures/mainnet/qex1-0-8, cut with the qex-slice example).
fn mainnet_slice() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mainnet/qex1-0-8")
}

#[test]
fn mainnet_slice_passes_with_the_colosseum_spec() {
    let s = replay(&mainnet_slice(), None);
    assert_eq!(s.failure, None, "{}", s.line());
    assert_eq!(s.environment, "colosseum");
    assert_eq!(s.highest_passed, Some(8));
    assert_eq!(s.records, [3, 5, 9]);
    assert_eq!(s.shares, 55);
}

#[test]
fn mainnet_receipts_must_match_the_header_receipt_root() {
    use prost::Message;
    use quai_node::qex::{ExportReader, Manifest};
    use quai_primitives::schema::block::ProtoReceiptsForStorage;
    let src = mainnet_slice();
    let manifest = Manifest::load(&src).unwrap();
    let mut reader = ExportReader::new(&src, manifest.clone(), None);
    let mut heights = vec![];
    while let Some(h) = reader.next_height().unwrap() {
        heights.push(h.records.into_iter().flatten().collect::<Vec<_>>());
    }
    // Change one stored receipt of the first block that has receipts.
    let (at, zone) = heights
        .iter_mut()
        .enumerate()
        .find_map(|(i, h)| {
            h.iter_mut()
                .find(|r| r.context == Context::Zone && !r.receipts.is_empty())
                .map(|r| (i, r))
        })
        .unwrap();
    let mut stored = ProtoReceiptsForStorage::decode(zone.receipts.as_slice()).unwrap();
    stored.receipts[0].cumulative_gas_used += 1;
    zone.receipts = stored.encode_to_vec();
    let dir = scratch("receipts");
    let mut w = ExportWriter::new(&dir, "colosseum", manifest.genesis_hash, true);
    for h in &heights[..=at] {
        w.push(h).unwrap();
    }
    w.finish().unwrap();
    let s = replay(&dir, None);
    let f = s.failure.unwrap();
    assert_eq!(f.check, "receipts: receipt root");
    assert_eq!(s.highest_passed, Some(at as u64 - 1));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fixture_chain_state_roots_match_go() {
    // Execution from genesis with the reviewed hierarchy profile: inbound
    // ETXs confirmed by the prime blocks at heights 5, 10 and 15 arrive at
    // heights 11 and 16.
    let dir = scratch("state");
    let s = run(&write(&dir, &heights()), None, true);
    assert_eq!(s.failure, None, "{}", s.line());
    assert_eq!(s.highest_passed, Some(16));
    assert_eq!(s.state_blocks, Some(16));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn mainnet_slice_state_roots_match_go() {
    // Block 1 credits the 12,612 genesis allocations (allocations.bin) and
    // every block pays its coinbase and workshares through ETXs.
    let s = run(&mainnet_slice(), None, true);
    assert_eq!(s.failure, None, "{}", s.line());
    assert_eq!(s.highest_passed, Some(8));
    assert_eq!(s.state_blocks, Some(8));
}
