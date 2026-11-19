//! The shadow bootstrap (`run --shadow-bootstrap`, a test tool) on the
//! private fixture (fixtures/protocol/canonical.json, chain 1337):
//!
//! - the window it derives at a trusted block from the ancestry back to a
//!   prime block (records, append snapshots, orders, region rollups, prime
//!   controller state) equals the window of a replay from the genesis, and
//!   the blocks after the trusted block pass every check against it;
//! - its checkpoints carry the trusted block, and a run without the test
//!   flag refuses them: production `run` starts only from the genesis or a
//!   checkpoint whose whole ancestry it validated.
use quai_consensus::pow::SharedCaches;
use quai_node::checkpoint::{self, Checkpoint};
use quai_node::log::{Level, Log};
use quai_node::processor::{
    self, BlockSource, BootstrapSeed, Next, Processor, Settings, SourceError,
};
use quai_node::qex::{Context, Height, RawRecord};
use quai_node::spec::Chain;
use quai_primitives::{decode_hex, Hash};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

fn doc() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json")).unwrap()
}
fn hash(v: &serde_json::Value) -> Hash {
    decode_hex(v.as_str().unwrap()).unwrap().try_into().unwrap()
}
/// Branch `first_branch`: its hashes and heights 0..=16.
fn branch() -> (Vec<Hash>, Vec<Height>) {
    let doc = doc();
    let br = doc["first_branch"].as_u64().unwrap() as usize;
    let go: Vec<Hash> = doc["canonical_events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["branch"] == br && e["selected"] == true && e["head_number"] == 16)
        .unwrap()["canonical_hashes"]
        .as_array()
        .unwrap()
        .iter()
        .map(hash)
        .collect();
    let b = &doc["branches"][br];
    let genesis = decode_hex(b["genesis"].as_str().unwrap()).unwrap();
    let mut out = vec![Height {
        zone_height: 0,
        records: [Context::Prime, Context::Region, Context::Zone].map(|c| {
            Some(RawRecord {
                context: c,
                number: 0,
                hash: go[0],
                block: genesis.clone(),
                receipts: vec![],
            })
        }),
    }];
    let mut numbers = [0u64; 3];
    for (i, v) in b["vectors"].as_array().unwrap().iter().enumerate() {
        let mut h = Height {
            zone_height: i as u64 + 1,
            records: [None, None, None],
        };
        for c in v["contexts"].as_array().unwrap() {
            let ctx = Context::from_u8(c["context"].as_u64().unwrap() as u8).unwrap();
            numbers[ctx.index()] += 1;
            h.records[ctx.index()] = Some(RawRecord {
                context: ctx,
                number: numbers[ctx.index()],
                hash: go[i + 1],
                block: decode_hex(c["work_object"].as_str().unwrap()).unwrap(),
                receipts: vec![],
            });
        }
        out.push(h);
    }
    (go, out)
}
struct Heights(VecDeque<Height>);
impl BlockSource for Heights {
    fn next(&mut self) -> Result<Next, SourceError> {
        Ok(self
            .0
            .pop_front()
            .map_or(Next::End, |h| Next::Height(Box::new(h))))
    }
    fn target(&self) -> Option<u64> {
        self.0.back().map(|h| h.zone_height)
    }
    fn receipts(&self) -> bool {
        false
    }
}
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("quai-node-shadow-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
fn settings(data: &Path, resume: bool, shadow: bool) -> Settings {
    Settings {
        jobs: 2,
        progress: Duration::from_secs(3600),
        state: false,
        datadir: Some(data.to_path_buf()),
        resume,
        checkpoint_every: 1_000,
        commit_every: 1_000,
        store: true,
        shadow,
    }
}
fn open(data: &Path, resume: bool, shadow: bool) -> Result<Processor, String> {
    Processor::open(
        Chain::for_environment("local").unwrap(),
        settings(data, resume, shadow),
        Some(Arc::new(SharedCaches::new(2).unwrap())),
        std::time::Instant::now(),
    )
}
fn latest(data: &Path) -> Checkpoint {
    let chain = Chain::for_environment("local").unwrap();
    let dir = processor::checkpoint_dir(data, &chain, false);
    let list = checkpoint::list(&dir).unwrap();
    checkpoint::read(&list.first().unwrap().1).unwrap()
}
/// Records of a checkpoint window by hash, with their snapshots.
fn window(cp: &Checkpoint, c: usize) -> BTreeMap<Hash, (Vec<u8>, checkpoint::Snapshot)> {
    cp.windows[c]
        .iter()
        .map(|(raw, s)| {
            let r = quai_consensus::history::Record::decode(raw).unwrap();
            (r.id(), (raw.clone(), s.clone()))
        })
        .collect()
}

/// Bootstrap at height 12 from the prime block at height 5; replay from
/// the genesis to 12; the windows, orders, rollups and prime state agree,
/// and heights 13..=16 (with the prime block 15) pass every check after it.
#[test]
fn a_shadow_bootstrap_equals_the_replay_window_and_checks_the_next_blocks() {
    let (go, heights) = branch();
    let log = Log::new(Level::Error);
    // The replay from the genesis to height 12.
    let replayed = scratch("replay");
    let mut p = open(&replayed, false, false).unwrap();
    p.run(&mut Heights(heights[..=12].iter().cloned().collect()), &log)
        .unwrap();
    assert!(p.finish(&log).unwrap().failure.is_none());
    let a = latest(&replayed);
    assert_eq!((a.height, a.block), (12, go[12]));
    // The bootstrap: seed at the prime block 5, trusted block 12, whose
    // prime head is the prime block 10 (prime number 2).
    let shadow = scratch("bootstrap");
    let seed = BootstrapSeed {
        trusted: (go[12], 12),
        prime_head: (go[10], 2),
        prime_view: heights[5].records[0].as_ref().unwrap().block.clone(),
        region_view: heights[5].records[1].as_ref().unwrap().block.clone(),
        rates: BTreeMap::new(),
    };
    let report = processor::bootstrap_checkpoint(
        Chain::for_environment("local").unwrap(),
        &shadow,
        seed,
        &mut Heights(heights[5..=12].iter().cloned().collect()),
        &AtomicBool::new(false),
        &log,
    )
    .unwrap();
    assert_eq!((report.start, report.trusted, report.heights), (5, 12, 8));
    assert_eq!(report.records, [1, 3, 7]);
    let b = latest(&shadow);
    assert_eq!(b.trusted, Some((go[12], 12)));
    assert_eq!(a.trusted, None);
    assert_eq!(b.heads, a.heads);
    // Every derived record and snapshot is the replay's, and each window
    // ends at the head.
    for c in 0..3 {
        let (wa, wb) = (window(&a, c), window(&b, c));
        assert!(!wb.is_empty());
        for (id, entry) in &wb {
            assert_eq!(wa.get(id), Some(entry), "context {c} record {id:?}");
        }
        assert_eq!(a.windows[c].last(), b.windows[c].last());
    }
    let orders: BTreeMap<_, _> = a.orders.iter().copied().collect();
    for (id, o) in &b.orders {
        assert_eq!(orders.get(id), Some(o));
    }
    let (pa, pb) = (a.prime.as_ref().unwrap(), b.prime.as_ref().unwrap());
    assert_eq!(
        (pa.head, pa.number, pa.first, &pa.rates),
        (pb.head, pb.number, pb.first, &pb.rates)
    );
    assert_eq!(pa.parent_window, pb.parent_window);
    assert_eq!(pa.head_window, pb.head_window);
    let rollups: BTreeMap<_, _> = pa.rollups.iter().cloned().collect();
    assert!(!pb.rollups.is_empty());
    for (id, r) in &pb.rollups {
        assert_eq!(rollups.get(id), Some(r));
    }
    // A run without the test flag refuses the shadow checkpoint.
    let e = open(&shadow, true, false).err().unwrap();
    assert!(e.contains("shadow run"), "{e}");
    // With it, heights 13..=16 pass every check after the trusted block.
    let mut p = open(&shadow, true, true).unwrap();
    assert_eq!(p.trusted(), Some((go[12], 12)));
    assert_eq!(p.zone_head(), Some((go[12], 12)));
    p.run(&mut Heights(heights[12..].iter().cloned().collect()), &log)
        .unwrap();
    assert_eq!(p.zone_head(), Some((go[16], 16)));
    let s = p.finish(&log).unwrap();
    assert!(s.failure.is_none(), "{:?}", s.failure);
    assert_eq!(s.run_heights, 4);
    // Its next checkpoint still carries the trusted block.
    assert_eq!(latest(&shadow).trusted, Some((go[12], 12)));
    std::fs::remove_dir_all(replayed).unwrap();
    std::fs::remove_dir_all(shadow).unwrap();
}

/// Production `run` (no --shadow-bootstrap) refuses a data directory whose
/// checkpoint descends from a trusted block, and starts from one it
/// validated; the test flag lets it continue the shadow run.
#[test]
fn run_refuses_a_shadow_checkpoint_without_the_test_flag() {
    let (go, heights) = branch();
    let log = Log::new(Level::Error);
    let data = scratch("refuse");
    let mut p = open(&data, false, false).unwrap();
    p.run(&mut Heights(heights[..=8].iter().cloned().collect()), &log)
        .unwrap();
    p.finish(&log).unwrap();
    // A validated checkpoint: every start accepts it.
    assert!(open(&data, true, false).is_ok());
    // The same checkpoint as a shadow bootstrap writes it.
    let chain = Chain::for_environment("local").unwrap();
    let dir = processor::checkpoint_dir(&data, &chain, false);
    let mut cp = latest(&data);
    cp.trusted = Some((go[8], 8));
    checkpoint::write(&dir, &cp).unwrap();
    let e = open(&data, true, false).err().unwrap();
    assert!(
        e.contains("shadow run") && e.contains("--shadow-bootstrap"),
        "{e}"
    );
    assert!(open(&data, true, true).is_ok());
    // `quai-node run` itself, without the flag, refuses before it syncs.
    let o = quai_node::node::Options {
        datadir: data.clone(),
        network: "local".into(),
        stateless: true,
        jobs: 1,
        progress: Duration::from_secs(60),
        rpc_listen: None,
        metrics_listen: None,
        peers: vec![
            "/ip4/127.0.0.1/tcp/9/p2p/12D3KooWB72Vs7gfLgvJsmR7cex7tG5WY7WDdqJD124Res3byDyk".into(),
        ],
        public: false,
        margin: 64,
        max_peers: 1,
        to: None,
        checkpoint_every: 1_000,
        commit_every: 1_000,
        gossip: false,
        shadow_bootstrap: None,
        bootstrap_quorum: 3,
        bootstrap_depth: 1_000,
        store: true,
    };
    let e = quai_node::node::run(&o, &log).err().unwrap();
    assert!(e.contains("shadow run"), "{e}");
    // The command line: the flag is a test tool for stateless runs only.
    let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let parse = quai_node::cli::parse;
    assert!(parse(&args(&["run", "--shadow-bootstrap", "auto"])).is_err());
    let quai_node::cli::Command::Run(r) =
        parse(&args(&["run", "--stateless", "--shadow-bootstrap", "auto"]))
            .unwrap()
            .command
    else {
        panic!("not run")
    };
    assert_eq!(r.shadow_bootstrap, Some(None));
    let quai_node::cli::Command::Run(r) = parse(&args(&["run", "--stateless"])).unwrap().command
    else {
        panic!("not run")
    };
    assert_eq!(r.shadow_bootstrap, None);
    assert!(r.store);
    assert!(quai_node::cli::USAGE.contains("TEST TOOL"));
    // A shadow run without the block store needs the RPC off.
    assert!(parse(&args(&["run", "--stateless", "--no-store"])).is_err());
    let quai_node::cli::Command::Run(r) = parse(&args(&[
        "run",
        "--stateless",
        "--no-store",
        "--rpc-listen",
        "off",
    ]))
    .unwrap()
    .command
    else {
        panic!("not run")
    };
    assert!(!r.store && r.rpc_listen.is_none());
    std::fs::remove_dir_all(data).unwrap();
}
