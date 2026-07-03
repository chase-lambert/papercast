# Vendored: rustvncserver 2.2.1

Vendored from <https://github.com/dustinmcafee/rustvncserver> (crates.io 2.2.1),
Apache-2.0 (see `LICENSE`/`NOTICE`, unchanged).

## Why vendored

Upstream `VncServer::listen(port)` hardcodes binding to `0.0.0.0`. PaperCast
serves an **unauthenticated** VNC session and must bind `127.0.0.1` by default
(clients arrive via `adb reverse`, which connects to loopback). No upstream
issue/PR exists for this as of 2026-07.

## Local changes

- `src/server.rs`: `listen(port: u16)` → `listen<A: ToSocketAddrs>(addr: A)`,
  binding exactly the given address (the loopback-bind patch above).
- `src/client.rs`: fixed the variable-length client-message parser for
  `SetEncodings` and `ClientCutText`. It consumed the fixed header before the
  full variable-length payload had arrived, so a client that split the message
  across TCP segments (TigerVNC 1.14.0 does this) left the first byte of the
  following data to be parsed as a bogus message type. The parser now waits for
  the complete message before advancing the buffer. Found during TigerVNC live
  validation (Phase 0, M4).

## Licensing

This crate stays Apache-2.0 (see `LICENSE`/`NOTICE`, unchanged). Its
`Cargo.toml` sets `license = "Apache-2.0"` explicitly so it does **not** inherit
the workspace's MIT license used by PaperCast's own crates.

## Clippy

This vendored copy still emits a handful of upstream clippy warnings (unused
assignments, missing doc backticks, etc.). They are intentionally left as-is:
PaperCast's own crates are kept clippy-clean, but the vendored tree is treated
as third-party code and will be cleaned as part of upstreaming (M16), not
patched locally.

## TODO (upstreaming — see roadmap M16)

Send both changes upstream as PRs: (a) the `listen` address change
(backwards-compatible variant: add `listen_addr()` alongside `listen()`), and
(b) the `SetEncodings`/`ClientCutText` parser fix. Drop this vendored copy and
return to the crates.io release once merged.
