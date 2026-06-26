use super::{EventSender, IngestMessage};
use crate::model::{Provider, UsageEvent};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader, SeekFrom};

#[derive(Debug, Deserialize)]
struct ClaudeLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<DateTime<Utc>>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    message: Option<ClaudeMessage>,
}

#[derive(Debug, Deserialize)]
struct ClaudeMessage {
    model: Option<String>,
    #[serde(default)]
    id: Option<String>,
    usage: Option<ClaudeUsage>,
}

#[derive(Debug, Deserialize)]
struct ClaudeUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

pub fn parse_event(line: &str) -> Option<UsageEvent> {
    let parsed: ClaudeLine = serde_json::from_str(line).ok()?;
    if parsed.kind.as_deref() != Some("assistant") {
        return None;
    }
    let msg = parsed.message?;
    let msg_id = msg.id.clone();
    let usage = msg.usage?;
    let total = usage.input_tokens
        + usage.output_tokens
        + usage.cache_read_input_tokens
        + usage.cache_creation_input_tokens;
    Some(UsageEvent {
        ts: parsed.timestamp.unwrap_or_else(Utc::now),
        provider: Provider::Claude,
        model: msg.model.unwrap_or_else(|| "unknown".into()),
        session_id: parsed.session_id.unwrap_or_else(|| "unknown".into()),
        cwd: parsed.cwd.map(PathBuf::from),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_input_tokens,
        cache_creation_tokens: usage.cache_creation_input_tokens,
        reasoning_tokens: 0,
        total_tokens: total,
        raw_source: "claude:jsonl".into(),
        id: msg_id,
    })
}

pub async fn run(root: PathBuf, tx: EventSender) -> Result<()> {
    if !root.exists() {
        let _ = tx
            .send(IngestMessage::Error(format!(
                "claude root not found: {}",
                root.display()
            )))
            .await;
        return Ok(());
    }

    let mut offsets: HashMap<PathBuf, u64> = HashMap::new();
    let mut files: HashMap<PathBuf, File> = HashMap::new();
    let mut last_meta: HashMap<PathBuf, std::time::Instant> = HashMap::new();

    let mut active = scan_active_jsonl(&root).await;
    let _ = tx
        .send(IngestMessage::FileCount {
            source: "claude".into(),
            count: active.len(),
        })
        .await;
    for path in &active {
        if let Err(e) = backfill_file(path, &mut offsets, &tx).await {
            tracing::warn!(?path, error = ?e, "claude backfill failed");
        }
    }

    let mut wake = super::fswatch::watch_dir(&root).ok();
    // Rescan only discovers new/rotated files; per-file change detection is an
    // always-on low-latency poll (below), so detection doesn't depend on
    // FSEvents or a file staying "hot".
    let mut rescan = tokio::time::interval(Duration::from_secs(2));

    let (active_tx, mut active_rx) = tokio::sync::mpsc::channel::<PathBuf>(64);
    let mut polling: HashMap<PathBuf, std::sync::Arc<std::sync::atomic::AtomicBool>> = HashMap::new();
    const POLL_INTERVAL: Duration = Duration::from_millis(75);
    super::fswatch::sync_active_polls(&active, &mut polling, &active_tx, POLL_INTERVAL);

    loop {
        let mut changed: Vec<PathBuf> = Vec::new();
        tokio::select! {
            _ = rescan.tick() => {
                active = scan_active_jsonl(&root).await;
                let _ = tx.send(IngestMessage::FileCount { source: "claude".into(), count: active.len() }).await;
                changed.extend(active.iter().cloned());
                super::fswatch::sync_active_polls(&active, &mut polling, &active_tx, POLL_INTERVAL);
            }
            Some(p) = async {
                if let Some(rx) = wake.as_mut() { rx.recv().await } else { std::future::pending().await }
            } => {
                if is_target_jsonl(&p) {
                    changed.push(p);
                }
                if let Some(rx) = wake.as_mut() {
                    while let Ok(more) = rx.try_recv() {
                        if is_target_jsonl(&more) { changed.push(more); }
                    }
                }
            }
            Some(p) = active_rx.recv() => {
                changed.push(p);
                while let Ok(more) = active_rx.try_recv() { changed.push(more); }
            }
        }
        changed.sort();
        changed.dedup();
        for path in changed {
            if let Err(e) = poll_file(&path, &mut offsets, &mut files, &mut last_meta, &tx).await {
                tracing::debug!(?path, error = ?e, "claude poll failed");
            }
        }
    }
}

