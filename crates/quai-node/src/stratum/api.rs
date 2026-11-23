//! HTTP and WebSocket Pool Dashboard API server matching `go-quai/stratum/api.go`.
//!
//! Endpoints:
//! - `GET /health`
//! - `GET /api/stats`, `/api/miners`, `/api/blocks`, `/api/payments`
//! - `GET /api/pool/stats`, `/api/pool/blocks`, `/api/pool/shares`, `/api/pool/workers`
//! - `GET /api/miner/{address}/stats`, `/api/miner/{address}/workers`
//! - `GET /api/node/info`
//! - `GET /api/ws` (1-second live JSON stats stream, origin-checked, max 128 clients)
use super::stats::PoolStats;
use crate::miner::Miner;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

const MAX_WS_CLIENTS: usize = 128;
const MAX_WS_READ_BYTES: usize = 4096;

#[derive(Clone)]
struct ApiState {
    stats: Arc<PoolStats>,
    miner: Arc<Miner>,
    ws_origins: Vec<String>,
    ws_permits: Arc<Semaphore>,
    stop: Arc<AtomicBool>,
}

pub async fn serve_dashboard(
    listener: TcpListener,
    stats: Arc<PoolStats>,
    miner: Arc<Miner>,
    ws_origins: Vec<String>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let state = ApiState {
        stats,
        miner,
        ws_origins,
        ws_permits: Arc::new(Semaphore::new(MAX_WS_CLIENTS)),
        stop: stop.clone(),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/stats", get(pool_stats))
        .route("/api/miners", get(miners))
        .route("/api/blocks", get(pool_blocks))
        .route("/api/payments", get(payments))
        .route("/api/pool/stats", get(pool_stats))
        .route("/api/pool/blocks", get(pool_blocks))
        .route("/api/pool/shares", get(pool_shares))
        .route("/api/pool/workers", get(pool_workers))
        .route("/api/miner/{address}/stats", get(miner_stats))
        .route("/api/miner/{address}/workers", get(miner_workers))
        .route("/api/node/info", get(node_info))
        .route("/api/ws", get(ws_handler))
        .with_state(state);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            while !stop.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .map_err(|e| format!("stratum api serve: {e}"))
}

async fn health(State(st): State<ApiState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "poolName": st.stats.pool_name(),
        "height": st.miner.view().head(),
    }))
}

async fn pool_stats(State(st): State<ApiState>) -> Json<Value> {
    Json(serde_json::to_value(st.stats.overview()).unwrap_or(Value::Null))
}

async fn miners(State(st): State<ApiState>) -> Json<Value> {
    Json(serde_json::to_value(st.stats.miners()).unwrap_or(Value::Null))
}

async fn pool_blocks(State(st): State<ApiState>) -> Json<Value> {
    Json(serde_json::to_value(st.stats.recent_blocks()).unwrap_or(Value::Null))
}

async fn pool_shares(State(st): State<ApiState>) -> Json<Value> {
    Json(serde_json::to_value(st.stats.recent_shares()).unwrap_or(Value::Null))
}

async fn pool_workers(State(st): State<ApiState>) -> Json<Value> {
    Json(serde_json::to_value(st.stats.workers()).unwrap_or(Value::Null))
}

async fn payments() -> Json<Value> {
    Json(json!([]))
}

async fn miner_stats(State(st): State<ApiState>, Path(address): Path<String>) -> Json<Value> {
    Json(serde_json::to_value(st.stats.miner_stats(&address)).unwrap_or(Value::Null))
}

async fn miner_workers(State(st): State<ApiState>, Path(address): Path<String>) -> Json<Value> {
    Json(serde_json::to_value(st.stats.miner_stats(&address).workers).unwrap_or(Value::Null))
}

async fn node_info(State(st): State<ApiState>) -> Json<Value> {
    let view = st.miner.view();
    let counts = view.peer_counts();
    Json(json!({
        "clientVersion": format!("quai-rust/{}", env!("CARGO_PKG_VERSION")),
        "network": st.miner.chain().spec().environment,
        "chainId": view.chain_id(),
        "location": view.location(),
        "blockHeight": view.head(),
        "peerCount": counts.peers,
        "mining": st.miner.is_mining(),
        "poolName": st.stats.pool_name(),
    }))
}

async fn ws_handler(
    State(st): State<ApiState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if !origin_allowed(&st.ws_origins, origin) {
        return (StatusCode::FORBIDDEN, "Forbidden\n").into_response();
    }
    let Ok(permit) = st.ws_permits.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "too many websocket clients\n",
        )
            .into_response();
    };
    upgrade
        .max_message_size(MAX_WS_READ_BYTES)
        .max_frame_size(MAX_WS_READ_BYTES)
        .on_upgrade(move |socket| serve_ws(socket, st, permit))
}

async fn serve_ws(socket: WebSocket, st: ApiState, _permit: tokio::sync::OwnedSemaphorePermit) {
    let (mut sink, mut stream) = socket.split();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        if st.stop.load(Ordering::Relaxed) {
            break;
        }
        tokio::select! {
            _ = tick.tick() => {
                let payload = json!({
                    "type": "stats",
                    "stats": st.stats.overview(),
                    "blocks": st.stats.recent_blocks(),
                    "blockHeight": st.miner.view().head(),
                });
                if sink.send(Message::Text(payload.to_string().into())).await.is_err() {
                    break;
                }
            }
            msg = stream.next() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(Message::Ping(p))) => {
                        if sink.send(Message::Pong(p)).await.is_err() {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn origin_allowed(allowed: &[String], origin: Option<&str>) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    if allowed.iter().any(|a| a == "*") {
        return true;
    }
    let lower = origin.to_ascii_lowercase();
    if allowed.is_empty() {
        return lower == "http://localhost"
            || lower.starts_with("http://localhost:")
            || lower == "http://127.0.0.1"
            || lower.starts_with("http://127.0.0.1:");
    }
    allowed.iter().any(|a| a.eq_ignore_ascii_case(&lower))
}
