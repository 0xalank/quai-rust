//! Checkpoints and `replay --resume` with `--state`, on real mainnet heights
//! 0..=8 (fixtures/mainnet/qex1-0-8) and on the Go-encoded private hierarchy
//! chain (fixtures/protocol/canonical.json branch 0, heights 0..=16).
use quai_consensus::pow::SharedCaches;
use quai_node::checkpoint::{self, Checkpoint};
use quai_node::log::{Level, Log};
use quai_node::qex::{Context, ExportWriter, Manifest, RawRecord};
use quai_node::replay::{self, checkpoint_dir, End, Options, Summary};
use quai_node::spec::Chain;
use quai_node::store;
use quai_primitives::{decode_hex, Hash};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

fn caches() -> Arc<SharedCaches> {
    static C: OnceLock<Arc<SharedCaches>> = OnceLock::new();
    C.get_or_init(|| Arc::new(SharedCaches::new(2).unwrap()))
        .clone()
}
fn unique(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let d = std::env::temp_dir().join(format!(
        "quai-node-ckpt-{}-{}-{name}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
fn mainnet_slice() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mainnet/qex1-0-8")
}
/// The private hierarchy fixture chain as a QEX1 export (environment local).
fn fixture_chain() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json"))
                .unwrap();
        let hash = |v: &serde_json::Value| -> Hash {
            decode_hex(v.as_str().unwrap()).unwrap().try_into().unwrap()
        };
        let event = corpus["canonical_events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["branch"] == 0 && e["selected"] == true && e["head_number"] == 16)
            .unwrap();
        let go: Vec<Hash> = event["canonical_hashes"]
            .as_array()
            .unwrap()
            .iter()
            .map(hash)
            .collect();
        let branch = &corpus["branches"][0];
        let dir = unique("fixture-export");
        let mut w = ExportWriter::new(&dir, "local", go[0], false);
        let genesis = decode_hex(branch["genesis"].as_str().unwrap()).unwrap();
        w.push(
            &[Context::Prime, Context::Region, Context::Zone].map(|c| RawRecord {
                context: c,
                number: 0,
                hash: go[0],
                block: genesis.clone(),
                receipts: vec![],
            }),
        )
        .unwrap();
        let mut numbers = [0u64; 3];
        for (i, v) in branch["vectors"].as_array().unwrap().iter().enumerate() {
            let records: Vec<RawRecord> = v["contexts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| {
                    let ctx = Context::from_u8(c["context"].as_u64().unwrap() as u8).unwrap();
                    numbers[ctx.index()] += 1;
                    RawRecord {
                        context: ctx,
                        number: numbers[ctx.index()],
                        hash: go[i + 1],
                        block: decode_hex(c["work_object"].as_str().unwrap()).unwrap(),
                        receipts: vec![],
                    }
                })
                .collect();
            w.push(&records).unwrap();
        }
        w.finish().unwrap();
        dir
    })
    .clone()
}
fn options(export: &Path, data: &Path, to: Option<u64>, resume: bool) -> Options {
    let mut o = Options::new(export.to_path_buf());
    o.to = to;
    o.jobs = 2;
    o.progress = Duration::from_secs(3600);
    o.state = true;
    o.datadir = Some(data.to_path_buf());
    o.resume = resume;
    o.checkpoint_every = 3;
    o.commit_every = 2;
    o
}
fn run(o: &Options) -> Result<Summary, String> {
    replay::run_with(o, &Log::new(Level::Warn), Some(caches()))
}
fn dir_of(export: &Path, data: &Path) -> PathBuf {
    let env = Manifest::load(export).unwrap().environment;
    checkpoint_dir(data, &Chain::for_environment(&env).unwrap(), true)
}
fn latest(export: &Path, data: &Path) -> Checkpoint {
    let dir = dir_of(export, data);
    let (_, path) = checkpoint::list(&dir).unwrap().into_iter().next().unwrap();
    checkpoint::read(&path).unwrap()
}
/// Roots, digest, confirmations, window, prime controller state, region
/// rollups and counters: everything a continuation depends on, without the
/// version id (which depends on where the earlier commits fell).
fn essence(c: &Checkpoint) -> impl PartialEq + std::fmt::Debug {
    let s = c.state.clone().unwrap();
    (
        (s.evm, s.etx, s.utxo, s.accounts, s.utxo_count, s.digest),
        s.confirmations,
        c.heads,
        c.windows.clone(),
        c.prime_views.clone(),
        c.orders.clone(),
        c.counters.clone(),
        (c.height, c.block),
        c.prime.clone(),
    )
}

