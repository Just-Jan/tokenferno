pub mod burn_meter;
pub mod dashboard;
pub mod doomfire;
pub mod fun;

use crate::model::Provider;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The full-screen Doom-fire showpiece. Default view.
    Fun,
    /// The merged live operations dashboard (former "hybrid" tiles + the
    /// detailed per-session / recent-event tables of the former "nerd").
    Dash,
}

impl Mode {
    pub fn next(self) -> Self {
        match self {
            Mode::Fun => Mode::Dash,
            Mode::Dash => Mode::Fun,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Mode::Fun => "INFERNO",
            Mode::Dash => "DASHBOARD",
        }
    }
}

/// A small "segmented control" rendered into the title bar of every view:
/// the available modes as pills with the active one highlighted, plus a hint
/// for how to switch. Kept here (not per-view) so both views show an
/// identical, always-visible switcher.
pub fn mode_switcher(active: Mode) -> Vec<Span<'static>> {
    let pill = move |m: Mode| {
        let style = if m == active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(255, 170, 40))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray).add_modifier(Modifier::DIM)
        };
        Span::styled(format!(" {} ", m.label()), style)
    };
    vec![
        pill(Mode::Fun),
        Span::raw(" "),
        pill(Mode::Dash),
        Span::styled(
            "  ‹1·2 or Tab to switch · q quit› ",
            Style::default().fg(Color::DarkGray),
        ),
    ]
}

const HEARTBEAT: [char; 8] = ['·', '∙', '•', '●', '◉', '●', '•', '∙'];

/// The complete, identical title bar shared by EVERY view: an animated
/// "alive" pulse (left), the app name, the segmented mode control, the
/// switch hint, and the BURNING/idle badge — all in the SAME columns in
/// both views, so switching modes never shifts the header or the tabs.
/// `frame` drives the pulse; `burning` tints the pulse + badge.
pub fn title_bar(active: Mode, frame: u64, burning: bool) -> Vec<Span<'static>> {
    let beat = HEARTBEAT[(frame / 4) as usize % HEARTBEAT.len()];
    let beat_style = if burning {
        Style::default()
            .fg(Color::Rgb(255, 140, 0))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let (badge, badge_style) = if burning {
        (
            " ● BURNING ",
            Style::default()
                .fg(Color::White)
                .bg(Color::Rgb(200, 30, 0))
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (" ○ idle ", Style::default().fg(Color::DarkGray))
    };
    let mut spans = vec![
        Span::styled(format!(" {beat} "), beat_style),
        Span::styled(
            "Tokenferno  ",
            Style::default()
                .fg(Color::Rgb(255, 180, 60))
                .add_modifier(Modifier::BOLD),
        ),
    ];
    spans.extend(mode_switcher(active));
    spans.push(Span::styled(badge, badge_style));
    spans
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Only(Provider),
}

pub fn fmt_int(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

pub fn short_id(id: &str) -> String {
    let n = id.replace('-', "");
    n.chars().take(6).collect()
}

pub fn humanize_age(secs: i64) -> String {
    if secs < 0 {
        return "now".into();
    }
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}
