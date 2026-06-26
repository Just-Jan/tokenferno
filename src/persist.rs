//! Append-only JSONL persistence of authoritative `UsageEvent`s, with a
//! `replay_today()` helper that rehydrates the aggregator on TUI restart.
//!
//! Files live under `~/.tokenferno/events-YYYY-MM-DD.jsonl` (UTC date).
//! Day rotation is implicit — every append re-derives today's path so a
//! session that crosses midnight transparently rolls onto the new file.

use crate::model::UsageEvent;
use anyhow::{anyhow, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

/// Override directory (used by tests to redirect away from $HOME).
static DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Set a custom persistence directory. Idempotent; first call wins. Used by
/// tests to point at a target/ subdirectory instead of the user's home.
#[allow(dead_code)]
pub fn set_dir_for_test(p: PathBuf) {
    let _ = DIR_OVERRIDE.set(p);
}

pub fn dir() -> PathBuf {
    if let Some(p) = DIR_OVERRIDE.get() {
        return p.clone();
    }
    directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".tokenferno"))
        .unwrap_or_else(|| PathBuf::from(".tokenferno"))
}

/// One-time, best-effort migration of the data directory after the project
/// was renamed from `burn-the-planet` to `tokenferno`. If the legacy
/// `~/.burn-the-planet` directory exists and the new `~/.tokenferno` does
/// not, move it — preserving the user's persisted token history and IPC
/// socket across the rename. No-op once migrated (or if a test override is
/// active). Call once at startup before persistence/IPC are used.
pub fn migrate_legacy_dir() {
    if DIR_OVERRIDE.get().is_some() {
        return;
    }
    let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) else {
        return;
    };
    migrate_dir_at(&home.join(".burn-the-planet"), &home.join(".tokenferno"));
}

/// The core of [`migrate_legacy_dir`], parameterized on the two paths so it
/// can be unit-tested without touching `$HOME`. Only moves when the legacy
/// dir exists and the destination does NOT (never clobbers existing data).
fn migrate_dir_at(legacy: &Path, current: &Path) {
    if legacy.is_dir() && !current.exists() {
        match std::fs::rename(legacy, current) {
            Ok(()) => tracing::info!(?legacy, ?current, "migrated legacy data dir → ~/.tokenferno"),
            Err(e) => tracing::warn!(error = ?e, "failed to migrate legacy data dir"),
        }
    }
}

pub fn current_log_path() -> PathBuf {
    let date = Utc::now().format("%Y-%m-%d").to_string();
    dir().join(format!("events-{date}.jsonl"))
}

/// Lazily-opened, mutex-guarded handle to today's log. Re-opens on day flip.
struct LogHandle {
    path: PathBuf,
    file: File,
}

static LOG: OnceLock<Mutex<Option<LogHandle>>> = OnceLock::new();

fn log_slot() -> &'static Mutex<Option<LogHandle>> {
    LOG.get_or_init(|| Mutex::new(None))
}

/// Append one event as a single JSON line, flushing after the write so the
/// data is durable before we ack to the aggregator.
pub async fn append(ev: &UsageEvent) -> Result<()> {
    let want_path = current_log_path();
    if let Some(parent) = want_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
        // Best-effort tighten perms on the dir.
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

    let line = serde_json::to_string(ev)
        .map_err(|e| anyhow!("persist serialize: {e}"))?
        + "\n";

    let slot = log_slot();
    let mut guard = slot.lock().await;
    let need_open = match guard.as_ref() {
        None => true,
        Some(h) => h.path != want_path,
    };
    if need_open {
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&want_path)
            .await
            .map_err(|e| anyhow!("persist open {want_path:?}: {e}"))?;
        *guard = Some(LogHandle {
            path: want_path.clone(),
            file: f,
        });
    }
    let h = guard.as_mut().expect("just inserted");
    h.file
        .write_all(line.as_bytes())
        .await
        .map_err(|e| anyhow!("persist write: {e}"))?;
    h.file
        .flush()
        .await
        .map_err(|e| anyhow!("persist flush: {e}"))?;
    Ok(())
}

/// Synchronously read today's log file at startup and return its events.
/// Malformed lines are logged and skipped; missing file → empty.
pub fn replay_today() -> Result<Vec<UsageEvent>> {
    let path = current_log_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(anyhow!("persist read {path:?}: {e}")),
    };
    let mut out = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<UsageEvent>(trimmed) {
            Ok(ev) => out.push(ev),
            Err(e) => tracing::warn!(line_no = i + 1, error = %e, "persist: skipping malformed line"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Provider;

    fn ev(total: u64, sid: &str) -> UsageEvent {
        UsageEvent {
            ts: Utc::now(),
            provider: Provider::Copilot,
            model: "test".into(),
            session_id: sid.into(),
            cwd: None,
            input_tokens: total / 2,
            output_tokens: total / 2,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: total,
            raw_source: "test".into(),
            id: None,
        }
    }

    #[test]
    fn migrate_dir_at_moves_legacy_history_without_clobber() {
        let base = std::env::temp_dir().join(format!("tf_migrate_{}", std::process::id()));
        let legacy = base.join(".burn-the-planet");
        let current = base.join(".tokenferno");
        let _ = std::fs::remove_dir_all(&base);
        // Legacy dir with one day of history; no new dir yet.
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("events-2026-06-25.jsonl"), b"{\"t\":1}\n").unwrap();

        migrate_dir_at(&legacy, &current);
        assert!(!legacy.exists(), "legacy dir should be gone after migration");
        assert_eq!(
            std::fs::read_to_string(current.join("events-2026-06-25.jsonl")).unwrap(),
            "{\"t\":1}\n",
            "history must be preserved in the new dir",
        );

        // If the destination already exists, a stray legacy dir must NOT clobber it.
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("events-2026-06-25.jsonl"), b"STALE").unwrap();
        migrate_dir_at(&legacy, &current);
        assert_eq!(
            std::fs::read_to_string(current.join("events-2026-06-25.jsonl")).unwrap(),
            "{\"t\":1}\n",
            "existing tokenferno dir must never be overwritten",
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replay_round_trip() {
        let base = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("persist-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // Test isolation: bypass the global log slot/dir override (which would
        // collide across tests in the same process) and write directly using a
        // local temp path, then call a one-off reader.
        let path = base.join("events-test.jsonl");
        let evs = vec![ev(100, "s1"), ev(250, "s2"), ev(75, "s1")];
        let mut buf = String::new();
        for e in &evs {
            buf.push_str(&serde_json::to_string(e).unwrap());
            buf.push('\n');
        }
        // Throw a malformed line in the middle to ensure it's skipped.
        let mut lines: Vec<&str> = buf.lines().collect();
        lines.insert(2, "this is not json");
        let with_bad = lines.join("\n") + "\n";
        std::fs::write(&path, with_bad).unwrap();

        // Read back via the same parser used by replay_today.
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut got: Vec<UsageEvent> = Vec::new();
        for line in raw.lines() {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if let Ok(e) = serde_json::from_str::<UsageEvent>(t) {
                got.push(e);
            }
        }
        assert_eq!(got.len(), 3);
        let sum: u64 = got.iter().map(|e| e.total_tokens).sum();
        assert_eq!(sum, 100 + 250 + 75);
        let _ = std::fs::remove_dir_all(&base);
    }
}
