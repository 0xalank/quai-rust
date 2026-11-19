//! The transaction pool wired into the node as `quai-node rpc` wires it
//! (`cli::start_pool` over a checkpoint view, `rpc_view::ViewChain`), on a
//! state replay of mainnet zone 0..=8 (fixtures/mainnet/qex1-0-8). The
//! decisions themselves are checked against go-quai in tests/txpool.rs;
//! this checks that the node's view feeds the pool its head, state and
//! base fee and answers the pool methods.
use quai_consensus::pow::SharedCaches;
use quai_node::cli::{start_pool, TxpoolOptions};
use quai_node::log::{Level, Log};
use quai_node::replay::{self, Options};
use quai_node::rpc_view::ExportView;
use quai_rpc::native::dispatch;
use quai_rpc::view::ChainView;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

fn mainnet_slice() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mainnet/qex1-0-8")
}
fn answer(r: Result<Value, quai_rpc::native::RpcError>) -> Value {
    match r {
        Ok(v) => json!({ "result": v }),
        Err(e) => json!({ "error": e.to_value() }),
    }
}

#[test]
fn rpc_pool_answers_from_the_checkpoint_state() {
    let data = std::env::temp_dir().join(format!("quai-node-txpool-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let mut o = Options::new(mainnet_slice());
    o.jobs = 2;
    o.progress = Duration::from_secs(3600);
    o.state = true;
    o.datadir = Some(data.clone());
    let log = Log::new(Level::Warn);
    let caches = Arc::new(SharedCaches::new(2).unwrap());
    let summary = replay::run_with(&o, &log, Some(caches)).unwrap();
    assert_eq!(summary.highest_passed, Some(8), "{}", summary.line());
    let mut view = ExportView::open(&mainnet_slice(), &data, &log).unwrap();
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
    assert_eq!(view.head(), 8);
    // alice of the Go oracle: a key that never held funds on mainnet.
    let corpus: Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol/txpool.json")).unwrap();
    let first = &corpus["scenarios"][0]["steps"][1];
    assert_eq!(first["op"], "send");
    let raw = first["raw"].clone();
    let base_fee = {
        let head = view.block_by_number(8).unwrap().unwrap();
        quai_node::txpool::block::base_fee(&head)
    };
    let got = answer(dispatch(&view, "quai_sendRawTransaction", &json!([raw])));
    // 1 gwei at 21,000 gas plus one wei of value.
    let gwei = quai_node::txpool::big::from_u64(1_000_000_000);
    let want = if quai_node::txpool::big::cmp(&base_fee, &gwei).is_gt() {
        format!(
            "tx has incorrect or low gas price, have 1000000000, want {}",
            quai_node::txpool::big::decimal(&base_fee)
        )
    } else {
        "insufficient funds for gas * price + value".to_string()
    };
    assert_eq!(got, json!({"error": {"code": -32000, "message": want}}));
    let got = answer(dispatch(&view, "quai_sendRawTransaction", &json!(["0xff"])));
    assert_eq!(
        got["error"]["message"],
        "proto: cannot parse invalid wire-format data"
    );
    let alice = "0x001a2686be31271F5c7c590e0F0666A52Ee32345";
    let got = answer(dispatch(
        &view,
        "quai_getTransactionCount",
        &json!([alice, "pending"]),
    ));
    assert_eq!(got, json!({"result": "0x0"}));
    let got = answer(dispatch(&view, "txpool_status", &json!([])));
    assert_eq!(
        got,
        json!({"result": {"pending": "0x0", "queued": "0x0", "qi": "0x0"}})
    );
    assert_eq!(pool.rpc_stats(), (0, 0, 0));
    // The journal was written at start.
    assert!(data.join("txpool/transactions.journal").exists());
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    drop(rt);
    std::fs::remove_dir_all(&data).unwrap();
}
