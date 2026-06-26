//! Ground-truth timeline: correlates what Copilot is ACTUALLY writing to its
//! log (raw, independent tail) against what the app's ingest+aggregate
//! pipeline detects — second by second. Run it while doing real work to find
//! exactly where "Copilot working but app shows nothing" happens.
//!
//! Columns:
//!   RAW:  +bytes written this tick, and markers seen (S=Sending-request,
//!         C=chat.completion w/ total_tokens)
//!   APP:  in_flight, today.total delta, live_rate (the fire fuel)

use tokenferno::aggregate::{self, Command};
use tokenferno::ingest::{self, IngestMessage};
use tokenferno::ui::fun;
use chrono::Utc;
use directories::BaseDirs;
use std::io::{Seek, SeekFrom, Read};
use tokio::sync::mpsc;

fn newest_log(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let name = p.file_name()?.to_string_lossy().to_string();
        if name.starts_with("process-") && name.ends_with(".log") {
            if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                if best.as_ref().map(|(bt, _)| m > *bt).unwrap_or(true) {
                    best = Some((m, p.clone()));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

#[tokio::main]
async fn main() {
    let home = BaseDirs::new().unwrap().home_dir().to_path_buf();
    let copilot_logs = home.join(".copilot/logs");

    // --- app detection pipeline (same wiring as main.rs) ---
    let (tx, rx) = mpsc::channel::<IngestMessage>(8192);
    let (_c, crx) = mpsc::channel::<Command>(8);
    {
        let t = tx.clone();
        let p = copilot_logs.clone();
        tokio::spawn(async move { let _ = ingest::copilot::run(p, t).await; });
    }
    drop(tx);
    let snap_rx = aggregate::spawn_with(rx, crx, false);

    // --- independent raw tail of the newest log ---
    let mut raw_path = newest_log(&copilot_logs);
    let mut offset: u64 = raw_path
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0);

    println!("{:>5}  {:>9} {:>3} {:>3}  | {:>8} {:>9} {:>9} {:>9}  notes",
        "t", "rawBytes", "S", "C", "inflght", "dToday", "live", "lastEvt");
    let start = std::time::Instant::now();
    let mut last_today = snap_rx.borrow().today.total;
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(1000));
    loop {
        tick.tick().await;
        // re-detect newest (a new turn may rotate to a new process log)
        if let Some(n) = newest_log(&copilot_logs) {
            if Some(&n) != raw_path.as_ref() {
                raw_path = Some(n);
                offset = 0;
            }
        }
        // read raw new bytes
        let mut added = 0u64;
        let mut s_count = 0;
        let mut c_count = 0;
        let mut toks: Vec<u64> = Vec::new();
        if let Some(p) = &raw_path {
            if let Ok(mut f) = std::fs::File::open(p) {
                let len = f.metadata().map(|m| m.len()).unwrap_or(0);
                if len < offset { offset = 0; }
                if len > offset {
                    let _ = f.seek(SeekFrom::Start(offset));
                    let mut buf = Vec::new();
                    let _ = f.take(len - offset).read_to_end(&mut buf);
                    added = buf.len() as u64;
                    offset = len;
                    let text = String::from_utf8_lossy(&buf);
                    s_count = text.matches("Sending request to the AI model").count();
                    c_count = text.matches("\"object\": \"chat.completion\"").count();
                    for cap in text.split("\"total_tokens\":").skip(1) {
                        let num: String = cap.trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
                        if let Ok(v) = num.parse::<u64>() { toks.push(v); }
                    }
                }
            }
        }
        let snap = snap_rx.borrow().clone();
        let inflight: u32 = snap.in_flight.values().sum();
        let d_today = snap.today.total.saturating_sub(last_today);
        last_today = snap.today.total;
        let live = fun::live_rate(&snap);
        let last_evt = snap.last_event_at
            .map(|t| format!("{:.0}s", (Utc::now() - t).num_milliseconds() as f64 / 1000.0))
            .unwrap_or_else(|| "—".into());
        let t = start.elapsed().as_secs();
        let note = if s_count > 0 { "<<< START" } else if c_count > 0 { "<<< DONE" } else { "" };
        println!("{:>5}  {:>9} {:>3} {:>3}  | {:>8} {:>9} {:>9.0} {:>9}  {}",
            t, added, s_count, c_count, inflight, d_today, live, last_evt, note);
        if t >= 90 { break; }
    }
}
