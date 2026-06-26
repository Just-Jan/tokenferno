//! FUN mode — the showpiece.
//!
//! A full-screen, real-time **Doom fire** whose height and ferocity are
//! fueled directly by how fast Claude + Copilot are burning tokens, with
//! a crisp heads-up overlay: a per-provider scoreboard, a hero odometer
//! of tokens burned today, and the live tokens/second rate. Idle? The
//! fire dies to a flickering pilot light. Burning hard? Towering inferno.

use crate::model::Snapshot;
use crate::ui::doomfire::FireField;
use crate::ui::{fmt_int, title_bar, Mode};
use chrono::{DateTime, Utc};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Persistent FUN-mode animation state. Owns the fire engine and a small
/// accumulator of pending flares (one per token burst) drained each frame.
pub struct FunState {
    fire: FireField,
    pending_flares: f64,
}

impl FunState {
    pub fn new() -> Self {
        Self {
            fire: FireField::new(),
            pending_flares: 0.0,
        }
    }

    /// Called by the run loop when new token events land. `n` is roughly
    /// `1 + log2(tokens)`; we accumulate it into flare energy that the
    /// next rendered frame turns into licks of flame. Capped so energy
    /// can't pile up while the user sits in another mode and then dump
    /// all at once on switching back to FUN.
    pub fn spawn(&mut self, n: u32) {
        self.pending_flares = (self.pending_flares + n as f64).min(40.0);
    }
}

impl Default for FunState {
    fn default() -> Self {
        Self::new()
    }
}

/// Threshold (tok/s) below which we consider the fire "out". Anything
/// above means real tokens are actually being credited right now.
const BURN_EPS: f64 = 0.5;

/// Map a tokens/second rate to a normalized fuel level (0..1) on a log
/// curve. Saturates high (~60k tok/s) because agentic turns push tens of
/// thousands of tok/s — a lower ceiling would peg the fire at full for
/// any real work and erase all dynamics (and any visible ramp-up).
fn fuel_for(rate_tps: f64) -> f64 {
    if rate_tps <= 0.0 {
        return 0.0;
    }
    (rate_tps + 1.0).log10() / 60_000.0_f64.log10()
}

pub fn render(
    f: &mut Frame,
    snap: &Snapshot,
    tick: u64,
    smoothed_rate: f64,
    displayed_total: u64,
    debug: bool,
    state: &mut FunState,
) {
    let area = f.area();
    // "Burning" reflects the *fire itself* — i.e. real measured token
    // throughput (which `smoothed_rate` eases from `live_rate`), NOT mere
    // process/log activity. This keeps the badge honest: idle agents that
    // are merely writing logs or spiking CPU don't read as burning.
    let burning = smoothed_rate > BURN_EPS;

    // ---- Title bar (shared with DASHBOARD so it never shifts on switch) ----
    let title = title_bar(Mode::Fun, tick, burning);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if burning {
            Color::Rgb(200, 60, 0)
        } else {
            Color::DarkGray
        }))
        .title(Line::from(title));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // ---- Fire simulation (fills the whole inner area) ----
    state.fire.resize(inner.width as usize, inner.height as usize);
    // Drain pending burst energy into flares.
    while state.pending_flares >= 1.0 {
        let strength = (state.pending_flares / 8.0).clamp(0.15, 1.0);
        state.fire.flare(strength);
        state.pending_flares -= 2.0;
    }
    if state.pending_flares < 0.0 {
        state.pending_flares = 0.0;
    }
    state.fire.set_fuel(fuel_for(smoothed_rate));
    state.fire.step();
    render_fire(f.buffer_mut(), inner, &state.fire);

    // ---- HUD overlay ----
    render_scoreboard(f, inner, snap);
    render_hero(f, inner, snap, displayed_total, smoothed_rate, tick, burning);
    if debug {
        render_debug_hud(f, inner, snap);
    }
}

