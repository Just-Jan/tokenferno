# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-06-26

Initial release.

### Added

- Real-time token-burn TUI for Claude Code and Copilot CLI, reading the logs
  the agents already write (`~/.claude/**`, `~/.copilot/**`).
- **INFERNO** mode: a full-screen DOOM fire driven by live tok/s, with a
  Claude-vs-Copilot scoreboard and a tokens-burned-today odometer.
- **DASHBOARD** mode: per-session tables, a tachometer, sparklines, and a
  recent-events feed -- driven by the same detection engine as the fire.
- `tokenferno exec -- <cmd>` PTY wrapper that streams an agent's token usage to
  a running TUI over a local Unix socket.
- Homebrew install via the `Just-Jan/tap` tap.

[Unreleased]: https://github.com/Just-Jan/tokenferno/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Just-Jan/tokenferno/releases/tag/v0.1.0