#[test]
fn straight_and_resumed_replays_end_with_identical_roots() {
    for (export, n, k) in [(mainnet_slice(), 8, 5), (fixture_chain(), 16, 7)] {
        let a = unique("straight");
        let s = run(&options(&export, &a, None, false)).unwrap();
        assert_eq!(s.failure, None, "{}", s.line());
        assert_eq!(
            (s.highest_passed, s.state_blocks, s.checkpoint),
            (Some(n), Some(n), Some(n))
        );
        let straight = latest(&export, &a);

        let b = unique("resumed");
        let s = run(&options(&export, &b, Some(k), false)).unwrap();
        assert_eq!(
            (s.end, s.highest_passed, s.checkpoint),
            (End::ReachedTo, Some(k), Some(k))
        );
        // A run without --resume refuses a directory with checkpoints.
        assert!(run(&options(&export, &b, None, false))
            .unwrap_err()
            .contains("--resume"));
        let s = run(&options(&export, &b, None, true)).unwrap();
        assert_eq!(s.failure, None, "{}", s.line());
        assert_eq!(s.resumed_from, Some(k));
        assert_eq!(s.run_heights, n - k);
        assert_eq!(
            (s.highest_passed, s.heights, s.state_blocks),
            (Some(n), n + 1, Some(n))
        );
        let resumed = latest(&export, &b);
        assert_eq!(essence(&straight), essence(&resumed));
        // Resuming at the end of the export checks the checkpoint and stops.
        let s = run(&options(&export, &b, None, true)).unwrap();
        assert_eq!(
            (s.end, s.resumed_from, s.run_heights),
            (End::Complete, Some(n), 0)
        );
        // Only the two latest checkpoints and their state files stay.
        let dir = dir_of(&export, &b);
        let kept: Vec<u64> = checkpoint::list(&dir)
            .unwrap()
            .iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(kept.len(), 2);
        let states = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                let name = e.as_ref().unwrap().file_name().into_string().unwrap();
                name.starts_with("state-")
            })
            .count();
        assert!(states <= 2, "{states} state files");
        std::fs::remove_dir_all(a).unwrap();
        std::fs::remove_dir_all(b).unwrap();
    }
}

#[test]
fn tampered_checkpoints_are_rejected() {
    let export = mainnet_slice();
    let data = unique("tamper");
    run(&options(&export, &data, None, false)).unwrap();
    let dir = dir_of(&export, &data);
    let path = checkpoint::path(&dir, 8);
    let original = std::fs::read(&path).unwrap();
    // A changed byte fails the integrity hash.
    let mut raw = original.clone();
    let mid = raw.len() / 2;
    raw[mid] ^= 1;
    std::fs::write(&path, &raw).unwrap();
    let e = run(&options(&export, &data, None, true)).unwrap_err();
    assert!(
        e.contains("checkpoint 8 rejected") && e.contains("integrity"),
        "{e}"
    );
    // A well-formed checkpoint whose roots are not the header roots.
    let mut cp = Checkpoint::decode(&original).unwrap();
    cp.state.as_mut().unwrap().evm[0] ^= 1;
    std::fs::write(&path, cp.encode()).unwrap();
    let e = run(&options(&export, &data, None, true)).unwrap_err();
    assert!(e.contains("checkpoint 8 rejected"), "{e}");
    // A checkpoint whose window does not end at its head.
    let mut cp = Checkpoint::decode(&original).unwrap();
    cp.heads[2] = Some(([9; 32], 8));
    std::fs::write(&path, cp.encode()).unwrap();
    assert!(run(&options(&export, &data, None, true)).is_err());
    // The original resumes.
    std::fs::write(&path, &original).unwrap();
    let s = run(&options(&export, &data, None, true)).unwrap();
    assert_eq!((s.end, s.resumed_from), (End::Complete, Some(8)));
    std::fs::remove_dir_all(data).unwrap();
}

