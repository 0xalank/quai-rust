//! Forwarding to `--txpool.sharing-clients` as go-quai v0.57.0 does
//! (`TxPool.SendTxToSharingClients`, `quaiclient.SendTransactionToPoolSharingClient`):
//! a JSON-RPC call `quai_receiveTxFromPoolSharingClient` with the
//! transaction's protobuf in hex, over HTTP, with a 2-second timeout. Each
//! client numbers its requests from 1, as go-quai's `rpc.Client` does.
//!
//! The request is go-quai's byte for byte in its body, method, path,
//! `Content-Type`, `Accept` and `User-Agent`. Error texts follow Go for the
//! answers a go-quai node gives (a JSON-RPC error, an HTTP status, no
//! result), a refused connection and the timeout; for a body that is not
//! JSON, only Go's first-character messages are reproduced. go-quai also
//! dials `ws://` endpoints; this client takes `http://` only.
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// `txSharingPoolTimeout`.
pub const TIMEOUT: Duration = Duration::from_secs(2);
const METHOD: &str = "quai_receiveTxFromPoolSharingClient";

pub struct Client {
    pub url: String,
    authority: String,
    path: String,
    next_id: AtomicU32,
}

impl Client {
    pub fn new(url: &str) -> Result<Self, String> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("sharing client {url}: only http:// endpoints are supported"))?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return Err(format!("sharing client {url}: no host"));
        }
        let authority = if authority.contains(':') {
            authority.to_string()
        } else {
            format!("{authority}:80")
        };
        Ok(Self {
            url: url.to_string(),
            authority,
            path: path.to_string(),
            next_id: AtomicU32::new(0),
        })
    }

    /// The JSON body of the next request.
    pub fn body(&self, proto: &[u8]) -> String {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{METHOD}\",\"params\":[\"{}\"]}}",
            quai_primitives::encode_hex(proto)
        )
    }

    /// Send one transaction (its canonical protobuf) and wait for the
    /// answer, at most `TIMEOUT`.
    pub async fn send(&self, proto: &[u8]) -> Result<(), String> {
        let body = self.body(proto);
        match tokio::time::timeout(TIMEOUT, self.post(&body)).await {
            Ok(r) => r,
            Err(_) => Err(format!("Post \"{}\": context deadline exceeded", self.url)),
        }
    }

    async fn post(&self, body: &str) -> Result<(), String> {
        let mut stream = tokio::net::TcpStream::connect(&self.authority)
            .await
            .map_err(|e| self.dial_error(&e))?;
        let host = self.authority.trim_end_matches(":80");
        let request = format!(
            "POST {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Go-http-client/1.1\r\nContent-Length: {}\r\nAccept: application/json\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
            self.path,
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| format!("Post \"{}\": {e}", self.url))?;
        let mut raw = vec![];
        stream
            .read_to_end(&mut raw)
            .await
            .map_err(|e| format!("Post \"{}\": {e}", self.url))?;
        let (status, body) =
            parse_response(&raw).map_err(|e| format!("Post \"{}\": {e}", self.url))?;
        let code: u16 = status
            .split_whitespace()
            .next()
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        if !(200..300).contains(&code) {
            // rpc.HTTPError.
            return Err(if body.is_empty() {
                status
            } else {
                format!("{status}: {}", String::from_utf8_lossy(&body))
            });
        }
        answer(&body)
    }

    fn dial_error(&self, e: &std::io::Error) -> String {
        let reason = match e.kind() {
            std::io::ErrorKind::ConnectionRefused => "connect: connection refused".to_string(),
            _ => e.to_string(),
        };
        format!(
            "Post \"{}\": dial tcp {}: {reason}",
            self.url, self.authority
        )
    }
}

/// The status text ("500 Internal Server Error") and body of an HTTP/1.1
/// response (Content-Length, chunked, or to the end of the stream).
fn parse_response(raw: &[u8]) -> Result<(String, Vec<u8>), String> {
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("malformed HTTP response")?;
    let head = String::from_utf8_lossy(&raw[..end]);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_once(' ')
        .map(|(_, s)| s.to_string())
        .unwrap_or_default();
    let mut chunked = false;
    let mut length = None;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            if k == "transfer-encoding" && v.eq_ignore_ascii_case("chunked") {
                chunked = true;
            }
            if k == "content-length" {
                length = v.parse::<usize>().ok();
            }
        }
    }
    let rest = &raw[end + 4..];
    let body = if chunked {
        let mut out = vec![];
        let mut p = rest;
        loop {
            let nl = p
                .windows(2)
                .position(|w| w == b"\r\n")
                .ok_or("malformed chunk")?;
            let size = usize::from_str_radix(
                String::from_utf8_lossy(&p[..nl])
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim(),
                16,
            )
            .map_err(|_| "malformed chunk")?;
            p = &p[nl + 2..];
            if size == 0 {
                break;
            }
            out.extend_from_slice(p.get(..size).ok_or("malformed chunk")?);
            p = p.get(size + 2..).unwrap_or_default();
        }
        out
    } else {
        match length {
            Some(n) => rest.get(..n).unwrap_or(rest).to_vec(),
            None => rest.to_vec(),
        }
    };
    Ok((status, body))
}

/// The result of a JSON-RPC answer as `Client.CallContext` reports it.
fn answer(body: &[u8]) -> Result<(), String> {
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return Err(go_json_error(body)),
    };
    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
        let message = e.get("message").and_then(|m| m.as_str()).unwrap_or("");
        if message.is_empty() {
            let code = e.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
            return Err(format!("json-rpc error {code}"));
        }
        return Err(message.to_string());
    }
    if v.get("result").is_none() {
        return Err("no result in JSON-RPC response".into());
    }
    Ok(())
}

/// encoding/json's error for a body that does not start a JSON value.
fn go_json_error(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let t = text.trim_start();
    let Some(c) = t.chars().next() else {
        return "EOF".into();
    };
    let literal = match c {
        'n' => Some("null"),
        't' => Some("true"),
        'f' => Some("false"),
        _ => None,
    };
    if let Some(word) = literal {
        for (have, want) in t.chars().zip(word.chars()).skip(1) {
            if have != want {
                return format!(
                    "invalid character '{have}' in literal {word} (expecting '{want}')"
                );
            }
        }
        return "unexpected EOF".into();
    }
    if "{[\"-0123456789".contains(c) {
        return "unexpected EOF".into();
    }
    format!("invalid character '{c}' looking for beginning of value")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn answers_and_errors_match_go() {
        assert_eq!(answer(br#"{"jsonrpc":"2.0","id":1,"result":null}"#), Ok(()));
        assert_eq!(
            answer(br#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"nope"}}"#),
            Err("nope".into())
        );
        assert_eq!(
            answer(br#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":""}}"#),
            Err("json-rpc error -32000".into())
        );
        assert_eq!(
            answer(br#"{"jsonrpc":"2.0","id":2}"#),
            Err("no result in JSON-RPC response".into())
        );
        assert_eq!(
            answer(b"not json"),
            Err("invalid character 'o' in literal null (expecting 'u')".into())
        );
        let c = Client::new("http://127.0.0.1:9/x").unwrap();
        assert_eq!(
            c.body(&[1, 2]),
            r#"{"jsonrpc":"2.0","id":1,"method":"quai_receiveTxFromPoolSharingClient","params":["0x0102"]}"#
        );
        assert!(c.body(&[]).contains(r#""id":2"#));
        assert!(Client::new("ws://127.0.0.1:9").is_err());
    }
}
