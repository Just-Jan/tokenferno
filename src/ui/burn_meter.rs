use crate::model::Snapshot;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// A small horizontal log-scale tachometer for the live token burn rate.
/// `smoothed_rate` is tokens / second (typically `micro_rate_tps`).
///
/// Layout: `[████████░░░░░░░░] 124 tok/s`
pub fn render(f: &mut Frame, area: Rect, _snap: &Snapshot, smoothed_rate: f64) {
    if area.width < 12 || area.height == 0 {
        return;
    }
    let width = area.width as usize;
    // Reserve right-hand label.
    let label = format_label(smoothed_rate);
    let label_w = label.len() + 1;
    let bar_w = width.saturating_sub(label_w + 2); // +2 for [ ]
    if bar_w < 4 {
        return;
    }

    // Log scale: 0.5 tok/s → 0, 10000 tok/s → 1.0
    let frac = log_scale(smoothed_rate);
    let filled = (frac * bar_w as f64).round() as usize;
    let filled = filled.min(bar_w);
    let color = rate_color(smoothed_rate);

    let spans: Vec<Span> = vec![
        Span::styled("[", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "█".repeat(filled),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "░".repeat(bar_w - filled),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled("] ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn format_label(tps: f64) -> String {
    if tps >= 1000.0 {
        format!("{:.1}k tok/s", tps / 1000.0)
    } else if tps >= 10.0 {
        format!("{:.0} tok/s", tps)
    } else {
        format!("{:.1} tok/s", tps)
    }
}

/// 0.5 tok/s → 0, ~10k tok/s → 1.0. Clamped.
pub fn log_scale(tps: f64) -> f64 {
    if tps <= 0.5 {
        return 0.0;
    }
    let l = (tps + 1.0).log10(); // log10(1.5) ~ 0.18 .. log10(10001) ~ 4
    (l / 4.0).clamp(0.0, 1.0)
}

/// Color ramp keyed to absolute rate (not relative).
pub fn rate_color(tps: f64) -> Color {
    if tps < 5.0 {
        Color::DarkGray
    } else if tps < 100.0 {
        Color::Yellow
    } else if tps < 1000.0 {
        Color::LightRed
    } else {
        Color::Red
    }
}
