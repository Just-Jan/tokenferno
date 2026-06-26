# Contributing

Thanks for your interest in tokenferno!

## Development

```bash
git clone https://github.com/Just-Jan/tokenferno.git
cd tokenferno
cargo build
cargo run            # launch the TUI
```

Preview the UI without a live TTY (this also generates the README mocks):

```bash
cargo run --example render_dump -- inferno   # or: dashboard
```

## Before opening a PR

CI runs these on every push -- please run them locally first:

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test --lib
```

## Guidelines

- Keep it dependency-light and **network-free** -- tokenferno never makes
  outbound connections (see [SECURITY.md](SECURITY.md)). PRs that add an
  HTTP/network client will not be merged.
- Match the existing style; `cargo fmt` is the source of truth.
- For user-visible changes, add a line under `Unreleased` in
  [CHANGELOG.md](CHANGELOG.md).

## Reporting security issues

Please report vulnerabilities privately -- see [SECURITY.md](SECURITY.md).