/// A `d`-toggled detection HUD pinned to the bottom row: shows exactly what
/// the app is detecting *right now*, so real-time detection can be verified
/// against an agent that's visibly working. `meas` = measured tok/s (real
/// landed tokens), `proj` = projected tok/s (a request is in flight), `live`
/// = what fuels the fire.
fn render_debug_hud(f: &mut Frame, area: Rect, snap: &Snapshot) {
    if area.height < 4 {
        return;
    }
    let y = area.y + area.height - 1;
    let buf = f.buffer_mut();
    for x in area.x..area.x + area.width {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char(' ');
            cell.set_bg(Color::Rgb(0, 0, 0));
            cell.set_fg(Color::Reset);
        }
    }
    let rect = Rect { x: area.x, y, width: area.width, height: 1 };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            detection_debug(snap),
            Style::default()
                .fg(Color::Rgb(120, 255, 160))
                .bg(Color::Rgb(0, 0, 0))
                .add_modifier(Modifier::BOLD),
        )))
        .style(Style::default().bg(Color::Rgb(0, 0, 0))),
        rect,
    );
}

/// One-line snapshot of the live detection signals (used by the `d` HUD).
pub fn detection_debug(snap: &Snapshot) -> String {
    let now = Utc::now();
    let age = snap
        .last_event_at
        .map(|t| format!("{:.1}s", (now - t).num_milliseconds().max(0) as f64 / 1000.0))
        .unwrap_or_else(|| "—".into());
    let in_flight: u32 = snap.in_flight.values().sum();
    let meas = measured_decis(&snap.burn_per_decisec);
    let proj = projected_rate(snap);
    let live = meas.max(proj);
    format!(
        " DET  watching {} file(s) · last-evt {} · in-flight {} · meas {} · proj {} · LIVE {} tok/s ",
        snap.watched_files,
        age,
        in_flight,
        fmt_rate(meas),
        fmt_rate(proj),
        fmt_rate(live),
    )
}

/// Blit the fire field onto the buffer.
fn render_fire(buf: &mut Buffer, area: Rect, fire: &FireField) {
    let (w, h) = fire.dims();
    for y in 0..h {
        for x in 0..w {
            let px = area.x + x as u16;
            let py = area.y + y as u16;
            if px >= area.x + area.width || py >= area.y + area.height {
                continue;
            }
            let (glyph, color) = fire.glyph_color(x, y);
            if let Some(cell) = buf.cell_mut((px, py)) {
                cell.set_char(glyph);
                cell.set_fg(color);
                cell.set_bg(Color::Reset);
            }
        }
    }
}

/// Two-row Claude vs Copilot scoreboard, plated dark so it reads cleanly
/// over the cool top of the fire.
fn render_scoreboard(f: &mut Frame, area: Rect, snap: &Snapshot) {
    if area.height < 3 {
        return;
    }
    let plate = Style::default().bg(Color::Rgb(10, 6, 6));
    let band = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 2,
    };
    // Clear the band of any fire glyphs so the scoreboard reads cleanly.
    let buf = f.buffer_mut();
    for yy in band.y..band.y + band.height {
        for xx in band.x..band.x + band.width {
            if let Some(cell) = buf.cell_mut((xx, yy)) {
                cell.set_char(' ');
                cell.set_bg(Color::Rgb(10, 6, 6));
                cell.set_fg(Color::Reset);
            }
        }
    }
    let lines = vec![
        provider_line(snap, "claude", "CLAUDE ", Color::Rgb(255, 150, 70)),
        provider_line(snap, "copilot", "COPILOT", Color::Rgb(120, 200, 255)),
    ];
    f.render_widget(Paragraph::new(lines).style(plate), band);
}

