# Project status — 2026-07-03

Phase 0 (all-Rust MVP: Wayland capture → e-ink pipeline → VNC on loopback) is complete
for the desktop-viewer path. This file is a working handoff note; delete it when the
project no longer needs local implementation context.

## Done (commits M0a → M4-docs, all on `main`)

| Commit | Milestone |
|---|---|
| `22ede88` | M0a: workspace + `papercast probe` (Wayland capture-protocol diagnostic) |
| `3f4cd19` | M0b: test-pattern source served over VNC (vendored rustvncserver, loopback-bind patch) |
| `62b8505` | M1: live capture via ext-image-copy-capture-v1 |
| `0520928` | M2: e-ink pipeline (gray → tone LUT → unsharp → scale → Bayer/FS dither) |
| `bd83635` | M3: dirty-tile VNC rects, periodic full refresh, `[eink]` config hot-reload, `--latency-test` |
| `04e26e4` | M4 (docs part): README, Boox setup guide, `boox-tab-x-c.toml` |
| `1e83628` | M4: TigerVNC live viewer validation, parser fix, viewer matrix update |

Verified: 28 unit tests green; scripted RFB 3.8 protocol checks (handshake, Raw
rects, tile-granular incremental updates down to 1×1, forced full refresh on schedule);
pipeline output inspected via `--save-frame` PNGs; ~23 ms/frame at 2560×1440 (release);
`boox-tab-x-c.toml` negotiates a 3200×2400 framebuffer end-to-end against the test source;
TigerVNC live viewer path verified against COSMIC capture on `DP-1`.

## Live viewer-matrix result

TigerVNC has been validated against live COSMIC capture on `DP-1`.

Machine facts (this box):

- Flathub is configured as a **user** flatpak remote — viewer installs need **no sudo**.
- TigerVNC is **already installed**: `flatpak run org.tigervnc.vncviewer`
- Remmina is optional and was not needed. Its Flatpak pulls the GNOME runtime, which is
  normal for GTK/GNOME Flatpaks but excessive for this check.
- Outputs: `eDP-1` (3072×1920, usually idle → damage-driven capture emits no frames) and
  `DP-1` (2560×1440, where the action is). Capture `DP-1` for testing.

Validated procedure:

```console
$ cargo build --release
$ ./target/release/papercast run --output DP-1
$ flatpak run org.tigervnc.vncviewer 127.0.0.1:5900   # on the OTHER monitor (avoid mirror tunnel)
```

Observed: TigerVNC 1.14.0 completed RFB 3.8 handshake with security type `None`, enabled
continuous updates, selected Tight encoding, transferred ~483 MPixels with ~42:1
compression, then exited cleanly. Server logged normal client connect/disconnect.

Fix made during validation: the vendored client-message parser consumed the
variable-length `SetEncodings` header before the full payload arrived. TigerVNC split the
message, leaving the first byte of its H.264 encoding (`0x48`, decimal 72) to be parsed
as a bogus client message type. `vendor/rustvncserver/src/client.rs` now waits for the
complete `SetEncodings` and `ClientCutText` messages before advancing the buffer.

## Phase 1 progress (host-side e-ink display modes)

Public roadmap now lives in `docs/ROADMAP.md`; this file stays as local handoff context.
Working from the plan + design-review corrections.

Decisions locked in for Phase 1 (from design review):

- **Clippy policy.** PaperCast's own three crates are kept clippy-clean; the vendored
  `rustvncserver` still emits upstream warnings and is treated as third-party (cleaned at
  upstreaming M16, not patched locally). So the per-commit gate is
  `cargo test --workspace` clean + `cargo clippy -p papercast-core -p papercast-capture
  -p papercast` clean, and `cargo clippy --workspace` must *pass* (compile) but not be
  warning-free until M16.
- **Mode settings live in the binary, not core.** `papercast-core` stays pixel-only
  (`EinkConfig`, dither/pipeline). The `ModeSettings { eink, fps, tile_size, refresh }`
  type and the built-in mode table go in the binary crate (`crates/papercast/src/`). This
  diverges from the plan's "put it in core" wording, per design-review point 5.
- **Mode state = central manager, not last-writer-wins.** Base config + active mode name +
  custom mode defs → effective settings = base + active-mode overlay. Config hot-reload
  and `ctl mode` both go through it so neither clobbers the other.

| Commit | Milestone |
|---|---|
| `482a957` | M5 + cleanup: MIT `LICENSE`, workspace `license = "MIT"` (vendored stays Apache-2.0), `docs/ROADMAP.md`, VENDORED.md parser-fix note, clippy-clean papercast crates |
| `a94b181` | M6: mode presets + central `ModeState` manager (`crates/papercast/src/mode.rs`), `[modes.<name>]` config + `[mirror].mode`, `--mode` CLI |
| `cde6e75` | M8: `DitherMode::Atkinson` (error diffusion, 6/8 spread), `--dither atkinson` + `dither = "atkinson"`, hand-computed unit test |
| `e1eb265` | docs: CLI/README/STATUS wording fixes after design review |
| `513fe65` | M7a: dynamic pacing at the source (widened `watch<ModeSettings>` + fps-only `watch<u32>` into the capture crate); deleted serve-loop dropping + `max_fps`/`mode_active` split |
| `9efca0d` | M7b+c: control socket + `papercast ctl` (mode/refresh/status), shared `Arc<Mutex<ModeState>>`, graceful shutdown, `tools/rfb_mode_check.py` |
| `612b349` | M7.1: lock-scoped watch sends in both mutators (race fix), idle-screen redraw limitation documented |
| _this_ | M8.5: vendored VNC continuous-update pacing patch (tick 16→8 ms, min-interval 33→30 ms, ~20→~31 fps); tightened `rfb_mode_check.py` writing assertion ≥18→≥27 |

