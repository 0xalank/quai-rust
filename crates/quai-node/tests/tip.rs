//! Tip mode on the private fixture (fixtures/protocol/canonical.json): two
//! branches of the reviewed private hierarchy chain 1337 that share their
//! first eight blocks. go-quai's coordinator (reference/canonical) took
//! branch `first_branch` to height 16, then the other branch from height 9
//! on, and moved the head to it at its block 15. The processor follows the
//! same heads; the reorg rolls back the executed state, the block store and
//! its indexes, then executes the new branch with every state root equal.
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
    }
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
    // A resumed processor starts at the new head (the view's snapshots
    // keep the store open until they are dropped).
    drop((view, _live, events));
    let p = Processor::open(
        chain,
        settings(&data, state, true),
        Some(caches),
        std::time::Instant::now(),
    )
    .unwrap();
    assert_eq!(p.zone_head(), Some((hb[16], 16)));
    drop(p);
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