fn provider_line(snap: &Snapshot, key: &str, label: &str, accent: Color) -> Line<'static> {
    let total = snap.by_provider.get(key).map(|t| t.total).unwrap_or(0);
    // Same definition as the fire, so the scoreboard rate and the flames
    // never disagree (e.g. scoreboard showing tens of k while the fire is out).
    let rate = provider_live_rate(snap, key);
    let spark = snap
        .burn_per_sec_by_provider
        .get(key)
        .map(|d| sparkline(d, 22))
        .unwrap_or_default();
    Line::from(vec![
        Span::styled(
            format!(" {label} "),
            Style::default()
                .fg(Color::Black)
                .bg(accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {:>13} tok ", fmt_int(total)),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(spark, Style::default().fg(accent)),
        Span::styled(
            format!("  {:>5} tok/s ", fmt_rate(rate)),
            Style::default().fg(Color::Gray),
        ),
    ])
}

/// Hero block: the big odometer of tokens burned today plus a live
/// tokens/second readout, centered in the upper third where the fire is
/// coolest (most readable).
fn render_hero(
    f: &mut Frame,
    area: Rect,
    snap: &Snapshot,
    displayed_total: u64,
    rate_tps: f64,
    tick: u64,
    burning: bool,
) {
    let top = area.y + 3;
    // Odometer needs 5 rows + a label row; bail to a compact readout if
    // the terminal is too short.
    if area.height >= 13 && top + 7 < area.y + area.height {
        render_odometer(f.buffer_mut(), area, top, displayed_total);
        let label_y = top + 5;
        let label = Line::from(vec![
            Span::styled(
                "  tokens burned today  ",
                Style::default()
                    .bg(Color::Rgb(12, 6, 0))
                    .fg(Color::Rgb(255, 200, 120)),
            ),
        ]);
        let label_rect = Rect {
            x: area.x,
            y: label_y,
            width: area.width,
            height: 1,
        };
        f.render_widget(Paragraph::new(label).alignment(Alignment::Center), label_rect);

        // Live rate readout, one line below.
        let flame = if burning {
            ["▴", "▲", "▴"][(tick / 3 % 3) as usize]
        } else {
            "·"
        };
        let rate_line = Line::from(vec![
            Span::styled(
                format!("  {flame} "),
                Style::default().bg(Color::Rgb(12, 6, 0)),
            ),
            Span::styled(
                format!("{} ", fmt_rate(rate_tps)),
                Style::default()
                    .bg(Color::Rgb(12, 6, 0))
                    .fg(Color::Rgb(255, 240, 200))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "tokens / second  ",
                Style::default()
                    .bg(Color::Rgb(12, 6, 0))
                    .fg(Color::Gray),
            ),
        ]);
        let rate_rect = Rect {
            x: area.x,
            y: label_y + 1,
            width: area.width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(rate_line).alignment(Alignment::Center),
            rate_rect,
        );

        // Breakdown: most of "tokens burned today" is the prompt context
        // re-sent every turn, not newly generated tokens — show the split so
        // the headline number is honest.
        let generated = snap.today.output + snap.today.reasoning;
        let context = snap.today.input + snap.today.cache_read + snap.today.cache_creation;
        let breakdown = Line::from(vec![
            Span::styled(
                format!("  ▲ generated {}  ", fmt_compact(generated)),
                Style::default()
                    .bg(Color::Rgb(12, 6, 0))
                    .fg(Color::Rgb(255, 210, 120)),
            ),
            Span::styled(
                format!("▼ context {}  ", fmt_compact(context)),
                Style::default()
                    .bg(Color::Rgb(12, 6, 0))
                    .fg(Color::Gray),
            ),
        ]);
        let breakdown_rect = Rect {
            x: area.x,
            y: label_y + 2,
            width: area.width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(breakdown).alignment(Alignment::Center),
            breakdown_rect,
        );
    } else {
        // Compact: single big readout of the live rate.
        let line = Line::from(vec![Span::styled(
            format!("  {} tok/s  ·  {} burned today  ", fmt_rate(rate_tps), fmt_int(snap.today.total)),
            Style::default()
                .bg(Color::Rgb(12, 6, 0))
                .fg(Color::Rgb(255, 240, 200))
                .add_modifier(Modifier::BOLD),
        )]);
        let rect = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: 1,
        };
        f.render_widget(Paragraph::new(line).alignment(Alignment::Center), rect);
    }
}

