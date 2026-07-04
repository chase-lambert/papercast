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
| _this_ | M7a: dynamic pacing at the source (widened `watch<ModeSettings>` + fps-only `watch<u32>` into the capture crate); deleted serve-loop dropping + `max_fps`/`mode_active` split |

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
- **7b — TODO.** Unix socket `$XDG_RUNTIME_DIR/papercast.sock` (fallback
  `/tmp/papercast-$UID.sock`), mode 0600, stale-socket unlink+rebind, "another papercast
  is running" if a live one answers, remove on clean exit. Newline-JSON
  mode/refresh/status; `papercast ctl` subcommand.
- **7c — TODO.** Replace the config watcher's `ModeState` **clone** with a shared
  `Arc<Mutex<ModeState>>` that both the watcher and the ctl server mutate; every mutation
  recomputes `effective()` under the lock and sends both channels. `ctl refresh` signals
  the serve loop to `mark_dirty_region` the whole frame. On mode switch force one full
  refresh even if tile size is unchanged (levels/dither changed globally). Then update
  `--mode` help + README (switching works with or without a startup mode; COSMIC shortcut
  example) and add `tools/rfb_mode_check.py` incl. a ~30 fps writing-mode regression check.
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
