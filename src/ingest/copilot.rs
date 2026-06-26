use super::{EventSender, IngestMessage};
use crate::model::{Provider, UsageEvent};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};

#[derive(Debug, Deserialize)]
struct CopilotChunk {
    #[serde(default)]
    object: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    config_model: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    usage: Option<CopilotUsage>,
}

#[derive(Debug, Deserialize)]
struct CopilotUsage {
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<PromptDetails>,
    #[serde(default)]
    completion_tokens_details: Option<CompletionDetails>,
}

#[derive(Debug, Deserialize, Default)]
struct PromptDetails {
    #[serde(default)]
    cached_tokens: u64,
}

#[derive(Debug, Deserialize, Default)]
struct CompletionDetails {
    #[serde(default)]
    reasoning_tokens: u64,
}

#[derive(Default)]
pub struct ProcessState {
    pub last_session_id: Option<String>,
    pub last_model: Option<String>,
}

/// Try to parse a JSON block (possibly multi-line) emitted by Copilot.
/// Returns Some(event) only if it's a chat.completion with usage.
pub fn parse_block(block: &str, state: &mut ProcessState, ts: DateTime<Utc>) -> Option<UsageEvent> {
    let chunk: CopilotChunk = serde_json::from_str(block).ok()?;
    if let Some(sid) = &chunk.session_id {
        state.last_session_id = Some(sid.clone());
    }
    if let Some(m) = chunk.model.clone().or_else(|| chunk.config_model.clone()) {
        state.last_model = Some(m);
    }
    if chunk.object.as_deref() != Some("chat.completion") {
        return None;
    }
    let usage = chunk.usage?;
    let cached = usage
        .prompt_tokens_details
        .as_ref()
        .map(|d| d.cached_tokens)
        .unwrap_or(0);
    let reasoning = usage
        .completion_tokens_details
        .as_ref()
        .map(|d| d.reasoning_tokens)
        .unwrap_or(0);
    let input = usage.prompt_tokens.saturating_sub(cached);
    let total = if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage.prompt_tokens + usage.completion_tokens
    };
    Some(UsageEvent {
        ts,
        provider: Provider::Copilot,
        model: chunk
            .model
            .or(chunk.config_model)
            .or_else(|| state.last_model.clone())
            .unwrap_or_else(|| "unknown".into()),
        session_id: chunk
            .session_id
            .or_else(|| state.last_session_id.clone())
            .unwrap_or_else(|| "unknown".into()),
        cwd: None,
        input_tokens: input,
        output_tokens: usage.completion_tokens,
        cache_read_tokens: cached,
        cache_creation_tokens: 0,
        reasoning_tokens: reasoning,
        total_tokens: total,
        raw_source: "copilot:log".into(),
        id: chunk.id,
    })
}

/// True only for a genuine "request started" *log line*: a timestamp-prefixed
/// `[INFO]` line carrying the group header. NOT the phrase appearing inside
/// logged conversation content — the Copilot log embeds the full
/// request/response, so a naive substring match counts assistant/tool/user
/// text that mentions the phrase as fake request starts, which desyncs
/// in-flight tracking and poisons the projected burn rate (observed: a single
/// chunk produced 30+ phantom starts, pushing the live rate to millions of
/// tok/s and masking real activity).
fn is_request_start_line(line: &str) -> bool {
    line.as_bytes()
        .first()
        .map_or(false, |b| b.is_ascii_digit())
        && line.contains("[INFO]")
        && line.contains("Start of group: Sending request to the AI model")
}