const DIGITS: [[&str; 5]; 10] = [
    [" ███ ", "█   █", "█   █", "█   █", " ███ "], // 0
    ["  █  ", " ██  ", "  █  ", "  █  ", " ███ "], // 1
    [" ██  ", "█  █ ", "  █  ", " █   ", "████ "], // 2
    [" ██  ", "█  █ ", "  █  ", "█  █ ", " ██  "], // 3
    ["█  █ ", "█  █ ", "████ ", "   █ ", "   █ "], // 4
    ["████ ", "█    ", "███  ", "   █ ", "███  "], // 5
    [" ███ ", "█    ", "███  ", "█  █ ", " ██  "], // 6
    ["████ ", "   █ ", "  █  ", " █   ", "█    "], // 7
    [" ██  ", "█  █ ", " ██  ", "█  █ ", " ██  "], // 8
    [" ██  ", "█  █ ", " ███ ", "   █ ", " ██  "], // 9
];

/// Draw `value` (with thousands spaces) as 5-row block digits, centered
/// horizontally, directly onto the buffer so the fire shows through the
/// gaps. Digit strokes are drawn molten (orange top → white-hot bottom)
/// over a subtle dark halo for contrast.
fn render_odometer(buf: &mut Buffer, area: Rect, top: u16, value: u64) {
    let s = fmt_int(value);
    const DIGIT_W: u16 = 5;
    const GAP: u16 = 1;
    const SPACE_W: u16 = 2;
    let total: u16 = s
        .chars()
        .map(|c| if c == ' ' { SPACE_W } else { DIGIT_W + GAP })
        .sum();
    if total == 0 || total > area.width {
        return;
    }
    let start_x = area.x + (area.width - total) / 2;
    let row_color = |r: usize| -> Color {
        // Top of the digit is cooler, bottom is white-hot.
        match r {
            0 => Color::Rgb(255, 170, 60),
            1 => Color::Rgb(255, 190, 80),
            2 => Color::Rgb(255, 210, 110),
            3 => Color::Rgb(255, 230, 160),
            _ => Color::Rgb(255, 248, 210),
        }
    };

    // Dark halo behind the whole number for legibility.
    for r in 0..5u16 {
        for c in 0..total {
            let x = start_x + c;
            let y = top + r;
            if x < area.x + area.width && y < area.y + area.height {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_bg(Color::Rgb(14, 6, 2));
                }
            }
        }
    }

    let mut cx = start_x;
    for ch in s.chars() {
        if ch == ' ' {
            cx += SPACE_W;
            continue;
        }
        let d = (ch as u8 - b'0') as usize;
        for (r, glyph_row) in DIGITS[d].iter().enumerate() {
            for (c, gch) in glyph_row.chars().enumerate() {
                if gch != ' ' {
                    let x = cx + c as u16;
                    let y = top + r as u16;
                    if x < area.x + area.width && y < area.y + area.height {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char('█');
                            cell.set_fg(row_color(r));
                            cell.set_bg(Color::Rgb(14, 6, 2));
                        }
                    }
                }
            }
        }
        cx += DIGIT_W + GAP;
    }
}

fn sparkline(data: &[u64], width: usize) -> String {
    const BARS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if data.is_empty() {
        return " ".repeat(width);
    }
    let slice = if data.len() > width {
        &data[data.len() - width..]
    } else {
        data
    };
    let max = *slice.iter().max().unwrap_or(&1).max(&1);
    let mut s: String = slice
        .iter()
        .map(|&v| {
            let idx = ((v as f64 / max as f64) * (BARS.len() - 1) as f64).round() as usize;
            BARS[idx.min(BARS.len() - 1)]
        })
        .collect();
    while s.chars().count() < width {
        s.insert(0, ' ');
    }
    s
}

fn fmt_rate(tps: f64) -> String {
    if !(tps >= 0.5) {
        return "0".to_string(); // also catches -0.0 / NaN
    }
    if tps >= 1000.0 {
        format!("{:.1}k", tps / 1000.0)
    } else {
        format!("{:.0}", tps)
    }
}

