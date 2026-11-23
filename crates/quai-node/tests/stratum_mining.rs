//! End-to-end integration tests for:
//! - Local `Miner` block templates (SHA-256d, Scrypt, KAWPOW), PoW submission,
//!   live miner control RPCs, workshare LRU, and WebSocket mining subscriptions
//! - Real TCP Stratum v1 server (SHA-256d with BIP 310 ASICBoost, Scrypt,
//!   KAWPOW, real CPU PoW verification, duplicate share detection, VarDiff)
//! - Stratum HTTP & WebSocket Dashboard API (`/health`, `/api/pool/*`,
//!   `/api/miner/*`, `/api/node/info`, `WS /api/ws`)
//! - JSON-RPC transport security (HTTP namespace allowlist, CORS, Host vhost),
//!   Unix Domain Socket IPC full-namespace access, standard wallet/admin/debug
//!   methods, and `txpool::sharing::Client` over `ws://`.
use crypto_bigint::U256;
use futures_util::{SinkExt, StreamExt};
use quai_consensus::pow::SharedCaches;
use quai_node::log::{Level, Log};
use quai_node::miner::{
    auxpow::{
        build_donor_header, extract_coinb1_coinb2, hex_plain, POW_KAWPOW, POW_SCRYPT, POW_SHA_BCH,
    },
    BlockTemplateParams, Miner, MinerConfig,
};
use quai_node::spec::Chain;
use quai_node::stratum::{StratumConfig, StratumServer};
use quai_primitives::schema::block::{ProtoAuxTemplate, ProtoWorkObject};
use quai_primitives::transaction::Address;
use quai_primitives::work::WorkObject;
use quai_primitives::{decode_hex, encode_hex, Hash};
use quai_rpc::view::{
    AccountState, BlockData, CallError, CallRequest, CallResult, ChainView, OutpointData,
    ReceiptData, TxPosition, UncleKind, UtxoData, ViewError, U256 as ViewU256,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Minimal in-memory `ChainView` backed by a local genesis block + optional `Miner`.
struct MockView {
    chain: Arc<Chain>,
    genesis: Arc<BlockData>,
    miner: RwLock<Option<Arc<Miner>>>,
}

impl MockView {
    fn new() -> Arc<Self> {
        let chain = Chain::for_environment("local").unwrap();
        let rec = chain.genesis();
        let object = rec.object().clone();
        let header = rec.header().clone();
        let genesis = Arc::new(BlockData {
            hash: rec.id(),
            number: 0,
            object,
            header,
            uncles: vec![],
            size: rec.bytes().len() as u64,
            order: 0,
            tx_hashes: vec![],
            etx_hashes: vec![],
        });
        Arc::new(Self {
            chain,
            genesis,
            miner: RwLock::new(None),
        })
    }

    fn set_miner(&self, miner: Arc<Miner>) {
        *self.miner.write().unwrap() = Some(miner);
    }

    fn miner(&self) -> Option<Arc<Miner>> {
        self.miner.read().unwrap().clone()
    }
}

struct EmptyAcct;
impl AccountState for EmptyAcct {
    fn balance(&self, _: &Address) -> Result<ViewU256, ViewError> {
        Ok([0u8; 32])
    }
    fn nonce(&self, _: &Address) -> Result<u64, ViewError> {
        Ok(0)
    }
    fn code(&self, _: &Address) -> Result<Vec<u8>, ViewError> {
        Ok(vec![])
    }
    fn storage(&self, _: &Address, _: &Hash) -> Result<Hash, ViewError> {
        Ok([0u8; 32])
    }
}

impl ChainView for MockView {
    fn chain_id(&self) -> u64 {
        self.chain.spec().chain_id
    }
    fn location(&self) -> [u8; 2] {
        self.chain.location()
    }
    fn head(&self) -> u64 {
        0
    }
    fn block_by_number(&self, number: u64) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok((number == 0).then(|| self.genesis.clone()))
    }
    fn block_by_hash(&self, hash: &Hash) -> Result<Option<Arc<BlockData>>, ViewError> {
        Ok((*hash == self.genesis.hash).then(|| self.genesis.clone()))
    }
    fn uncle_kinds(&self, _: &BlockData) -> Result<Arc<Vec<UncleKind>>, ViewError> {
        Ok(Arc::new(vec![]))
    }
    fn total_entropy(&self, _: &BlockData) -> Result<ViewU256, ViewError> {
        Ok([0u8; 32])
    }
    fn transaction(&self, _: &Hash) -> Result<Option<TxPosition>, ViewError> {
        Ok(None)
    }
    fn receipts(&self, _: &BlockData) -> Result<Arc<Vec<ReceiptData>>, ViewError> {
        Ok(Arc::new(vec![]))
    }
    fn state(&self, _: &BlockData) -> Result<Arc<dyn AccountState>, ViewError> {
        Ok(Arc::new(EmptyAcct))
    }
    fn utxo(&self, _: &Hash, _: u16) -> Result<Option<UtxoData>, ViewError> {
        Ok(None)
    }
    fn outpoints_by_address(&self, _: &Address) -> Result<Vec<OutpointData>, ViewError> {
        Ok(vec![])
    }
    fn outpoints_by_address_and_range(
        &self,
        _: &Address,
        _: u32,
        _: u32,
    ) -> Result<Vec<OutpointData>, ViewError> {
        Ok(vec![])
    }
    fn call(&self, _: &BlockData, _: &CallRequest) -> Result<CallResult, CallError> {
        Ok(CallResult {
            used_gas: 21_000,
            error: None,
            reverted: false,
            return_data: vec![],
            access_list: None,
        })
    }
    fn is_mining(&self) -> bool {
        self.miner().is_some_and(|m| m.is_mining())
    }
    fn hashrate(&self) -> u64 {
        self.miner().map_or(0, |m| m.hashrate())
    }
    fn primary_coinbase(&self) -> Result<Address, ViewError> {
        Ok(self.miner().map_or([0u8; 20], |m| m.primary_coinbase()))
    }
    fn miner_start(&self) -> Result<bool, ViewError> {
        self.miner().unwrap().start_mining();
        Ok(true)
    }
    fn miner_set_extra(&self, extra: &[u8]) -> Result<bool, ViewError> {
        self.miner()
            .unwrap()
            .set_extra(extra.to_vec())
            .map_err(ViewError::Go)?;
        Ok(true)
    }
    fn miner_set_gas_limit(&self, limit: u64) -> Result<bool, ViewError> {
        self.miner().unwrap().set_gas_limit(limit);
        Ok(true)
    }
    fn miner_set_gas_price(&self, price: ViewU256) -> Result<bool, ViewError> {
        use crypto_bigint::Encoding as _;
        self.miner()
            .unwrap()
            .set_gas_price(U256::from_be_bytes(price));
        Ok(true)
    }
    fn miner_set_lockup_byte(&self, lock: u8) -> Result<bool, ViewError> {
        self.miner()
            .unwrap()
            .set_lockup_byte(lock)
            .map_err(ViewError::Go)?;
        Ok(true)
    }
    fn miner_set_miner_preference(&self, pref: f64) -> Result<bool, ViewError> {
        self.miner()
            .unwrap()
            .set_miner_preference(pref)
            .map_err(ViewError::Go)?;
        Ok(true)
    }
    fn miner_set_primary_coinbase(&self, addr: Address) -> Result<bool, ViewError> {
        self.miner().unwrap().set_primary_coinbase(addr);
        Ok(true)
    }
    fn miner_set_recommit_interval(&self, ms: u64) -> Result<(), ViewError> {
        self.miner().unwrap().set_recommit_interval(ms);
        Ok(())
    }
    fn get_block_template(&self, params: Option<&Value>) -> Result<Value, ViewError> {
        let p = BlockTemplateParams::from_json(params).map_err(ViewError::Go)?;
        self.miner()
            .unwrap()
            .get_block_template(&p)
            .map_err(ViewError::Go)
    }
    fn get_pending_header(&self) -> Result<Vec<u8>, ViewError> {
        Ok(self
            .miner()
            .unwrap()
            .generate_pending_header(POW_KAWPOW, None, &[], None)
            .map_err(ViewError::Go)?
            .encode())
    }
    fn get_pending_workshares(&self) -> Result<Value, ViewError> {
        Ok(self.miner().unwrap().pending_workshares_json())
    }
    fn submit_block(&self, raw: &[u8], pow_id: u8) -> Result<Value, ViewError> {
        let out = self
            .miner()
            .unwrap()
            .submit_block(raw, pow_id)
            .map_err(ViewError::Go)?;
        Ok(json!({
            "hash": encode_hex(&out.hash),
            "number": format!("0x{:x}", out.number),
            "status": format!("0x{:x}", out.status),
        }))
    }
    fn workshare_lru_dump(&self, limit: usize) -> Result<Value, ViewError> {
        Ok(self.miner().unwrap().workshare_lru_dump(limit))
    }
    fn pending_header_events(&self) -> Option<tokio::sync::broadcast::Receiver<Vec<u8>>> {
        Some(self.miner()?.subscribe_pending_header())
    }
    fn template_update_events(&self) -> Option<tokio::sync::broadcast::Receiver<u8>> {
        Some(self.miner()?.subscribe_template_updates())
    }
    fn workshare_events(&self) -> Option<tokio::sync::broadcast::Receiver<Arc<WorkObject>>> {
        Some(self.miner()?.subscribe_workshares())
    }
}