### M7 progress (control socket + runtime switching — redesigned per review)

Review rejected the "runtime switching needs a startup mode" constraint and the M6
serve-loop pacing (jitter bug + wasted CPU). New design paces **at the source**.

- **7a (this commit) — DONE.** The eink-only hot-reload channel is now
  `watch::channel(ModeSettings)` (pipeline reads `.eink`, serve loop reads
  tile/`full_refresh_*` each iteration and rebuilds `TileDiff` on tile-size change). A
  separate `watch::channel(u32)` feeds fps into the capture crate; the Wayland thread and
  the test source re-read it (non-blocking `borrow()`, no `await`) and pace themselves.
  Serve-loop frame-dropping and the `mode_active`/`max_fps` split are gone; `max_fps()`
  removed. No startup-mode constraint anymore.
- **7b + 7c (this commit) — DONE** (built together: shipping 7b with a still-cloned
  watcher would knowingly introduce the divergence finding 5 warns about).
  - `crates/papercast/src/control.rs`: Unix socket `$XDG_RUNTIME_DIR/papercast.sock`
    (fallback `/tmp/papercast-$UID.sock`), mode 0600, stale-socket unlink+rebind, "another
    papercast is running" if a live peer answers, removed on clean exit via a drop guard.
    Newline-JSON `{"cmd":"mode|refresh|status",...}`. `papercast ctl mode|refresh|status`.
    `ctl status` reports mode/fps/levels/dither/tile/refresh/framebuffer/output.
  - **One shared `Arc<Mutex<ModeState>>`** mutated by both the config watcher and the ctl
    server (no more clone); every mutation recomputes `effective()` under the lock and
    sends both channels. `ctl mode` forces a full redraw (tiler rebuild on tile-size
    change, else `mark_dirty_region`); `ctl refresh` signals the serve loop directly.
  - Settings-change and refresh are their own `select!` arms, so switches/refreshes apply
    even on an idle screen (no frames arriving).
  - Graceful SIGINT/SIGTERM shutdown so the socket guard runs (verified: socket removed).
  - `tools/rfb_mode_check.py`: RFB client (ContinuousUpdates) + `ctl`, asserts per-mode
    cadence and a full-frame redraw after each switch. **Passes.**

