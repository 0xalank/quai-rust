//! Compare the structure of go-quai's live "newHeads" notifications with the
//! native RPC's payload for mainnet blocks, read-only. It subscribes to
//! "newHeads" on a go-quai WebSocket endpoint and collects COUNT
//! notifications, then builds `RPCMarshalWorkObject("v1")`
//! (`quai_rpc::native::json::work_object`) for the last COUNT blocks of a
//! QEX1 export. The live head is above the export, so the blocks differ:
//! the check compares member paths and JSON types (each array element
//! merged into "[]"), not values. Equal values come from the Go oracle
//! (fixtures/protocol/filters.json, tests/filters.rs).
//!
//!     cargo run --release -p quai-node --example live-new-heads -- \
//!         EXPORT ws://127.0.0.1:8200 COUNT [OUT.json]
//!
//! It sends only `quai_subscribe` and never a write method.
use futures_util::{SinkExt, StreamExt};
use quai_consensus::verifier::PINNED_SCHEDULE;
use quai_node::qex::{ExportReader, Manifest};
use quai_primitives::work::WorkObject;
use quai_rpc::view::BlockData;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use tokio_tungstenite::tungstenite::Message;

/// Member paths with their JSON types.
fn shape(v: &Value, path: &str, out: &mut BTreeSet<String>) {
    let kind = match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    out.insert(format!("{path}: {kind}"));
    match v {
        Value::Array(items) => {
            for i in items {
                shape(i, &format!("{path}[]"), out);
            }
        }
        Value::Object(m) => {
            for (k, x) in m {
                shape(x, &format!("{path}.{k}"), out);
            }
        }
        _ => {}
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: live-new-heads EXPORT WS_URL COUNT [OUT.json]");
        std::process::exit(2);
    }
    let export = Path::new(&args[0]);
    let count: usize = args[2].parse().expect("COUNT");
    let (ws, _) = tokio_tungstenite::connect_async(args[1].as_str())
        .await
        .expect("connect");
    let (mut sink, mut stream) = ws.split();
    let subscribe = json!({"jsonrpc":"2.0","id":1,"method":"quai_subscribe","params":["newHeads"]});
    sink.send(Message::Text(subscribe.to_string().into()))
        .await
        .unwrap();
    let (mut go, mut go_shape, mut answer) = (vec![], BTreeSet::new(), Value::Null);
    while go.len() < count {
        let Some(Ok(Message::Text(t))) = stream.next().await else {
            continue;
        };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["method"] == "quai_subscription" {
            let mut envelope = v.clone();
            envelope["params"]["subscription"] = Value::String("<id>".into());
            envelope["params"]["result"] = Value::Null;
            shape(&envelope, "", &mut go_shape);
            shape(&v["params"]["result"], "result", &mut go_shape);
            eprintln!(
                "go newHeads {}",
                v["params"]["result"]["woHeader"]["number"]
            );
            go.push(v["params"]["result"].clone());
        } else {
            answer = v;
        }
    }
    let _ = sink.send(Message::Close(None)).await;
    let manifest = Manifest::load(export).expect("manifest");
    let last = manifest.last_zone;
    let first = last + 1 - count as u64;
    let mut reader = ExportReader::starting_at(export, manifest, first, Some(last));
    let mut rust_shape = BTreeSet::new();
    let mut envelope = json!({"jsonrpc":"2.0","method":"quai_subscription","params":{"subscription":"<id>","result":null}});
    shape(&envelope, "", &mut rust_shape);
    let mut built = 0;
    while let Some(h) = reader.next_height().expect("export") {
        if h.zone_height < first {
            continue;
        }
        let z = h.zone();
        let object = WorkObject::decode_auxiliary(&z.block, PINNED_SCHEDULE).unwrap();
        let header = object.auxiliary_work_header(PINNED_SCHEDULE).unwrap();
        let uncles = object.uncle_headers(PINNED_SCHEDULE).unwrap();
        let tx_hashes = object
            .transactions()
            .iter()
            .map(|t| t.id(Some([0, 0])).unwrap())
            .collect();
        let etx_hashes = object
            .outbound_etxs()
            .iter()
            .map(|t| t.id(None).unwrap())
            .collect();
        let block = BlockData {
            hash: z.hash,
            number: h.zone_height,
            object,
            header,
            uncles,
            size: z.block.len() as u64,
            order: 2,
            tx_hashes,
            etx_hashes,
        };
        let payload = quai_rpc::native::json::work_object(&block);
        // The Rust frame: the native notification text.
        let frame: Value =
            serde_json::from_str(&quai_rpc::native::filters::notification("<id>", &payload))
                .unwrap();
        envelope = frame.clone();
        envelope["params"]["result"] = Value::Null;
        shape(&envelope, "", &mut rust_shape);
        shape(&payload, "result", &mut rust_shape);
        built += 1;
    }
    let only_go: Vec<&String> = go_shape.difference(&rust_shape).collect();
    let only_rust: Vec<&String> = rust_shape.difference(&go_shape).collect();
    let report = json!({
        "subscribe_answer_is_id": answer["result"].as_str().is_some_and(|s| s.starts_with("0x")),
        "go_heads": go.len(),
        "go_numbers": go.iter().map(|g| g["woHeader"]["number"].clone()).collect::<Vec<_>>(),
        "rust_blocks": built,
        "rust_range": [first, last],
        "paths_go": go_shape.len(),
        "paths_rust": rust_shape.len(),
        "only_go": only_go,
        "only_rust": only_rust,
    });
    if let Some(out) = args.get(3) {
        std::fs::write(out, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    println!("{report}");
}