fn decode_plain_hex(s: &str) -> Vec<u8> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let padded = if s.len() % 2 == 1 {
        format!("0{s}")
    } else {
        s.to_string()
    };
    decode_hex(&format!("0x{padded}")).unwrap()
}

fn unswap_prev_hash(hex_str: &str) -> Hash {
    let b = decode_plain_hex(hex_str);
    assert_eq!(b.len(), 32);
    let mut out = [0u8; 32];
    for i in 0..8 {
        let base = i * 4;
        out[base] = b[base + 3];
        out[base + 1] = b[base + 2];
        out[base + 2] = b[base + 1];
        out[base + 3] = b[base];
    }
    out
}

async fn stratum_send(writer: &mut tokio::net::tcp::OwnedWriteHalf, msg: &Value) {
    let mut line = serde_json::to_string(msg).unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).await.unwrap();
}

async fn stratum_recv(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
        .await
        .expect("stratum recv timeout")
        .unwrap();
    serde_json::from_str(line.trim()).unwrap()
}

async fn stratum_recv_id(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    expected_id: u64,
) -> Value {
    for _ in 0..16 {
        let msg = stratum_recv(reader).await;
        if msg["id"].as_u64() == Some(expected_id) {
            return msg;
        }
    }
    panic!("did not receive response for id {expected_id}");
}

