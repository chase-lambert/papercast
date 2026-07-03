# PaperCast roadmap

PaperCast mirrors a Linux (Wayland/COSMIC) desktop to an e-ink tablet, processing the
image for the medium rather than just shipping gray pixels. This is the public roadmap;
`STATUS.md` at the repo root tracks local, machine-specific implementation state.

## Phase 0 — all-Rust MVP (done)

Wayland capture (`ext-image-copy-capture-v1`) → e-ink pipeline (grayscale, tone LUT,
unsharp, scale, dither) → dirty-tile VNC on loopback, validated live against TigerVNC.
The result is a *processed mirror*: correct, but not yet optimized for e-ink as a medium.

## Phase 1 — e-ink display modes (host-side; no tablet required)

Task-based display modes in the spirit of the Modos Flow monitor — **Reading**,
**Browsing**, **Writing**, **Video** — each trading update speed against image quality
and visual stability, switchable at runtime. All of Phase 1 is testable with a desktop
VNC viewer.

- **Modes.** A mode is a named bundle of pipeline + refresh settings (fps, gray levels,
  dither, sharpening, full-refresh policy, tile size), applied as an overlay on the
  user's base config so per-user settings (e.g. `invert`) are preserved.
- **Runtime switching.** PaperCast exposes a Unix control socket; `papercast ctl`
  changes the active mode, forces a full "clear ghosts" refresh, or reports status.
  Bind compositor shortcuts to these commands for hotkey-style switching.
- **Atkinson dithering.** Error-diffusion variant that reads lighter and crisper than
  Floyd–Steinberg for text/UI on e-ink; added as an option, gated on visual comparison
  before becoming any mode's default.

## Phase 2 — custom protocol + Android/Onyx receiver

Replace the generic VNC viewer app with a minimal receiver that uses the Onyx SDK to
control the EPD refresh mode per update (fast partial refresh for writing, full GC16 for
clean redraws). This is where PaperCast beats generic mirroring. VNC stays as a fallback
transport.

- A small `papercast-proto` crate (framing + message types) plus a host sender.
- A Kotlin receiver (`android/`), testable in an emulator before hardware, then
  Onyx-SDK refresh-mode integration on the Boox Tab X C.
- Pull-based flow control (client requests the next frame) so a slow EPD never builds a
  latency queue.

## Phase 3 — reach

Ordered by demand; none blocks daily use of Phases 1–2.

- **Portal/PipeWire capture backend** for GNOME/KDE (behind a cargo feature).
- **True extended display** (not a mirror) — starts as a written ADR evaluating COSMIC
  virtual outputs, a nested headless compositor, and wlroots approaches.
- **GPU (wgpu) pipeline** — only if on-device measurements show the CPU pipeline limiting
  fps or battery.
- **Upstream the rustvncserver patches** (loopback bind address; the
  `SetEncodings`/`ClientCutText` parser fix) and drop the vendored copy.
