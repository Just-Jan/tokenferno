//! Synthetic load harness measuring log-write → snapshot-publish latency.
//!
//! Spawns the real copilot ingester pointed at a freshly-created temp dir,
//! writes timestamped `chat.completion` JSON blocks at controlled rates, and
//! measures wall-clock from "write done" to "snapshot's today.total reflects
//! the new tokens". Prints P50/P95.

use anyhow::Result;
use tokenferno::aggregate;
use tokenferno::ingest;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

const N_SAMPLES: usize = 50;
const TOKENS_PER_SAMPLE: u64 = 100;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    // Use a per-run subdirectory under target/ to honour the "no /tmp" rule.
    let root = std::env::current_dir()?
        .join("target")
        .join(format!("latency-probe-{}", std::process::id()));
    tokio::fs::create_dir_all(&root).await?;
    let log_path = root.join("process-test.log");
    // Touch the file before the ingester starts so it picks it up immediately.
    tokio::fs::File::create(&log_path).await?;

    let (event_tx, event_rx) = mpsc::channel(1024);
    let (_cmd_tx, cmd_rx) = mpsc::channel(8);
    let snap_rx = aggregate::spawn(event_rx, cmd_rx);

    // Spawn the copilot ingester rooted at our temp dir.
    let logs_root = root.clone();
    let tx = event_tx.clone();
    tokio::spawn(async move {
        if let Err(e) = ingest::copilot::run(logs_root, tx).await {
            eprintln!("ingester exited: {e:?}");
        }
    });
    drop(event_tx);

    // Give ingester a moment to scan the file and register.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut latencies_ms: Vec<f64> = Vec::with_capacity(N_SAMPLES);

    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .open(&log_path)
        .await?;

    for i in 0..N_SAMPLES {
        // Snapshot baseline.
        let baseline = snap_rx.borrow().today.total;
        let expected = baseline + TOKENS_PER_SAMPLE;

        let block = format!(
            "2026-04-07T09:16:13.019Z [DEBUG] {{\"object\":\"chat.completion\",\"model\":\"latency-probe\",\"session_id\":\"probe-{i}\",\"usage\":{{\"prompt_tokens\":{0},\"completion_tokens\":{1},\"total_tokens\":{2}}}}}\n",
            TOKENS_PER_SAMPLE / 2,
            TOKENS_PER_SAMPLE / 2,
            TOKENS_PER_SAMPLE,
        );
        let t0 = Instant::now();
        file.write_all(block.as_bytes()).await?;
        file.flush().await?;

        // Wait for snapshot to reflect the new total.
        let mut rx = snap_rx.clone();
        let observed = loop {
            let res = tokio::time::timeout(Duration::from_secs(2), rx.changed()).await;
            if res.is_err() {
                eprintln!("[sample {i}] timeout waiting for snapshot");
                break None;
            }
            let snap = rx.borrow().clone();
            if snap.today.total >= expected {
                break Some(t0.elapsed());
            }
        };
        if let Some(d) = observed {
            latencies_ms.push(d.as_secs_f64() * 1000.0);
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    if latencies_ms.is_empty() {
        eprintln!("no samples captured");
        std::process::exit(1);
    }
    latencies_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = percentile(&latencies_ms, 0.50);
    let p95 = percentile(&latencies_ms, 0.95);
    let p99 = percentile(&latencies_ms, 0.99);
    let mean: f64 = latencies_ms.iter().copied().sum::<f64>() / latencies_ms.len() as f64;
    println!(
        "samples={}  mean={:.1} ms  p50={:.1} ms  p95={:.1} ms  p99={:.1} ms  min={:.1}  max={:.1}",
        latencies_ms.len(),
        mean,
        p50,
        p95,
        p99,
        latencies_ms.first().copied().unwrap_or(0.0),
        latencies_ms.last().copied().unwrap_or(0.0),
    );

    drop(file);
    let _ = tokio::fs::remove_dir_all(&root).await;
    Ok(())
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() { return 0.0; }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}
