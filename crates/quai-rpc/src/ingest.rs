//! Bounded loopback RPC follower. Source Go remains the consensus authority.
use crate::{App, Config};
use quai_store::{Identity, Snapshot};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub struct Follower {
    config: Config,
    client: reqwest::Client,
    previous: Option<Value>,
    identity: Option<Identity>,
    status: Value,
    _lock: Arc<File>,
}

fn field<'a>(v: &'a Value, name: &str) -> Result<&'a str, String> {
    v.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing {name}"))
}
fn number(v: &Value) -> Result<u64, String> {
    u64::from_str_radix(
        field(&v["woHeader"], "number")?
            .strip_prefix("0x")
            .ok_or("invalid height")?,
        16,
    )
    .map_err(|_| "invalid height".into())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Same-directory rename with file and directory sync. The lock is held by the
/// process for its lifetime; a crashed writer does not leave a stale OS lock.
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|v| !v.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn account_size(used: &mut usize, blocks: &[Value], receipts: &[Value]) -> Result<(), String> {
    for value in blocks.iter().chain(receipts) {
        let length = serde_json::to_vec(value).map_err(|e| e.to_string())?.len();
        *used = used
            .checked_add(length)
            .filter(|n| *n <= 64 * 1024 * 1024)
            .ok_or("ingest candidate exceeds 64 MiB")?;
    }
    Ok(())
}

fn persist(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let pending = path.with_extension("pending");
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&pending)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&pending, path)?;
        File::open(parent(path))?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&pending);
    }
    result.map_err(|e| format!("checkpoint persistence: {e}"))
}