const ACTIVE_JSONL_MAX_AGE_SECS: u64 = 2 * 60 * 60; // 2h

fn is_target_jsonl(p: &Path) -> bool {
    p.extension().map(|e| e == "jsonl").unwrap_or(false)
}

async fn scan_active_jsonl(root: &Path) -> Vec<PathBuf> {
    let all = scan_jsonl(root);
    let cutoff = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(ACTIVE_JSONL_MAX_AGE_SECS));
    let mut out = Vec::new();
    for p in all {
        if let Ok(meta) = tokio::fs::metadata(&p).await {
            if let Ok(mtime) = meta.modified() {
                if cutoff.map(|c| mtime >= c).unwrap_or(true) {
                    out.push(p);
                }
            }
        }
    }
    out
}

fn scan_jsonl(root: &Path) -> Vec<PathBuf> {
    let pattern = format!("{}/**/*.jsonl", root.display());
    glob::glob(&pattern)
        .map(|paths| paths.filter_map(|p| p.ok()).collect())
        .unwrap_or_default()
}

const BACKFILL_BYTES: u64 = 256 * 1024;

async fn backfill_file(
    path: &Path,
    offsets: &mut HashMap<PathBuf, u64>,
    tx: &EventSender,
) -> Result<()> {
    let mut file = File::open(path)
        .await
        .with_context(|| format!("open {}", path.display()))?;
    let len = file.metadata().await?.len();
    let start = len.saturating_sub(BACKFILL_BYTES);
    file.seek(SeekFrom::Start(start)).await?;
    let mut reader = BufReader::new(file);
    if start > 0 {
        // skip partial first line
        let mut throwaway = String::new();
        let _ = reader.read_line(&mut throwaway).await?;
    }
    read_lines(&mut reader, tx).await?;
    let pos = reader.into_inner().stream_position().await.unwrap_or(len);
    offsets.insert(path.to_path_buf(), pos);
    Ok(())
}

