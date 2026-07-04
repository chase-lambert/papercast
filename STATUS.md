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
| `a94b181` | M6: mode presets + central `ModeState` manager (`crates/papercast/src/mode.rs`), `[modes.<name>]` config + `[mirror].mode`, `--mode` CLI, effective-settings wiring, serve-loop fps pacing, capture-fps rule |
| _this_ | M8: `DitherMode::Atkinson` (error diffusion, 6/8 spread), `--dither atkinson` + `dither = "atkinson"`, hand-computed unit test |

### M6 notes / open items for M7

- `ModeState` (base + active + custom → effective) is implemented and unit-tested
  (8 tests). `--mode`/`[mirror].mode` select the startup mode; unknown names error and
  list valid names.
- **fps rule (design pt 7) is honored:** no mode → capture at configured `[mirror].fps`,
  no serve-loop drop (identical to pre-modes behavior). Mode active → capture at
  `ModeState::max_fps()` (30 for the built-ins), serve loop drops to the effective fps.
  Consequence: **runtime mode switching (M7) only works if a mode is active at startup**
  (capture fps is fixed once). Start with `--mode browsing` to use `ctl`. Documented here
  so M7 wiring keeps this.
- **Hot-reload already routes through the manager:** the config watcher owns a `ModeState`
  clone, updates the base `[eink]`, and pushes the *effective* eink — so editing the file
  never drops the active mode's overlay. The watch channel still carries `EinkConfig`.
- **M7 will:** widen the watch channel to `ModeSettings`, add the Unix control socket +
  `papercast ctl` (mode/refresh/status), and share **one** `ModeState` between the socket
  and the config watcher so both feed the same manager. The serve loop must then re-read
  fps/tile/refresh from the channel (currently read once at startup). Do **not** build M7
  on the current setup as-is — the config watcher today gets a *clone* of `ModeState`
  (fine for fixed startup modes) and the serve loop reads tile/fps/refresh once. If M7
  keeps that, `ctl mode` and config hot-reload will operate on separate copies and
  diverge. The single shared manager is the fix.
- **M8 done, but Atkinson is NOT yet any mode's default** (design pt 8: visual-gate
  first). It's opt-in via `dither = "atkinson"`. Before making it the `reading` default,
  compare against Bayer with `--save-frame` PNGs and a live viewer check, then flip the
  built-in `reading` overlay in `mode.rs`.

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
