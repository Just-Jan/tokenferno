//! Offline render harness: builds a synthetic Snapshot and dumps a frame
//! of each UI mode to the terminal as ANSI text, so we can inspect the
//! layout/colors without a live TTY.
//!
//! Usage: cargo run --example render_dump -- [mode] [rate_tps] [ticks]

use tokenferno::model::{Provider, RecentEvent, SessionStat, Snapshot, Totals};
use tokenferno::ui::{self, Filter};
use chrono::Utc;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use ratatui::Terminal;

fn to_rgb(c: Color, bg: bool) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::Red => (205, 49, 49),
        Color::Green => (13, 188, 121),
        Color::Yellow => (229, 229, 16),
        Color::Blue => (36, 114, 200),
        Color::Magenta => (188, 63, 188),
        Color::Cyan => (17, 168, 205),
        Color::Gray => (190, 190, 190),
        Color::DarkGray => (102, 102, 102),
        Color::LightRed => (241, 76, 76),
        Color::LightGreen => (35, 209, 139),
        Color::LightYellow => (245, 245, 67),
        Color::LightBlue => (59, 142, 234),
        Color::LightMagenta => (214, 112, 214),
        Color::LightCyan => (41, 184, 219),
        Color::White => (229, 229, 229),
        Color::Indexed(_) => (180, 180, 180),
        Color::Reset => {
            if bg {
                (0, 0, 0)
            } else {
                (200, 200, 200)
            }
        }
    }
}

fn dump(buf: &Buffer) {
    let area = buf.area;
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            let cell = buf.cell((x, y)).unwrap();
            let (fr, fg, fb) = to_rgb(cell.fg, false);
            let (br, bg, bb) = to_rgb(cell.bg, true);
            line.push_str(&format!(
                "\x1b[38;2;{fr};{fg};{fb};48;2;{br};{bg};{bb}m{}",
                cell.symbol()
            ));
        }
        line.push_str("\x1b[0m");
        println!("{line}");
    }
}