/// Tail Copilot process logs and emit events.
pub async fn run(logs_root: PathBuf, tx: EventSender) -> Result<()> {
    if !logs_root.exists() {
        let _ = tx
            .send(IngestMessage::Error(format!(
                "copilot logs not found: {}",
                logs_root.display()
            )))
            .await;
        return Ok(());
    }

    let mut offsets: HashMap<PathBuf, u64> = HashMap::new();
    let mut files: HashMap<PathBuf, File> = HashMap::new();
    let mut last_meta: HashMap<PathBuf, std::time::Instant> = HashMap::new();
    let mut states: HashMap<PathBuf, ProcessState> = HashMap::new();
    // tail buffers (incomplete remainder per file)
    let mut tails: HashMap<PathBuf, String> = HashMap::new();

    // Backfill: only the active files (skip stale).
    let mut active = scan_active_logs(&logs_root).await;
    for path in &active {
        if let Err(e) = backfill_log(path, &mut offsets, &mut tails, &mut states, &tx).await {
            tracing::warn!(?path, error = ?e, "copilot backfill failed");
        }
    }
    let _ = tx
        .send(IngestMessage::FileCount {
            source: "copilot".into(),
            count: active.len(),
        })
        .await;

    let mut wake = super::fswatch::watch_dir(&logs_root).ok();
    // Rescan only *discovers* new/rotated files now; per-file change detection
    // is handled by an always-on low-latency poll (below), so detection no
    // longer depends on FSEvents (which can lag 100–500 ms or drop entirely)
    // or on a file staying "hot".
    let mut rescan = tokio::time::interval(Duration::from_secs(2));

    let (active_tx, mut active_rx) = tokio::sync::mpsc::channel::<PathBuf>(64);
    let mut polling: HashMap<PathBuf, std::sync::Arc<std::sync::atomic::AtomicBool>> = HashMap::new();
    // Every active log file is polled continuously at this cadence, so a
    // working agent is reflected within ~this latency even after a long
    // tool-execution gap (the old 30 s "hot window" prune caused multi-second
    // misses between agentic turns).
    const POLL_INTERVAL: Duration = Duration::from_millis(75);
    super::fswatch::sync_active_polls(&active, &mut polling, &active_tx, POLL_INTERVAL);

    loop {
        let mut changed: Vec<PathBuf> = Vec::new();
        tokio::select! {
            _ = rescan.tick() => {
                // Refresh the active set (catches newly created log files) and
                // keep a low-latency poll running for exactly those files.
                active = scan_active_logs(&logs_root).await;
                let _ = tx.send(IngestMessage::FileCount { source: "copilot".into(), count: active.len() }).await;
                changed.extend(active.iter().cloned());
                super::fswatch::sync_active_polls(&active, &mut polling, &active_tx, POLL_INTERVAL);
            }
            Some(p) = async {
                if let Some(rx) = wake.as_mut() { rx.recv().await } else { std::future::pending().await }
            } => {
                if is_target_log(&p) {
                    changed.push(p);
                }
                // Drain any other queued events in the same wakeup batch.
                if let Some(rx) = wake.as_mut() {
                    while let Ok(more) = rx.try_recv() {
                        if is_target_log(&more) { changed.push(more); }
                    }
                }
            }
            Some(p) = active_rx.recv() => {
                changed.push(p);
                while let Ok(more) = active_rx.try_recv() { changed.push(more); }
            }
        }

        // Dedupe within this batch.
        changed.sort();
        changed.dedup();

        // Read each changed file from its saved offset to EOF.
        for path in changed {
            if let Err(e) = poll_log(&path, &mut offsets, &mut files, &mut last_meta, &mut tails, &mut states, &tx).await {
                tracing::debug!(?path, error = ?e, "copilot poll failed");
            }
        }
    }
}

const ACTIVE_LOG_MAX_AGE_SECS: u64 = 6 * 60 * 60; // 6h

fn is_target_log(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with("process-") && n.ends_with(".log"))
        .unwrap_or(false)
}

