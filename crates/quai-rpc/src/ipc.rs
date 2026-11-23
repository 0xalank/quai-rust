//! Unix Domain Socket IPC JSON-RPC server (`--ipc-path`), serving newline-
//! delimited JSON-RPC with full local namespace access (`admin`, `debug`,
//! `miner`, `quai`, `eth`, `net`, `txpool`, `workshare`, `web3`, `rpc`).
use crate::{App, TransportKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Semaphore;

const MAX_IPC_CLIENTS: usize = 64;

/// Serve newline-delimited JSON-RPC on a Unix domain socket until `shutdown`.
#[cfg(unix)]
pub async fn serve_ipc(
    app: Arc<App>,
    path: PathBuf,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("ipc create dir {}: {e}", parent.display()))?;
        }
    }
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    let listener = tokio::net::UnixListener::bind(&path)
        .map_err(|e| format!("ipc bind {}: {e}", path.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    let permits = Arc::new(Semaphore::new(MAX_IPC_CLIENTS));
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    continue;
                };
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let app = app.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    handle_ipc_conn(app, stream).await;
                });
            }
        }
    }
    cleanup_socket(&path);
    Ok(())
}

#[cfg(unix)]
async fn handle_ipc_conn(app: Arc<App>, stream: tokio::net::UnixStream) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let max_body = app.config.max_body_bytes;
    let mut line = Vec::with_capacity(1024);
    loop {
        line.clear();
        let read_res = read_bounded(&mut reader, &mut line, max_body).await;
        match read_res {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if line.iter().all(|b| b.is_ascii_whitespace()) {
                    continue;
                }
                if let Some(mut reply) = app.handle_transport(&line, TransportKind::Ipc).await {
                    reply.push(b'\n');
                    if write_half.write_all(&reply).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(unix)]
async fn read_bounded<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max_len: usize,
) -> std::io::Result<usize> {
    let mut total = 0usize;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(total);
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            let take = pos + 1;
            if total + take > max_len {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ipc frame exceeds max_body_bytes",
                ));
            }
            buf.extend_from_slice(&available[..take]);
            reader.consume(take);
            return Ok(total + take);
        }
        let len = available.len();
        if total + len > max_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "ipc frame exceeds max_body_bytes",
            ));
        }
        buf.extend_from_slice(available);
        reader.consume(len);
        total += len;
    }
}

fn cleanup_socket(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(not(unix))]
pub async fn serve_ipc(
    _app: Arc<App>,
    _path: PathBuf,
    _shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    Err("Unix domain socket IPC is only supported on Unix platforms".into())
}