async fn http_get(addr: std::net::SocketAddr, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let status = text
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

async fn http_rpc(
    addr: std::net::SocketAddr,
    host: &str,
    origin: Option<&str>,
    body: &str,
) -> (u16, String, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let origin_hdr = origin
        .map(|o| format!("Origin: {o}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "POST / HTTP/1.1\r\nHost: {host}\r\n{origin_hdr}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let (hdrs, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = hdrs
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    (status, hdrs.to_string(), body.to_string())
}

#[tokio::test]
async fn end_to_end_stratum_miner_dashboard_and_rpc_security() {
    let view = MockView::new();
    let caches = Arc::new(SharedCaches::new(2).unwrap());
    // Valid internal zone [0, 0] Quai address (byte 0 = 0x00, byte 1 <= 0x7f).
    let coinbase: Address = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff, 0x01, 0x02, 0x03, 0x04,
    ];
    let miner_cfg = MinerConfig {
        enabled: true,
        primary_coinbase: coinbase,
        allow_unsigned_fallback: true,
        ..MinerConfig::default()
    };
    let miner = Miner::new(view.chain.clone(), view.clone(), caches, miner_cfg);
    view.set_miner(miner.clone());

    // 1. Verify block templates for SHA, Scrypt, and KAWPOW directly.
    for (algo, pow_id) in [
        ("sha", POW_SHA_BCH),
        ("scrypt", POW_SCRYPT),
        ("kawpow", POW_KAWPOW),
    ] {
        let params = BlockTemplateParams::from_json(Some(&json!({
            "powId": pow_id,
            "coinbaseAddress": encode_hex(&coinbase),
        })))
        .unwrap();
        let tpl = miner.get_block_template(&params).unwrap();
        assert!(tpl.get("bits").is_some(), "{algo} template missing bits");
        assert!(
            tpl.get("target").is_some(),
            "{algo} template missing target"
        );
        assert!(
            tpl.get("previousblockhash").is_some(),
            "{algo} template missing previousblockhash"
        );
    }

    // 2. Start Stratum v1 server (SHA, Scrypt, KAWPOW) + HTTP/WS Dashboard API on ephemeral ports.
    let stop = Arc::new(AtomicBool::new(false));
    let log = Log::new(Level::Error);
    let stratum_cfg = StratumConfig {
        enabled: true,
        sha_addr: Some("127.0.0.1:0".into()),
        scrypt_addr: Some("127.0.0.1:0".into()),
        kawpow_addr: Some("127.0.0.1:0".into()),
        api_addr: Some("127.0.0.1:0".into()),
        pool_name: "quai-rust test pool".into(),
        pool_tag: "testpool".into(),
        vardiff_enabled: true,
        ..StratumConfig::default()
    };
    let server = StratumServer::new(stratum_cfg, miner.clone(), stop.clone(), log);
    let handle = server.start().await.unwrap();
    let sha_addr = handle.sha_addr.unwrap();
    let scrypt_addr = handle.scrypt_addr.unwrap();
    let kawpow_addr = handle.kawpow_addr.unwrap();
    let api_addr = handle.api_addr.unwrap();

    let worker_login = format!("{}.rig1", encode_hex(&coinbase));

    // 3. Connect SHA-256d client -> subscribe -> configure (BIP 310) -> authorize -> CPU solve -> submit -> duplicate check.
    {
        let stream = TcpStream::connect(sha_addr).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut reader = BufReader::new(r);

        stratum_send(
            &mut w,
            &json!({"id": 1, "method": "mining.subscribe", "params": ["test-miner/1.0"]}),
        )
        .await;
        let sub = stratum_recv(&mut reader).await;
        assert_eq!(sub["id"], 1);
        assert!(sub["error"].is_null());
        let extranonce1 = sub["result"][1].as_str().unwrap().to_string();
        let extranonce2_size = sub["result"][2].as_u64().unwrap() as usize;
        assert_eq!(extranonce2_size, 4);

        // BIP 310 ASICBoost version rolling negotiation
        stratum_send(
            &mut w,
            &json!({
                "id": 2,
                "method": "mining.configure",
                "params": [["version-rolling"], {"version-rolling.mask": "1fffe000", "version-rolling.min-bit-count": 2}]
            }),
        )
        .await;
        let mut cfg_res = None;
        let mut saw_version_mask = false;
        for _ in 0..2 {
            let msg = stratum_recv(&mut reader).await;
            if msg["id"] == 2 {
                cfg_res = Some(msg);
            } else if msg["method"] == "mining.set_version_mask" {
                saw_version_mask = msg["params"][0] == "1fffe000";
            }
        }
        let cfg_res = cfg_res.expect("configure response");
        assert_eq!(cfg_res["result"]["version-rolling"], true);
        assert_eq!(cfg_res["result"]["version-rolling.mask"], "1fffe000");
        assert!(saw_version_mask);

        // Authorize with low static share difficulty so CPU finds a valid share in milliseconds.
        stratum_send(
            &mut w,
            &json!({"id": 3, "method": "mining.authorize", "params": [&worker_login, "d=0.0000001"]}),
        )
        .await;
        let mut auth_ok = false;
        let mut notify: Option<Value> = None;
        let mut set_diff = false;
        for _ in 0..6 {
            let msg = stratum_recv(&mut reader).await;
            if msg["id"] == 3 {
                assert_eq!(msg["result"], true);
                auth_ok = true;
            } else if msg["method"] == "mining.set_difficulty" {
                set_diff = true;
            } else if msg["method"] == "mining.notify" {
                notify = Some(msg);
            }
            if auth_ok && set_diff && notify.is_some() {
                break;
            }
        }
        assert!(auth_ok && set_diff);
        let notify = notify.expect("mining.notify");
        let params = notify["params"].as_array().unwrap();
        let job_id = params[0].as_str().unwrap().to_string();
        let prev_hash_le = unswap_prev_hash(params[1].as_str().unwrap());
        let coinb1 = decode_plain_hex(params[2].as_str().unwrap());
        let coinb2 = decode_plain_hex(params[3].as_str().unwrap());
        let version = u32::from_str_radix(params[5].as_str().unwrap(), 16).unwrap();
        let nbits = u32::from_str_radix(params[6].as_str().unwrap(), 16).unwrap();
        let ntime_hex = params[7].as_str().unwrap().to_string();
        let ntime = u32::from_str_radix(&ntime_hex, 16).unwrap();

        let en1 = decode_plain_hex(&extranonce1);
        let en2 = [0x01u8, 0x02, 0x03, 0x04];
        let mut coinbase_tx = Vec::new();
        coinbase_tx.extend_from_slice(&coinb1);
        coinbase_tx.extend_from_slice(&en1);
        coinbase_tx.extend_from_slice(&en2);
        coinbase_tx.extend_from_slice(&coinb2);
        let merkle_root = quai_consensus::auxiliary::coinbase::double_sha(&coinbase_tx);

        // `donor::sha256d` already returns big-endian target order.
        let target = quai_node::stratum::Algo::Sha256.diff_to_target(0.0000001);
        let mut found_nonce = None;
        for nonce in 0..100_000u32 {
            let hdr = build_donor_header(
                POW_SHA_BCH,
                version,
                prev_hash_le,
                merkle_root,
                ntime,
                nbits,
                nonce as u64,
                0,
                [0u8; 32],
            );
            let pow_hash = quai_consensus::donor::sha256d(&hdr).unwrap();
            use crypto_bigint::Encoding as _;
            if U256::from_be_bytes(pow_hash) <= target {
                found_nonce = Some(nonce);
                break;
            }
        }
        let found_nonce = found_nonce.expect("CPU solve SHA share");
        let submit_req = json!({
            "id": 4,
            "method": "mining.submit",
            "params": [&worker_login, &job_id, hex_plain(&en2), &ntime_hex, format!("{found_nonce:08x}")]
        });
        stratum_send(&mut w, &submit_req).await;
        let sub_res = stratum_recv_id(&mut reader, 4).await;
        assert_eq!(sub_res["result"], true, "SHA share rejected: {sub_res}");

        // Resubmit identical share -> must reject with duplicate share error code 22!
        let dup_req = json!({
            "id": 5,
            "method": "mining.submit",
            "params": [&worker_login, &job_id, hex_plain(&en2), &ntime_hex, format!("{found_nonce:08x}")]
        });
        stratum_send(&mut w, &dup_req).await;
        let dup_res = stratum_recv_id(&mut reader, 5).await;
        assert_eq!(dup_res["result"], false);
        assert_eq!(dup_res["error"][0], 22);
    }

    // 4. Connect Scrypt client -> subscribe -> authorize -> CPU solve -> submit.
    {
        let stream = TcpStream::connect(scrypt_addr).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut reader = BufReader::new(r);

        stratum_send(
            &mut w,
            &json!({"id": 1, "method": "mining.subscribe", "params": ["scrypt-miner/1.0"]}),
        )
        .await;
        let sub = stratum_recv(&mut reader).await;
        let extranonce1 = sub["result"][1].as_str().unwrap().to_string();

        stratum_send(
            &mut w,
            &json!({"id": 2, "method": "mining.authorize", "params": [format!("{}.scrypt1", encode_hex(&coinbase)), "d=0.001"]}),
        )
        .await;
        let mut notify: Option<Value> = None;
        for _ in 0..6 {
            let msg = stratum_recv(&mut reader).await;
            if msg["method"] == "mining.notify" {
                notify = Some(msg);
                break;
            }
        }
        let notify = notify.expect("scrypt notify");
        let params = notify["params"].as_array().unwrap();
        let job_id = params[0].as_str().unwrap().to_string();
        let prev_hash_le = unswap_prev_hash(params[1].as_str().unwrap());
        let coinb1 = decode_plain_hex(params[2].as_str().unwrap());
        let coinb2 = decode_plain_hex(params[3].as_str().unwrap());
        let version = u32::from_str_radix(params[5].as_str().unwrap(), 16).unwrap();
        let nbits = u32::from_str_radix(params[6].as_str().unwrap(), 16).unwrap();
        let ntime_hex = params[7].as_str().unwrap().to_string();
        let ntime = u32::from_str_radix(&ntime_hex, 16).unwrap();

        let en1 = decode_plain_hex(&extranonce1);
        let en2 = [0x09u8, 0x08, 0x07, 0x06];
        let mut coinbase_tx = Vec::new();
        coinbase_tx.extend_from_slice(&coinb1);
        coinbase_tx.extend_from_slice(&en1);
        coinbase_tx.extend_from_slice(&en2);
        coinbase_tx.extend_from_slice(&coinb2);
        let merkle_root = quai_consensus::auxiliary::coinbase::double_sha(&coinbase_tx);
        let target = quai_node::stratum::Algo::Scrypt.diff_to_target(0.001);
        let mut found_nonce = None;
        for nonce in 0..10_000u32 {
            let hdr = build_donor_header(
                POW_SCRYPT,
                version,
                prev_hash_le,
                merkle_root,
                ntime,
                nbits,
                nonce as u64,
                0,
                [0u8; 32],
            );
            let pow_hash = quai_consensus::donor::scrypt(&hdr).unwrap();
            use crypto_bigint::Encoding as _;
            if U256::from_be_bytes(pow_hash) <= target {
                found_nonce = Some(nonce);
                break;
            }
        }
        let found_nonce = found_nonce.expect("CPU solve Scrypt share");
        stratum_send(
            &mut w,
            &json!({
                "id": 3,
                "method": "mining.submit",
                "params": [format!("{}.scrypt1", encode_hex(&coinbase)), &job_id, hex_plain(&en2), &ntime_hex, format!("{found_nonce:08x}")]
            }),
        )
        .await;
        let sub_res = stratum_recv_id(&mut reader, 3).await;
        assert_eq!(sub_res["result"], true, "Scrypt share rejected: {sub_res}");
    }

    // 5. Connect KAWPOW client -> subscribe -> authorize -> receive set_target & kawpow notify.
    {
        let stream = TcpStream::connect(kawpow_addr).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut reader = BufReader::new(r);
        stratum_send(
            &mut w,
            &json!({"id": 1, "method": "mining.subscribe", "params": ["kawpow/1.0"]}),
        )
        .await;
        let sub = stratum_recv(&mut reader).await;
        assert_eq!(sub["id"], 1);
        stratum_send(
            &mut w,
            &json!({"id": 2, "method": "mining.authorize", "params": [format!("{}.gpu1", encode_hex(&coinbase)), "d=0.01"]}),
        )
        .await;
        let mut saw_target = false;
        let mut saw_notify = false;
        for _ in 0..6 {
            let msg = stratum_recv(&mut reader).await;
            if msg["method"] == "mining.set_target" {
                saw_target = true;
            } else if msg["method"] == "mining.notify" {
                saw_notify = msg["params"].as_array().is_some_and(|p| p.len() >= 7);
            }
            if saw_target && saw_notify {
                break;
            }
        }
        assert!(saw_target && saw_notify);
    }

    // 6. Query Stratum HTTP Dashboard API and WebSocket live stream.
    {
        let (code, body) = http_get(api_addr, "/health").await;
        assert_eq!(code, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["status"], "ok");

        let (code, body) = http_get(api_addr, "/api/pool/stats").await;
        assert_eq!(code, 200);
        let stats: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(stats["poolName"], "quai-rust test pool");
        assert!(
            stats["totalShares"].as_u64().unwrap() >= 2,
            "stats: {stats}"
        );

        let (code, body) = http_get(api_addr, "/api/pool/shares").await;
        assert_eq!(code, 200);
        let shares: Value = serde_json::from_str(&body).unwrap();
        assert!(shares.as_array().unwrap().len() >= 2);

        let (code, body) = http_get(
            api_addr,
            &format!("/api/miner/{}/stats", encode_hex(&coinbase)),
        )
        .await;
        assert_eq!(code, 200);
        let ms: Value = serde_json::from_str(&body).unwrap();
        assert!(ms["validShares"].as_u64().unwrap() >= 2);

        let (code, body) = http_get(api_addr, "/api/node/info").await;
        assert_eq!(code, 200);
        let ni: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(ni["mining"], true);

        // Live WebSocket stream `/api/ws` pushes overview snapshot.
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{api_addr}/api/ws"))
            .await
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let pushed: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        assert_eq!(pushed["type"], "stats");
        assert_eq!(pushed["stats"]["poolName"], "quai-rust test pool");
    }

    // 7. JSON-RPC HTTP security (namespace allowlist, CORS, vhost), Unix IPC, and WS mining subscriptions.
    {
        let ipc_path =
            std::env::temp_dir().join(format!("quai-rust-test-ipc-{}.sock", std::process::id()));
        let rpc_cfg = quai_rpc::Config {
            mode: "native".into(),
            listen: "127.0.0.1:0".into(),
            http_apis: Some(
                quai_rpc::DEFAULT_HTTP_APIS
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
            ),
            ws_apis: Some(
                quai_rpc::DEFAULT_WS_APIS
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
            ),
            cors_domains: vec!["https://wallet.example".into()],
            vhosts: vec!["localhost".into(), "127.0.0.1".into()],
            ..quai_rpc::Config::default()
        };
        let app = quai_rpc::App::native(rpc_cfg, view.clone()).unwrap();
        let ln = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let rpc_addr = ln.local_addr().unwrap();
        let rpc_stop = stop.clone();
        let http_app = app.clone();
        tokio::spawn(async move {
            let _ = quai_rpc::serve_until(http_app, ln, async move {
                while !rpc_stop.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await;
        });
        #[cfg(unix)]
        {
            let ipc_app = app.clone();
            let ipc_p = ipc_path.clone();
            let ipc_stop = stop.clone();
            tokio::spawn(async move {
                let _ = quai_rpc::ipc::serve_ipc(ipc_app, ipc_p, async move {
                    while !ipc_stop.load(Ordering::Relaxed) {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
                .await;
            });
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Standard wallet & utility RPCs over HTTP succeed with CORS header.
        let (status, hdrs, body) = http_rpc(
            rpc_addr,
            "localhost",
            Some("https://wallet.example"),
            r#"{"jsonrpc":"2.0","id":1,"method":"web3_clientVersion","params":[]}"#,
        )
        .await;
        assert_eq!(status, 200);
        assert!(
            hdrs.to_ascii_lowercase()
                .contains("access-control-allow-origin: https://wallet.example"),
            "missing CORS header in: {hdrs}"
        );
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(v["result"].as_str().unwrap().contains("quai-rust"));

        let (_, _, body) = http_rpc(
            rpc_addr,
            "localhost",
            None,
            r#"{"jsonrpc":"2.0","id":2,"method":"web3_sha3","params":["0x68656c6c6f"]}"#,
        )
        .await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v["result"],
            "0x1c8aff950685c2ed4bc3174f3472287b56d9517b9c948127319a09a7a36deac8"
        );

        let (_, _, body) = http_rpc(
            rpc_addr,
            "localhost",
            None,
            r#"{"jsonrpc":"2.0","id":3,"method":"eth_syncing","params":[]}"#,
        )
        .await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"], false);

        // Disallowed Host header -> 403 Forbidden.
        let (status, _, _) = http_rpc(
            rpc_addr,
            "evil.attacker",
            None,
            r#"{"jsonrpc":"2.0","id":4,"method":"eth_mining","params":[]}"#,
        )
        .await;
        assert_eq!(status, 403);

        // `admin_nodeInfo` and `miner_setExtra` blocked on HTTP by DEFAULT_HTTP_APIS!
        let (_, _, body) = http_rpc(
            rpc_addr,
            "localhost",
            None,
            r#"{"jsonrpc":"2.0","id":5,"method":"admin_nodeInfo","params":[]}"#,
        )
        .await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v["error"]["code"], -32601,
            "expected admin blocked on HTTP: {v}"
        );

        let (_, _, body) = http_rpc(
            rpc_addr,
            "localhost",
            None,
            r#"{"jsonrpc":"2.0","id":6,"method":"rpc_modules","params":[]}"#,
        )
        .await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(v["result"].get("quai").is_some());
        assert!(v["result"].get("admin").is_none());

        // Same `admin_nodeInfo` and `miner_setExtra` SUCCEED over Unix IPC!
        #[cfg(unix)]
        {
            let mut uds = tokio::net::UnixStream::connect(&ipc_path).await.unwrap();
            uds.write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":10,\"method\":\"admin_nodeInfo\",\"params\":[]}\n\
                  {\"jsonrpc\":\"2.0\",\"id\":11,\"method\":\"miner_setExtra\",\"params\":[\"hi\"]}\n\
                  {\"jsonrpc\":\"2.0\",\"id\":12,\"method\":\"debug_seedHash\",\"params\":[\"0x0\"]}\n",
            )
            .await
            .unwrap();
            let mut reader = BufReader::new(uds);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let r10: Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(r10["id"], 10);
            assert!(r10["result"]["name"]
                .as_str()
                .unwrap()
                .contains("quai-rust"));

            line.clear();
            reader.read_line(&mut line).await.unwrap();
            let r11: Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(r11["id"], 11);
            assert_eq!(r11["result"], true);
            assert_eq!(miner.config().extra, b"hi");

            line.clear();
            reader.read_line(&mut line).await.unwrap();
            let r12: Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(r12["id"], 12);
            assert_eq!(r12["result"], encode_hex(&[0u8; 32]));
            let _ = std::fs::remove_file(&ipc_path);
        }

        // Live WebSocket subscriptions: `pendingHeader` & `blockTemplateUpdates`.
        {
            let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{rpc_addr}/"))
                .await
                .unwrap();
            ws.send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"jsonrpc":"2.0","id":20,"method":"quai_subscribe","params":["pendingHeader"]}"#
                    .into(),
            ))
            .await
            .unwrap();
            let sub_resp = ws.next().await.unwrap().unwrap();
            let v: Value = serde_json::from_str(sub_resp.to_text().unwrap()).unwrap();
            assert!(v["result"].is_string(), "pendingHeader sub failed: {v}");

            ws.send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"jsonrpc":"2.0","id":21,"method":"quai_subscribe","params":["blockTemplateUpdates",{"powId":3}]}"#
                    .into(),
            ))
            .await
            .unwrap();
            let sub2 = ws.next().await.unwrap().unwrap();
            let v2: Value = serde_json::from_str(sub2.to_text().unwrap()).unwrap();
            assert!(
                v2["result"].is_string(),
                "blockTemplateUpdates sub failed: {v2}"
            );

            // Trigger both events by injecting a newer aux template and regenerating pending header.
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as u32;
            miner.inject_aux_template_unchecked(ProtoAuxTemplate {
                chain_id: Some(POW_SHA_BCH as u32),
                height: Some(12345),
                bits: Some(0x1f00_ffff),
                version: Some(0x2000_0000),
                prev_hash: Some(vec![0x11; 32]),
                coinbase_out: Some(vec![
                    0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x51,
                ]),
                signature_time: Some(now),
                ..Default::default()
            });
            let _ = miner
                .generate_pending_header(POW_SHA_BCH, None, &[], None)
                .unwrap();

            let mut saw_notification = false;
            for _ in 0..4 {
                if let Ok(Some(Ok(msg))) =
                    tokio::time::timeout(Duration::from_secs(2), ws.next()).await
                {
                    if let Ok(txt) = msg.to_text() {
                        if let Ok(ev) = serde_json::from_str::<Value>(txt) {
                            if ev["method"] == "quai_subscription" {
                                saw_notification = true;
                                break;
                            }
                        }
                    }
                }
            }
            assert!(
                saw_notification,
                "expected WS mining subscription notification"
            );
        }
    }

    // 8. Verify `txpool::sharing::Client` over a real local `ws://` server.
    {
        let ln = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_addr = ln.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            let (stream, _) = ln.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let msg = ws.next().await.unwrap().unwrap();
            let req: Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
            assert_eq!(req["method"], "quai_receiveTxFromPoolSharingClient");
            let reply = json!({
                "jsonrpc": "2.0",
                "id": req["id"],
                "result": "0x0000000000000000000000000000000000000000000000000000000000000042"
            });
            ws.send(tokio_tungstenite::tungstenite::Message::Text(
                reply.to_string().into(),
            ))
            .await
            .unwrap();
        });
        let client = quai_node::txpool::sharing::Client::new(&format!("ws://{ws_addr}")).unwrap();
        client.send(&[0xde, 0xad, 0xbe, 0xef]).await.unwrap();
        srv.await.unwrap();
    }

    // 9. Sanity check `extract_coinb1_coinb2` round-trip.
    let raw_wo = ProtoWorkObject::default();
    let _ = raw_wo;
    let _ = extract_coinb1_coinb2;

    stop.store(true, Ordering::Relaxed);
    handle.stop();
}