/// Compact token-count formatting for the breakdown line (e.g. 45.2k, 2.05M).
pub fn fmt_compact(n: u64) -> String {
    let n = n as f64;
    if n >= 1_000_000_000.0 {
        format!("{:.2}B", n / 1_000_000_000.0)
    } else if n >= 1_000_000.0 {
        format!("{:.2}M", n / 1_000_000.0)
    } else if n >= 1_000.0 {
        format!("{:.1}k", n / 1_000.0)
    } else {
        format!("{n:.0}")
    }
}

/// Window used by BOTH the fire and the scoreboard so they always agree.
/// ~3 s: long enough to bridge the brief gaps between an agent's turns,
/// short enough to fade promptly when work actually stops.
const LIVE_WINDOW_DECIS: usize = 30; // 30 × 100 ms = 3 s

/// The live tokens/second that fuels the fire = measured throughput over the
/// shared window, or the in-flight projection if higher (so a long *single*
/// generation, which logs nothing until it finishes, still burns).
pub fn live_rate(snap: &Snapshot) -> f64 {
    measured_decis(&snap.burn_per_decisec).max(projected_rate(snap))
}

/// Per-provider live rate (same definition as `live_rate`), for the
/// scoreboard — keeps the scoreboard numbers consistent with the fire.
pub fn provider_live_rate(snap: &Snapshot, key: &str) -> f64 {
    let measured = snap
        .burn_per_decisec_by_provider
        .get(key)
        .map(|d| measured_decis(d))
        .unwrap_or(0.0);
    let projected = snap
        .in_flight_by_provider
        .get(key)
        .map(|(n, oldest)| project_one(snap, key, *n, *oldest))
        .unwrap_or(0.0);
    measured.max(projected)
}

/// tok/s over the shared window from a decisecond bucket series.
fn measured_decis(d: &[u64]) -> f64 {
    if d.is_empty() {
        return 0.0;
    }
    let n = d.len().min(LIVE_WINDOW_DECIS);
    let sum: u64 = d.iter().rev().take(n).sum();
    sum as f64 / (n as f64 * 0.1)
}

/// Sum of the in-flight projection across providers (drives the global fire).
fn projected_rate(snap: &Snapshot) -> f64 {
    snap.in_flight_by_provider
        .iter()
        .map(|(provider, (n, oldest))| project_one(snap, provider, *n, *oldest))
        .sum()
}

/// Projected tok/s for ONE provider's in-flight request(s), so the fire
/// stays alive while the model is generating — including during *long*
/// tasks. Sustains at the provider's median tok/s for the whole request
/// (after a brief gradual ignition). Two guards keep a stuck/orphaned start
/// from burning forever (and neither can cause idle burning, since idle has
/// no in-flight start): a recent-activity gate and a hard elapsed cap.
fn project_one(snap: &Snapshot, provider: &str, n: u32, oldest: DateTime<Utc>) -> f64 {
    const SUSTAIN_CAP_S: f64 = 90.0;
    const SILENCE_CUTOFF_S: f64 = 45.0;
    if n == 0 {
        return 0.0;
    }
    let now = Utc::now();
    let elapsed = (now - oldest).num_milliseconds().max(0) as f64 / 1000.0;
    if elapsed > SUSTAIN_CAP_S {
        return 0.0;
    }
    let silent_for = snap
        .last_activity_at
        .get(provider)
        .map(|t| (now - *t).num_milliseconds() as f64 / 1000.0)
        .unwrap_or(f64::INFINITY);
    if silent_for > SILENCE_CUTOFF_S {
        return 0.0;
    }
    let median_tps = snap
        .median_tps_by_provider
        .get(provider)
        .copied()
        .unwrap_or(5000.0);
    let ramp = (elapsed / 1.8).min(1.0).powi(2);
    let wind = if elapsed < SUSTAIN_CAP_S - 15.0 {
        1.0
    } else {
        ((SUSTAIN_CAP_S - elapsed) / 15.0).clamp(0.0, 1.0)
    };
    let factor = ramp * wind;
    if factor <= 0.0 {
        return 0.0;
    }
    median_tps * (n as f64) * factor
}

