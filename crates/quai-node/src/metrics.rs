//! Prometheus text metrics of a running node (`GET /metrics`).
use crate::peers;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Default)]
pub struct Metrics {
    /// The zone head with every check passed.
    pub head: AtomicU64,
    /// Zone heights checked in this run.
    pub blocks: AtomicU64,
    /// Validation failures and source errors.
    pub failures: AtomicU64,
    /// Head changes that removed canonical blocks.
    pub reorgs: AtomicU64,
    /// Blocks removed from the canonical chain by reorgs.
    pub reorged_blocks: AtomicU64,
    /// Blocks per second over the last minute, times 1000.
    rate_milli: AtomicU64,
    samples: Mutex<Vec<(Instant, u64)>>,
    peers: OnceLock<Arc<peers::Status>>,
    started: OnceLock<Instant>,
}
impl Metrics {
    pub fn new(head: u64) -> Self {
        let m = Self::default();
        m.head.store(head, Ordering::Relaxed);
        let _ = m.started.set(Instant::now());
        m
    }
    pub fn set_peers(&self, status: Arc<peers::Status>) {
        let _ = self.peers.set(status);
    }
    pub fn peer_head(&self) -> u64 {
        self.peers
            .get()
            .map_or(0, |p| p.head.load(Ordering::Relaxed))
    }
    pub fn rate(&self) -> f64 {
        self.rate_milli.load(Ordering::Relaxed) as f64 / 1000.0
    }
    /// Record the block count now and update the one-minute rate.
    pub fn sample_now(&self) {
        let now = Instant::now();
        let blocks = self.blocks.load(Ordering::Relaxed);
        let mut s = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        s.push((now, blocks));
        s.retain(|(t, _)| now.duration_since(*t) <= Duration::from_secs(60));
        if let (Some(first), Some(last)) = (s.first(), s.last()) {
            let secs = last.0.duration_since(first.0).as_secs_f64();
            if secs > 0.0 {
                let rate = (last.1 - first.1) as f64 / secs;
                self.rate_milli
                    .store((rate * 1000.0) as u64, Ordering::Relaxed);
            }
        }
    }
    /// The Prometheus text exposition.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut put = |name: &str, kind: &str, help: &str, value: String| {
            out.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}\n"
            ));
        };
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed).to_string();
        put(
            "quai_node_head_height",
            "gauge",
            "Zone head with every check passed.",
            get(&self.head),
        );
        put(
            "quai_node_blocks_total",
            "counter",
            "Zone heights checked in this run.",
            get(&self.blocks),
        );
        put(
            "quai_node_blocks_per_second",
            "gauge",
            "Zone heights checked per second over the last minute.",
            format!("{:.3}", self.rate()),
        );
        put(
            "quai_node_failures_total",
            "counter",
            "Validation failures and source errors.",
            get(&self.failures),
        );
        put(
            "quai_node_reorgs_total",
            "counter",
            "Head changes that removed canonical blocks.",
            get(&self.reorgs),
        );
        put(
            "quai_node_reorged_blocks_total",
            "counter",
            "Blocks removed from the canonical chain by reorgs.",
            get(&self.reorged_blocks),
        );
        if let Some(p) = self.peers.get() {
            put(
                "quai_node_peers",
                "gauge",
                "Peers the node fetches blocks from.",
                p.peers.load(Ordering::Relaxed).to_string(),
            );
            put(
                "quai_node_peer_head_height",
                "gauge",
                "The best zone head of the peers.",
                p.head.load(Ordering::Relaxed).to_string(),
            );
            put(
                "quai_node_peer_blocks_total",
                "counter",
                "Blocks fetched from peers.",
                p.blocks.load(Ordering::Relaxed).to_string(),
            );
            put(
                "quai_node_peer_faults_total",
                "counter",
                "Peer replies that failed (time-outs, missing or bad data).",
                p.faults.load(Ordering::Relaxed).to_string(),
            );
        }
        if let Some(t) = self.started.get() {
            put(
                "quai_node_uptime_seconds",
                "gauge",
                "Seconds since the node started.",
                t.elapsed().as_secs().to_string(),
            );
        }
        out
    }
}
/// Update the rate every ten seconds until `stop`.
pub async fn sample(m: Arc<Metrics>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        m.sample_now();
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}
/// A minimal HTTP/1.1 server: `GET /metrics` gets the text, anything else 404.
pub async fn serve(
    listener: tokio::net::TcpListener,
    m: Arc<Metrics>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) {
    tokio::pin!(shutdown);
    loop {
        let accepted = tokio::select! {
            _ = &mut shutdown => return,
            a = listener.accept() => a,
        };
        let Ok((mut stream, _)) = accepted else {
            continue;
        };
        let m = m.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let n = match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await
            {
                Ok(Ok(n)) => n,
                _ => return,
            };
            let request = String::from_utf8_lossy(&buf[..n]);
            let line = request.lines().next().unwrap_or_default();
            let (status, body) = if line.starts_with("GET /metrics ") || line == "GET /metrics" {
                ("200 OK", m.render())
            } else {
                ("404 Not Found", "not found\n".to_string())
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_prometheus_text() {
        let m = Metrics::new(5);
        m.blocks.store(7, Ordering::Relaxed);
        let text = m.render();
        assert!(text.contains("# TYPE quai_node_head_height gauge\nquai_node_head_height 5\n"));
        assert!(text.contains("quai_node_blocks_total 7\n"));
        assert!(!text.contains("quai_node_peers "));
        let status = Arc::new(peers::Status::default());
        status.peers.store(3, Ordering::Relaxed);
        m.set_peers(status);
        assert!(m.render().contains("quai_node_peers 3\n"));
    }
    #[test]
    fn serves_metrics_over_http() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let m = Arc::new(Metrics::new(42));
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let server = tokio::spawn(serve(listener, m, async {
                let _ = rx.await;
            }));
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n")
                .await
                .unwrap();
            let mut body = String::new();
            s.read_to_string(&mut body).await.unwrap();
            assert!(body.starts_with("HTTP/1.1 200 OK"), "{body}");
            assert!(body.contains("quai_node_head_height 42"));
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
            let mut body = String::new();
            s.read_to_string(&mut body).await.unwrap();
            assert!(body.starts_with("HTTP/1.1 404"));
            let _ = tx.send(());
            server.await.unwrap();
        });
    }
}
