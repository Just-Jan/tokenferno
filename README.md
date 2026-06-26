# 🔥 Tokenferno

> Real-time TUI that shows how many tokens your AI coding agents are
> incinerating right now — and how much runway you have left this
> hour / day / month.

**Status:** research notes only. No code yet. Target stack:
**Rust + [ratatui]**.

## v1 scope

Real-time **token burn only** for both Claude Code and Copilot CLI.
No quota / "tokens left" gauges, no $ cost column. Those are v2+.

Supported (planned) data sources:

| Provider           | Source                                     | Real-time? |
| ------------------ | ------------------------------------------ | ---------- |
| Claude Code        | `~/.claude/projects/**/*.jsonl`            | ✅ append  |
| GitHub Copilot CLI | `~/.copilot/logs/process-*.log`            | ✅ append  |
| GitHub Copilot CLI | `~/.copilot/session-store.db` (context)    | ✅ SQLite  |

## ASCII mock (v1)

```
┌─ Tokenferno ───────────────────────────────────  q quit  r reset  c/p filter ─┐
│ TOTAL TODAY   in 1 432 105   out 87 412   cache-r 412 880                     │
│ NOW           ▁▂▅▇█▇▅▃▂▁▁▂▄▆█  18.2k tok/min                                  │
├─ Claude Code ─────────────────────────────────────────────────────────────────┤
│ session 3c1149  sonnet-4-6   in 12.3k  out 1.1k  cache-r 88k    last 2s ago   │
│ session 00a1b2  opus-4-7     in  4.0k  out   210 cache-r  2k    last 12s ago  │
├─ Copilot CLI ─────────────────────────────────────────────────────────────────┤
│ session f4f18c  claude-opus-4.7  in 23 156  out 90  cached 22 068  last 1s    │
└──────────────────────────────────────────────────────────────────────────────┘
```

## Goals

- Single binary, zero network calls, no creds required.
- Sub-second latency from "agent emits usage" → "TUI updates".
- Works while multiple Claude/Copilot processes run concurrently.
- Cross-platform (macOS first, Linux next, Windows best-effort).

## Non-goals

- Replacing official billing dashboards.
- Persisting historical data beyond what providers already write to disk.
- Sending any session content off the machine.

## Capabilities

### v1 (acceptance bar)

Live, per-second view of token consumption across both Claude Code
and Copilot CLI, with **zero network calls**.

**Two view modes** — a visible segmented control in the title bar of every
view shows which mode is active and how to switch (`1` / `2` or `Tab`).
**`inferno` is the default.**

#### Mode 1 · `inferno` — the burn, made visible (default)

The showpiece. No tables — **the screen is on fire, and your tokens are
the fuel.** A full-screen, real-time [**DOOM fire**][doomfire] whose
height and ferocity are driven directly by how fast Claude + Copilot are
burning tokens right now. Idle? It dies to a flickering pilot light.
Working hard? A towering, white-hot inferno. Good for showing a
colleague or putting on a side monitor.

What's on screen:

- **Live Doom fire** (`ui::doomfire`). A heat grid propagated every frame
  with the classic random-cooldown + lateral-shift rule, mapped to a
  true-color heat gradient (black → red → orange → yellow → white) and an
  ASCII/box glyph ramp (` .:*o▒▓█`). The bottom fuel row is set each
  frame from the smoothed tokens/second rate (log-scaled).
- **Token-burst flares.** Every assistant turn injects a hot flare that
  whooshes up the flames — so you *see* each burst land.
- **Claude vs Copilot scoreboard.** Per-provider totals, a live
  sparkline, and tok/s — Claude in ember-orange, Copilot in cyan.
- **Hero odometer.** Tokens burned today in big block digits, with an
  honest `▲ generated / ▼ context` breakdown (almost all of the total is
  the prompt context re-sent every turn, not newly generated output).
- **`● BURNING` / `○ idle` badge** and a `🔄 N in-flight` indicator.

```
┌ ● Tokenferno  INFERNO  DASHBOARD  ‹1·2 or Tab to switch · q quit› ● BURNING ─────┐
│ CLAUDE       1 310 000 tok  ▇██▇▆▅▄▂▁▂▄▆▇█  5.9k tok/s                         │
│ COPILOT        711 637 tok  ▆▇███████▇▆▅▃▁▂  3.1k tok/s                        │
│                  ███   ███   ███     ███   ███                                │
│                 █   █ █   █ █   █   █   █ █   █     tokens burned today         │
│                 █   █  ██  ███      ███   ███      9.0k tokens / second         │
│                              ▲ generated 88.6k  ▼ context 1.93M                │
│        *o:▒o*:'.:*o▒▒o:*'▒o:.*o▒█▒o*:'▒▒o:*o▒▒*:'o▒o*:.:*o▒o:*'.o▒*            │
│  ▓█▒▓██▒▓█▓▒██▓█▒▓██▓█▒▓█▓██▒▓██▓▒██▓█▒▓██▓▒█▓██▒▓█▓██▒▓██▓▒██▓█▒▓██▓▒█        │
└───────────────────────────────────────────────────────────────────────────────┘
```