async fn poll_file(
    path: &Path,
    offsets: &mut HashMap<PathBuf, u64>,
    files: &mut HashMap<PathBuf, File>,
    last_meta: &mut HashMap<PathBuf, std::time::Instant>,
    tx: &EventSender,
) -> Result<()> {
    // Hot-loop fast path: try to read directly from a persistent File handle
    // (no metadata syscall every cycle). Re-check metadata every ~250 ms or
    // on read failure as a rotation/truncation guard.
    let now = std::time::Instant::now();
    let need_meta = last_meta
        .get(path)
        .map(|t| now.duration_since(*t) >= std::time::Duration::from_millis(250))
        .unwrap_or(true);
    if need_meta {
        let meta = tokio::fs::metadata(path).await?;
        let len = meta.len();
        let last = offsets.get(path).copied().unwrap_or(len);
        last_meta.insert(path.to_path_buf(), now);
        if len < last {
            // Rotation/truncation — drop cached handle and reset.
            files.remove(path);
            offsets.insert(path.to_path_buf(), 0);
            return Ok(());
        }
    }
    // Ensure we have an open File seeked to our last offset.
    if !files.contains_key(path) {
        let mut f = File::open(path).await?;
        let pos = offsets.get(path).copied().unwrap_or(0);
        f.seek(SeekFrom::Start(pos)).await?;
        files.insert(path.to_path_buf(), f);
    }
    // Read whatever is available; AsyncReadExt::read returns 0 if at EOF.
    let f = files.get_mut(path).expect("file handle just inserted");
    let mut buf = Vec::with_capacity(4096);
    use tokio::io::AsyncReadExt;
    let n = match f.read_to_end(&mut buf).await {
        Ok(n) => n,
        Err(_) => {
            files.remove(path);
            return Ok(());
        }
    };
    if n == 0 {
        return Ok(());
    }
    let _ = tx
        .send(IngestMessage::Activity { source: "claude".into() })
        .await;
    // Parse complete lines from buf; if it doesn't end in '\n', stash the
    // tail back by rewinding the file position so next read sees it again.
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut consumed = 0usize;
    for line in text.split_inclusive('\n') {
        if !line.ends_with('\n') {
            break; // partial trailing line
        }
        consumed += line.len();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.contains(r#""type":"user""#) {
            let _ = tx
                .send(IngestMessage::RequestStart { source: "claude".into() })
                .await;
        }
        if !trimmed.contains(r#""type":"assistant""#)
            && (trimmed.contains(r#""type":"tool_use""#)
                || trimmed.contains(r#""type":"tool_result""#))
        {
            let _ = tx
                .send(IngestMessage::MicroBurn { source: "claude".into(), tokens: 5 })
                .await;
        }
        if let Some(ev) = parse_event(trimmed) {
            let _ = tx.send(IngestMessage::Event(ev)).await;
        }
    }
    // Track new offset; if there's a partial trailing line, rewind so next
    // poll reads it again from the start of that line.
    let unconsumed = buf.len() - consumed;
    if unconsumed > 0 {
        f.seek(SeekFrom::Current(-(unconsumed as i64))).await?;
    }
    let pos = f.stream_position().await.unwrap_or_default();
    offsets.insert(path.to_path_buf(), pos);
    Ok(())
}

async fn read_lines<R>(reader: &mut BufReader<R>, tx: &EventSender) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        if !line.ends_with('\n') {
            // partial line; back-up via caller's offset tracking
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Cheap kind check: a `"type":"user"` line means a new turn was just kicked off
        // (could be a real user message or a tool result — both indicate Claude is about
        // to issue an inference request). Use as a request-start marker.
        if trimmed.contains(r#""type":"user""#) {
            let _ = tx
                .send(IngestMessage::RequestStart { source: "claude".into() })
                .await;
        }
        // Tool-use / tool-result markers in non-assistant lines: emit a small
        // synthetic micro-burn so long tool-heavy turns don't look frozen.
        if !trimmed.contains(r#""type":"assistant""#)
            && (trimmed.contains(r#""type":"tool_use""#)
                || trimmed.contains(r#""type":"tool_result""#))
        {
            let _ = tx
                .send(IngestMessage::MicroBurn { source: "claude".into(), tokens: 5 })
                .await;
        }
        if let Some(ev) = parse_event(trimmed) {
            let _ = tx.send(IngestMessage::Event(ev)).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{"parentUuid":"x","isSidechain":false,"type":"assistant","timestamp":"2026-04-10T08:23:12.441Z","sessionId":"00623301-5b4b-41ed-bd4f-111037102780","cwd":"/home/user/project","version":"2.1.90","gitBranch":"main","message":{"model":"claude-sonnet-4-6","id":"msg_01","role":"assistant","usage":{"input_tokens":3,"cache_creation_input_tokens":4950,"cache_read_input_tokens":11225,"output_tokens":6,"service_tier":"standard"}}}"#;

    #[test]
    fn parses_assistant_line() {
        let ev = parse_event(FIXTURE).expect("event parsed");
        assert_eq!(ev.provider as u8, Provider::Claude as u8);
        assert_eq!(ev.input_tokens, 3);
        assert_eq!(ev.output_tokens, 6);
        assert_eq!(ev.cache_read_tokens, 11225);
        assert_eq!(ev.cache_creation_tokens, 4950);
        assert_eq!(ev.total_tokens, 3 + 6 + 11225 + 4950);
        assert_eq!(ev.model, "claude-sonnet-4-6");
        assert_eq!(ev.session_id, "00623301-5b4b-41ed-bd4f-111037102780");
    }

    #[test]
    fn ignores_user_lines() {
        let line = r#"{"type":"user","timestamp":"2026-04-10T08:00:00Z"}"#;
        assert!(parse_event(line).is_none());
    }
}
