# 🔥 Tokenferno

**Your context window, all ablaze.**

<img width="1672" height="941" alt="E6E50818-1946-444F-932F-98590147C5F6" src="https://github.com/user-attachments/assets/e28d3388-7181-47b8-9831-0558266b0646" />

Watch your AI coding agents incinerate tokens in real time -- rendered as an actual fire.

tokenferno is a single-binary terminal app that turns token burn from Claude Code and GitHub Copilot CLI into a live, full-screen inferno. Every prompt is fuel: the harder your agents work, the higher the flames climb. Go idle and it dies to a flickering pilot light. Push it hard and it's a white-hot blaze -- with a live Claude-vs-Copilot scoreboard, a tok/s tachometer, and a running odometer of everything you've torched today.

Two modes, one keystroke apart: **INFERNO** (the screen is on fire, your tokens are the fuel) and **DASHBOARD** (the same data as cold, honest tables). It also surfaces the uncomfortable truth: an `▲ generated / ▼ context` split that shows exactly how much of the blaze is just prompt context you re-send every single turn.

Under the hood: Rust + ratatui. Sub-second latency, zero network calls, no credentials, nothing leaves your machine.

No quota gauges. No brakes. Just watch it burn.

---

## Install

Requires [Rust](https://rustup.rs/) 1.75+.

```bash
git clone https://github.com/Just-Jan/tokenferno
cd tokenferno
cargo install --path . --bin tokenferno
```

That puts `tokenferno` on your `PATH` (in `~/.cargo/bin`), so you can run it from
anywhere. Prefer not to install? Build and run in place with
`cargo build --release && ./target/release/tokenferno`.

**Homebrew:**

```bash
brew install just-jan/tap/tokenferno
```

Upgrade later with `brew update && brew upgrade tokenferno`.

> Prebuilt binaries are also attached to each [release](https://github.com/Just-Jan/tokenferno/releases).

---

## Usage

```bash
tokenferno
```

Auto-discovers Claude Code and Copilot CLI log files and starts monitoring immediately. No config, no credentials, no setup.

---

## Two modes

Switch anytime with `1` / `2` or `Tab`.

### Mode 1 -- INFERNO (default)

The showpiece. No tables -- **the screen is on fire, and your tokens are the fuel.** A full-screen real-time [DOOM fire](https://fabiensanglard.net/doom_fire_psx/index.html) whose height and intensity are driven directly by how fast your agents are burning tokens right now. Idle? It dies to a flickering pilot light. Working hard? A towering, white-hot inferno.

```
┌ ● Tokenferno  INFERNO  DASHBOARD  ‹1·2 or Tab to switch · q quit›  ● BURNING ─┐
│ CLAUDE       1 310 000 tok  ▇██▇▆▅▄▂▁▂▄▆▇█  5.9k tok/s                       │
│ COPILOT        711 637 tok  ▆▇███████▇▆▅▃▁▂  3.1k tok/s                       │
│                  ███   ███   ███     ███   ███                                 │
│                 █   █ █   █ █   █   █   █ █   █     tokens burned today        │
│                 █   █  ██  ███      ███   ███      9.0k tokens / second        │
│                              ▲ generated 88.6k  ▼ context 1.93M               │
│        *o:▒o*:'.:*o▒▒o:*'▒o:.*o▒█▒o*:'▒▒o:*o▒▒*:'o▒o*:.:*o▒o:*'.o▒*          │
│  ▓█▒▓██▒▓█▓▒██▓█▒▓██▓█▒▓█▓██▒▓██▓▒██▓█▒▓██▓▒█▓██▒▓█▓██▒▓██▓▒██▓█▒▓██▓▒█      │
└─────────────────────────────────────────────────────────────────────────────────┘
```

What's on screen:

- **Live DOOM fire** -- a heat grid propagated every frame, mapped to a true-color gradient (black > red > orange > yellow > white) and an ASCII glyph ramp. The bottom fuel row is set from the smoothed tok/s rate.
- **Token-burst flares** -- every assistant turn injects a hot flare you can see land.
- **Claude vs Copilot scoreboard** -- per-provider totals, live sparklines, and tok/s. Claude in ember-orange, Copilot in cyan.
- **Hero odometer** -- tokens burned today in big block digits, with an honest `▲ generated / ▼ context` breakdown.
- **`● BURNING` / `○ idle` badge** and a `🔄 N in-flight` indicator.

### Mode 2 -- DASHBOARD

One live ops dashboard that merges the live tile readings with detailed per-session and recent-event tables. Every live number is driven by the same detection engine that fuels the fire, so the two modes never disagree about what's burning.

```
┌ ● Tokenferno  INFERNO  DASHBOARD  ‹1·2 or Tab to switch · q quit›  ● BURNING ─┐
│ TOTAL TODAY  2 021 637   ▲ generated 88.6k  ▼ context 1.93M     💧 0.4/min    │
├── LIVE ──────┬─ LAST BURST ┬ EVENTS/MIN ┬ TTF p50 ┬─ in-flight ───────────────┤
│ 8.0k tok/s   │ 412 tok     │ 24.0       │ 240 ms  │ P●1·3.0s                  │
├───────────────────────────────────────────────────────────────────────────────┤
│ [████████████████████████████████████████████████████████████]  8.0k tok/s   │
│ ▁▃▅▆█▆▅▃▁······▁▃▅▆█  (100 ms heartbeat strip)                               │
├─ CLAUDE ● 5.3k/s · in 900k · out 50k · cache-r 300k ─┬─ recent events ────────┤
│ session  model       in      out    cache-r   age     │ 13:37 C sonnet out 18 │
│ 3c1149   sonnet-4-6  12 312  1 102  88 003    0s      │ 13:37 P opus   out 90 │
├─ COPILOT ● 2.7k/s · in 532k · out 37k · cache-r 112k ┤                       │
│ session  model       in      out    cache-r   age     │                       │
│ f4f18c   opus-4.7    23 156  90     22 068    0s      │                       │
└─────────────────────────────────────────────────────────────────────────────────┘
```

---

## Keyboard shortcuts

| Key | Action |
|-----|--------|
| `1` | INFERNO mode |
| `2` | DASHBOARD mode |
| `Tab` | Toggle modes |
| `c` | Claude only |
| `p` | Copilot only |
| `a` | All providers |
| `r` | Reset session counters |
| `d` | Toggle detection HUD |
| `q` | Quit |

---

## Data sources

No network calls, no credentials. tokenferno reads only the log files your agents already write.

| Provider | Source | Real-time? |
|----------|--------|------------|
| Claude Code | `~/.claude/projects/**/*.jsonl` | ✅ append |
| GitHub Copilot CLI | `~/.copilot/logs/process-*.log` | ✅ append |
| GitHub Copilot CLI | `~/.copilot/session-store.db` (context) | ✅ SQLite |

---

## Roadmap

### v1 -- current
- [x] Real-time token burn for Claude Code + Copilot CLI
- [x] INFERNO mode (DOOM fire)
- [x] DASHBOARD mode
- [x] Claude-vs-Copilot scoreboard with live sparklines
- [x] `▲ generated / ▼ context` split

### v2+
- [ ] "Tokens left" gauges (Claude rolling 5h window; Copilot monthly quota estimate)
- [ ] $ cost estimate for Claude (model + cache-tier aware)
- [ ] Per-repo / per-branch breakdown
- [ ] Model-mix breakdown
- [ ] 80%-of-window desktop notification
- [ ] Prometheus exporter (`--serve :9876`)
- [ ] Replay mode (scrub historical usage)
- [ ] CSV / JSON export

---

## Goals

- Single binary, zero network calls, no credentials required
- Sub-second latency from "agent emits usage" to "TUI updates"
- Works while multiple Claude / Copilot processes run concurrently
- Cross-platform: macOS first, Linux next, Windows best-effort

## Non-goals

- Replacing official billing dashboards
- Persisting historical data beyond what providers already write to disk
- Sending any session content off the machine
- Authoritative "remaining quota" (providers don't expose this locally; any v2 gauge is a lower-bound estimate)

---

## Prior art

- [`ccusage`](https://github.com/ryoppippi/ccusage) -- node CLI that parses Claude Code JSONL and prints usage tables. Good reference for field semantics and price tables.

---

Built with [Rust](https://www.rust-lang.org/) + [ratatui](https://ratatui.rs/). MIT license.
