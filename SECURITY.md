# Security Policy

tokenferno runs entirely on your machine and makes no network calls -- there is
no HTTP/network-client crate in its dependency tree. Its security surface is
deliberately small:

- the local files it reads: `~/.claude/**` and `~/.copilot/**`
- the local Unix socket it opens: `~/.tokenferno/sock`
- the token-count logs it writes: `~/.tokenferno/events-*.jsonl` (counts and
  metadata only -- never your prompts, responses, or code)

## Reporting a vulnerability

Please report security issues **privately** -- don't open a public issue:

- Open a private security advisory:
  <https://github.com/Just-Jan/tokenferno/security/advisories/new>

You'll get an acknowledgement as soon as possible, and a fix will ship in the
next tagged release.

## Supported versions

Only the latest released version is supported.