async fn scan_active_logs(root: &Path) -> Vec<PathBuf> {
    let all = scan_logs(root);
    let cutoff = std::time::SystemTime::now()
        .checked_sub(Duration::from_secs(ACTIVE_LOG_MAX_AGE_SECS));
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

fn scan_logs(root: &Path) -> Vec<PathBuf> {
    let pattern = format!("{}/process-*.log", root.display());
    glob::glob(&pattern)
        .map(|p| p.filter_map(|x| x.ok()).collect())
        .unwrap_or_default()
}

const BACKFILL_BYTES: u64 = 1024 * 1024;

async fn backfill_log(
    path: &Path,
    offsets: &mut HashMap<PathBuf, u64>,
    tails: &mut HashMap<PathBuf, String>,
    states: &mut HashMap<PathBuf, ProcessState>,
    tx: &EventSender,
) -> Result<()> {
    let mut file = File::open(path)
        .await
        .with_context(|| format!("open {}", path.display()))?;
    let len = file.metadata().await?.len();
    let start = len.saturating_sub(BACKFILL_BYTES);
    file.seek(SeekFrom::Start(start)).await?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).await.ok();
    if start > 0 {
        // discard up to first newline (likely partial)
        if let Some(idx) = buf.find('\n') {
            buf = buf.split_off(idx + 1);
        }
    }
    let state = states.entry(path.to_path_buf()).or_default();
    let tail = tails.entry(path.to_path_buf()).or_default();
    *tail = process_chunk(&buf, state, tx).await;
    offsets.insert(path.to_path_buf(), len);
    Ok(())
}

async fn poll_log(
    path: &Path,
    offsets: &mut HashMap<PathBuf, u64>,
    files: &mut HashMap<PathBuf, File>,
    last_meta: &mut HashMap<PathBuf, std::time::Instant>,
    tails: &mut HashMap<PathBuf, String>,
    states: &mut HashMap<PathBuf, ProcessState>,
    tx: &EventSender,
) -> Result<()> {
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
            files.remove(path);
            offsets.insert(path.to_path_buf(), 0);
            tails.remove(path);
            return Ok(());
        }
    }
    if !files.contains_key(path) {
        let mut f = File::open(path).await?;
        let pos = offsets.get(path).copied().unwrap_or(0);
        f.seek(SeekFrom::Start(pos)).await?;
        files.insert(path.to_path_buf(), f);
    }
    let f = files.get_mut(path).expect("file handle just inserted");
    let mut buf = String::new();
    if f.read_to_string(&mut buf).await.is_err() {
        files.remove(path);
        return Ok(());
    }
    if buf.is_empty() {
        return Ok(());
    }
    let _ = tx
        .send(IngestMessage::Activity { source: "copilot".into() })
        .await;
    for line in buf.lines() {
        if is_request_start_line(line) {
            let _ = tx
                .send(IngestMessage::RequestStart { source: "copilot".into() })
                .await;
        }
    }
    let state = states.entry(path.to_path_buf()).or_default();
    let tail = tails.entry(path.to_path_buf()).or_default();
    tail.push_str(&buf);
    let leftover = process_chunk(tail, state, tx).await;
    *tail = leftover;
    let pos = f.stream_position().await.unwrap_or_default();
    offsets.insert(path.to_path_buf(), pos);
    Ok(())
}