**~20 fps VNC delivery ceiling — found writing the regression test, RESOLVED in
M8.5.** The vendored rustvncserver quantized continuous-update pushes to a 16 ms check
tick vs a 33 ms min-interval, so (with one wasted "start deferring" tick) the first send
landed at ~48 ms → a hard ~20 fps delivery ceiling over VNC plus 16-32 ms of quantization
latency on every update — which hurts writing-mode pen latency, not just throughput. The
reviewer approved pulling the fix forward as **M8.5** (before M9, the "is VNC good enough
daily?" decision point, so the ceiling doesn't bias that call). The patch is two constants
(tick 16→8 ms, min-interval 33→30 ms ⇒ ~31 fps), documented as the third vendored patch
and folded into the M16 upstream PR. Measured after the patch: writing 29 fps (was ~20),
reading 5.5, browsing 14.5. The tool's writing assertion tightened ≥18 → ≥27, which now
catches both the serve-loop jitter regression (~15) and a revert of the pacing patch (~20).
**M7.1 (review follow-up) — DONE.** Both mutators (`control::apply_mode`, the
config watcher) now recompute `effective()` *and* send the watch channels under
the `ModeState` lock, so a config save racing a `ctl mode` switch can't interleave
and leave a channel carrying stale settings (`watch::Sender::send` is sync — no
await across the std `Mutex`). The watcher also compares against the channels'
current values (`settings_tx.borrow()` / `fps_tx.borrow()`) instead of a private
`last_sent` cache that a `ctl` switch would leave stale. Documented the known
idle-screen limitation (a switch on a fully idle screen resends old-settings
pixels until the next damage-driven frame; pipeline-caches-last-raw-frame is the
eventual fix, Phase 1 backlog).

- **M8 visual gate — DONE; default stays Bayer, decision deferred to M9.** Compared
  Bayer vs Atkinson `--save-frame` PNGs at `reading` settings (16 levels, sharpen 1.0).
  Findings: Atkinson wins on static reading content (smoother tonal gradients; flat
  mid-gray goes near-solid vs Bayer's ordered crosshatch; text/line-gratings identical).
  Bayer wins on temporal stability — it's coordinate-anchored, so unchanged pixels
  re-dither identically; Atkinson is order-dependent and can re-dither/shimmer under
  partial updates (scroll/type). `reading` mode's full-refresh-per-change largely masks
  that, making Atkinson a plausible *reading-only* default — but not for browsing/writing/
  video. Verdict: an LCD PNG can't settle it (EPD ghosting/refresh-mode/Boox pipeline
  change the outcome), so **Bayer remains the default and Atkinson stays opt-in; A/B the
  two on the actual Boox at M9, then decide.** The experiment is pre-written (commented
  `[modes.reading] dither = "atkinson"`) in `boox-tab-x-c.toml`. Flip = one line in the
  built-in `reading` overlay in `mode.rs` if M9 confirms it.

## Phase 2 progress (custom protocol + native receiver)

| Commit | Milestone |
|---|---|
| `b4514ff` | M10a: `papercast-proto` crate — framing + message types, no I/O/async, cross-compiles for the NDK (12 tests) |
| _this_ | M10b: host sender behind `--transport vnc\|papercast`, pull-based flow control, loopback integration test |

- **M10a — DONE.** New `crates/papercast-proto`: envelope `[u32 BE len][u8 type]
  [payload]`; messages ServerHello/Update/ModeChanged (server→client), ClientHello/
  Ready (client→server). `encode()`/`decode()` over `&[u8]`/`Vec<u8>` only — decode
  is streaming (`Ok(None)` = need more bytes) and hardened (payload cap, per-rect
  pixel cap, decompressed-size check, exact-consumption check). Per-rect Gray8 is
  zstd level-1. Only heavy dep is `zstd`, which builds for host and NDK. Kept free
  of tokio/host-only deps per the M10 constraint.
- **M10b — DONE.** `--transport vnc|papercast` (default `vnc`); papercast serves TCP
  `127.0.0.1:5920` (override with `--listen`). Shares all of `serve()`'s setup (source,
  mode state, control socket, config watcher) and branches only the output half as an
  early return, so the VNC path is byte-for-byte unchanged (it's the M9 baseline and
  M11 fallback). `crates/papercast/src/transport.rs::serve_proto` is a single-client
  pull loop: keep-newest partial update + a pending full-refresh flag, sent only when a
  `Ready` is outstanding; forced refreshes (mode change / periodic / `ctl refresh`) send
  a full-quality repaint from a cached last frame even on an idle screen. `refresh_hint`
  = Quality on full refresh, Fast in writing/video, else Auto. Verified: the specced
  tokio loopback test (no Update before Ready, then one flows) **plus** an end-to-end
  smoke test against the real binary (handshake, silent-before-Ready, full-frame Quality
  first paint at 320×240 → 1973 B, flow-control cycling). VNC path re-checked green via
  `rfb_mode_check.py`.
- **M10 polish (review verdict 15) — DONE.** (1) Reconnect stale-Ready: `Sink` carries
  a generation bumped in `attach()`; each `Ready` is tagged with the generation it was
  read under and ignored if stale, so a replaced client's in-flight `Ready` can't trigger
  an unrequested `Update` to the new client. (2) Client read buffer capped at 1 KB (legit
  client messages are ≤ 6 bytes; `adb reverse` exposes the port to any tablet app, so
  larger is treated as malformed and dropped). (3) `ModeChanged` de-duplicated via a
  last-sent-name check, so a config edit that keeps the active mode no longer re-announces
  it. Plus the cosmetic proto nit: the 255-byte name truncation now floors to a UTF-8 char
  boundary. All verified (51 tests, clippy-clean, end-to-end re-smoke green).
- **Noted (pre-existing, not M10):** the test-pattern source panics with a subtract
  overflow for framebuffers shorter than ~64 px (box size exceeds the band). Harmless at
  real sizes; worth a clamp when convenient.

## After Phase 0 (backlog, see README roadmap)

- Tablet arrival: Boox USB-debugging + `adb reverse` + AVNC (README has the walkthrough).
  `adb` itself is **not installed** on this box yet.
- Upstream the rustvncserver bind-address patch (`vendor/rustvncserver/VENDORED.md`).
- Upstream the rustvncserver variable-length message parser fix.
- Live resize on output mode change; use capture damage to pre-narrow processing/diffing;
  damage passthrough when scaling; rotated outputs.
- Remaining phases (see `docs/ROADMAP.md`): finish Phase 1 (M7 runtime mode switching),
  then Phase 2 (custom protocol + Kotlin/Onyx receiver), then Phase 3 (portal backend,
  extended display, wgpu, upstreaming).

## Context for a fresh session

- Full plan: `~/.claude/plans/i-ve-been-discussing-a-kind-quokka.md`
- Gotcha: import `rustvncserver::server::ServerEvent` (fields use `client_id`), not the
  re-exported `events::ServerEvent` — two distinct enums.
- Wayland proxies can't cross await points: capture runs on a dedicated OS thread bridged
  to tokio with `blocking_send`/`blocking_recv`.
- Bayer dithering is anchored to absolute framebuffer coordinates so partial-rect updates
  never seam; keep it that way if touching the quantizer.
