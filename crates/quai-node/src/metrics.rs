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
    /// The deepest reorg (blocks removed).
    pub deepest_reorg: AtomicU64,
    /// Gossip blocks and workshares that go-quai's validators accepted.
    pub gossip_blocks: AtomicU64,
    pub gossip_workshares: AtomicU64,
    /// Tip blocks offered to the processor, and the ones it rejected.
    pub tip_offered: AtomicU64,
    pub blocks_rejected: AtomicU64,
    /// Gossip workshares: every check passed (with the parent rules), passed
    /// without the parent rules (parent unknown), rejected, or older than
    /// go-quai's intake distance (ignored, as go-quai does).
    pub workshares_valid: AtomicU64,
    pub workshares_parent_unknown: AtomicU64,
    pub workshares_rejected: AtomicU64,
    pub workshares_old: AtomicU64,
    /// Checks that could not run for a missing input ("unavailable").
    pub checks_unavailable: AtomicU64,
    /// Divergences from the peers: a block they accept and this node
    /// rejects, a workshare they include and this node rejected, the
    /// canonical chain a few blocks below the head differing from theirs,
    /// and the head left behind theirs.
    pub divergences_block: AtomicU64,
    pub divergences_workshare: AtomicU64,
    pub divergences_head: AtomicU64,
    pub divergences_stall: AtomicU64,
    /// Comparisons of the canonical chain with the peers', and agreements.
    pub head_checks: AtomicU64,
    pub head_agreements: AtomicU64,
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
    /// A block the peers announced (tip mode): the peers' head is at least
    /// its number.
    pub fn note_peer_head(&self, number: u64) {
        if let Some(p) = self.peers.get() {
            p.head.fetch_max(number, Ordering::Relaxed);
        }
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
        // Families with labels go after the others.
        let mut labeled = String::new();
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
        put(
            "quai_node_deepest_reorg_blocks",
            "gauge",
            "Blocks removed by the deepest reorg.",
            get(&self.deepest_reorg),
        );
        put(
            "quai_node_gossip_blocks_total",
            "counter",
            "Gossip zone blocks that go-quai's validators accepted.",
            get(&self.gossip_blocks),
        );
        put(
            "quai_node_gossip_workshares_total",
            "counter",
            "Gossip zone workshares that go-quai's validators accepted.",
            get(&self.gossip_workshares),
        );
        put(
            "quai_node_tip_offered_total",
            "counter",
            "Blocks offered to the head choice in tip mode.",
            get(&self.tip_offered),
        );
        put(
            "quai_node_blocks_rejected_total",
            "counter",
            "Blocks this node rejected in tip mode.",
            get(&self.blocks_rejected),
        );
        labeled.push_str("# HELP quai_node_workshares_total Gossip workshares by result.\n# TYPE quai_node_workshares_total counter\n");
        for (result, v) in [
            ("valid", &self.workshares_valid),
            ("valid_parent_unknown", &self.workshares_parent_unknown),
            ("rejected", &self.workshares_rejected),
            ("old", &self.workshares_old),
        ] {
            labeled.push_str(&format!(
                "quai_node_workshares_total{{result=\"{result}\"}} {}\n",
                v.load(Ordering::Relaxed)
            ));
        }
        put(
            "quai_node_checks_unavailable_total",
            "counter",
            "Checks that could not run for a missing input.",
            get(&self.checks_unavailable),
        );
        labeled.push_str("# HELP quai_node_divergences_total Divergences from the peers by kind.\n# TYPE quai_node_divergences_total counter\n");
        for (kind, v) in [
            ("block", &self.divergences_block),
            ("workshare", &self.divergences_workshare),
            ("head", &self.divergences_head),
            ("stall", &self.divergences_stall),
        ] {
            labeled.push_str(&format!(
                "quai_node_divergences_total{{kind=\"{kind}\"}} {}\n",
                v.load(Ordering::Relaxed)
            ));
        }
        put(
            "quai_node_head_checks_total",
            "counter",
            "Comparisons of the canonical chain a few blocks below the head with the peers'.",
            get(&self.head_checks),
        );
        put(
            "quai_node_head_agreements_total",
            "counter",
            "Comparisons where the peers' canonical block was this node's.",
            get(&self.head_agreements),
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
        out.push_str(&labeled);
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
/// Maximum concurrent HTTP connections to the metrics endpoint.
const MAX_METRICS_CONNECTIONS: usize = 32;

/// A minimal HTTP/1.1 server: `GET /metrics` gets the text, anything else 404.
pub async fn serve(
    listener: tokio::net::TcpListener,
    m: Arc<Metrics>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) {
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_METRICS_CONNECTIONS));
    tokio::pin!(shutdown);
    loop {
        let accepted = tokio::select! {
            _ = &mut shutdown => return,
            a = listener.accept() => a,
        };
        let Ok((mut stream, _)) = accepted else {
            continue;
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let m = m.clone();
        tokio::spawn(async move {
            let _permit = permit;
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
            let _ = tokio::time::timeout(Duration::from_secs(5), async {
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            })
            .await;
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
        assert!(text.contains("quai_node_divergences_total{kind=\"block\"} 0\n"));
        assert!(text.contains("quai_node_workshares_total{result=\"valid\"} 0\n"));
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
