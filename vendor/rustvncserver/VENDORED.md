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
  binding exactly the given address.

## TODO

Send the `listen` change upstream as a PR (backwards-compatible variant:
add `listen_addr()` alongside `listen()`); drop this vendored copy and return
to the crates.io release once merged.
