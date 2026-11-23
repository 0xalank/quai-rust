//! JSON-RPC over WebSocket, as go-quai's rpc/websocket.go serves it: the
//! methods of the HTTP endpoint plus `quai_subscribe` and `*_unsubscribe`,
//! on a WebSocket upgrade of "/" at the RPC listen address (go-quai serves
//! WebSocket on its HTTP server when both use one port). The listen address
//! keeps the HTTP rules: loopback by default.
//!
//! Like go-quai, each message runs on its own task, a batch answers in one
//! array, a subscription's notifications go out after its subscribe answer
//! (`Notifier.activate`), messages are limited to 32 MiB
//! (`wsMessageSizeLimit`), an idle connection gets a ping every 30 seconds,
//! and a browser `Origin` must be allowed (`wsHandshakeValidator`).
//! Answers end with a newline, as `json.Encoder` writes them.
use crate::App;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;

/// go-quai's `wsMessageSizeLimit`.
const MESSAGE_LIMIT: usize = 32 * 1024 * 1024;
/// go-quai's `wsPingInterval`.
const PING_INTERVAL: Duration = Duration::from_secs(30);
/// Maximum concurrent in-flight requests per WebSocket connection.
const MAX_WS_CONCURRENT: usize = 32;

/// One WebSocket connection, as the request handlers see it.
pub(crate) struct Connection {
    pub id: u64,
    /// Text frames to write, in order.
    pub out: mpsc::Sender<String>,
}

static NEXT: AtomicU64 = AtomicU64::new(1);

pub(crate) async fn handler(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let origin = headers
        .get(header::ORIGIN)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned());
    if app.view.is_none() || !origin_allowed(&app.config.ws_origins, origin.as_deref()) {
        // gorilla/websocket's answer to a refused origin.
        return (StatusCode::FORBIDDEN, "Forbidden\n").into_response();
    }
    upgrade
        .max_message_size(MESSAGE_LIMIT)
        .max_frame_size(MESSAGE_LIMIT)
        .on_upgrade(move |socket| serve(app, socket))
}

async fn serve(app: Arc<App>, socket: WebSocket) {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<String>(crate::native::filters::OUTBOX);
    // The writer: frames in order, and a ping after 30 idle seconds.
    let writer = tokio::spawn(async move {
        loop {
            match tokio::time::timeout(PING_INTERVAL, rx.recv()).await {
                Ok(Some(text)) => {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(_) => {
                    if sink.send(Message::Ping(Default::default())).await.is_err() {
                        return;
                    }
                }
            }
        }
    });
    let conn = Arc::new(Connection { id, out: tx });
    let inflight = Arc::new(tokio::sync::Semaphore::new(MAX_WS_CONCURRENT));
    while let Some(Ok(message)) = stream.next().await {
        let body = match message {
            Message::Text(t) => t.as_bytes().to_vec(),
            Message::Binary(b) => b.to_vec(),
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => continue,
        };
        let value = match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(v) => v,
            Err(_) => {
                // go-quai answers a syntax error and closes the connection.
                let _ = conn
                    .out
                    .send(
                        "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32700,\"message\":\"parse error\"}}\n"
                            .into(),
                    )
                    .await;
                break;
            }
        };
        let Ok(permit) = inflight.clone().acquire_owned().await else {
            break;
        };
        let app = app.clone();
        let conn = conn.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let (reply, created) = app.handle_value(value, Some(&*conn)).await;
            if let Some(reply) = reply {
                let mut text = String::from_utf8_lossy(&reply).into_owned();
                text.push('\n');
                let _ = conn.out.send(text).await;
            }
            for api in app.filter_apis() {
                for sub in &created {
                    api.activate(sub);
                }
            }
        });
    }
    for api in app.filter_apis() {
        api.close(id);
    }
    // Let queued answers go out, then stop the writer.
    drop(conn);
    let abort = writer.abort_handle();
    if tokio::time::timeout(Duration::from_secs(1), writer)
        .await
        .is_err()
    {
        abort.abort();
    }
}

/// `wsHandshakeValidator`: no Origin header passes; "*" allows any; an
/// empty list allows "http://localhost" and "http://<host name>".
pub(crate) fn origin_allowed(allowed: &[String], origin: Option<&str>) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let mut rules: Vec<String> = allowed.iter().filter(|o| !o.is_empty()).cloned().collect();
    if rules.iter().any(|o| o == "*") {
        return true;
    }
    if rules.is_empty() {
        rules.push("http://localhost".into());
        if let Some(host) = host_name() {
            rules.push(format!("http://{host}"));
        }
    }
    let Some(browser) = parse_origin(&origin.to_lowercase()) else {
        return false;
    };
    rules.iter().any(|rule| {
        let Some(allowed) = parse_origin(&rule.to_lowercase()) else {
            return false;
        };
        (allowed.0.is_empty() || allowed.0 == browser.0)
            && (allowed.1.is_empty() || allowed.1 == browser.1)
            && (allowed.2.is_empty() || allowed.2 == browser.2)
    })
}

/// `parseOriginURL`: scheme, host name and port of an origin rule or
/// header ("host:port" without a scheme is a host and a port). Rejects
/// authorities carrying userinfo (`user@host`), which could otherwise spoof
/// allowed hosts.
fn parse_origin(origin: &str) -> Option<(String, String, String)> {
    let split_port = |authority: &str| -> (String, String) {
        if let Some(rest) = authority.strip_prefix('[') {
            if let Some((host, port)) = rest.split_once(']') {
                return (host.to_string(), port.trim_start_matches(':').to_string());
            }
        }
        match authority.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h.to_string(), p.to_string()),
            _ => (authority.to_string(), String::new()),
        }
    };
    if let Some((scheme, rest)) = origin.split_once("://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.contains('@') {
            return None;
        }
        let (host, port) = split_port(authority);
        return Some((scheme.to_string(), host, port));
    }
    if origin.contains('@') {
        return None;
    }
    Some(match origin.split_once(':') {
        Some((host, port)) => (String::new(), host.to_string(), port.to_string()),
        None => (String::new(), origin.to_string(), String::new()),
    })
}

/// `os.Hostname()`, read once from the `hostname` program.
fn host_name() -> Option<String> {
    static NAME: OnceLock<Option<String>> = OnceLock::new();
    NAME.get_or_init(|| {
        let out = std::process::Command::new("hostname").output().ok()?;
        let name = String::from_utf8(out.stdout).ok()?.trim().to_string();
        (!name.is_empty()).then_some(name)
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origins_follow_go_rules() {
        let none: Vec<String> = vec![];
        assert!(origin_allowed(&none, None));
        assert!(origin_allowed(&none, Some("http://localhost")));
        assert!(origin_allowed(&none, Some("http://localhost:3000")));
        assert!(!origin_allowed(&none, Some("https://localhost")));
        assert!(!origin_allowed(&none, Some("http://evil.example")));
        assert!(!origin_allowed(&none, Some("chrome-extension://abc")));
        let any = vec!["*".to_string()];
        assert!(origin_allowed(&any, Some("chrome-extension://abc")));
        let one = vec!["chrome-extension://abc".to_string()];
        assert!(origin_allowed(&one, Some("chrome-extension://ABC")));
        assert!(!origin_allowed(&one, Some("chrome-extension://abd")));
        let port = vec!["localhost:8080".to_string()];
        assert!(origin_allowed(&port, Some("http://localhost:8080")));
        assert!(!origin_allowed(&port, Some("http://localhost:8081")));
    }
}
