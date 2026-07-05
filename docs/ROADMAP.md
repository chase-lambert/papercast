# PaperCast roadmap

PaperCast mirrors a Linux (Wayland/COSMIC) desktop to an e-ink tablet, processing the
image for the medium rather than just shipping gray pixels. This is the public roadmap;
`STATUS.md` at the repo root tracks local, machine-specific implementation state.

## Phase 0 — all-Rust MVP (done)

Wayland capture (`ext-image-copy-capture-v1`) → e-ink pipeline (grayscale, tone LUT,
unsharp, scale, dither) → dirty-tile VNC on loopback, validated live against TigerVNC.
The result is a *processed mirror*: correct, but not yet optimized for e-ink as a medium.

## Phase 1 — e-ink display modes (done; host-side, no tablet required)

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
- **Atkinson dithering.** Error-diffusion variant that renders smoother gradients and
  cleaner flat backgrounds than ordered Bayer on static content; added as an option. A
  visual comparison at `reading` settings found Atkinson better for static reading but
  less stable than coordinate-anchored Bayer under partial updates, so Bayer stays the
  default — the `reading`-mode default flip is deferred to an on-device Bayer-vs-Atkinson
  A/B during Boox validation, since an EPD's ghosting and refresh behavior can change the
  verdict.

## Phase 2 — custom protocol + native receiver (in progress)

Replace the generic VNC viewer app with a minimal receiver that controls the EPD refresh
mode per update (fast partial refresh for writing, full GC16 for clean redraws). This is
where PaperCast beats generic mirroring. VNC stays as a fallback transport.

**Landed (host side):**

- **`papercast-proto`** — framing + message types (hello / update / mode-changed /
  ready), with per-rect Gray8 zstd-compressed. No I/O or async, so it cross-compiles for
  Android (NDK) and the receiver links it directly rather than reimplementing the
  protocol.
- **Host sender** — `papercast run --transport papercast` serves the e-ink pipeline over
  TCP (loopback `:5920`, bridged with `adb reverse tcp:5920 tcp:5920`) with **pull-based
  flow control**: the receiver requests each frame and the sender keeps only the newest,
  so a slow EPD never builds a latency queue.
- **`papercast-recv-core`** — the tablet-side receiver core as host-testable Rust (built
  as a `cdylib` for Android). One native thread owns the connection, handshake, decode,
  pull pacing, and reconnect, handing decoded frames to a sink; verified end-to-end
  against the host sender over loopback.

**Remaining:**

- A **thin Kotlin shell** over the Rust core (JNI): Activity, SurfaceView, socket
  lifecycle, and the device's EPD refresh calls; no protocol or decode logic in Kotlin.
  Testable in an emulator before hardware.
- **Device-neutral by design.** The core and protocol carry zero device-specific code;
  refresh intent (Auto / Fast / Quality) passes through untouched. A small per-device
  backend maps intent to a concrete waveform — Onyx/Boox (`EpdController`), Daylight, or a
  `generic` fallback that simply draws — selected at runtime. The target device isn't
  finalized (Boox Tab X C, Daylight DC-1, or another Android e-ink device), and the
  architecture stays correct for any of them.

## Phase 3 — reach

Ordered by demand; none blocks daily use of Phases 1–2.

- **Portal/PipeWire capture backend** for GNOME/KDE (behind a cargo feature).
- **True extended display** (not a mirror) — starts as a written ADR evaluating COSMIC
  virtual outputs, a nested headless compositor, and wlroots approaches.
- **GPU (wgpu) pipeline** — only if on-device measurements show the CPU pipeline limiting
  fps or battery.
- **Upstream the rustvncserver patches** (loopback bind address; the
  `SetEncodings`/`ClientCutText` parser fix) and drop the vendored copy.
