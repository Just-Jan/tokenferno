use anyhow::Result;
use notify::{recommended_watcher, Event, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;

/// Spawn a recursive `notify` watcher rooted at `path`. Each filesystem
/// event becomes a `PathBuf` (the changed file) on the returned channel —
/// dropped silently if the consumer is slow (we already have a polling
/// safety net upstream).
pub fn watch_dir(path: &Path) -> Result<mpsc::Receiver<PathBuf>> {
    let (tx, rx) = mpsc::channel::<PathBuf>(256);
    let send_tx = tx.clone();
    let mut watcher = recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(ev) = res {
            for p in ev.paths {
                let _ = send_tx.try_send(p);
            }
        }
    })?;
    if path.exists() {
        watcher.watch(path, RecursiveMode::Recursive)?;
    }
    // Leak the watcher to keep it alive for the program's lifetime.
    std::mem::forget(watcher);
    Ok(rx)
}

/// Poll a single file at `interval` and emit the path on every detected
/// `metadata().len()` change. Used as a low-latency wake source for the
/// "currently hot" log file, complementary to the FS watcher (which on
/// macOS FSEvents can lag 100–500 ms). Returns when `stop` is set or the
/// receiver is dropped.
pub fn poll_file(
    path: PathBuf,
    interval: Duration,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> mpsc::Receiver<PathBuf> {
    let (tx, rx) = mpsc::channel::<PathBuf>(8);
    tokio::spawn(async move {
        let mut last_len: Option<u64> = None;
        let mut errs = 0u32;
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            match tokio::fs::metadata(&path).await {
                Ok(meta) => {
                    errs = 0;
                    let len = meta.len();
                    if last_len.map(|l| l != len).unwrap_or(true) {
                        last_len = Some(len);
                        if tx.send(path.clone()).await.is_err() {
                            return;
                        }
                    }
                }
                Err(_) => {
                    errs += 1;
                    if errs > 30 {
                        return;
                    }
                }
            }
        }
    });
    rx
}

/// Spawn a poll task whose results are forwarded into `out_tx` (so multiple
/// hot paths can share a single mpsc queue downstream). Returns the stop
/// flag — set to `true` to terminate the task.
pub fn spawn_active_poll(
    path: PathBuf,
    interval: Duration,
    out_tx: mpsc::Sender<PathBuf>,
) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut rx = poll_file(path, interval, stop.clone());
    tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            if out_tx.send(p).await.is_err() {
                return;
            }
        }
    });
    stop
}

/// Keep a continuous low-latency poll running for exactly the files in
/// `active`: start one for any newly-active file, and stop the poll for any
/// file that has left the active set. Idempotent — call once at startup and
/// on every rescan.
///
/// This is what makes detection real-time and deterministic: every active
/// log is polled every `interval`, so a working agent is noticed within
/// ~`interval` regardless of FSEvents latency/drops or how long the file was
/// previously idle (no "went cold" gap).
pub fn sync_active_polls(
    active: &[PathBuf],
    polling: &mut std::collections::HashMap<PathBuf, std::sync::Arc<std::sync::atomic::AtomicBool>>,
    out_tx: &mpsc::Sender<PathBuf>,
    interval: Duration,
) {
    use std::collections::HashSet;
    let want: HashSet<&PathBuf> = active.iter().collect();
    for p in active {
        if !polling.contains_key(p) {
            let stop = spawn_active_poll(p.clone(), interval, out_tx.clone());
            polling.insert(p.clone(), stop);
        }
    }
    let gone: Vec<PathBuf> = polling
        .keys()
        .filter(|p| !want.contains(p))
        .cloned()
        .collect();
    for p in gone {
        if let Some(stop) = polling.remove(&p) {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
