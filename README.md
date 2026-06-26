# 🔥 Tokenferno

![tokenferno](https://github.com/user-attachments/assets/e28d3388-7181-47b8-9831-0558266b0646)

Renders Claude Code and Copilot CLI token burn as an actual fire. The harder your agents work, the higher the flames.

Single binary, zero network calls. Built with Rust + ratatui. It reads the logs your agents already write, and sets them alight.

*Your context window, all ablaze.*

---

## Getting started

### Requirements

- A true-color terminal (macOS or Linux)
- Rust stable only if building from source -- https://rustup.rs

### Install

**Homebrew** (macOS, easiest)

```bash
brew install Just-Jan/tap/tokenferno
```

First time? Homebrew 6+ asks you to approve the tap once with `brew trust Just-Jan/tap`, then re-run. Updates later: `brew upgrade tokenferno`.

**Build from source**

```bash
git clone https://github.com/Just-Jan/tokenferno.git
cd tokenferno

cargo build --release
./target/release/tokenferno
```

**Or install to your PATH**

```bash
cargo install --path .
tokenferno
```

### Keys

```
 1  INFERNO  ·  2  DASHBOARD  ·  Tab  toggle  ·  c  Claude only  ·  p  Copilot only  ·
 a  all  ·  r  reset counters  ·  d  detection HUD  ·  q  quit
```

> Reads `~/.claude/projects/**` and `~/.copilot/logs/**` -- nothing else. Zero network, no credentials, nothing leaves your machine.

---

## Two modes, one keystroke apart

Switch with `1` / `2` or `Tab`. INFERNO is the default.

### INFERNO

The showpiece. No tables -- **the screen is on fire, and your tokens are the fuel.** Full-screen [DOOM fire](https://fabiensanglard.net/doom_fire_psx/index.html) driven by live tok/s. Every prompt is a flare you can watch land. Good for a side monitor, or for showing a colleague who asks "wait, what's that?"

What's on screen:

- **Live DOOM fire** -- a heat grid propagated every frame (random-cooldown + lateral-shift), mapped to a true-color gradient (black → red → orange → yellow → white) and an ASCII glyph ramp (` .:*o▒▓█`). Fuel set from smoothed tok/s, log-scaled.
- **Token-burst flares** -- every assistant turn injects a hot flare that whooshes up the flames.
- **Claude vs Copilot scoreboard** -- per-provider totals, live sparklines, tok/s. Claude in ember-orange, Copilot in cyan.
- **Hero odometer** -- tokens burned today in big block digits, with an honest `▲ generated / ▼ context` breakdown. Almost all of that number is prompt context you re-send every turn, not new output. Now you know.
- **`● BURNING` / `○ idle` badge** and a `🔄 N in-flight` indicator.

```
┌ ● Tokenferno  INFERNO  DASHBOARD  ‹1·2 or Tab to switch · q quit›  ● BURNING ───┐
│ CLAUDE       1 310 000 tok  ▇██▇▆▅▄▂▁▂▄▆▇█  5.9k tok/s                          │
│ COPILOT        711 637 tok  ▆▇███████▇▆▅▃▁▂  3.1k tok/s                         │
│                  ███   ███   ███     ███   ███                                  │
│                 █   █ █   █ █   █   █   █ █   █     tokens burned today         │
│                 █   █  ██  ███      ███   ███      9.0k tokens / second         │
│                              ▲ generated 88.6k  ▼ context 1.93M                 │
│        *o:▒o*:'.:*o▒▒o:*'▒o:.*o▒█▒o*:'▒▒o:*o▒▒*:'o▒o*:.:*o▒o:*'.o▒*             │
│  ▓█▒▓██▒▓█▓▒██▓█▒▓██▓█▒▓█▓██▒▓██▓▒██▓█▒▓██▓▒█▓██▒▓█▓██▒▓██▓▒██▓█▒▓██▓▒█         │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### DASHBOARD

The same data, cold and honest. Per-session tables, a tachometer, a recent-events feed -- every live number driven by the same detection engine as the fire, so the two modes never disagree about what's burning.

```
┌ ● Tokenferno  INFERNO  DASHBOARD  ‹1·2 or Tab to switch · q quit›  ● BURNING ───┐
│ TOTAL TODAY  2 021 637   ▲ generated 88.6k  ▼ context 1.93M     💧 0.4/min      │
├── LIVE ──────┬─ LAST BURST ┬ EVENTS/MIN ┬ TTF p50 ┬─ in-flight ─────────────────┤
│ 8.0k tok/s   │ 412 tok     │ 24.0       │ 240 ms  │ P●1·3.0s                    │
├─────────────────────────────────────────────────────────────────────────────────┤
│ [████████████████████████████████████████████████████████████]  8.0k tok/s      │
│ ▁▃▅▆█▆▅▃▁······▁▃▅▆█  (100 ms heartbeat strip)                                  │
├─ tokens/sec · last 60 s    ·    AVG 480/min · PEAK 41.0k/min @ 13:37 ───────────┤
│      ▁▂▅▇█▇▅▃          ▁▂▄▆█▇▅▃▂          ▁▃▅▇█▇▆▄▂                             │
├─ CLAUDE ● 5.3k/s · in 900k · out 50k · cache-r 300k ─┬─ recent events ──────────┤
│ session  model       in      out    cache-r   age     │ 13:37 C sonnet out 18   │
│ 3c1149   sonnet-4-6  12 312  1 102  88 003    0s      │ 13:37 P opus   out 90   │
├─ COPILOT ● 2.7k/s · in 532k · out 37k · cache-r 112k ┤                          │
│ session  model       in      out    cache-r   age     │                         │
│ f4f18c   opus-4.7    23 156  90     22 068    0s      │                         │
└─────────────────────────────────────────────────────────────────────────────────┘
```

---

## Data sources

tokenferno reads only the log files your agents already write.

| Provider           | Source                                  | Real-time? |
| ------------------ | --------------------------------------- | ---------- |
| Claude Code        | `~/.claude/projects/**/*.jsonl`         | ✅ tail     |
| GitHub Copilot CLI | `~/.copilot/logs/process-*.log`         | ✅ tail     |
| GitHub Copilot CLI | `~/.copilot/session-store.db` (context) | ✅ SQLite   |

Non-CLI clients (claude.ai, Copilot-in-IDE, mobile) don't write local logs -- out of scope for any version.

---

## What's next

- [ ] **Quota gauges** -- tokens left this hour / day / month for Claude + Copilot
- [ ] **$ cost column** -- per-model, cache-tier aware. For when someone asks.
- [ ] **Cursor + Aider support** -- same fire, more agents
- [ ] **80%-of-window alert** -- desktop notification before your context blows up
- [ ] **Replay mode** -- scrub back through today's burn like a post-mortem
- [ ] **Global leaderboard (opt-in)** -- submit your stats, get your archetype (*The Firestarter*, *The Context Hoarder*...), see collective burn milestones. Off by default. Nothing leaves your machine until you say so.

---

## Prior art

- [`ccusage`](https://github.com/ryoppippi/ccusage) -- node CLI that parses Claude Code JSONL and prints usage tables. Good reference for field semantics and price tables.

---

Built with [Rust](https://www.rust-lang.org/) + [ratatui](https://ratatui.rs/). MIT license.
