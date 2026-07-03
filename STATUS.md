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

## After Phase 0 (backlog, see README roadmap)

- Tablet arrival: Boox USB-debugging + `adb reverse` + AVNC (README has the walkthrough).
  `adb` itself is **not installed** on this box yet.
- Upstream the rustvncserver bind-address patch (`vendor/rustvncserver/VENDORED.md`).
- Upstream the rustvncserver variable-length message parser fix.
- Live resize on output mode change; use capture damage to pre-narrow processing/diffing;
  damage passthrough when scaling; rotated outputs.
- Phase 1 (custom protocol + Kotlin/Onyx receiver), Phase 2 (wgpu, virtual display).
- Pick a license (leaning Apache-2.0/MIT dual; vendored code is Apache-2.0).

## Context for a fresh session

- Full plan: `~/.claude/plans/i-ve-been-discussing-a-kind-quokka.md`
- Gotcha: import `rustvncserver::server::ServerEvent` (fields use `client_id`), not the
  re-exported `events::ServerEvent` — two distinct enums.
- Wayland proxies can't cross await points: capture runs on a dedicated OS thread bridged
  to tokio with `blocking_send`/`blocking_recv`.
- Bayer dithering is anchored to absolute framebuffer coordinates so partial-rect updates
  never seam; keep it that way if touching the quantizer.
