# PaperCast

Mirror a Linux desktop onto an Android e-ink tablet over USB.

PaperCast captures a Wayland output, tunes each frame for e-paper (grayscale, tone
curve, sharpening, dithering), and serves the result on localhost only. The tablet
reaches it through `adb reverse`, so your screen never touches a network. Two
transports:

- **VNC** (`:5900`) — works today with any VNC viewer app on the tablet.
- **papercast** (`:5920`) — a custom pull-based protocol and the native receiver app
  in `android/`, designed so a slow e-ink panel never builds a latency queue.

```
Wayland compositor ──ext-image-copy-capture──▶ e-ink pipeline ──▶ 127.0.0.1 (:5900 VNC, :5920 papercast)
                                                                      ▲
                                                  tablet ── USB / adb reverse ──┘
```

**Status (July 2026).** Everything buildable without a tablet is done and verified:
live mirroring with runtime-switchable display modes over VNC, and the custom
transport + native Android receiver, emulator-tested end to end. What's left needs
hardware in hand: on-device tuning and a vendor refresh backend. See
[Roadmap](#roadmap).

## Requirements

- A Wayland compositor with **ext-image-copy-capture-v1** (COSMIC and recent wlroots
  compositors; GNOME/KDE not yet — portal capture is on the roadmap). Verify with
  `papercast probe`.
- Stable Rust to build.
- For the tablet path: `adb` (`android-tools` on Fedora, `adb` on Debian/Ubuntu/Pop!_OS).

## Quick start (desktop, no tablet needed)

```console
$ cargo build --release
$ ./target/release/papercast probe          # capture support + output names
$ ./target/release/papercast run --output DP-1
$ vncviewer 127.0.0.1:5900                  # any VNC viewer
```

You'll see a grayscale, dithered mirror of that output. Put the viewer on a
*different* monitor than the one you capture, or you get an infinite tunnel. No
viewer installed? `flatpak install --user flathub org.tigervnc.vncviewer`.

Useful flags (`papercast run --help` lists every knob):

```console
$ papercast run --source test               # synthetic pattern; no compositor needed
$ papercast run --raw                       # skip the pipeline (color passthrough)
$ papercast run --invert                    # dark desktop theme → black-on-paper
$ papercast run --latency-test              # ms counter stamped on every frame
$ papercast run --config papercast.toml     # config file; [eink] hot-reloads on save
$ papercast run --transport papercast       # custom transport on :5920
```

Precedence: built-in defaults < config file < CLI flags.

## Tablet over VNC (works today)

Written against the Boox Tab X C (13.3″ Kaleido 3, Android 13); the flow is the same
for any Android device.

1. **Enable USB debugging:** Settings → About Device → tap *Build Number* seven
   times → Developer Options → *USB Debugging*.
2. **Connect and authorize:** plug in, run `adb devices`, accept the prompt on the
   tablet — it must list as `device`, not `unauthorized`.
3. **Install a VNC viewer:** [AVNC](https://github.com/gujjwal00/avnc) recommended.
4. **Run and bridge:**
   ```console
   $ papercast run --config boox-tab-x-c.toml
   $ adb reverse tcp:5900 tcp:5900          # re-run after replugging
   ```
   Point the viewer at `127.0.0.1:5900` (no password — loopback only, see
   [Security](#security)).

On Boox, give the viewer app a fast panel mode in the control center (**A2/X** for
typing latency, **Regal/GC** for reading quality); PaperCast's `full-refresh-secs`
periodically clears the ghosting fast modes accumulate. Kaleido panels are sharpest
in monochrome (300 ppi vs 150 ppi muted color), which is why the pipeline is
grayscale — see [Roadmap](#roadmap) for the color plan.

## Native receiver (custom transport)

The `android/` app receives `--transport papercast`: a thin Kotlin shell (Activity,
SurfaceView, lifecycle) over the Rust `papercast-recv-core`, which owns the
connection, decoding, and flow control. Refresh *intent* (Auto / Fast / Quality)
maps to a panel waveform in a small per-device `RefreshBackend`; today only
`generic` (draw, ignore hints) exists — vendor backends arrive with real hardware.

Prerequisites (one-time):

- Android SDK + NDK, `ANDROID_HOME`/`ANDROID_NDK_HOME` set:
  `sdkmanager "platform-tools" "platforms;android-34" "ndk;<version>"`
- `rustup target add aarch64-linux-android x86_64-linux-android && cargo install cargo-ndk`
- A JDK 17–21 (`export JAVA_HOME=...` if your default is newer)
- `android/local.properties` containing `sdk.dir=<your SDK path>` (gitignored)

Build, install, run:

```console
$ scripts/build-recv-core.sh                # cross-compile core → android/.../jniLibs/ (re-run when it changes)
$ cd android && ./gradlew assembleDebug
$ adb install -r app/build/outputs/apk/debug/app-debug.apk
$ adb reverse tcp:5920 tcp:5920
$ adb shell am start -n com.papercast/.MainActivity
$ papercast run --output DP-1 --transport papercast    # on the host; or --source test
```

The app connects to `127.0.0.1:5920` and reconnects on its own after host restarts
or replugs. Force a backend with `--es backend generic` on the `am start` line.
Verified in an emulator (Android 14 tablet image); not yet on real hardware.

## Configuration

Two annotated examples ship in the repo root: `papercast.toml` (desktop defaults)
and `boox-tab-x-c.toml`. The `[eink]` section **hot-reloads** on save — tune
contrast live with the viewer open. (`target-size` is the exception; it fixes the
framebuffer size at startup.)

### `[eink]`

| Key | Default | Meaning |
|---|---|---|
| `contrast` | `1.2` | Multiplier around mid-gray; e-ink wants more than LCD |
| `gamma` | `1.0` | Exponent; `<1` brightens midtones |
| `black-point` | `8` | Input level at/below which pixels become full ink |
| `white-point` | `248` | Input level at/above which pixels become paper |
| `invert` | `false` | Flip luminance — dark desktop themes become black-on-paper |
| `sharpen` | `1.0` | Unsharp-mask strength; `0` disables |
| `sharpen-radius` | `1` | Unsharp blur radius (px) |
| `dither` | `"bayer"` | `"none"`, `"bayer"` (ordered, stable), `"floyd-steinberg"`, `"atkinson"` |
| `levels` | `16` | Gray levels to quantize to (match the panel) |
| `fit` | `"letterbox"` | `"letterbox"`, `"crop"`, `"stretch"` when scaling changes shape |
| `target-size` | source size | `[w, h]` output resolution, e.g. `[3200, 2400]` |

### `[mirror]`

| Key | Default | Meaning |
|---|---|---|
| `listen` | `"127.0.0.1:5900"` | VNC bind address |
| `fps` | `15` | Frame-rate cap (capture is damage-driven; idle screen = zero frames) |
| `output` | first output | Monitor to capture, by name from `papercast probe` |
| `tile-size` | `64` | Dirty-diff granularity (px) |
| `full-refresh-secs` | `60` | Force a full redraw every N seconds; `0` off |
| `full-refresh-updates` | `0` | ...or after N incremental updates; `0` off |
| `mode` | unset | Startup display mode (see below) |

### Display modes

A **mode** is a named bundle of pipeline + refresh settings trading update speed
against quality and stability. Modes are **overlays on your base config** — they
change only the fields below, so settings like `invert` are preserved.

| Mode | fps | levels | sharpen | tile | full refresh | Intent |
|---|---|---|---|---|---|---|
| `reading` | 5 | 16 | 1.0 | 64 | every update | Max quality; each page turn earns a clean redraw |
| `browsing` | 15 | 16 | 1.0 | 64 | every 60 s | Balanced default |
| `writing` | 30 | 4 | 1.5 | 32 | every 300 s | Min latency; few levels = crisp, cheap text updates |
| `video` | 30 | 16 | 0.0 | 64 | never | Motion; no sharpen halos, no interrupting redraws |

Select at startup with `--mode <name>`, or **switch at runtime** — no restart
needed:

```console
$ papercast ctl mode writing    # switch mode (forces a clean full redraw)
$ papercast ctl refresh         # force a full redraw now (clears ghosting)
$ papercast ctl status          # effective mode / fps / levels / dither / …
```

Wayland has no global-hotkey API, so bind these to compositor shortcuts (COSMIC:
Settings → Keyboard → Custom Shortcuts, e.g. `Super+F2` → `papercast ctl mode
writing`). The socket is `$XDG_RUNTIME_DIR/papercast.sock`, user-only.

Override a built-in or define your own mode with a `[modes.<name>]` table — same
keys as `[eink]` (except `target-size`) plus `fps`, `tile-size`,
`full-refresh-secs`, `full-refresh-updates`; only the keys you set are applied:

```toml
[modes.reading]        # slow reading down, try Atkinson dithering
fps = 3
dither = "atkinson"

[modes.proofing]       # a new mode, selectable as --mode proofing
levels = 2
sharpen = 2.0
fps = 4
```

## Security

Both transports are **unauthenticated**, so PaperCast binds `127.0.0.1` and the
tablet connects over USB via `adb reverse` — nothing is exposed to any network.
`--listen 0.0.0.0:5900` puts your unauthenticated screen on the LAN; only do that
on a network you trust.

## How it works

Cargo workspace, five crates plus one vendored dependency:

- **`papercast-core`** — the pixel pipeline, no I/O: BT.709 grayscale → tone LUT →
  unsharp mask → scale → quantize with Bayer dithering anchored to absolute
  framebuffer coordinates (so partial updates never seam) → dirty-tile diff.
- **`papercast-capture`** — frame sources behind one channel interface: a synthetic
  test pattern, and Wayland ext-image-copy-capture on a dedicated thread. Capture is
  damage-driven — an idle mirror costs ~zero CPU, a natural fit for e-ink.
- **`papercast-proto`** — the custom transport's wire format: length-prefixed
  messages (hello / update / mode-changed / ready), per-rect Gray8, zstd. No I/O or
  async, so it cross-compiles to the Android NDK. Flow control is **pull-based**:
  the client requests each frame, so a slow panel never queues latency.
- **`papercast-recv-core`** — tablet-side receiver: host-testable Rust, built as a
  `cdylib` for Android. One native thread owns connect, handshake, decode, pull
  pacing, and reconnect; frames reach Kotlin zero-copy via a direct `ByteBuffer`.
- **`papercast`** — the binary: CLI, hot-reloading config, control socket, and both
  transports.
- **`vendor/rustvncserver`** — [rustvncserver](https://crates.io/crates/rustvncserver)
  2.2.1 (Apache-2.0) with three local patches (bind address, a protocol parser fix,
  update pacing ~20 → ~31 fps), documented in
  [`vendor/rustvncserver/VENDORED.md`](vendor/rustvncserver/VENDORED.md) and slated
  for upstreaming.

**Latency.** `--latency-test` stamps a millisecond counter on every frame; film both
screens and subtract. The pipeline runs ~23 ms/frame at 2560×1440 (release build).
On real e-ink the panel dominates: ~100–150 ms in A2 mode, several hundred in GC16 —
physics, not transport; USB adds single-digit milliseconds.

## Roadmap

**Done.** Phase 1: display modes, runtime switching (`papercast ctl`), Atkinson
dithering, VNC pacing — all host-side. Phase 2 software: wire protocol, host sender,
Rust receiver core, Android shell — emulator-verified end to end.

**Next (needs a real tablet).**

- On-device validation: latency measurements, panel-specific TOML tuning, the
  Bayer-vs-Atkinson A/B for reading mode.
- A vendor `RefreshBackend` mapping refresh intent to actual panel waveforms
  (Onyx `EpdController` first; other devices as they appear).
- Color: the stack is grayscale end to end **by design** — on Kaleido 3 panels
  monochrome is 300 ppi while color is 150 ppi and muted, so mono wins for text.
  A color mode (per-mode opt-in, protocol v2) is designed and will be built only if
  side-by-side comparison on the real panel says color content is worth it.

**Later.** Portal/PipeWire capture (GNOME/KDE); true extended display rather than a
mirror; GPU pipeline if on-device measurements demand it; upstreaming the vendored
VNC patches.

## License

MIT (see [`LICENSE`](LICENSE)). The vendored `rustvncserver` remains Apache-2.0
(compatible) with its `LICENSE`/`NOTICE` intact.
