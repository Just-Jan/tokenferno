//! DASHBOARD mode — the data view.
//!
//! A single, live operations dashboard that merges the former "hybrid" live
//! tiles with the detailed per-session / recent-event tables of the former
//! "nerd" view.
//!
//! Crucially, every *live* reading here is driven by the SAME corrected
//! detection that fuels the FUN fire — [`live_rate`] / [`provider_live_rate`]
//! (measured ~3 s throughput **or** the in-flight projection, whichever is
//! higher). The old hybrid view read raw 200 ms / 500 ms windows
//! (`micro_rate_tps` / `instant_rate_tps`), which miss Copilot entirely
//! (it logs its usage as one lump at the *end* of a turn) and carry no
//! in-flight projection — so it sat at "nothing happening" while an agent
//! was clearly working. Sourcing from `live_rate` is what makes this
//! dashboard actually move.

use super::{fmt_int, humanize_age, short_id, title_bar, Filter, Mode};
use crate::model::{Provider, Snapshot, Totals};
use crate::ui::fun::{fmt_compact, live_rate, provider_live_rate};
use chrono::Utc;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Sparkline, Table};
use ratatui::Frame;

pub fn render(f: &mut Frame, snap: &Snapshot, filter: Filter, displayed_total: u64, frame: u64) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header: switcher + total + breakdown
            Constraint::Length(3), // KPI tiles
            Constraint::Length(1), // burn meter (tachometer)
            Constraint::Length(1), // heartbeat strip (100 ms buckets)
            Constraint::Length(5), // tokens/sec · last 60 s
            Constraint::Min(8),    // providers | recent events
            Constraint::Length(1), // footer
        ])
        .split(area);

    render_header(f, chunks[0], snap, filter, displayed_total, frame);
    render_kpis(f, chunks[1], snap, filter);
    crate::ui::burn_meter::render(f, chunks[2], snap, filtered_live(snap, filter));
    render_heartbeat(f, chunks[3], snap, filter);
    render_per_sec(f, chunks[4], snap, filter);
    render_bottom(f, chunks[5], snap, filter);
    render_footer(f, chunks[6], snap);
}

// ---- filter helpers -------------------------------------------------------

fn filter_label(filter: Filter) -> Option<&'static str> {
    match filter {
        Filter::All => None,
        Filter::Only(p) => Some(p.label()),
    }
}

/// THE fix: live tok/s for the active filter, from the same source as the
/// FUN fire (measured window ∨ in-flight projection).
fn filtered_live(snap: &Snapshot, filter: Filter) -> f64 {
    match filter {
        Filter::All => live_rate(snap),
        Filter::Only(p) => provider_live_rate(snap, p.label()),
    }
}

fn filtered_current_rate_tpm(snap: &Snapshot, filter: Filter) -> f64 {
    match filter_label(filter) {
        Some(p) => snap
            .current_rate_tpm_by_provider
            .get(p)
            .copied()
            .unwrap_or(0.0),
        None => snap.current_rate_tpm,
    }
}

fn filtered_today(snap: &Snapshot, filter: Filter) -> Totals {
    match filter_label(filter) {
        Some(p) => snap.by_provider.get(p).cloned().unwrap_or_default(),
        None => snap.today.clone(),
    }
}

// ---- header ---------------------------------------------------------------

fn drip_badge(drip_per_min: f64) -> Span<'static> {
    if drip_per_min < 0.1 {
        Span::styled(" — ", Style::default().fg(Color::DarkGray))
    } else {
        Span::styled(
            format!(" {drip_per_min:.1}/min "),
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightCyan)
                .add_modifier(Modifier::BOLD),
        )
    }
}

fn render_header(
    f: &mut Frame,
    area: Rect,
    snap: &Snapshot,
    filter: Filter,
    displayed_total: u64,
    frame: u64,
) {
    let t = filtered_today(snap, filter);
    let live = filtered_live(snap, filter);
    let burning = live > 0.5;

    let total_style = if burning {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    };
    let label = match filter {
        Filter::All => "TOTAL TODAY ",
        Filter::Only(Provider::Claude) => "CLAUDE TODAY ",
        Filter::Only(Provider::Copilot) => "COPILOT TODAY ",
    };
    // Same generated-vs-context split the FUN hero shows: almost all of the
    // "tokens burned today" total is the prompt context re-sent every turn,
    // not newly generated output — so show the honest breakdown here too.
    let generated = t.output + t.reasoning;
    let context = t.input + t.cache_read + t.cache_creation;
    let content = Line::from(vec![
        Span::styled(label, Style::default().fg(Color::Gray)),
        Span::styled(format!(" {} ", fmt_int(displayed_total)), total_style),
        Span::raw("    "),
        Span::styled(
            format!("▲ generated {}", fmt_compact(generated)),
            Style::default().fg(Color::Rgb(255, 210, 120)),
        ),
        Span::raw("   "),
        Span::styled(
            format!("▼ context {}", fmt_compact(context)),
            Style::default().fg(Color::Gray),
        ),
        Span::raw("   "),
        drip_badge(snap.drip_per_min),
    ]);

    let title = title_bar(Mode::Dash, frame, burning);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if burning {
            Color::Rgb(200, 90, 0)
        } else {
            Color::DarkGray
        }))
        .title(Line::from(title));
    f.render_widget(Paragraph::new(content).block(block), area);
}

