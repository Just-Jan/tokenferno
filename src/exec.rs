//! `tokenferno exec -- <cmd> <args>...` — run an agent under a PTY,
//! tee its output to the user's terminal, and emit ingest events from the
//! stream as it arrives. This gives a sub-disk-flush activity heartbeat:
//! the wrapper sees bytes the moment the agent writes them, before any
//! file system flush / FSEvent latency kicks in.
//!
//! Pragmatic v1: this mode does NOT launch the TUI in the same process —
//! the alternate screen would clobber the agent's output. Instead, run a
//! second `tokenferno` instance in another pane to see the dashboard;
//! it will pick up the same events from disk as a fallback / complete
//! record. The PTY tee is the early signal, the disk path is the floor.

use crate::ingest::copilot::{process_chunk, ProcessState};
use crate::ingest::{claude, EventSender, IngestMessage};
use anyhow::{anyhow, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::Path;

/// Which provider parser to use for the bytes we read off the PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Claude,
    Copilot,
    Unknown,
}

/// Detect the provider from `argv[0]` (the binary name).
pub fn detect_route(argv0: &str) -> Route {
    let name = Path::new(argv0)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(argv0);
    if name.ends_with("claude") {
        Route::Claude
    } else if name.ends_with("copilot") || name.ends_with("copilot-cli") {
        Route::Copilot
    } else {
        Route::Unknown
    }
}

pub async fn run(argv: Vec<String>, tx: EventSender) -> Result<()> {
    if argv.is_empty() {
        return Err(anyhow!("exec: no command given"));
    }
    let prog = argv[0].clone();
    let route = detect_route(&prog);

    // Match the user's actual terminal size if we can; otherwise use a
    // sane default so the child program lays out reasonably.
    let (cols, rows) = crossterm::terminal::size().unwrap_or((120, 32));
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| anyhow!("openpty: {e}"))?;

    let mut cmd = CommandBuilder::new(&prog);
    for a in &argv[1..] {
        cmd.arg(a);
    }
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| anyhow!("spawn {prog}: {e}"))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| anyhow!("clone reader: {e}"))?;

    // Bridge blocking PTY reads into the async world via a tokio mpsc.
    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let _reader_handle = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if chunk_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut copilot_state = ProcessState::default();
    let mut copilot_tail = String::new();
    let mut claude_partial = String::new();
    let source: &'static str = match route {
        Route::Claude => "claude",
        Route::Copilot => "copilot",
        Route::Unknown => "copilot",
    };

    let stdout = std::io::stdout();
    while let Some(bytes) = chunk_rx.recv().await {
        {
            let mut h = stdout.lock();
            let _ = h.write_all(&bytes);
            let _ = h.flush();
        }
        let _ = tx
            .send(IngestMessage::Activity {
                source: source.into(),
            })
            .await;
        let text = String::from_utf8_lossy(&bytes);
        match route {
            Route::Claude => {
                claude_partial.push_str(&text);
                while let Some(idx) = claude_partial.find('\n') {
                    let line: String = claude_partial.drain(..=idx).collect();
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if trimmed.contains(r#""type":"user""#) {
                        let _ = tx
                            .send(IngestMessage::RequestStart {
                                source: "claude".into(),
                            })
                            .await;
                    }
                    if let Some(ev) = claude::parse_event(trimmed) {
                        let _ = tx.send(IngestMessage::Event(ev)).await;
                    }
                }
            }
            Route::Copilot | Route::Unknown => {
                for _ in text.matches("Sending request to the AI model") {
                    let _ = tx
                        .send(IngestMessage::RequestStart {
                            source: "copilot".into(),
                        })
                        .await;
                }
                copilot_tail.push_str(&text);
                let leftover = process_chunk(&copilot_tail, &mut copilot_state, &tx).await;
                copilot_tail = leftover;
            }
        }
    }

    let status = tokio::task::spawn_blocking(move || child.wait())
        .await
        .map_err(|e| anyhow!("join: {e}"))?
        .map_err(|e| anyhow!("wait: {e}"))?;

    eprintln!(
        "tokenferno: child exited ({:?}) — provider={:?}",
        status, route
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_detection_picks_right_parser() {
        assert_eq!(detect_route("claude"), Route::Claude);
        assert_eq!(detect_route("/usr/local/bin/claude"), Route::Claude);
        assert_eq!(detect_route("copilot"), Route::Copilot);
        assert_eq!(detect_route("/opt/gh/copilot-cli"), Route::Copilot);
        assert_eq!(detect_route("/usr/bin/copilot"), Route::Copilot);
        assert_eq!(detect_route("python"), Route::Unknown);
    }
}
