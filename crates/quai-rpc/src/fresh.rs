//! Canonical reads use two uncached upstream identity/head barriers.
use crate::{App, Outcome};
use quai_store::Snapshot;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

#[derive(Debug)]
enum ProbeError {
    Unavailable,
    Identity,
}

impl App {
    async fn canonical_probe(&self, snapshot: &Snapshot) -> Result<bool, ProbeError> {
        self.freshness_http_calls.fetch_add(1, Ordering::Relaxed);
        let requests = json!([
            {"jsonrpc":"2.0","id":"chain","method":"quai_chainId","params":[]},
            {"jsonrpc":"2.0","id":"location","method":"quai_nodeLocation","params":[]},
            {"jsonrpc":"2.0","id":"genesis","method":"quai_getBlockByNumber","params":["0x0",false]},
            {"jsonrpc":"2.0","id":"head","method":"quai_getBlockByNumber","params":["latest",false]}
        ]);
        let mut response = self
            .client
            .post(
                self.config
                    .upstream
                    .as_ref()
                    .ok_or(ProbeError::Unavailable)?,
            )
            .json(&requests)
            .send()
            .await
            .map_err(|_| ProbeError::Unavailable)?;
        if !response.status().is_success() {
            return Err(ProbeError::Unavailable);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| ProbeError::Unavailable)?
        {
            if bytes.len() + chunk.len() > self.config.max_response_bytes {
                return Err(ProbeError::Unavailable);
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| ProbeError::Unavailable)?;
        let replies = value
            .as_array()
            .filter(|v| v.len() == 4)
            .ok_or(ProbeError::Unavailable)?;
        let mut results = std::collections::HashMap::new();
        for reply in replies {
            let id = reply["id"].as_str().ok_or(ProbeError::Unavailable)?;
            if !["chain", "location", "genesis", "head"].contains(&id)
                || reply["jsonrpc"] != "2.0"
                || reply.get("error").is_some()
                || reply.get("result").is_none()
                || results.insert(id, &reply["result"]).is_some()
            {
                return Err(ProbeError::Unavailable);
            }
        }
        // Four distinct known IDs in four replies: every lookup below succeeds.
        let result = |id: &str| results.get(id).copied().ok_or(ProbeError::Unavailable);
        let identity = &snapshot.identity;
        if result("chain")? != &json!(identity.chain_id)
            || result("location")?
                != &json!(identity
                    .location
                    .iter()
                    .map(|n| format!("0x{n:x}"))
                    .collect::<Vec<_>>())
            || result("genesis")?["hash"] != identity.genesis_hash
        {
            return Err(ProbeError::Identity);
        }
        let head = result("head")?;
        Ok(head["hash"] == *snapshot.head().1
            && head["woHeader"]["number"] == format!("0x{:x}", snapshot.head().0))
    }

    pub(crate) async fn canonical_read(&self, method: &str, params: &Value) -> Option<Outcome> {
        if !self.config.native_canonical_reads
            || !["quai_getTransactionReceipt", "quai_getLogs"].contains(&method)
        {
            return None;
        }
        let snapshot = self.store.as_ref()?.snapshot();
        if self.ingestion_status()["network_verified"] != true {
            self.canonical_unavailable.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let mut candidate = Vec::new();
        for barrier in 0..2 {
            if barrier == 1 {
                candidate = match snapshot.indexed_read(
                    method,
                    params,
                    self.config.max_response_bytes.saturating_sub(256),
                ) {
                    Ok(bytes) => bytes,
                    Err(miss) => {
                        self.canonical_misses[miss as usize].fetch_add(1, Ordering::Relaxed);
                        return None;
                    }
                };
            }
            match self.canonical_probe(&snapshot).await {
                Ok(true) => (),
                Ok(false) => {
                    self.canonical_stale.fetch_add(1, Ordering::Relaxed);
                    return None;
                }
                Err(ProbeError::Unavailable) => {
                    self.canonical_unavailable.fetch_add(1, Ordering::Relaxed);
                    return None;
                }
                Err(ProbeError::Identity) => {
                    self.canonical_identity_rejections
                        .fetch_add(1, Ordering::Relaxed);
                    return Some(crate::error(-32098, "upstream network identity changed"));
                }
            }
        }
        if self.ingestion_status()["network_verified"] != true {
            self.canonical_unavailable.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        // The pinned Arc keeps one canonical view even if ingestion replaces it.
        self.native_hits.fetch_add(1, Ordering::Relaxed);
        if method == "quai_getTransactionReceipt" {
            self.native_receipt_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.native_log_hits.fetch_add(1, Ordering::Relaxed);
        }
        Some(Outcome::Encoded(candidate))
    }
}