/// Walk text, find `[DEBUG] {`-style multi-line JSON blocks, parse them.
/// Returns leftover (incomplete trailing block).
pub async fn process_chunk(
    text: &str,
    state: &mut ProcessState,
    tx: &EventSender,
) -> String {
    let mut cursor = 0usize;
    let bytes = text.as_bytes();
    let len = bytes.len();
    while cursor < len {
        let Some(rel) = text[cursor..].find('{') else {
            return String::new();
        };
        let brace_at = cursor + rel;
        // capture timestamp from the start of the line containing brace_at
        let line_start = text[..brace_at].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let prefix = &text[line_start..brace_at];
        let ts = parse_ts(prefix).unwrap_or_else(Utc::now);
        // brace count
        let mut depth: i32 = 0;
        let mut in_str = false;
        let mut escape = false;
        let mut end_at: Option<usize> = None;
        for i in brace_at..len {
            let c = bytes[i] as char;
            if in_str {
                if escape {
                    escape = false;
                } else if c == '\\' {
                    escape = true;
                } else if c == '"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end_at = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end_at else {
            // incomplete, return remainder starting at line_start
            return text[line_start..].to_string();
        };
        let block = &text[brace_at..end];
        if let Some(ev) = parse_block(block, state, ts) {
            let _ = tx.send(IngestMessage::Event(ev)).await;
        } else {
            // Streaming chunk? Look for SSE-style delta content and emit
            // PartialDelta micro-events so the meter ticks while the
            // response is still arriving. The full chat.completion above
            // remains the authoritative reconciliation event.
            scan_stream_deltas(block, tx).await;
        }
        cursor = end;
    }
    String::new()
}

/// Lightweight scan for SSE-style streaming deltas inside a JSON block.
/// We don't fully parse the chunk (we don't need to); we just look for
/// `"content":"…"` strings and estimate tokens as `len/4` per chunk.
async fn scan_stream_deltas(block: &str, tx: &EventSender) {
    if !block.contains("chat.completion.chunk") && !block.contains("\"delta\"") {
        return;
    }
    let bytes = block.as_bytes();
    let needle = b"\"content\"";
    let mut i = 0usize;
    while i + needle.len() < bytes.len() {
        if &bytes[i..i + needle.len()] != needle {
            i += 1;
            continue;
        }
        // Find ':' then opening quote.
        let mut j = i + needle.len();
        while j < bytes.len() && bytes[j] != b':' { j += 1; }
        if j >= bytes.len() { break; }
        j += 1;
        while j < bytes.len() && (bytes[j] as char).is_whitespace() { j += 1; }
        if j >= bytes.len() || bytes[j] != b'"' { i = j; continue; }
        j += 1;
        let str_start = j;
        let mut escape = false;
        while j < bytes.len() {
            let c = bytes[j];
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == b'"' {
                break;
            }
            j += 1;
        }
        if j >= bytes.len() { break; }
        let content_len = j - str_start;
        if content_len > 0 {
            let est = ((content_len as u32) / 4).max(1);
            let _ = tx
                .send(IngestMessage::PartialDelta {
                    source: "copilot".into(),
                    est_tokens: est,
                })
                .await;
        }
        i = j + 1;
    }
}

fn parse_ts(prefix: &str) -> Option<DateTime<Utc>> {
    // Expect e.g. "2026-04-07T09:16:13.019Z [DEBUG] " — grab first whitespace-delimited token.
    let token = prefix.split_whitespace().next()?;
    DateTime::parse_from_rfc3339(token)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    const BLOCK: &str = "2026-04-07T09:16:13.019Z [DEBUG] {\n  \"object\": \"chat.completion\",\n  \"model\": \"claude-opus-4.7\",\n  \"session_id\": \"f4f18c\",\n  \"usage\": {\n    \"completion_tokens\": 156,\n    \"prompt_tokens\": 45000,\n    \"total_tokens\": 45156,\n    \"prompt_tokens_details\": { \"cached_tokens\": 22000 },\n    \"completion_tokens_details\": { \"reasoning_tokens\": 128 }\n  },\n  \"id\": \"abc\"\n}\n";

    #[tokio::test]
    async fn parses_chat_completion_block() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut state = ProcessState::default();
        let leftover = process_chunk(BLOCK, &mut state, &tx).await;
        assert!(leftover.is_empty(), "no leftover expected, got {leftover:?}");
        let msg = rx.recv().await.expect("event");
        match msg {
            IngestMessage::Event(ev) => {
                assert_eq!(ev.provider as u8, Provider::Copilot as u8);
                assert_eq!(ev.input_tokens, 45000 - 22000);
                assert_eq!(ev.output_tokens, 156);
                assert_eq!(ev.cache_read_tokens, 22000);
                assert_eq!(ev.reasoning_tokens, 128);
                assert_eq!(ev.total_tokens, 45156);
                assert_eq!(ev.model, "claude-opus-4.7");
                assert_eq!(ev.session_id, "f4f18c");
            }
            other => panic!("unexpected msg: {other:?}"),
        }
    }

    #[tokio::test]
    async fn returns_remainder_for_partial_block() {
        let partial = "2026-04-07T09:16:13.019Z [DEBUG] {\n  \"object\": \"chat.completion\",\n  \"usage\": {\n    \"prompt_tokens\": 1";
        let (tx, _rx) = mpsc::channel(8);
        let mut state = ProcessState::default();
        let leftover = process_chunk(partial, &mut state, &tx).await;
        assert!(leftover.contains("chat.completion"));
    }
}