impl Follower {
    pub async fn open(config: &Config) -> Result<Self, String> {
        let path = PathBuf::from(
            config
                .snapshot
                .as_ref()
                .ok_or("follower requires snapshot")?,
        );
        fs::create_dir_all(parent(&path)).map_err(|e| e.to_string())?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock()
            .map_err(|_| "checkpoint already owned by another follower")?;
        let previous = if path.exists() {
            let mut bytes = Vec::new();
            File::open(&path)
                .map_err(|e| e.to_string())?
                .take(128 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            Snapshot::from_bytes(&bytes)?;
            Some(
                serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string())?["payload"]
                    .clone(),
            )
        } else {
            None
        };
        let identity = previous
            .as_ref()
            .map(|v| {
                serde_json::from_value::<Identity>(v["identity"].clone()).map_err(|e| e.to_string())
            })
            .transpose()?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .map_err(|e| e.to_string())?;
        let mut follower = Self {
            config: config.clone(),
            client,
            previous,
            identity,
            status: json!({"enabled":true,"network_verified":false,"state":"starting","successful_polls":0,"commits":0,"errors":0,"consecutive_errors":0,"reorgs":0,"poll_ms":config.poll_ms,"window_blocks":config.window_blocks,"last_success_unix":null,"last_error":null,"upstream_head_number":null,"upstream_head_hash":null,"lag_blocks":null}),
            _lock: Arc::new(lock),
        };
        // A valid existing checkpoint can start during an outage, after identity
        // is checked on the next successful poll. No new data is trusted yet.
        if follower.previous.is_none() {
            let next = tokio::time::timeout(Duration::from_secs(30), follower.capture())
                .await
                .map_err(|_| "initial capture deadline")??
                .ok_or("empty initial capture")?;
            follower.commit(next, None).await?;
        }
        if let Some(previous) = &follower.previous {
            let height = previous["base_number"].as_u64().unwrap_or(0)
                + previous["canonical"]
                    .as_array()
                    .ok_or("missing canonical view")?
                    .len() as u64
                - 1;
            follower.status["indexed_head_number"] = json!(height);
            follower.status["indexed_head_hash"] = previous["canonical"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .clone();
        }
        Ok(follower)
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let mut response = self
            .client
            .post(self.config.upstream.as_ref().ok_or("missing upstream")?)
            .json(&json!({"jsonrpc":"2.0","id":"ingest","method":method,"params":params}))
            .send()
            .await
            .map_err(|_| format!("{method}: upstream unavailable"))?;
        if !response.status().is_success() {
            return Err(format!("{method}: upstream HTTP error"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "upstream body failed")? {
            if bytes.len() + chunk.len() > self.config.max_response_bytes {
                return Err("ingest response limit".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid ingest JSON")?;
        if v["jsonrpc"] != "2.0"
            || v["id"] != "ingest"
            || v.get("error").is_some()
            || v.get("result").is_none()
        {
            return Err(format!("{method}: invalid response or RPC error"));
        }
        Ok(v["result"].clone())
    }

    async fn read_identity(&self) -> Result<Identity, String> {
        let chain_id = self
            .rpc("quai_chainId", json!([]))
            .await?
            .as_str()
            .ok_or("invalid chain ID")?
            .to_owned();
        let loc = self.rpc("quai_nodeLocation", json!([])).await?;
        let location = loc
            .as_array()
            .ok_or("invalid location")?
            .iter()
            .map(|v| {
                u8::from_str_radix(
                    v.as_str()
                        .and_then(|s| s.strip_prefix("0x"))
                        .ok_or("invalid location")?,
                    16,
                )
                .map_err(|_| "invalid location")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let genesis = self
            .rpc("quai_getBlockByNumber", json!(["0x0", false]))
            .await?;
        Ok(Identity {
            chain_id,
            location,
            genesis_hash: field(&genesis, "hash")?.to_owned(),
        })
    }

    async fn capture(&mut self) -> Result<Option<Value>, String> {
        let identity = self.read_identity().await?;
        if self.identity.as_ref().is_some_and(|old| old != &identity) {
            return Err("upstream network identity changed".into());
        }
        let head = self
            .rpc("quai_getBlockByNumber", json!(["latest", true]))
            .await?;
        let height = number(&head)?;
        let start = height.saturating_sub(self.config.window_blocks as u64 - 1);
        let head_hash = field(&head, "hash")?.to_owned();
        self.status["upstream_head_number"] = json!(height);
        self.status["upstream_head_hash"] = json!(head_hash);
        if self
            .previous
            .as_ref()
            .and_then(|v| v["canonical"].as_array())
            .and_then(|v| v.last())
            .and_then(Value::as_str)
            == Some(&head_hash)
            && self.previous.as_ref().is_some_and(|v| {
                v["base_number"].as_u64().unwrap_or(0) == start
                    && v["blocks"]
                        .as_array()
                        .is_some_and(|b| b.len() <= self.config.window_blocks * 2)
                    && v["receipts_complete"] == true
            })
        {
            let previous_head = self
                .previous
                .as_ref()
                .and_then(|v| v["blocks"].as_array())
                .and_then(|blocks| blocks.iter().find(|b| b["hash"] == head_hash))
                .ok_or("checkpoint head missing")?;
            if previous_head != &head {
                return Err("immutable block content changed".into());
            }
            self.status["lag_blocks"] = json!(0);
            return Ok(None);
        }
        let mut known: HashMap<String, Value> = HashMap::new();
        let mut old_receipts: HashMap<String, Vec<Value>> = HashMap::new();
        if let Some(previous) = &self.previous {
            for block in previous["blocks"]
                .as_array()
                .ok_or("invalid checkpoint blocks")?
            {
                known.insert(field(block, "hash")?.to_owned(), block.clone());
            }
            for receipt in previous["receipts"]
                .as_array()
                .ok_or("invalid checkpoint receipts")?
            {
                old_receipts
                    .entry(field(receipt, "blockHash")?.to_owned())
                    .or_default()
                    .push(receipt.clone());
            }
        }
        let mut chain = Vec::new();
        let mut current = head;
        let mut receipts = Vec::new();
        let mut bytes_used = 0;
        for n in (start..=height).rev() {
            let receipt_start = receipts.len();
            if number(&current)? != n {
                return Err("wrong block height during ingestion".into());
            }
            let key = field(&current, "hash")?.to_owned();
            if let Some(old) = known.get(&key) {
                if old != &current {
                    return Err("immutable block content changed".into());
                }
                let entries = old_receipts.get(&key).cloned().unwrap_or_default();
                if entries.len()
                    == current["transactions"]
                        .as_array()
                        .ok_or("missing transactions")?
                        .len()
                {
                    receipts.extend(entries);
                } else {
                    for tx in current["transactions"]
                        .as_array()
                        .ok_or("missing transactions")?
                    {
                        let receipt = self
                            .rpc("quai_getTransactionReceipt", json!([field(tx, "hash")?]))
                            .await?;
                        if receipt["blockHash"] != key {
                            return Err("missing receipt while resuming".into());
                        }
                        receipts.push(receipt);
                    }
                }
            } else {
                for tx in current["transactions"]
                    .as_array()
                    .ok_or("missing transactions")?
                {
                    let receipt = self
                        .rpc("quai_getTransactionReceipt", json!([field(tx, "hash")?]))
                        .await?;
                    if receipt["blockHash"] != key {
                        return Err("missing or noncanonical receipt during ingestion".into());
                    }
                    receipts.push(receipt);
                }
            }
            let parent = field(&current["woHeader"], "parentHash")?.to_owned();
            chain.push(std::mem::take(&mut current));
            account_size(
                &mut bytes_used,
                std::slice::from_ref(chain.last().unwrap()),
                &receipts[receipt_start..],
            )?;
            if n > start {
                current = match known.get(&parent) {
                    Some(v) => v.clone(),
                    None => {
                        self.rpc("quai_getBlockByHash", json!([parent, true]))
                            .await?
                    }
                };
            }
        }
        chain.reverse();
        let anchor = if start > 0 {
            json!({"number":start-1,"hash":field(&chain[0]["woHeader"],"parentHash")?})
        } else {
            Value::Null
        };
        // Pin the selected head after receipt collection. An extension is fine;
        // replacement at this height discards the whole candidate.
        let after = self
            .rpc(
                "quai_getBlockByNumber",
                json!([format!("0x{height:x}"), false]),
            )
            .await?;
        if after["hash"] != head_hash {
            return Err("chain changed during capture".into());
        }
        if self.read_identity().await? != identity {
            return Err("network changed during capture".into());
        }
        let canonical: Vec<_> = chain.iter().map(|v| v["hash"].clone()).collect();
        // Retain at most one window of orphan blocks, plus the canonical window.
        let selected: HashSet<_> = canonical.iter().filter_map(Value::as_str).collect();
        let mut orphans: Vec<_> = known
            .into_iter()
            .filter(|(key, _)| !selected.contains(key.as_str()))
            .map(|(_, v)| v)
            .collect();
        orphans.sort_by_key(|v| {
            (
                number(v).unwrap_or(0),
                v["hash"].as_str().unwrap_or("").to_owned(),
            )
        });
        let keep_from = orphans.len().saturating_sub(self.config.window_blocks);
        for orphan in orphans.into_iter().skip(keep_from) {
            let receipt_start = receipts.len();
            receipts.extend(
                old_receipts
                    .remove(field(&orphan, "hash")?)
                    .unwrap_or_default(),
            );
            chain.push(orphan);
            account_size(
                &mut bytes_used,
                std::slice::from_ref(chain.last().unwrap()),
                &receipts[receipt_start..],
            )?;
        }
        Ok(Some(
            json!({"schema":1,"identity":identity,"base_number":start,"anchor":anchor,"canonical":canonical,"blocks":chain,"receipts":receipts,"receipts_complete":true,"source":{"captured_unix":now(),"trust":"RPC follower; consensus remains the source Go node responsibility"}}),
        ))
    }

    async fn commit(&mut self, next: Value, app: Option<Arc<App>>) -> Result<(), String> {
        let reorg = self.previous.as_ref().is_some_and(|old| {
            let first = old["base_number"].as_u64().unwrap_or(0);
            let second = next["base_number"].as_u64().unwrap_or(0);
            let old_map: BTreeMap<_, _> = old["canonical"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(n, h)| (first + n as u64, h))
                .collect();
            next["canonical"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .any(|(n, h)| {
                    old_map
                        .get(&(second + n as u64))
                        .is_some_and(|previous| *previous != h)
                })
                || next["canonical"].as_array().unwrap().len() as u64 + second
                    < old["canonical"].as_array().unwrap().len() as u64 + first
        });
        let path = PathBuf::from(self.config.snapshot.as_ref().unwrap());
        let payload = next.clone();
        let lock = self._lock.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let _lock_guard = lock;
            let bytes = quai_store::seal_export(&payload)?;
            let snapshot = Snapshot::from_bytes(&bytes)?;
            persist(&path, &bytes)?;
            if let Some(app) = app {
                app.install_snapshot(snapshot)?;
            }
            Ok(())
        })
        .await
        .map_err(|_| "checkpoint worker failed")??;
        self.identity =
            Some(serde_json::from_value(next["identity"].clone()).map_err(|e| e.to_string())?);
        self.status["indexed_head_number"] = json!(
            next["base_number"].as_u64().unwrap_or(0)
                + next["canonical"].as_array().unwrap().len() as u64
                - 1
        );
        self.status["indexed_head_hash"] = next["canonical"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        self.status["commits"] = json!(self.status["commits"].as_u64().unwrap() + 1);
        if reorg {
            self.status["reorgs"] = json!(self.status["reorgs"].as_u64().unwrap() + 1);
        }
        self.status["lag_blocks"] = json!(0);
        self.previous = Some(next);
        Ok(())
    }

    pub async fn run(mut self, app: Arc<App>) {
        loop {
            let result = match tokio::time::timeout(Duration::from_secs(30), self.capture()).await {
                Ok(Ok(Some(next))) => self.commit(next, Some(app.clone())).await,
                Ok(Ok(None)) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(_) => Err("capture deadline exceeded".into()),
            };
            match result {
                Ok(()) => {
                    self.status["state"] = json!("following");
                    self.status["network_verified"] = json!(true);
                    self.status["last_success_unix"] = json!(now());
                    self.status["last_error"] = Value::Null;
                    self.status["consecutive_errors"] = json!(0);
                    self.status["successful_polls"] =
                        json!(self.status["successful_polls"].as_u64().unwrap() + 1);
                }
                Err(error) => {
                    self.status["state"] = json!("degraded");
                    self.status["network_verified"] = json!(false);
                    self.status["last_error"] = json!(error);
                    self.status["lag_blocks"] = Value::Null;
                    self.status["errors"] = json!(self.status["errors"].as_u64().unwrap() + 1);
                    self.status["consecutive_errors"] =
                        json!(self.status["consecutive_errors"].as_u64().unwrap() + 1);
                }
            }
            app.set_ingestion_status(self.status.clone());
            // Slow failed polling with a bounded exponential delay. No busy loop.
            let multiplier = 1u64 << self.status["consecutive_errors"].as_u64().unwrap().min(5);
            tokio::time::sleep(Duration::from_millis(
                (self.config.poll_ms * multiplier).min(30_000),
            ))
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn follows_only_explicit_bounded_loopback_hybrid_configuration() {
        let mut config = Config {
            mode: "hybrid".into(),
            upstream: Some("http://127.0.0.1:1".into()),
            snapshot: Some("view.json".into()),
            follow: true,
            ..Config::default()
        };
        assert!(config.validate().is_ok());
        config.upstream = Some("http://localhost:1".into());
        assert!(config.validate().is_err());
        config.upstream = Some("http://127.0.0.1:1".into());
        config.window_blocks = 0;
        assert!(config.validate().is_err());
        config.window_blocks = 1025;
        assert!(config.validate().is_err());
        config.window_blocks = 3;
        config.poll_ms = 0;
        assert!(config.validate().is_err());
        config.poll_ms = 100;
        config.mode = "snapshot".into();
        assert!(config.validate().is_err());
    }
    #[test]
    fn checkpoint_commit_preserves_old_file_if_rename_fails() {
        let directory = std::env::temp_dir().join(format!(
            "quai-checkpoint-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("checkpoint.json");
        persist(&path, b"old").unwrap();
        persist(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        let destination = directory.join("existing-directory");
        fs::create_dir(&destination).unwrap();
        assert!(persist(&destination, b"invalid-destination").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert!(!destination.with_extension("pending").exists());
        assert_eq!(parent(Path::new("view.json")), Path::new("."));
        fs::remove_dir_all(directory).unwrap();
    }
}
