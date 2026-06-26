mod aggregate;
mod exec;
mod ingest;
mod ipc;
mod model;
mod persist;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use directories::BaseDirs;
use futures::StreamExt;
use model::Provider;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::path::PathBuf;
use tokio::sync::mpsc;
use ui::{Filter, Mode};

#[derive(Parser, Debug)]
#[command(version, about = "real-time TUI showing AI agent token burn")]
struct Args {
    /// Override Claude projects root.
    #[arg(long)]
    claude_root: Option<PathBuf>,
    /// Override Copilot logs directory.
    #[arg(long)]
    copilot_logs: Option<PathBuf>,
    /// Start in a specific mode.
    #[arg(long, value_parser = ["dashboard","inferno"], default_value = "inferno")]
    mode: String,
    /// Override the IPC socket path used to link `tokenferno exec` wrappers
    /// to a running TUI. Defaults to `~/.tokenferno/sock`. Running
    /// `tokenferno exec -- <cmd>` will auto-link to this socket if a TUI is up.
    #[arg(long)]
    ipc_socket: Option<PathBuf>,
    /// Disable persistence of authoritative events to
    /// `~/.tokenferno/events-YYYY-MM-DD.jsonl` (and skip replay-on-start).
    #[arg(long)]
    no_persist: bool,
    /// Render to the normal screen instead of the alternate screen (useful
    /// for capturing/recording the UI with a terminal emulator).
    #[arg(long)]
    no_altscreen: bool,
    /// Start with the detection HUD enabled (same as pressing `d`).
    #[arg(long)]
    debug: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run an agent under a PTY wrapper that tees output and emits ingest
    /// events as bytes arrive (sub-disk-flush latency). If a `tokenferno`
    /// TUI is running on the same machine, this command auto-links to it via
    /// a Unix socket so events show up in that dashboard. Falls back to
    /// stderr summary lines if no TUI is reachable.
    Exec {
        /// Command and arguments. Use `--` to separate from this CLI's flags.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 1..)]
        argv: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let args = Args::parse();
    // Preserve the user's history/socket across the burn-the-planet→tokenferno
    // rename: move ~/.burn-the-planet → ~/.tokenferno once, if needed.
    persist::migrate_legacy_dir();
    let ipc_path = args
        .ipc_socket
        .clone()
        .unwrap_or_else(ipc::default_socket_path);

    if let Some(Cmd::Exec { argv }) = args.cmd {
        let (event_tx, mut event_rx) = mpsc::channel::<ingest::IngestMessage>(1024);
        // Bridge events both to a local stderr-summary fallback AND (best
        // effort) to a running TUI via the IPC socket. If no TUI is up the
        // socket sender silently retries; the stderr fallback always works.
        let (ipc_tx, ipc_rx) = mpsc::channel::<ingest::IngestMessage>(1024);
        let ipc_path_for_send = ipc_path.clone();
        let ipc_handle = tokio::spawn(async move {
            let _ = ipc::send_from_rx(ipc_path_for_send, ipc_rx).await;
        });
        let drain = tokio::spawn(async move {
            while let Some(msg) = event_rx.recv().await {
                // Forward a clone to the TUI socket sender (drop if full —
                // we don't want to back-pressure the agent on a missing TUI).
                let dup = msg.clone();
                let _ = ipc_tx.try_send(dup);
                if let ingest::IngestMessage::Event(ev) = &msg {
                    eprintln!(
                        "tokenferno: detected {} tokens ({:?} {})",
                        ev.total_tokens, ev.provider, ev.model
                    );
                }
            }
        });
        let result = exec::run(argv, event_tx).await;
        drop(drain);
        ipc_handle.abort();
        return result;
    }

    let home = BaseDirs::new().map(|b| b.home_dir().to_path_buf());
    let claude_root = args
        .claude_root
        .or_else(|| home.as_ref().map(|h| h.join(".claude/projects")))
        .unwrap_or_else(|| PathBuf::from(".claude/projects"));
    let copilot_logs = args
        .copilot_logs
        .or_else(|| home.as_ref().map(|h| h.join(".copilot/logs")))
        .unwrap_or_else(|| PathBuf::from(".copilot/logs"));

    let (event_tx, event_rx) = mpsc::channel(1024);
    let (cmd_tx, cmd_rx) = mpsc::channel(8);

    {
        let tx = event_tx.clone();
        let path = claude_root.clone();
        tokio::spawn(async move {
            if let Err(e) = ingest::claude::run(path, tx).await {
                tracing::warn!(error = ?e, "claude ingest exited");
            }
        });
    }
    {
        let tx = event_tx.clone();
        let path = copilot_logs.clone();
        tokio::spawn(async move {
            if let Err(e) = ingest::copilot::run(path, tx).await {
                tracing::warn!(error = ?e, "copilot ingest exited");
            }
        });
    }
    // NOTE: there used to be a `proc_watch` ingest that emitted a synthetic
    // RequestStart on every CPU rising-edge of a claude/copilot/node
    // process. It fired several phantom starts per second for any running
    // CLI that merely spiked CPU (even an idle Copilot session just
    // rendering), which saturated `in_flight` and made the dashboard read
    // as "burning" while no tokens were being produced. It was removed:
    // real inference requests are detected from the providers' own log
    // signals ("Sending request to the AI model" for Copilot; the Claude
    // JSONL turn markers), and the live burn rate is now measured from
    // actual token throughput — so detection stays accurate without the
    // false positives.
    {
        let tx = event_tx.clone();
        let path = ipc_path.clone();
        tokio::spawn(async move {
            if let Err(e) = ipc::serve(&path, tx).await {
                tracing::warn!(error = ?e, "ipc serve exited");
            }
        });
    }
    drop(event_tx);

    let snap_rx = aggregate::spawn_with(event_rx, cmd_rx, !args.no_persist);

    let mut mode = match args.mode.as_str() {
        "dashboard" | "dash" => Mode::Dash,
        _ => Mode::Fun,
    };
    let mut filter = Filter::All;

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    if !args.no_altscreen {
        execute!(stdout, EnterAlternateScreen)?;
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(
        &mut terminal,
        snap_rx,
        cmd_tx,
        &mut mode,
        &mut filter,
        args.debug,
    )
    .await;

    disable_raw_mode().ok();
    if !args.no_altscreen {
        execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    }
    terminal.show_cursor().ok();
    result
}

async fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    mut snap_rx: tokio::sync::watch::Receiver<std::sync::Arc<model::Snapshot>>,
    cmd_tx: mpsc::Sender<aggregate::Command>,
    mode: &mut Mode,
    filter: &mut Filter,
    debug_init: bool,
) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(33));
    let mut current_tick_ms: u64 = 33;
    let mut tick_counter: u64 = 0;
    let mut displayed_total: u64 = 0;
    let mut current = snap_rx.borrow().clone();
    let mut displayed_rate: f64 = 0.0;
    let mut fun_state = ui::fun::FunState::new();
    let mut debug_hud = debug_init;
    let mut last_event_count_for_particles: u64 = current.today.events;
    let mut last_frame_at = std::time::Instant::now();
    loop {
        // Adaptive frame rate: 60 fps while tokens are actually flowing
        // (or the fire is still visibly cooling down), 30 fps when idle.
        // Keyed off measured token throughput + the smoothed display rate
        // — NOT in-flight/activity heuristics, which fire on mere process
        // liveness and would pin us at 60 fps while nothing is burning.
        let token_active = current.micro_rate_tps > 0.0 || current.instant_rate_tps > 0.0;
        let want_ms: u64 = if token_active || displayed_rate > 0.5 {
            16
        } else {
            33
        };
        if want_ms != current_tick_ms {
            tick = tokio::time::interval(std::time::Duration::from_millis(want_ms));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            current_tick_ms = want_ms;
        }
        // Odometer: snap to the historical total at startup, then roll up
        // only while tokens are actually being burned (see below).
        let frame_dt = last_frame_at.elapsed().as_secs_f64();
        last_frame_at = std::time::Instant::now();
        let target = match *filter {
            Filter::All => current.today.total,
            Filter::Only(p) => current
                .by_provider
                .get(p.label())
                .map(|t| t.total)
                .unwrap_or(0),
        };
        if displayed_total >= target {
            // Already there, or the day rolled over and the total reset —
            // snap down.
            displayed_total = target;
        } else {
            let gap = target - displayed_total;
            // Only treat the gap as live spending if tokens are actually
            // being burned right now. Otherwise the gap is historical —
            // the startup backfill / persisted-replay total, or a day
            // rollover — and must NOT be animated, or the odometer
            // "counts up from 0" while nothing is happening.
            let burn = ui::fun::live_rate(&current);
            if burn < 0.5 {
                displayed_total = target;
            } else {
                // Live burn: roll the odometer up smoothly. Ease ~18 % of
                // the remaining gap per frame (a snappy odometer spin),
                // floored by the measured rate, capped at the gap.
                let ease = (gap as f64 * 0.18).round() as u64;
                let by_rate = (burn * frame_dt).round() as u64;
                let step = ease.max(by_rate).max(1);
                displayed_total = (displayed_total + step).min(target);
            }
        }
        // Smoothly ease the displayed live rate (used by fun mode) toward
        // the target rate, so the big number ramps up/down instead of jumping.
        // Asymmetric easing: rise faster than fall, so a new burst feels snappy
        // but dropoff still glides instead of snapping to 0.
        let target_rate = ui::fun::live_rate(&current);
        // Asymmetric easing: rise fast (a new burst feels snappy), fall
        // even faster so the fire visibly cools the moment token
        // traffic drops instead of lingering for a second after.
        let alpha = if target_rate > displayed_rate {
            0.45
        } else {
            0.72
        };
        displayed_rate += (target_rate - displayed_rate) * alpha;
        if (target_rate - displayed_rate).abs() < 0.05 {
            displayed_rate = target_rate;
        }
        // Particle accounting: any new event since last frame spawns embers.
        let cur_events = current.today.events;
        if cur_events > last_event_count_for_particles {
            let new_events = cur_events - last_event_count_for_particles;
            // Approximate per-event token magnitude by recent burst average.
            let approx_tokens = (current.last_event_tokens.max(1) as f64).log2().max(1.0) as u32;
            for _ in 0..new_events.min(8) {
                fun_state.spawn(1 + approx_tokens);
            }
            last_event_count_for_particles = cur_events;
        }
        terminal.draw(|f| {
            let snap = &current;
            match *mode {
                Mode::Dash => {
                    ui::dashboard::render(f, snap, *filter, displayed_total, tick_counter)
                }
                Mode::Fun => ui::fun::render(
                    f,
                    snap,
                    tick_counter,
                    displayed_rate,
                    displayed_total,
                    debug_hud,
                    &mut fun_state,
                ),
            }
        })?;

        tokio::select! {
            _ = tick.tick() => { tick_counter = tick_counter.wrapping_add(1); }
            changed = snap_rx.changed() => {
                if changed.is_err() { break; }
                current = snap_rx.borrow().clone();
            }
            ev = events.next() => {
                match ev {
                    Some(Ok(Event::Key(k))) if k.kind == KeyEventKind::Press => {
                        match k.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break,
                            KeyCode::Char('1') => *mode = Mode::Fun,
                            KeyCode::Char('2') => *mode = Mode::Dash,
                            KeyCode::Tab      => *mode = mode.next(),
                            KeyCode::Char('c') => *filter = toggle_filter(*filter, Provider::Claude),
                            KeyCode::Char('p') => *filter = toggle_filter(*filter, Provider::Copilot),
                            KeyCode::Char('a') => *filter = Filter::All,
                            KeyCode::Char('d') => debug_hud = !debug_hud,
                            KeyCode::Char('r') => { let _ = cmd_tx.send(aggregate::Command::Reset).await; }
                            _ => {}
                        }
                    }
                    Some(Ok(Event::Resize(_,_))) => {}
                    Some(Err(_)) | None => break,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn toggle_filter(current: Filter, p: Provider) -> Filter {
    match current {
        Filter::All => Filter::Only(p),
        Filter::Only(x) if x == p => Filter::All,
        Filter::Only(_) => Filter::Only(p),
    }
}