// ---- KPI tiles ------------------------------------------------------------

fn tile(text: String, style: Style, title: &'static str) -> Paragraph<'static> {
    Paragraph::new(text)
        .style(style)
        .block(Block::default().borders(Borders::ALL).title(title))
}

fn render_kpis(f: &mut Frame, area: Rect, snap: &Snapshot, filter: Filter) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(24), // LIVE
            Constraint::Length(22), // last burst
            Constraint::Length(16), // events/min
            Constraint::Length(14), // ttf
            Constraint::Min(18),    // in-flight
        ])
        .split(area);

    // LIVE tok/s — from live_rate (the corrected source). The BURNING/idle
    // badge itself lives in the shared title bar (so it's identical in both
    // views); here we just surface the rate.
    let live = filtered_live(snap, filter);
    f.render_widget(
        tile(
            format!(" {}/s", human_rate(live)),
            rate_color(live * 60.0),
            " LIVE ",
        ),
        chunks[0],
    );

    // Last completed burst.
    let last_burst = if snap.last_event_tokens > 0 {
        format!(" {} tok", fmt_int(snap.last_event_tokens))
    } else {
        " —".to_string()
    };
    f.render_widget(
        tile(last_burst, Style::default().fg(Color::Cyan), " LAST BURST "),
        chunks[1],
    );

    f.render_widget(
        tile(
            format!(" {:.1}", snap.events_per_min),
            Style::default().fg(Color::LightBlue),
            " EVENTS/MIN ",
        ),
        chunks[2],
    );

    let ttf_med: u64 = match filter_label(filter) {
        Some(p) => snap.ttf_median_ms_by_provider.get(p).copied().unwrap_or(0),
        None => snap
            .ttf_median_ms_by_provider
            .values()
            .copied()
            .max()
            .unwrap_or(0),
    };
    let (ttf_text, ttf_style) = if ttf_med > 0 {
        let st = if ttf_med < 200 {
            Style::default().fg(Color::LightGreen)
        } else if ttf_med < 1000 {
            Style::default().fg(Color::LightYellow)
        } else {
            Style::default().fg(Color::Yellow)
        };
        (format!(" {ttf_med} ms"), st)
    } else {
        (" —".to_string(), Style::default().fg(Color::DarkGray))
    };
    f.render_widget(tile(ttf_text, ttf_style, " TTF p50 "), chunks[3]);

    render_in_flight(f, chunks[4], snap, filter);
}

