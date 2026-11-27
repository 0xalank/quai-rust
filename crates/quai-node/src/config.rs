//! The node's configuration file (`--config FILE`): a small TOML subset.
//!
//! ```toml
//! datadir = "/Users/me/.quai-node"
//! network = "colosseum"          # colosseum, garden, orchard, lighthouse, local
//! slice = "0-0"                  # zone slice (region-zone)
//! log_level = "info"             # error, warn, info, debug
//!
//! [rpc]
//! listen = "127.0.0.1:8547"      # "off": no RPC
//! ipc_path = "/Users/me/.quai-node/quai.ipc"
//! http_api = ["quai", "eth", "net", "web3", "txpool"]
//! ws_api = ["quai", "eth", "net", "web3", "txpool", "miner"]
//! cors_domain = ["http://localhost:3000"]
//! vhosts = ["localhost", "127.0.0.1"]
//!
//! [metrics]
//! listen = "127.0.0.1:9101"      # "off": no metrics endpoint
//!
//! [sync]
//! stateless = false              # all checks except execution
//! jobs = 3                       # proof-of-work threads
//! margin = 64                    # stay this many zone blocks below the peers' head
//! max_peers = 8
//! public = true                  # boot peers and the DHT
//! peers = ["/ip4/127.0.0.1/tcp/4002/p2p/12D3Koo..."]
//! archive = false                # persist per-height state versions
//! index_address_utxos = false    # workshare, donor, supply & locked-Quai indexes
//! prune_blocks = 0               # optional canonical block retention window
//! checkpoint_every = 10000
//! commit_every = 1000
//!
//! [p2p]
//! relay_service = false          # libp2p circuit relay v2 server
//! publish = false                # broadcast mined blocks & workshares
//!
//! [txpool]
//! enabled = true                 # the transaction pool (rpc and run with state)
//! sharing_clients = ["http://127.0.0.1:9201"]   # http:// or ws:// endpoints
//! sync_tx_with_return = true     # go-quai's --txpool.sync-tx-with-return
//!
//! [miner]
//! enabled = false
//! coinbase = "0x0000000000000000000000000000000000000001"
//! extra = "quai-rust"
//! lockup_byte = 0
//! preference = "0.5"
//!
//! [stratum]
//! enabled = false
//! sha_addr = "0.0.0.0:3333"
//! scrypt_addr = "0.0.0.0:3334"
//! kawpow_addr = "0.0.0.0:3335"
//! api_addr = "0.0.0.0:3336"
//! name = "quai-rust Stratum"
//! pool_tag = "quai-rust"
//! no_vardiff = false
//! proxy_protocol = false
//! ```
//!
//! Values are strings in double quotes (with `\"` and `\\` escapes),
//! integers, `true`/`false`, or one-line arrays of strings. `#` starts a
//! comment outside a string. Command line options override the file. An
//! unknown key is an error, so a typo does not go unnoticed.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Number(u64),
    Bool(bool),
    List(Vec<String>),
}
/// The keys a configuration file may set, as `section.key`.
pub const KEYS: &[&str] = &[
    "datadir",
    "network",
    "slice",
    "log_level",
    "rpc.listen",
    "rpc.ipc_path",
    "rpc.http_api",
    "rpc.ws_api",
    "rpc.cors_domain",
    "rpc.vhosts",
    "metrics.listen",
    "sync.stateless",
    "sync.jobs",
    "sync.margin",
    "sync.max_peers",
    "sync.public",
    "sync.peers",
    "sync.checkpoint_every",
    "sync.commit_every",
    "sync.archive",
    "sync.compact",
    "sync.compact_miner",
    "sync.keep_checkpoints",
    "sync.index_address_utxos",
    "sync.prune_blocks",
    "p2p.relay_service",
    "p2p.publish",
    "txpool.enabled",
    "txpool.sharing_clients",
    "txpool.sync_tx_with_return",
    "miner.enabled",
    "miner.workshares_only",
    "miner.builder_rpc",
    "miner.coinbase",
    "miner.extra",
    "miner.gas_limit",
    "miner.gas_price",
    "miner.lockup_byte",
    "miner.preference",
    "light.upstream_rpc",
    "light.max_headers",
    "light.checkpoint_height",
    "stratum.enabled",
    "stratum.sha_addr",
    "stratum.scrypt_addr",
    "stratum.kawpow_addr",
    "stratum.api_addr",
    "stratum.name",
    "stratum.pool_tag",
    "stratum.no_vardiff",
    "stratum.proxy_protocol",
    "stratum.api_ws_origins",
    "stratum.auth_token",
    "stratum.subshares",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    values: BTreeMap<String, Value>,
}
fn text(input: &str, line: usize) -> Result<(String, &str), String> {
    let mut out = String::new();
    let mut chars = input.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Ok((out, &input[i + 1..])),
            '\\' => match chars.next() {
                Some((_, '"')) => out.push('"'),
                Some((_, '\\')) => out.push('\\'),
                Some((_, 'n')) => out.push('\n'),
                Some((_, 't')) => out.push('\t'),
                _ => return Err(format!("line {line}: unknown escape in a string")),
            },
            c => out.push(c),
        }
    }
    Err(format!("line {line}: unterminated string"))
}
fn rest_is_comment(rest: &str, line: usize) -> Result<(), String> {
    let rest = rest.trim();
    if rest.is_empty() || rest.starts_with('#') {
        Ok(())
    } else {
        Err(format!("line {line}: unexpected {rest:?} after the value"))
    }
}
fn value(raw: &str, line: usize) -> Result<Value, String> {
    let raw = raw.trim_start();
    if let Some(body) = raw.strip_prefix('"') {
        let (s, rest) = text(body, line)?;
        rest_is_comment(rest, line)?;
        return Ok(Value::Text(s));
    }
    if let Some(mut body) = raw.strip_prefix('[') {
        let mut list = vec![];
        loop {
            body = body.trim_start();
            if let Some(rest) = body.strip_prefix(']') {
                rest_is_comment(rest, line)?;
                return Ok(Value::List(list));
            }
            let Some(s) = body.strip_prefix('"') else {
                return Err(format!("line {line}: arrays hold strings"));
            };
            let (item, rest) = text(s, line)?;
            list.push(item);
            body = rest.trim_start();
            if let Some(rest) = body.strip_prefix(',') {
                body = rest;
            } else if !body.starts_with(']') {
                return Err(format!("line {line}: expected , or ] in an array"));
            }
        }
    }
    let token = raw.split('#').next().unwrap_or_default().trim();
    match token {
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        _ => token
            .replace('_', "")
            .parse::<u64>()
            .map(Value::Number)
            .map_err(|_| {
                format!("line {line}: {token:?} is not a string, number, boolean or array")
            }),
    }
}
impl Config {
    pub fn parse(source: &str) -> Result<Self, String> {
        let mut values = BTreeMap::new();
        let mut section = String::new();
        for (i, raw) in source.lines().enumerate() {
            let line = i + 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some(name) = trimmed.strip_prefix('[') {
                let name = name
                    .split('#')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .strip_suffix(']')
                    .ok_or_else(|| format!("line {line}: a section header ends with ]"))?
                    .trim();
                if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(format!("line {line}: section name {name:?}"));
                }
                section = name.to_string();
                continue;
            }
            let (key, rest) = trimmed
                .split_once('=')
                .ok_or_else(|| format!("line {line}: expected key = value"))?;
            let key = key.trim();
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(format!("line {line}: key {key:?}"));
            }
            let full = if section.is_empty() {
                key.to_string()
            } else {
                format!("{section}.{key}")
            };
            if !KEYS.contains(&full.as_str()) {
                return Err(format!("line {line}: unknown key {full}"));
            }
            if values.insert(full.clone(), value(rest, line)?).is_some() {
                return Err(format!("line {line}: {full} is set twice"));
            }
        }
        Ok(Self { values })
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        let source =
            std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&source).map_err(|e| format!("{}: {e}", path.display()))
    }
    pub fn text(&self, key: &str) -> Result<Option<String>, String> {
        match self.values.get(key) {
            None => Ok(None),
            Some(Value::Text(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("{key} must be a string")),
        }
    }
    pub fn path(&self, key: &str) -> Result<Option<PathBuf>, String> {
        Ok(self.text(key)?.map(PathBuf::from))
    }
    pub fn number(&self, key: &str) -> Result<Option<u64>, String> {
        match self.values.get(key) {
            None => Ok(None),
            Some(Value::Number(n)) => Ok(Some(*n)),
            Some(_) => Err(format!("{key} must be a number")),
        }
    }
    pub fn flag(&self, key: &str) -> Result<Option<bool>, String> {
        match self.values.get(key) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(format!("{key} must be true or false")),
        }
    }
    pub fn list(&self, key: &str) -> Result<Option<Vec<String>>, String> {
        match self.values.get(key) {
            None => Ok(None),
            Some(Value::List(l)) => Ok(Some(l.clone())),
            Some(_) => Err(format!("{key} must be an array of strings")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_the_documented_subset() {
        let c = Config::parse(
            r#"
# node
datadir = "/data/quai"   # where
network = "colosseum"
log_level = "debug"
[rpc]
listen = "127.0.0.1:8547"
[sync]
stateless = true
jobs = 3
checkpoint_every = 10_000
peers = ["/ip4/1.2.3.4/tcp/4002", "a \"b\""]
"#,
        )
        .unwrap();
        assert_eq!(
            c.path("datadir").unwrap(),
            Some(PathBuf::from("/data/quai"))
        );
        assert_eq!(
            c.text("rpc.listen").unwrap().as_deref(),
            Some("127.0.0.1:8547")
        );
        assert_eq!(c.flag("sync.stateless").unwrap(), Some(true));
        assert_eq!(c.number("sync.jobs").unwrap(), Some(3));
        assert_eq!(c.number("sync.checkpoint_every").unwrap(), Some(10_000));
        assert_eq!(
            c.list("sync.peers").unwrap(),
            Some(vec!["/ip4/1.2.3.4/tcp/4002".into(), "a \"b\"".into()])
        );
        assert_eq!(c.text("metrics.listen").unwrap(), None);
        assert!(c.number("datadir").is_err());
        for bad in [
            "datadir = /x",
            "dataddir = \"/x\"",
            "[rpc\nlisten = \"a\"",
            "network = \"a\"\nnetwork = \"b\"",
            "[sync]\njobs = 3 4",
            "[sync]\npeers = [\"a\" \"b\"]",
            "network = \"open",
        ] {
            assert!(Config::parse(bad).is_err(), "{bad}");
        }
    }
}
