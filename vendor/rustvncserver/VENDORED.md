# Vendored: rustvncserver 2.2.1

Vendored from <https://github.com/dustinmcafee/rustvncserver> (crates.io 2.2.1),
Apache-2.0 (see `LICENSE`/`NOTICE`, unchanged).

## Why vendored

Upstream `VncServer::listen(port)` hardcodes binding to `0.0.0.0`. PaperCast
serves an **unauthenticated** VNC session and must bind `127.0.0.1` by default
(clients arrive via `adb reverse`, which connects to loopback).

## Local changes

- `src/server.rs`: `listen(port: u16)` → `listen<A: ToSocketAddrs>(addr: A)`,
  binding exactly the given address (the loopback-bind patch above).
- `src/client.rs`: fixed the variable-length client-message parser for
  `SetEncodings` and `ClientCutText`. It consumed the fixed header before the
  full variable-length payload had arrived, so a client that split the message
  across TCP segments (TigerVNC 1.14.0 does this) left the first byte of the
  following data to be parsed as a bogus message type. The parser now waits for
  the complete message before advancing the buffer. Found during TigerVNC live
  validation.
- `src/client.rs`: continuous-update **pacing** — the update-check tick was
  16 ms and the min send-interval 33 ms. Combined with one wasted "start
  deferring" tick, the first push after a frame landed at ~48 ms: a hard
  ~20 fps delivery ceiling (independent of source fps) plus 16-32 ms of
  quantization latency on every update. Lowered the tick to 8 ms and the
  min-interval to 30 ms → ~31 fps ceiling, so writing mode's 30 fps target is
  actually observable and pen latency drops. Found while building
  `tools/rfb_mode_check.py`.

## Licensing

This crate stays Apache-2.0 (see `LICENSE`/`NOTICE`, unchanged). Its
`Cargo.toml` sets `license = "Apache-2.0"` explicitly so it does **not** inherit
the workspace's MIT license used by PaperCast's own crates.

## Clippy

This vendored copy still emits a handful of upstream clippy warnings (unused
assignments, missing doc backticks, etc.). They are intentionally left as-is:
PaperCast's own crates are kept clippy-clean, but the vendored tree is treated
as third-party code rather than patched locally for style.

## Upstreaming

Send all three changes upstream as PRs: (a) the `listen` address change
(backwards-compatible variant: add `listen_addr()` alongside `listen()`),
(b) the `SetEncodings`/`ClientCutText` parser fix, and (c) the continuous-update
pacing fix (tick 16→8 ms, min-interval 33→30 ms; upstream may prefer these as
tunables rather than hardcoded constants). Drop this vendored copy and return to
the crates.io release once the changes are merged and released.