fn render_in_flight(f: &mut Frame, area: Rect, snap: &Snapshot, filter: Filter) {
    let now = Utc::now();
    let mut spans: Vec<Span> = vec![Span::raw(" ↻ ")];
    let iter: Vec<_> = snap
        .in_flight_by_provider
        .iter()
        .filter(|(provider, _)| match filter_label(filter) {
            Some(only) => **provider == only,
            None => true,
        })
        .collect();
    if iter.is_empty() {
        spans.push(Span::styled("idle", Style::default().fg(Color::DarkGray)));
    } else {
        for (provider, (n, oldest)) in iter {
            let elapsed_s = (now - *oldest).num_milliseconds().max(0) as f64 / 1000.0;
            let short = match *provider {
                "claude" => "C",
                "copilot" => "P",
                other => &other[..1],
            };
            spans.push(Span::styled(
                format!(" {short}●{n}·{elapsed_s:.1}s "),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightYellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    f.render_widget(
        Paragraph::new(Line::from(spans))
            .block(Block::default().borders(Borders::ALL).title(" in-flight ")),
        area,
    );
}

// ---- heartbeat strip ------------------------------------------------------

/// 1-row strip: each cell = one 100 ms `burn_per_decisec` bucket, hue keyed
/// by absolute rate, glyph height by log magnitude.
fn render_heartbeat(f: &mut Frame, area: Rect, snap: &Snapshot, filter: Filter) {
    let empty: Vec<u64> = Vec::new();
    let data: &[u64] = match filter_label(filter) {
        Some(p) => snap
            .burn_per_decisec_by_provider
            .get(p)
            .map(|v| v.as_slice())
            .unwrap_or(&empty),
        None => &snap.burn_per_decisec,
    };
    if area.width == 0 || data.is_empty() {
        return;
    }
    let cells = area.width as usize;
    let start = data.len().saturating_sub(cells);
    let slice = &data[start..];
    let mut spans: Vec<Span> = Vec::with_capacity(slice.len());
    for &v in slice {
        if v == 0 {
            spans.push(Span::styled("·", Style::default().fg(Color::DarkGray)));
            continue;
        }
        let tps = v as f64 * 10.0; // 100 ms bucket ⇒ tok/s = v × 10
        let color = crate::ui::burn_meter::rate_color(tps);
        let g = match v {
            0 => '·',
            1..=2 => '▁',
            3..=8 => '▃',
            9..=24 => '▅',
            25..=80 => '▆',
            _ => '█',
        };
        spans.push(Span::styled(
            g.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ---- tokens/sec sparkline -------------------------------------------------

fn render_per_sec(f: &mut Frame, area: Rect, snap: &Snapshot, filter: Filter) {
    let now = Utc::now();
    let event_age_ms = snap
        .last_event_at
        .map(|ts| (now - ts).num_milliseconds())
        .unwrap_or(i64::MAX);
    let flash_color = if event_age_ms < 200 {
        Color::White
    } else if event_age_ms < 500 {
        Color::LightYellow
    } else if event_age_ms < 900 {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    // Keep AVG / PEAK visible in the title (saves a whole row vs a separate
    // tile while preserving the old "nerd" rate readouts).
    let avg = filtered_current_rate_tpm(snap, filter);
    let peak_at = snap
        .peak_rate_at
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_else(|| "—".to_string());
    let scope = match filter {
        Filter::All => String::new(),
        Filter::Only(p) => format!(" · {}", p.label()),
    };
    let title = format!(
        " tokens/sec · last 60 s{scope}   ·   AVG {}/min · PEAK {}/min @ {peak_at} ",
        human_rate(avg),
        human_rate(snap.peak_rate_tpm),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(flash_color))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let empty: Vec<u64> = Vec::new();
    let data: &[u64] = match filter_label(filter) {
        Some(p) => snap
            .burn_per_sec_by_provider
            .get(p)
            .map(|v| v.as_slice())
            .unwrap_or(&empty),
        None => &snap.burn_per_sec,
    };
    let spark = Sparkline::default()
        .data(data)
        .style(Style::default().fg(if event_age_ms < 600 {
            Color::LightYellow
        } else {
            Color::Yellow
        }));
    f.render_widget(spark, inner);
}

// ---- bottom: providers | recent events ------------------------------------

fn render_bottom(f: &mut Frame, area: Rect, snap: &Snapshot, filter: Filter) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(area);
    render_providers(f, chunks[0], snap, filter);
    render_recent(f, chunks[1], snap);
}

fn render_providers(f: &mut Frame, area: Rect, snap: &Snapshot, filter: Filter) {
    let panes: Vec<Provider> = match filter {
        Filter::All => vec![Provider::Claude, Provider::Copilot],
        Filter::Only(p) => vec![p],
    };
    let constraints: Vec<Constraint> = panes
        .iter()
        .map(|_| Constraint::Percentage(100 / panes.len() as u16))
        .collect();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    for (i, p) in panes.iter().enumerate() {
        render_provider(f, chunks[i], snap, *p);
    }
}

fn render_provider(f: &mut Frame, area: Rect, snap: &Snapshot, provider: Provider) {
    let label = provider.label();
    let totals = snap.by_provider.get(label).cloned().unwrap_or_default();
    let live = provider_live_rate(snap, label);
    let live_badge = if live > 0.5 {
        format!(" ● {}/s ", human_rate(live))
    } else {
        " ○ idle ".to_string()
    };
    let title = Line::from(vec![
        Span::styled(
            format!(" {} ", provider.label().to_uppercase()),
            Style::default()
                .fg(provider_color(provider))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            live_badge,
            if live > 0.5 {
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
        Span::styled(
            format!(
                " in {} · out {} · cache-r {} · cache-w {} ",
                fmt_int(totals.input),
                fmt_int(totals.output),
                fmt_int(totals.cache_read),
                fmt_int(totals.cache_creation),
            ),
            Style::default().fg(Color::Gray),
        ),
    ]);

    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    // sparkline strip on top
    let spark_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1.min(inner.height),
    };
    if let Some(buckets) = snap.burn_per_min_by_provider.get(label) {
        let data: Vec<u64> = buckets.iter().map(|b| b.tokens).collect();
        let spark = Sparkline::default()
            .data(&data)
            .style(Style::default().fg(provider_color(provider)));
        f.render_widget(spark, spark_area);
    }
    if inner.height <= 1 {
        return;
    }

    let table_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height - 1,
    };
    let now = Utc::now();
    let show_reasoning = snap
        .sessions
        .iter()
        .any(|s| s.provider == provider && s.totals.reasoning > 0);
    let rows: Vec<Row> = snap
        .sessions
        .iter()
        .filter(|s| s.provider == provider)
        .take(8)
        .map(|s| {
            let age = (now - s.last_seen).num_seconds();
            let mut cells = vec![
                short_id(&s.session_id),
                truncate(&s.model, 18),
                fmt_int(s.totals.input),
                fmt_int(s.totals.output),
                fmt_int(s.totals.cache_read),
            ];
            if show_reasoning {
                cells.push(fmt_int(s.totals.reasoning));
            }
            cells.push(humanize_age(age));
            Row::new(cells)
        })
        .collect();

    let mut widths = vec![
        Constraint::Length(8),
        Constraint::Length(18),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(9),
    ];
    let mut header_cells = vec!["session", "model", "in", "out", "cache-r"];
    if show_reasoning {
        header_cells.push("reason");
        widths.push(Constraint::Length(8));
    }
    header_cells.push("age");
    widths.push(Constraint::Length(8));
    let header = Row::new(header_cells).style(Style::default().fg(Color::DarkGray));
    let table = Table::new(rows, widths).header(header);
    f.render_widget(table, table_area);
}

/// Recent-events feed (folded in from the former "nerd" view) — a live
/// scroll of the most recent completions across both providers.
fn render_recent(f: &mut Frame, area: Rect, snap: &Snapshot) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" recent events ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let max = inner.height as usize;
    let lines: Vec<Line> = snap
        .recent
        .iter()
        .take(max)
        .map(|e| {
            let ctx = e.input + e.cache_read;
            Line::from(vec![
                Span::styled(
                    e.ts.format("%H:%M:%S").to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(" "),
                Span::styled(
                    provider_letter(e.provider),
                    Style::default()
                        .fg(provider_color(e.provider))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(truncate(&e.model, 12), Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("  out {}", fmt_compact(e.output)),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    format!("  ctx {}", fmt_compact(ctx)),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

// ---- footer ---------------------------------------------------------------

fn render_footer(f: &mut Frame, area: Rect, snap: &Snapshot) {
    let mut spans = vec![
        Span::styled(" watching ", Style::default().fg(Color::DarkGray)),
        Span::raw(snap.watched_files.to_string()),
        Span::styled(" files · dropped ", Style::default().fg(Color::DarkGray)),
        Span::raw(snap.dropped_events.to_string()),
        Span::styled(
            "   ·   c Claude · p Copilot · a all · r reset · d detection-hud",
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if let Some(err) = &snap.last_error {
        spans.push(Span::styled(
            format!("  · {err}"),
            Style::default().fg(Color::Red),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ---- shared formatting ----------------------------------------------------

pub fn human_rate(tpm: f64) -> String {
    if tpm >= 1000.0 {
        format!("{:.1}k tok", tpm / 1000.0)
    } else {
        format!("{:.0} tok", tpm)
    }
}

fn rate_color(tpm: f64) -> Style {
    let color = if tpm < 1_000.0 {
        Color::Blue
    } else if tpm < 10_000.0 {
        Color::Green
    } else if tpm < 30_000.0 {
        Color::Yellow
    } else if tpm < 60_000.0 {
        Color::LightRed
    } else {
        Color::Red
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

pub fn provider_color(p: Provider) -> Color {
    match p {
        Provider::Claude => Color::Magenta,
        Provider::Copilot => Color::Cyan,
    }
}

/// Single-letter provider tag. NOTE: both labels start with "c"
/// (`claude` / `copilot`), so a naive first-letter would collide — Copilot
/// is tagged "P" (as in the in-flight readout) to stay distinct.
fn provider_letter(p: Provider) -> &'static str {
    match p {
        Provider::Claude => "C",
        Provider::Copilot => "P",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}
