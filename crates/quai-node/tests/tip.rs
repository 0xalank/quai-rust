//! Tip mode on the private fixture (fixtures/protocol/canonical.json): two
//! branches of the reviewed private hierarchy chain 1337 that share their
//! first eight blocks. go-quai's coordinator (reference/canonical) took
//! branch `first_branch` to height 16, then the other branch from height 9
//! on, and moved the head to it at its block 15. The processor follows the
//! same heads; the reorg rolls back the executed state, the block store and
//! its indexes, then executes the new branch with every state root equal.
//!
//! With a short tip (8 or 12 blocks above the final base instead of 240),
//! the tests also cross the base checkpoints and their state compaction,
//! stop the processor in the middle of a reorg, and offer a block that
//! forks below the base.
use quai_consensus::pow::SharedCaches;
use quai_node::cli::{start_pool, TxpoolOptions};
use quai_node::log::{Level, Log};
use quai_node::processor::{BlockSource, Next, Offer, Processor, Settings, SourceError};
use quai_node::qex::{Context, Height, RawRecord};
use quai_node::spec::Chain;
use quai_primitives::{decode_hex, Hash};
use quai_rpc::view::ChainView;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn doc() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../fixtures/protocol/canonical.json")).unwrap()
}
fn hash(v: &serde_json::Value) -> Hash {
    decode_hex(v.as_str().unwrap()).unwrap().try_into().unwrap()
}
/// Go's hashes of a branch, heights 0..=16.
fn go_hashes(doc: &serde_json::Value, branch: usize) -> Vec<Hash> {
    doc["canonical_events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["branch"] == branch && e["selected"] == true && e["head_number"] == 16)
        .unwrap()["canonical_hashes"]
        .as_array()
        .unwrap()
        .iter()
        .map(hash)
        .collect()
}
/// The heights of a branch: genesis, then 16 blocks.
fn heights(doc: &serde_json::Value, branch: usize) -> Vec<Height> {
    let b = &doc["branches"][branch];
    let go = go_hashes(doc, branch);
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
    out
}
/// The genesis only.
struct Genesis(Option<Height>);
impl BlockSource for Genesis {
    fn next(&mut self) -> Result<Next, SourceError> {
        Ok(self
            .0
            .take()
            .map_or(Next::End, |h| Next::Height(Box::new(h))))
    }
    fn target(&self) -> Option<u64> {
        Some(0)
    }
    fn receipts(&self) -> bool {
        false
    }
}
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("quai-node-tip-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
fn settings(data: &std::path::Path, state: bool, resume: bool) -> Settings {
    Settings {
        jobs: 2,
        progress: Duration::from_secs(3600),
        state,
        datadir: Some(data.to_path_buf()),
        resume,
        checkpoint_every: 1_000,
        commit_every: 1_000,
        store: true,
        shadow: false,
        archive: false,
        index_address_utxos: false,
        prune_blocks: None,
    }
}
/// Heights in order from a list.
struct Heights(std::collections::VecDeque<Height>);
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
fn checkpoints(data: &std::path::Path, state: bool) -> Vec<u64> {
    let chain = Chain::for_environment("local").unwrap();
    let dir = quai_node::processor::checkpoint_dir(data, &chain, state);
    let mut out: Vec<u64> = quai_node::checkpoint::list(&dir)
        .unwrap()
        .into_iter()
        .map(|(h, _)| h)
        .collect();
    out.sort();
    out
}

fn reorg_follows_go(state: bool) {
    let doc = doc();
    let first = doc["first_branch"].as_u64().unwrap() as usize;
    let second = 1 - first;
    let (a, b) = (heights(&doc, first), heights(&doc, second));
    let (ha, hb) = (go_hashes(&doc, first), go_hashes(&doc, second));
    assert_eq!(ha[..9], hb[..9], "the branches share heights 0..=8");
    let caches = Arc::new(SharedCaches::new(2).unwrap());
    let log = Log::new(Level::Warn);
    let data = scratch(if state { "state" } else { "stateless" });
    let chain = Chain::for_environment("local").unwrap();
    let mut p = Processor::open(
        chain.clone(),
        settings(&data, state, false),
        Some(caches.clone()),
        std::time::Instant::now(),
    )
    .unwrap();
    p.run(&mut Genesis(Some(a[0].clone())), &log).unwrap();
    let metrics = Arc::new(quai_node::metrics::Metrics::new(0));
    p.set_metrics(metrics.clone());
    let (view, _live) = p.live_view().unwrap();
    let mut events = view.head_events().unwrap();
    // The base, final, is 8 canonical blocks below the head: the reorg at
    // height 15 returns to it.
    p.set_tip_keep(8);
    p.enable_tip().unwrap();
    // A child before its parent waits for it; the genesis is known.
    assert_eq!(
        p.offer(a[2].clone(), &log).unwrap(),
        Offer::NeedParent(ha[1])
    );
    assert_eq!(p.offer(a[0].clone(), &log).unwrap(), Offer::Known);
    let mut go = doc["canonical_events"].as_array().unwrap().iter();
    for (h, i) in a.iter().skip(1).zip(0..) {
        let Offer::Accepted { head, removed, .. } = p.offer(h.clone(), &log).unwrap() else {
            panic!("block {i} not accepted")
        };
        let e = go.next().unwrap();
        assert_eq!(head.0, hash(&e["head"]), "first branch block {i}");
        assert!(removed.is_empty());
    }
    // The shared blocks are known; the second branch reorganizes at its
    // block 15 (index 14), as go-quai's coordinator did.
    for h in &b[1..9] {
        assert_eq!(p.offer(h.clone(), &log).unwrap(), Offer::Known);
    }
    let mut reorgs = 0;
    for (h, i) in b.iter().skip(9).zip(8..) {
        let Offer::Accepted { head, removed, .. } = p.offer(h.clone(), &log).unwrap() else {
            panic!("second branch block {i} not accepted")
        };
        let e = go.next().unwrap();
        assert_eq!(head.0, hash(&e["head"]), "second branch block {i}");
        if i == 14 {
            assert_eq!(removed.len(), 8);
            assert_eq!(removed[0], (ha[16], 16));
            assert_eq!(removed[7], (ha[9], 9));
            reorgs += 1;
        } else {
            assert!(removed.is_empty(), "block {i}");
        }
    }
    assert_eq!(reorgs, 1);
    assert_eq!(p.tip().unwrap().head(), (hb[16], 16));
    assert_eq!(metrics.reorgs.load(std::sync::atomic::Ordering::Relaxed), 1);
    // The view and its head events follow.
    assert_eq!(view.head(), 16);
    let mut seen = vec![];
    while let Ok(h) = events.try_recv() {
        seen.push(h);
    }
    assert_eq!(seen.last(), Some(&hb[16]));
    assert!(seen.contains(&ha[16]));
    for n in 0..=16 {
        let block = view.block_by_number(n).unwrap().unwrap();
        assert_eq!(block.hash, hb[n as usize], "height {n}");
    }
    // Transactions of the removed blocks are no longer indexed there.
    let removed_block = quai_consensus::history::Record::decode(&a[12].zone().block).unwrap();
    for tx in removed_block.object().transactions() {
        let id = tx.id(Some(chain.location())).unwrap();
        if let Some(pos) = view.transaction(&id).unwrap() {
            assert_eq!(
                view.block_by_number(pos.block_number)
                    .unwrap()
                    .unwrap()
                    .hash,
                pos.block_hash
            );
            assert_eq!(hb[pos.block_number as usize], pos.block_hash);
        }
    }
    let s = p.finish(&log).unwrap();
    assert_eq!(s.highest_passed, Some(16));
    if state {
        // Every block of both branches executed with all roots equal.
        assert!(s.state_blocks.unwrap() >= 16);
    }
    // A resumed processor starts at the final base, 8 below the head
    // (the view's snapshots keep the store open until they are dropped),
    // and continues on the new branch.
    drop((view, _live, events));
    let mut p = Processor::open(
        chain,
        settings(&data, state, true),
        Some(caches),
        std::time::Instant::now(),
    )
    .unwrap();
    assert_eq!(p.zone_head(), Some((hb[8], 8)));
    p.run(&mut Heights(b[8..].iter().cloned().collect()), &log)
        .unwrap();
    assert_eq!(p.zone_head(), Some((hb[16], 16)));
    let s = p.finish(&log).unwrap();
    assert!(s.failure.is_none(), "{:?}", s.failure);
    std::fs::remove_dir_all(data).unwrap();
}

#[test]
fn reorgs_follow_go_with_state() {
    reorg_follows_go(true);
}
#[test]
fn reorgs_follow_go_without_state() {
    reorg_follows_go(false);
}

/// Offer branch A to its head, then branch B; the reorg at B's block 15
/// returns to the common ancestor at height 8.
fn follow_both(
    p: &mut Processor,
    a: &[Height],
    b: &[Height],
    hb: &[quai_primitives::Hash],
    log: &Log,
) -> Result<(), String> {
    for h in &a[1..] {
        assert!(
            matches!(p.offer(h.clone(), log)?, Offer::Accepted { .. }),
            "A {}",
            h.zone_height
        );
    }
    for h in &b[9..] {
        match p.offer(h.clone(), log)? {
            Offer::Accepted { head, removed, .. } => {
                if h.zone_height == 15 {
                    assert_eq!(removed.len(), 8);
                    assert_eq!(head, (hb[15], 15));
                }
            }
            other => panic!("B {}: {other:?}", h.zone_height),
        }
    }
    Ok(())
}
fn opened(data: &std::path::Path, state: bool, resume: bool, every: u64) -> Processor {
    let mut s = settings(data, state, resume);
    s.checkpoint_every = every;
    s.commit_every = 2;
    Processor::open(
        Chain::for_environment("local").unwrap(),
        s,
        Some(Arc::new(SharedCaches::new(2).unwrap())),
        std::time::Instant::now(),
    )
    .unwrap()
}

/// Every canonical tip block commits its own state version; the base
/// checkpoints compact the state and execute the blocks above the base
/// again. The reorg to height 8 crosses the commits of 9..=16 and, with a
/// 12-block tip, uses a version re-created after the compaction at the base
/// 4; with an 8-block tip it returns to the base itself (compacted at 8).
fn reorg_across_commits(state: bool, keep: u64, compacted: u64) {
    let doc = doc();
    let first = doc["first_branch"].as_u64().unwrap() as usize;
    let (a, b) = (heights(&doc, first), heights(&doc, 1 - first));
    let hb = go_hashes(&doc, 1 - first);
    let log = Log::new(Level::Warn);
    let data = scratch(&format!("commits-{state}-{keep}"));
    let mut p = opened(&data, state, false, 4);
    p.run(&mut Genesis(Some(a[0].clone())), &log).unwrap();
    p.set_tip_keep(keep);
    p.enable_tip().unwrap();
    follow_both(&mut p, &a, &b, &hb, &log).unwrap();
    assert_eq!(p.tip().unwrap().head(), (hb[16], 16));
    assert_eq!(p.tip().unwrap().base().1, compacted.max(16 - keep));
    // The base checkpoints of branch A; with state, compacted files.
    let cps = checkpoints(&data, state);
    assert!(cps.contains(&compacted), "{cps:?}");
    if state {
        let chain = Chain::for_environment("local").unwrap();
        let dir = quai_node::processor::checkpoint_dir(&data, &chain, true);
        assert!(dir.join(format!("state-{compacted:010}.redb")).is_file());
    }
    let s = p.finish(&log).unwrap();
    assert!(s.failure.is_none());
    if state {
        assert!(s.state_blocks.unwrap() >= 16);
    }
    // The run resumes at the final base and checks branch B to its head.
    let base = *checkpoints(&data, state).last().unwrap();
    let mut p = opened(&data, state, true, 4);
    assert_eq!(p.zone_head(), Some((hb[base as usize], base)));
    p.run(
        &mut Heights(b[base as usize..].iter().cloned().collect()),
        &log,
    )
    .unwrap();
    assert_eq!(p.zone_head(), Some((hb[16], 16)));
    assert!(p.finish(&log).unwrap().failure.is_none());
    std::fs::remove_dir_all(data).unwrap();
}
#[test]
fn reorg_across_state_commits_and_a_compaction_below_it() {
    reorg_across_commits(true, 12, 4);
}
#[test]
fn reorg_to_a_compacted_base_with_state() {
    reorg_across_commits(true, 8, 8);
}
#[test]
fn reorg_to_a_base_checkpoint_without_state() {
    reorg_across_commits(false, 8, 8);
}

/// The processor stops in the middle of a reorg (after the rollback, or
/// after some blocks of the new branch); the next start resumes at the
/// last base checkpoint and checks the new branch to its head.
fn crash_in_reorg(state: bool, point: &str, at: u64) {
    let doc = doc();
    let first = doc["first_branch"].as_u64().unwrap() as usize;
    let (a, b) = (heights(&doc, first), heights(&doc, 1 - first));
    let hb = go_hashes(&doc, 1 - first);
    let log = Log::new(Level::Error);
    let data = scratch(&format!("crash-{state}-{point}"));
    let chain = Chain::for_environment("local").unwrap();
    let dir = quai_node::processor::checkpoint_dir(&data, &chain, state);
    let mut p = opened(&data, state, false, 4);
    p.run(&mut Genesis(Some(a[0].clone())), &log).unwrap();
    p.set_tip_keep(8);
    p.enable_tip().unwrap();
    quai_node::store::inject_fault(&dir, point, at);
    let crashed = follow_both(&mut p, &a, &b, &hb, &log);
    let e = crashed.expect_err("the injected fault stops the reorg");
    assert!(e.contains("test fault"), "{e}");
    // A crash: no finish, no final checkpoint.
    drop(p);
    let base = *checkpoints(&data, state).last().unwrap();
    assert_eq!(base, 8);
    let mut p = opened(&data, state, true, 4);
    assert_eq!(p.zone_head(), Some((hb[8], 8)));
    p.run(&mut Heights(b[8..].iter().cloned().collect()), &log)
        .unwrap();
    assert_eq!(p.zone_head(), Some((hb[16], 16)));
    let s = p.finish(&log).unwrap();
    assert!(s.failure.is_none(), "{:?}", s.failure);
    if state {
        assert!(s.state_blocks.unwrap() >= 16);
    }
    std::fs::remove_dir_all(data).unwrap();
}
#[test]
fn a_crash_after_the_reorg_rollback_resumes_with_state() {
    crash_in_reorg(true, "reorg-rolled-back", 8);
}
#[test]
fn a_crash_inside_the_new_branch_resumes_with_state() {
    crash_in_reorg(true, "reorg-applied", 11);
}
#[test]
fn a_crash_inside_the_new_branch_resumes_without_state() {
    crash_in_reorg(false, "reorg-applied", 11);
}

/// A block whose parent is at or below the final base without being it is
/// refused without stopping the node, and the head stays.
#[test]
fn a_block_forking_below_the_base_is_refused() {
    let doc = doc();
    let first = doc["first_branch"].as_u64().unwrap() as usize;
    let (a, b) = (heights(&doc, first), heights(&doc, 1 - first));
    let (ha, hb) = (go_hashes(&doc, first), go_hashes(&doc, 1 - first));
    let log = Log::new(Level::Error);
    let data = scratch("below-base");
    let mut p = opened(&data, true, false, 1_000);
    p.run(&mut Genesis(Some(a[0].clone())), &log).unwrap();
    p.set_tip_keep(4);
    p.enable_tip().unwrap();
    for h in &a[1..] {
        assert!(matches!(
            p.offer(h.clone(), &log).unwrap(),
            Offer::Accepted { .. }
        ));
    }
    assert_eq!(p.tip().unwrap().base(), (ha[12], 12));
    assert_eq!(p.offer(b[11].clone(), &log).unwrap(), Offer::Known);
    assert_eq!(
        p.offer(b[14].clone(), &log).unwrap(),
        Offer::NeedParent(hb[13])
    );
    assert_eq!(p.offer(b[13].clone(), &log).unwrap(), Offer::BelowBase);
    assert_eq!(p.tip().unwrap().head(), (ha[16], 16));
    assert_eq!(p.tip().unwrap().canonical_at(14), Some(ha[14]));
    drop(p);
    std::fs::remove_dir_all(data).unwrap();
}

/// A shadow run without the block store (`run --no-store`): the head
/// choice, the reorg and the base checkpoints work without it.
#[test]
fn tip_mode_without_a_block_store() {
    let doc = doc();
    let first = doc["first_branch"].as_u64().unwrap() as usize;
    let (a, b) = (heights(&doc, first), heights(&doc, 1 - first));
    let hb = go_hashes(&doc, 1 - first);
    let log = Log::new(Level::Error);
    let data = scratch("no-store");
    let mut s = settings(&data, false, false);
    s.store = false;
    s.checkpoint_every = 4;
    let mut p = Processor::open(
        Chain::for_environment("local").unwrap(),
        s,
        Some(Arc::new(SharedCaches::new(2).unwrap())),
        std::time::Instant::now(),
    )
    .unwrap();
    p.run(&mut Genesis(Some(a[0].clone())), &log).unwrap();
    assert!(p.live_view().is_err());
    p.set_tip_keep(8);
    p.enable_tip().unwrap();
    follow_both(&mut p, &a, &b, &hb, &log).unwrap();
    assert_eq!(p.tip().unwrap().head(), (hb[16], 16));
    assert!(p.finish(&log).unwrap().failure.is_none());
    assert_eq!(checkpoints(&data, false).last(), Some(&8));
    std::fs::remove_dir_all(data).unwrap();
}

/// The transaction pool on the live view, started as `quai-node run` starts
/// it, follows the heads through the reorg. It starts at the head of the
/// first branch and remembers that block with its ancestors. After the
/// reorg the block store no longer holds the first branch; the pool's reset
/// walks back through the remembered blocks to the common ancestor (go-quai
/// reads them from its database) and reaches the new head without stopping.
#[test]
fn pool_follows_the_reorg() {
    let doc = doc();
    let first = doc["first_branch"].as_u64().unwrap() as usize;
    let (a, b) = (heights(&doc, first), heights(&doc, 1 - first));
    let (ha, hb) = (go_hashes(&doc, first), go_hashes(&doc, 1 - first));
    let log = Log::new(Level::Warn);
    let data = scratch("pool");
    let mut p = Processor::open(
        Chain::for_environment("local").unwrap(),
        settings(&data, true, false),
        Some(Arc::new(SharedCaches::new(2).unwrap())),
        std::time::Instant::now(),
    )
    .unwrap();
    p.run(&mut Genesis(Some(a[0].clone())), &log).unwrap();
    let (mut view, live) = p.live_view().unwrap();
    p.enable_tip().unwrap();
    for h in &a[1..] {
        assert!(matches!(
            p.offer(h.clone(), &log).unwrap(),
            Offer::Accepted { .. }
        ));
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let pool = start_pool(
        &mut view,
        &TxpoolOptions::default(),
        &data,
        rt.handle(),
        stop.clone(),
        log,
    )
    .unwrap()
    .expect("a pool over a view with state");
    assert_eq!(pool.reset_state(), (Some(ha[16]), 1, 0));
    let wait = |want: Hash| {
        for _ in 0..3000 {
            if pool.reset_state().0 == Some(want) {
                return pool.reset_state();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "the pool did not reset to {want:?}: {:?}",
            pool.reset_state()
        );
    };
    // The second branch takes the head at its block 15.
    for h in &b[9..16] {
        p.offer(h.clone(), &log).unwrap();
    }
    assert_eq!(p.tip().unwrap().head(), (hb[15], 15));
    assert!(view.block_by_hash(&ha[15]).unwrap().is_none());
    let (_, _, stops) = wait(hb[15]);
    assert_eq!(stops, 0, "the reorg reset stopped early");
    p.offer(b[16].clone(), &log).unwrap();
    assert_eq!(wait(hb[16]).2, 0);
    stop.store(true, Ordering::Relaxed);
    drop(rt);
    drop((pool, view, live));
    p.finish(&log).unwrap();
    std::fs::remove_dir_all(data).unwrap();
}

/// The catch-up by number of tip mode (`tip::catch_up_from`), which a node
/// far behind the peers runs after a sleep or a network change. On branch
/// A at 12 while the peers hold branch B, it starts at 4 (8 below the head,
/// shared), takes B's blocks 9..=16 and moves the head to B at its block 15,
/// as go-quai's coordinator did. A source whose block at the start is not
/// this node's stops at once.
#[test]
fn a_catch_up_by_number_takes_the_peers_branch() {
    let doc = doc();
    let first = doc["first_branch"].as_u64().unwrap() as usize;
    let (a, b) = (heights(&doc, first), heights(&doc, 1 - first));
    let (ha, hb) = (go_hashes(&doc, first), go_hashes(&doc, 1 - first));
    let log = Log::new(Level::Warn);
    let data = scratch("catch-up");
    let mut p = opened(&data, false, false, 1_000);
    p.run(&mut Genesis(Some(a[0].clone())), &log).unwrap();
    p.enable_tip().unwrap();
    for h in &a[1..=12] {
        assert!(matches!(
            p.offer(h.clone(), &log).unwrap(),
            Offer::Accepted { .. }
        ));
    }
    assert_eq!(p.tip().unwrap().head(), (ha[12], 12));
    let (start, heads) = quai_node::tip::catch_up_start(&p).unwrap();
    assert_eq!(start, (ha[4], 4));
    assert_eq!(heads[2], (ha[4], 4));
    let mut detector = quai_node::divergence::Detector::new(None, None, log);
    let mut stats = quai_node::tip::TipStats::default();
    let stop = AtomicBool::new(false);
    // Another block at the start: nothing is offered.
    let mut other = a[4].clone();
    other.records[2].as_mut().unwrap().hash = [9; 32];
    let mut source = Heights(vec![other, a[5].clone()].into());
    let done = quai_node::tip::catch_up_from(
        &mut p,
        &mut source,
        start,
        &stop,
        &log,
        &mut detector,
        &mut stats,
        None,
    )
    .unwrap();
    assert_eq!((done.offered, done.accepted), (0, 0));
    assert!(done.end.contains("not this node's"), "{}", done.end);
    assert_eq!(p.tip().unwrap().head(), (ha[12], 12));
    // The peers' chain: branch B from the shared block 4.
    let mut source = Heights(b[4..].iter().cloned().collect());
    let done = quai_node::tip::catch_up_from(
        &mut p,
        &mut source,
        start,
        &stop,
        &log,
        &mut detector,
        &mut stats,
        None,
    )
    .unwrap();
    // 5..=8 are known; B's 9..=16 are new, and the head moves at 15.
    assert_eq!(done.offered, 12);
    assert_eq!(done.accepted, 8);
    assert_eq!(done.reorgs, 1);
    assert_eq!(done.rejected, None);
    assert_eq!(done.end, "the source ended");
    assert_eq!(p.tip().unwrap().head(), (hb[16], 16));
    assert_eq!(
        (stats.accepted, stats.reorgs, stats.reorged_blocks),
        (8, 1, 4)
    );
    let s = p.finish(&log).unwrap();
    assert!(s.failure.is_none(), "{:?}", s.failure);
    std::fs::remove_dir_all(data).unwrap();
}

/// The RPC's appended-block events (go-quai's `ChainEvent`) in tip mode:
/// every checked block, canonical or not, in the order it was offered; a
/// block the head choice did not take is still found by hash
/// (`GetBlockOrCandidateByHash`). With state, the rolling fee window
/// (`txpool_getRollingFeeInfo`) appears once a block with a Quai or Qi
/// transaction executes.
fn appended_blocks_reach_the_rpc(state: bool) {
    let doc = doc();
    let first = doc["first_branch"].as_u64().unwrap() as usize;
    let (a, b) = (heights(&doc, first), heights(&doc, 1 - first));
    let (ha, hb) = (go_hashes(&doc, first), go_hashes(&doc, 1 - first));
    let log = Log::new(Level::Warn);
    let data = scratch(&format!("appended-{state}"));
    let mut p = opened(&data, state, false, 1_000);
    p.run(&mut Genesis(Some(a[0].clone())), &log).unwrap();
    let (view, live) = p.live_view().unwrap();
    let mut chain = view.chain_events().unwrap();
    assert_eq!(view.rolling_fee_info(), None);
    p.enable_tip().unwrap();
    for h in a[1..=16].iter().chain(&b[9..=12]) {
        assert!(matches!(
            p.offer(h.clone(), &log).unwrap(),
            Offer::Accepted { .. }
        ));
    }
    let mut seen = vec![];
    while let Ok(h) = chain.try_recv() {
        seen.push(h);
    }
    let want: Vec<Hash> = ha[1..=16].iter().chain(&hb[9..=12]).copied().collect();
    assert_eq!(seen, want);
    // B's blocks 9..=12 are not canonical; the view finds them by hash.
    assert_eq!(p.tip().unwrap().head(), (ha[16], 16));
    assert_eq!(view.block_by_number(10).unwrap().unwrap().hash, ha[10]);
    let side = view.block_or_candidate(&hb[10]).unwrap().unwrap();
    assert_eq!((side.hash, side.number), (hb[10], 10));
    let pinned = view.pinned().unwrap();
    assert_eq!(
        pinned.block_or_candidate(&hb[11]).unwrap().unwrap().hash,
        hb[11]
    );
    if state {
        let with_fees = a[1..=16].iter().any(|h| {
            let r = quai_consensus::history::Record::decode(&h.zone().block).unwrap();
            r.object()
                .transactions()
                .iter()
                .any(|t| !matches!(t.kind(), quai_primitives::transaction::Kind::External(_)))
        });
        assert_eq!(view.rolling_fee_info().is_some(), with_fees);
    } else {
        assert_eq!(view.rolling_fee_info(), None);
    }
    drop((view, live, chain, pinned));
    let s = p.finish(&log).unwrap();
    assert!(s.failure.is_none(), "{:?}", s.failure);
    std::fs::remove_dir_all(data).unwrap();
}
#[test]
fn appended_blocks_reach_the_rpc_with_state() {
    appended_blocks_reach_the_rpc(true);
}
#[test]
fn appended_blocks_reach_the_rpc_without_state() {
    appended_blocks_reach_the_rpc(false);
}

/// Answers to the peers' requests from the block store (`serve.rs`): zone
/// hashes by number (the genesis at 0), a zone block by hash or number, ten
/// blocks from a hash or none (go-quai `GetWorkObjectsFrom`), a block the
/// head choice did not take, header views, the prime and region views of
/// coincident blocks, and go-quai's "not found" for other locations and for
/// what the store does not hold.
#[test]
fn the_block_store_answers_peer_requests_as_go_quai() {
    use prost::Message;
    use quai_network::public::serve::Store;
    use quai_primitives::schema::{
        common::{ProtoHash, ProtoLocation},
        quaiprotocol::{
            quai_request_message as rq, quai_response_message as rs, QuaiRequestMessage,
        },
    };
    let doc = doc();
    let first = doc["first_branch"].as_u64().unwrap() as usize;
    let (a, b) = (heights(&doc, first), heights(&doc, 1 - first));
    let (ha, hb) = (go_hashes(&doc, first), go_hashes(&doc, 1 - first));
    let log = Log::new(Level::Warn);
    let data = scratch("serve");
    let mut p = opened(&data, false, false, 1_000);
    p.run(&mut Genesis(Some(a[0].clone())), &log).unwrap();
    let (view, live) = p.live_view().unwrap();
    p.enable_tip().unwrap();
    for h in a[1..=16].iter().chain(&b[9..=10]) {
        p.offer(h.clone(), &log).unwrap();
    }
    // The tip publishes its head to the view when the processor finishes or
    // checkpoints; publish it here as a run does.
    let s = p.finish(&log).unwrap();
    assert!(s.failure.is_none(), "{:?}", s.failure);
    let chain = Chain::for_environment("local").unwrap();
    let server = quai_node::serve::StoreServer::new(view.share(), ha[0], chain.location());
    let ask = |location: Vec<u8>, data: rq::Data, kind: rq::Request| {
        server.answer(&QuaiRequestMessage {
            id: 1,
            location: Some(ProtoLocation { value: location }),
            data: Some(data),
            request: Some(kind),
        })
    };
    let zone = chain.location().to_vec();
    let num = |n: u64| {
        rq::Data::Number(if n == 0 {
            vec![]
        } else {
            n.to_be_bytes().to_vec()
        })
    };
    let by_hash = |h: Hash| rq::Data::Hash(ProtoHash { value: h.to_vec() });
    let hash_of = |wo: &quai_primitives::schema::block::ProtoWorkObject| -> Hash {
        let raw = wo.encode_to_vec();
        quai_consensus::history::Record::decode(&raw).unwrap().id()
    };
    for (n, want) in [(0u64, ha[0].to_vec()), (5, ha[5].to_vec()), (99, vec![])] {
        let Some(rs::Response::BlockHash(h)) = ask(
            zone.clone(),
            num(n),
            rq::Request::BlockHash(Default::default()),
        ) else {
            panic!("no block hash answer for {n}")
        };
        assert_eq!(h.value, want, "hash at {n}");
    }
    // A block by hash and by number.
    for data in [by_hash(ha[3]), num(3)] {
        let Some(rs::Response::WorkObjectBlockView(v)) = ask(
            zone.clone(),
            data,
            rq::Request::WorkObjectBlock(Default::default()),
        ) else {
            panic!("no block answer")
        };
        assert_eq!(hash_of(v.work_object.as_ref().unwrap()), ha[3]);
    }
    // Ten blocks from a hash: 2..=11.
    let Some(rs::Response::WorkObjectBlocksView(v)) = ask(
        zone.clone(),
        by_hash(ha[2]),
        rq::Request::WorkObjectBlocks(Default::default()),
    ) else {
        panic!("no blocks answer")
    };
    let got: Vec<Hash> = v
        .work_objects
        .iter()
        .map(|b| hash_of(b.work_object.as_ref().unwrap()))
        .collect();
    assert_eq!(got, ha[2..=11].to_vec());
    // From 10 only seven blocks follow: none, as go-quai answers.
    let Some(rs::Response::WorkObjectBlocksView(v)) = ask(
        zone.clone(),
        by_hash(ha[10]),
        rq::Request::WorkObjectBlocks(Default::default()),
    ) else {
        panic!("no blocks answer")
    };
    assert!(v.work_objects.is_empty());
    // A side-branch block the node appended is found by hash.
    let Some(rs::Response::WorkObjectBlockView(v)) = ask(
        zone.clone(),
        by_hash(hb[10]),
        rq::Request::WorkObjectBlock(Default::default()),
    ) else {
        panic!("no block answer")
    };
    assert_eq!(hash_of(v.work_object.as_ref().unwrap()), hb[10]);
    // A header view: the block without transactions, manifest and
    // interlink hashes (present and empty, as go-quai encodes them).
    let Some(rs::Response::WorkObjectHeaderView(v)) = ask(
        zone.clone(),
        by_hash(ha[3]),
        rq::Request::WorkObjectHeader(Default::default()),
    ) else {
        panic!("no header answer")
    };
    let wo = v.work_object.unwrap();
    let body = wo.wo_body.as_ref().unwrap();
    assert_eq!(body.transactions, Some(Default::default()));
    assert_eq!(body.manifest, Some(Default::default()));
    assert_eq!(body.interlink_hashes, Some(Default::default()));
    assert_eq!(wo.wo_header, {
        let raw = &a[3].zone().block;
        quai_primitives::schema::block::ProtoWorkObject::decode(raw.as_slice())
            .unwrap()
            .wo_header
    });
    // The prime and region chains: every coincident view by number and by
    // hash, as the height carried it.
    let locations = [vec![], vec![zone[0]], zone.clone()];
    let mut dominant = 0;
    for h in &a[1..=16] {
        for ctx in 0..2 {
            let Some(r) = h.records[ctx].as_ref() else {
                continue;
            };
            dominant += 1;
            let Some(rs::Response::BlockHash(got)) = ask(
                locations[ctx].clone(),
                num(r.number),
                rq::Request::BlockHash(Default::default()),
            ) else {
                panic!("no hash answer")
            };
            assert_eq!(
                got.value,
                r.hash.to_vec(),
                "context {ctx} number {}",
                r.number
            );
            let Some(rs::Response::WorkObjectBlockView(v)) = ask(
                locations[ctx].clone(),
                by_hash(r.hash),
                rq::Request::WorkObjectBlock(Default::default()),
            ) else {
                panic!("no block answer")
            };
            assert_eq!(v.work_object.unwrap().encode_to_vec(), {
                quai_primitives::schema::block::ProtoWorkObject::decode(r.block.as_slice())
                    .unwrap()
                    .encode_to_vec()
            });
        }
    }
    assert!(dominant > 0, "the fixture has coincident blocks");
    // Unknown blocks and other locations: not found.
    for (location, data, kind) in [
        (
            zone.clone(),
            by_hash([7; 32]),
            rq::Request::WorkObjectBlock(Default::default()),
        ),
        (
            vec![0],
            by_hash([7; 32]),
            rq::Request::WorkObjectHeader(Default::default()),
        ),
        (vec![1], num(1), rq::Request::BlockHash(Default::default())),
        (
            vec![0, 1],
            by_hash(ha[3]),
            rq::Request::WorkObjectBlock(Default::default()),
        ),
    ] {
        let answer = ask(location, data.clone(), kind.clone());
        let not_found = quai_network::public::serve::NotFound.answer(&QuaiRequestMessage {
            id: 1,
            location: None,
            data: Some(data),
            request: Some(kind),
        });
        assert_eq!(answer, not_found);
    }
    drop((server, view, live));
    std::fs::remove_dir_all(data).unwrap();
}

/// Another Rust node fetches every context from this one (`serve.rs` over
/// the network): node A holds the fixture chain in its block store and
/// answers from it; node B uses the sync's request functions: A's genesis
/// and zone head (`peer_head`), ten zone blocks by number (`fetch_range`,
/// one WorkObjectBlocks batch), and each prime and region record by number
/// and by hash (`peer_hash_at`, `peer_block`), every one equal to the
/// fixture's. (The fixture's prime and region chains end inside their first
/// ten-block batch, which go-quai, and so the sync scheduler, takes only
/// whole; a mainnet sync hands that last batch to gossip.)
#[test]
fn a_node_fetches_every_context_from_another_rust_node() {
    use prost::Message;
    use quai_network::public::{Config as NetConfig, DialPolicy, Event, PublicNode};
    let doc = doc();
    let first = doc["first_branch"].as_u64().unwrap() as usize;
    let a = heights(&doc, first);
    let ha = go_hashes(&doc, first);
    let log = Log::new(Level::Warn);
    let data = scratch("fetch-from-rust");
    let mut p = opened(&data, false, false, 1_000);
    p.run(&mut Heights(a.iter().cloned().collect()), &log)
        .unwrap();
    let (view, live) = p.live_view().unwrap();
    let chain = Chain::for_environment("local").unwrap();
    assert_eq!(chain.location(), [0, 0]);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    // Node A: listens on loopback and answers from its store.
    let mut config = NetConfig::read_only(DialPolicy::Loopback);
    config.serve = true;
    config.listen = vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()];
    let server = {
        let _guard = rt.enter();
        std::sync::Arc::new(
            PublicNode::launch(config, libp2p::identity::Keypair::generate_ed25519()).unwrap(),
        )
    };
    server.serve_from(std::sync::Arc::new(quai_node::serve::StoreServer::new(
        view.share(),
        ha[0],
        chain.location(),
    )));
    let decode = |b: &[u8]| quai_primitives::schema::block::ProtoWorkObject::decode(b).unwrap();
    rt.block_on(async {
        let listen = loop {
            if let Event::ListenAddr(a) = server.next().await.unwrap() {
                break a;
            }
        };
        // Node B.
        let client = std::sync::Arc::new(
            PublicNode::launch(
                NetConfig::read_only(DialPolicy::Loopback),
                libp2p::identity::Keypair::generate_ed25519(),
            )
            .unwrap(),
        );
        client
            .dial(listen.with_p2p(server.id).unwrap())
            .await
            .unwrap();
        loop {
            if let Event::Connected { peer, .. } = client.next().await.unwrap() {
                if peer == server.id {
                    break;
                }
            }
        }
        let peer = server.id;
        // The genesis check and the zone head.
        let head = quai_node::peers::peer_head(&client, peer, ha[0])
            .await
            .unwrap();
        assert_eq!(head, 16);
        // Zone 0..=9: one WorkObjectBlocks batch.
        let zone = quai_node::peers::fetch_range(&client, &[peer], 2, 0, 9, log)
            .await
            .unwrap();
        assert_eq!(zone.len(), 10);
        for (b, want) in zone.iter().zip(&a) {
            let w = want.zone();
            assert_eq!((b.number, b.hash), (w.number, w.hash));
            assert_eq!(decode(&b.raw), decode(&w.block), "zone {}", w.number);
        }
        // Every prime and region record, by number and by hash.
        let mut dominant = 0;
        for h in &a[1..] {
            for ctx in 0..2 {
                let Some(r) = h.records[ctx].as_ref() else {
                    continue;
                };
                let hash = quai_node::peers::peer_hash_at(&client, peer, ctx, r.number)
                    .await
                    .unwrap();
                assert_eq!(hash, Some(r.hash), "context {ctx} number {}", r.number);
                let b = quai_node::peers::peer_block(&client, peer, ctx, r.hash)
                    .await
                    .unwrap();
                assert_eq!((b.number, b.ctx), (r.number, ctx));
                assert_eq!(
                    decode(&b.raw),
                    decode(&r.block),
                    "context {ctx} number {}",
                    r.number
                );
                dominant += 1;
            }
        }
        assert!(dominant >= 6, "{dominant} prime and region records");
        // Past the head: no block.
        let none = quai_node::peers::peer_hash_at(&client, peer, 2, 17)
            .await
            .unwrap();
        assert_eq!(none, None);
    });
    drop(server);
    rt.shutdown_timeout(Duration::from_secs(5));
    drop((view, live));
    std::fs::remove_dir_all(data).unwrap();
}