#### Mode 2 · `dashboard` — the data view

One live operations dashboard that merges the former **hybrid** live tiles
with the detailed per-session / recent-event tables of the former
**nerd** view. Every *live* reading (the `LIVE` tile, the `● BURNING`
badge, the tachometer, and each provider's tok/s) is driven by the **same
corrected detection that fuels the fire** (`fun::live_rate` — measured ~3 s
throughput *or* the in-flight projection) — so it actually moves while an
agent is working, including Copilot (which logs its usage as one lump at
the *end* of a turn and was invisible to the old short-window rates).

```
┌ ● Tokenferno  INFERNO  DASHBOARD  ‹1·2 or Tab to switch · q quit› ● BURNING ─────┐
│ TOTAL TODAY  2 021 637   ▲ generated 88.6k  ▼ context 1.93M     💧 0.4/min     │
├── LIVE ──────┬─ LAST BURST ┬ EVENTS/MIN ┬ TTF p50 ┬─ in-flight ────────────────┤
│ 8.0k tok/s   │ 412 tok     │ 24.0       │ 240 ms  │ P●1·3.0s                   │
├───────────────────────────────────────────────────────────────────────────────┤
│ [██████████████████████████████████████████████████████████]  8.0k tok/s      │  ← tachometer
│ ▁▃▅▆█▆▅▃▁······▁▃▅▆█  (100 ms heartbeat strip)                                 │
├─ tokens/sec · last 60 s    ·    AVG 480/min · PEAK 41.0k/min @ 13:37 ──────────┤
│      ▁▂▅▇█▇▅▃          ▁▂▄▆█▇▅▃▂          ▁▃▅▇█▇▆▄▂                             │
├─ CLAUDE ● 5.3k/s · in 900k · out 50k · cache-r 300k ──┬─ recent events ────────┤
│ session  model        in      out    cache-r   age    │ 13:37 C sonnet out 18  │
│ 3c1149   sonnet-4-6   12 312  1 102  88 003     0s     │ 13:37 P opus   out 90  │
├─ COPILOT ● 2.7k/s · in 532k · out 37k · cache-r 112k ─┤                        │
│ session  model        in      out    cache-r   age    │                        │
│ f4f18c   opus-4.7     23 156  90     22 068     0s     │                        │
└───────────────────────────────────────────────────────────────────────────────┘
```

#### Shared keys (both modes)

`1` INFERNO · `2` DASHBOARD · `Tab` toggle · `c` Claude only · `p` Copilot
only · `a` all · `r` reset session counters · `d` toggle detection HUD ·
`q` quit.

#### Implementation notes for modes

- Both modes read the **same** `Snapshot` from the aggregator; modes are
  pure presentation. No mode-specific ingestion logic.
- Every view's title bar is built by the shared `ui::title_bar`, so the
  animated pulse, the app name, the mode tabs, and the BURNING badge sit in
  identical columns — switching modes never shifts the header.
- `ui::fun::render` (INFERNO) and `ui::dashboard::render` each take the
  snapshot; switching modes is a one-line state change. The DASHBOARD
  sources all of its live readings from `ui::fun::live_rate` /
  `provider_live_rate`, so it and the INFERNO fire never disagree about
  what's burning.
- INFERNO animation state (the `doomfire::FireField` heat grid + pending
  flare energy) lives in `ui::fun::FunState` across frames, so `render`
  stays a pure function of its inputs and the aggregator stays pure.
- Default mode: `inferno`.

[doomfire]: https://fabiensanglard.net/doom_fire_psx/index.html

### v2+ (feasible from the same local data)

- "Tokens left" gauges (Claude rolling 5h turns vs plan; Copilot
  premium-requests vs monthly quota).
- $ cost estimate for Claude (model + cache-tier aware). Copilot stays
  in "premium requests" — GitHub doesn't bill in $.
- Per-repo / per-branch breakdown (`cwd` + `gitBranch` from Claude
  JSONL; `sessions.cwd/branch` from Copilot's SQLite).
- Model-mix pie chart.
- Reasoning-token call-out for Copilot o-series.
- 80%-of-window desktop notification.
- Prometheus exporter (`--serve :9876`) → Grafana.
- Replay mode (scrub historical usage).
- CSV/JSON export.
- Diff-with-yesterday mode.

### Out of scope (any version)

- Usage from non-CLI clients (claude.ai web, Copilot-in-IDE, mobile)
  — those don't write to `~/.claude` or `~/.copilot`.
- Authoritative "remaining quota" — providers don't expose it
  locally; v2's number is always a **lower-bound estimate**.
- Tracking server-side rate limits that change without notice.

## Prior art worth studying

- [`ccusage`](https://github.com/ryoppippi/ccusage) — node CLI that
  parses Claude Code JSONL and prints usage tables. Good reference for
  field semantics & price tables.

[ratatui]: https://ratatui.rs/
