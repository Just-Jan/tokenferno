//! Unix-socket IPC broker. Allows a single TUI to receive `IngestMessage`
//! events from any number of `tokenferno exec ...` wrappers running in
//! other terminals, so the user only ever needs one dashboard open.
//!
//! Wire format: newline-delimited JSON (one `IngestMessage` per line) over a
//! Unix stream socket. The TUI binds the socket; wrappers connect and write.
//! If no TUI is running, `send` retries with backoff but never crashes the
//! caller — the wrapper still functions standalone.

use crate::ingest::{EventSender, IngestMessage};
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

/// Default socket path: `~/.tokenferno/sock`. Falls back to a relative
/// path if the home directory can't be resolved.
pub fn default_socket_path() -> PathBuf {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".tokenferno"))
        .unwrap_or_else(|| PathBuf::from(".tokenferno"))
        .join("sock")
}

/// Ensure the parent directory exists with 0700 perms (best-effort on unix).
fn ensure_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(parent) {
                let mut perms = meta.permissions();
                if perms.mode() & 0o777 != 0o700 {
                    perms.set_mode(0o700);
                    let _ = std::fs::set_permissions(parent, perms);
                }
            }
        }
    }
    Ok(())
}

/// Bind a Unix socket at `path` and forward every JSON-line message received
/// on every connection into `tx`. Removes any pre-existing socket file (best
/// effort) so a stale path from a previous crashed run doesn't block bind.
pub async fn serve(path: &Path, tx: EventSender) -> Result<()> {
    ensure_parent(path).ok();
    // Stale socket file may exist from a previous crashed TUI.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).map_err(|e| anyhow!("ipc bind {path:?}: {e}"))?;
    tracing::info!(?path, "ipc: listening");
    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "ipc accept failed");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let reader = BufReader::new(stream);
            let mut lines = reader.lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<IngestMessage>(trimmed) {
                            Ok(msg) => {
                                if tx.send(msg).await.is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "ipc: malformed line");
                            }
                        }
                    }
                    Ok(None) => return,
                    Err(e) => {
                        tracing::debug!(error = %e, "ipc connection read error");
                        return;
                    }
                }
            }
        });
    }
}

/// Send messages from `rx` to the socket at `path`. If the connection drops
/// or the socket isn't there, reconnect with exponential backoff capped at
/// 1 s. Drops messages while disconnected (the wrapper's local fallback path
/// still surfaces them on stderr).
pub async fn send_from_rx(path: PathBuf, mut rx: mpsc::Receiver<IngestMessage>) -> Result<()> {
    let mut backoff_ms: u64 = 50;
    loop {
        let stream = match UnixStream::connect(&path).await {
            Ok(s) => {
                backoff_ms = 50;
                tracing::debug!(?path, "ipc: connected");
                s
            }
            Err(_e) => {
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms * 2).min(1_000);
                // Drain any backlog beyond what would feel "live" to a TUI
                // user; otherwise the queue grows unboundedly when no TUI is
                // ever started.
                while rx.try_recv().is_ok() {}
                // Wait for next message before retrying; if the channel is
                // closed, exit cleanly.
                match rx.recv().await {
                    Some(_) => continue,
                    None => return Ok(()),
                }
            }
        };
        let (_, mut w) = stream.into_split();
        // Forward loop; on broken pipe, fall back to the reconnect loop.
        while let Some(msg) = rx.recv().await {
            let mut line = match serde_json::to_string(&msg) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "ipc: serialize failed");
                    continue;
                }
            };
            line.push('\n');
            if let Err(e) = w.write_all(line.as_bytes()).await {
                tracing::debug!(error = %e, "ipc: write failed, will reconnect");
                break;
            }
            if let Err(e) = w.flush().await {
                tracing::debug!(error = %e, "ipc: flush failed, will reconnect");
                break;
            }
        }
        // If rx is closed, exit; otherwise reconnect.
        if rx.is_closed() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_receive_round_trip() {
        // Allocate a unique socket path under target/.
        let base = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("ipc-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let sock = base.join("test.sock");

        let (event_tx, mut event_rx) = mpsc::channel::<IngestMessage>(16);
        let server_path = sock.clone();
        let server = tokio::spawn(async move {
            let _ = serve(&server_path, event_tx).await;
        });
        // Give the listener a moment to bind.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let (snd_tx, snd_rx) = mpsc::channel::<IngestMessage>(8);
        let client_path = sock.clone();
        let client = tokio::spawn(async move {
            let _ = send_from_rx(client_path, snd_rx).await;
        });

        snd_tx
            .send(IngestMessage::Activity {
                source: "claude".into(),
            })
            .await
            .unwrap();
        snd_tx
            .send(IngestMessage::RequestStart {
                source: "copilot".into(),
            })
            .await
            .unwrap();
        drop(snd_tx); // close sender so client task exits cleanly

        let mut got = Vec::new();
        for _ in 0..2 {
            match tokio::time::timeout(Duration::from_secs(2), event_rx.recv()).await {
                Ok(Some(m)) => got.push(m),
                _ => break,
            }
        }
        assert_eq!(got.len(), 2);
        match &got[0] {
            IngestMessage::Activity { source } => assert_eq!(source, "claude"),
            other => panic!("unexpected first message: {other:?}"),
        }
        match &got[1] {
            IngestMessage::RequestStart { source } => assert_eq!(source, "copilot"),
            other => panic!("unexpected second message: {other:?}"),
        }

        server.abort();
        client.abort();
        let _ = std::fs::remove_dir_all(&base);
    }
}