#[test]
fn interrupted_checkpoint_writes_resume_from_the_previous_checkpoint() {
    let export = mainnet_slice();
    let base = unique("baseline");
    run(&options(&export, &base, None, false)).unwrap();
    let expected = latest(&export, &base);
    for (point, previous) in [
        ("state-copied", 3),
        ("checkpoint-half-written", 3),
        ("checkpoint-before-rename", 3),
        ("checkpoint-written", 6),
    ] {
        let data = unique(point);
        let dir = dir_of(&export, &data);
        std::fs::create_dir_all(&dir).unwrap();
        store::inject_fault(&dir, point, 6);
        let e = run(&options(&export, &data, None, false)).unwrap_err();
        assert!(e.contains(&format!("test fault at {point}@6")), "{e}");
        assert_eq!(checkpoint::list(&dir).unwrap()[0].0, previous, "{point}");
        let s = run(&options(&export, &data, None, true)).unwrap();
        assert_eq!(s.failure, None, "{point}: {}", s.line());
        assert_eq!(
            (s.resumed_from, s.highest_passed),
            (Some(previous), Some(8)),
            "{point}"
        );
        assert_eq!(
            essence(&latest(&export, &data)),
            essence(&expected),
            "{point}"
        );
        std::fs::remove_dir_all(data).unwrap();
    }
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn a_killed_process_resumes_from_the_previous_checkpoint() {
    let export = mainnet_slice();
    let data = unique("killed");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_quai-node"))
        .args([
            "--datadir",
            data.to_str().unwrap(),
            "--log-level",
            "error",
            "replay",
        ])
        .args([
            "--export",
            export.to_str().unwrap(),
            "--state",
            "--jobs",
            "2",
        ])
        .args([
            "--checkpoint-every",
            "3",
            "--commit-every",
            "2",
            "--no-report",
        ])
        .env("QUAI_NODE_TEST_FAULT", "checkpoint-half-written@6")
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(75));
    let dir = dir_of(&export, &data);
    let partial = checkpoint::path(&dir, 6).with_extension("qrc.tmp");
    assert!(partial.is_file() && !checkpoint::path(&dir, 6).exists());
    assert_eq!(checkpoint::list(&dir).unwrap()[0].0, 3);
    let s = run(&options(&export, &data, None, true)).unwrap();
    assert_eq!(s.failure, None, "{}", s.line());
    assert_eq!((s.resumed_from, s.highest_passed), (Some(3), Some(8)));
    assert!(!partial.exists());
    std::fs::remove_dir_all(data).unwrap();
}

#[test]
fn checkpoints_without_a_prime_section_resume_before_the_kick_in() {
    // A checkpoint written before the prime controllers were ported has no
    // prime section and keeps the region rollups in its state section. The
    // replay rebuilds the controller state (every prime block keeps the
    // starting rate before ControllerKickInBlock) and ends where a straight
    // run ends.
    let export = mainnet_slice();
    let base = unique("prime-baseline");
    run(&options(&export, &base, None, false)).unwrap();
    let expected = latest(&export, &base);
    assert!(expected.prime.is_some());
    let data = unique("prime-legacy");
    run(&options(&export, &data, Some(5), false)).unwrap();
    let dir = dir_of(&export, &data);
    let path = checkpoint::path(&dir, 5);
    let mut legacy = checkpoint::read(&path).unwrap();
    let prime = legacy.prime.take().unwrap();
    for (id, _, rollup) in &mut legacy.state.as_mut().unwrap().confirmations {
        *rollup = prime
            .rollups
            .iter()
            .find(|(h, _)| h == id)
            .map(|(_, r)| r.clone());
    }
    std::fs::write(&path, legacy.encode()).unwrap();
    let s = run(&options(&export, &data, None, true)).unwrap();
    assert_eq!(s.failure, None, "{}", s.line());
    assert_eq!((s.resumed_from, s.highest_passed), (Some(5), Some(8)));
    assert_eq!(essence(&latest(&export, &data)), essence(&expected));
    // A prime section whose rates are not the rates of its prime records,
    // or whose state does not follow the prime head, is refused.
    let path = checkpoint::path(&dir, 8);
    let good = checkpoint::read(&path).unwrap();
    let mut wrong = good.clone();
    let rates = &mut wrong.prime.as_mut().unwrap().rates;
    let last = rates.len() - 1;
    rates[last][31] ^= 1;
    std::fs::write(&path, wrong.encode()).unwrap();
    let e = run(&options(&export, &data, None, true)).unwrap_err();
    assert!(
        e.contains("checkpoint 8 rejected") && e.contains("rate of prime"),
        "{e}"
    );
    let mut wrong = good.clone();
    wrong.prime.as_mut().unwrap().head = [9; 32];
    std::fs::write(&path, wrong.encode()).unwrap();
    let e = run(&options(&export, &data, None, true)).unwrap_err();
    assert!(e.contains("checkpoint 8 rejected"), "{e}");
    std::fs::write(&path, good.encode()).unwrap();
    let s = run(&options(&export, &data, None, true)).unwrap();
    assert_eq!((s.end, s.resumed_from), (End::Complete, Some(8)));
    std::fs::remove_dir_all(base).unwrap();
    std::fs::remove_dir_all(data).unwrap();
}