fn synthetic(rate_tps: f64) -> Snapshot {
    let now = Utc::now();
    let mut s = Snapshot::default();
    s.generated_at = now;
    s.today = Totals {
        input: 1_432_105,
        output: 87_412,
        cache_read: 412_880,
        cache_creation: 88_040,
        reasoning: 1200,
        total: 2_021_637,
        events: 318,
    };
    s.by_provider.insert(
        "claude",
        Totals { input: 900_000, output: 50_000, cache_read: 300_000, cache_creation: 60_000, reasoning: 0, total: 1_310_000, events: 200 },
    );
    s.by_provider.insert(
        "copilot",
        Totals { input: 532_105, output: 37_412, cache_read: 112_880, cache_creation: 28_040, reasoning: 1200, total: 711_637, events: 118 },
    );
    s.micro_rate_tps = rate_tps;
    s.instant_rate_tps = rate_tps;
    s.current_rate_tps = rate_tps;
    s.current_rate_tpm = rate_tps * 60.0;
    s.peak_rate_tpm = 41_000.0;
    s.peak_rate_at = Some(now);
    s.last_event_at = Some(now);
    s.last_event_tokens = 412;
    s.events_per_min = 24.0;
    s.watched_files = 6;
    s.micro_rate_tps_by_provider.insert("claude", rate_tps * 0.66);
    s.micro_rate_tps_by_provider.insert("copilot", rate_tps * 0.34);
    s.current_rate_tps_by_provider.insert("claude", rate_tps * 0.66);
    s.current_rate_tps_by_provider.insert("copilot", rate_tps * 0.34);
    s.current_rate_tpm_by_provider.insert("claude", rate_tps * 60.0 * 0.66);
    s.current_rate_tpm_by_provider.insert("copilot", rate_tps * 60.0 * 0.34);
    s.burn_per_sec = (0..60).map(|i| ((i as f64 / 6.0).sin().abs() * rate_tps) as u64).collect();
    s.burn_per_sec_by_provider.insert(
        "claude",
        (0..60).map(|i| ((i as f64 / 5.0).sin().abs() * rate_tps * 0.66) as u64).collect(),
    );
    s.burn_per_sec_by_provider.insert(
        "copilot",
        (0..60).map(|i| ((i as f64 / 7.0).cos().abs() * rate_tps * 0.34) as u64).collect(),
    );
    s.burn_per_min = Vec::new();
    // Live decisecond buckets — THIS is what `live_rate` (and so the whole
    // dashboard's "live" readings) actually reads. Make the last ~3 s hot so
    // the harness exercises the same path the real app uses. 30 buckets ×
    // (rate·0.1) summed over 3 s ⇒ LIVE ≈ rate_tps.
    let decis: Vec<u64> = (0..600)
        .map(|i| {
            if i >= 570 {
                (rate_tps * 0.1) as u64
            } else if i >= 540 {
                (rate_tps * 0.05) as u64
            } else {
                0
            }
        })
        .collect();
    s.burn_per_decisec_by_provider
        .insert("claude", decis.iter().map(|v| (*v as f64 * 0.66) as u64).collect());
    s.burn_per_decisec_by_provider
        .insert("copilot", decis.iter().map(|v| (*v as f64 * 0.34) as u64).collect());
    s.burn_per_decisec = decis;
    // One in-flight Copilot request, mid-generation, to light the in-flight
    // tile and the projection.
    s.in_flight_by_provider
        .insert("copilot", (1, now - chrono::Duration::seconds(3)));
    s.median_tps_by_provider.insert("claude", rate_tps * 0.66);
    s.median_tps_by_provider.insert("copilot", rate_tps * 0.34);
    s.ttf_median_ms_by_provider.insert("claude", 180);
    s.ttf_median_ms_by_provider.insert("copilot", 240);
    s.drip_per_min = 0.4;
    s.last_activity_at.insert("claude", now);
    s.last_activity_at.insert("copilot", now);
    s.sessions = vec![
        SessionStat { provider: Provider::Claude, session_id: "3c1149aa".into(), model: "claude-sonnet-4-6".into(), totals: Totals { input: 12_312, output: 1_102, cache_read: 88_003, cache_creation: 4_950, reasoning: 0, total: 106_367, events: 12 }, last_seen: now },
        SessionStat { provider: Provider::Claude, session_id: "00a1b2cc".into(), model: "claude-opus-4-7".into(), totals: Totals { input: 4_050, output: 210, cache_read: 1_998, cache_creation: 0, reasoning: 0, total: 6_258, events: 4 }, last_seen: now },
        SessionStat { provider: Provider::Copilot, session_id: "f4f18cdd".into(), model: "claude-opus-4.7".into(), totals: Totals { input: 23_156, output: 90, cache_read: 22_068, cache_creation: 0, reasoning: 0, total: 23_246, events: 9 }, last_seen: now },
    ];
    s.recent = vec![
        RecentEvent { ts: now, provider: Provider::Claude, session_id: "3c1149".into(), model: "sonnet-4-6".into(), input: 412, output: 18, cache_read: 1200 },
        RecentEvent { ts: now, provider: Provider::Copilot, session_id: "f4f18c".into(), model: "opus-4.7".into(), input: 23000, output: 90, cache_read: 22000 },
    ].into();
    s
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("inferno");
    let rate: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(120.0);
    let ticks: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(40);
    let debug = args.get(4).map(|s| s == "debug").unwrap_or(false);

    let snap = synthetic(rate);
    let w: u16 = std::env::var("TF_DUMP_W").ok().and_then(|s| s.parse().ok()).unwrap_or(100);
    let h: u16 = std::env::var("TF_DUMP_H").ok().and_then(|s| s.parse().ok()).unwrap_or(34);
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut fun_state = ui::fun::FunState::new();

    // Advance animation a few ticks so particles/fire have spawned.
    for t in 0..ticks {
        terminal
            .draw(|f| match mode {
                "dashboard" | "dash" => {
                    ui::dashboard::render(f, &snap, Filter::All, snap.today.total, t)
                }
                _ => ui::fun::render(f, &snap, t, rate, snap.today.total, debug, &mut fun_state),
            })
            .unwrap();
    }
    dump(terminal.backend().buffer());
}
