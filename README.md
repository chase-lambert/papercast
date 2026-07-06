# PaperCast

Mirror a Linux desktop onto an e-ink tablet — a Linux answer to macOS "SuperMirror".

PaperCast captures a Wayland output, runs it through an e-ink-tuned image pipeline
(grayscale, contrast, sharpening, dithering), and serves the result as a VNC session on
localhost. An Android e-ink tablet connected over USB reaches it through `adb reverse`, so
nothing ever touches the network.

```
Wayland compositor ──ext-image-copy-capture──▶ e-ink pipeline ──▶ VNC :5900 (loopback)
                                                                        ▲
                                              tablet ──USB/adb reverse──┘
```

**Status.** Live mirroring with runtime display modes (Reading / Browsing / Writing /
Video) works today over VNC and is testable with any desktop viewer. A custom pull-based
transport and a native Android receiver are built and emulator-verified end to end;
what's left is a vendor refresh backend and tuning on real tablet hardware. See
[Roadmap](#roadmap).

## Requirements

- A Wayland compositor implementing **ext-image-copy-capture-v1** (COSMIC and recent
  wlroots compositors do; GNOME/KDE don't yet — portal support is on the roadmap). Check
  yours with `papercast probe`.
- Rust (stable) to build.
- For the tablet path: `adb` (`android-tools` on Fedora, `adb` on Debian/Ubuntu/Pop!_OS).

## Quick start (desktop, no tablet needed)

```console
$ cargo build --release
$ ./target/release/papercast probe          # confirm capture support, list outputs
$ ./target/release/papercast run --output DP-1
$ vncviewer 127.0.0.1:5900                  # TigerVNC, or any VNC viewer
```

You should see a grayscale, dithered mirror of the chosen output. Point the viewer at a
*different* monitor than the one you're capturing, or you get an infinite tunnel — for a
demo, capture one display and put the viewer window on the other.

No local VNC viewer? TigerVNC via Flatpak works without root:

```console
$ flatpak install --user flathub org.tigervnc.vncviewer
$ flatpak run org.tigervnc.vncviewer 127.0.0.1:5900
```

Useful variations (`papercast run --help` lists every knob):

```console
$ papercast run --source test               # synthetic pattern; no compositor needed
$ papercast run --raw                        # skip the e-ink pipeline (color passthrough)
$ papercast run --invert                     # dark desktop theme → black-on-paper
$ papercast run --save-frame out.png         # dump processed frame #10 for inspection
$ papercast run --latency-test               # stamp a ms counter on every frame
$ papercast run --config papercast.toml      # config file; [eink] hot-reloads on save
$ papercast run --transport papercast        # custom e-ink transport on :5920 (Phase 2)
```

Precedence: built-in defaults < config file < explicit CLI flags.

## Tablet setup (Onyx Boox / Android)

Written for the Boox Tab X C (13.3″ Kaleido 3, 3200×2400 monochrome, Android 13); the
flow is the same for any Android e-ink device.

1. **Enable USB debugging.** Settings → About Device → tap *Build Number* seven times,
   then Developer Options → *USB Debugging*. (Boox moves these around; search Settings for
   "developer".)
2. **Connect and authorize.** Plug into the Linux box, run `adb devices`, accept the
   prompt on the tablet. It should list as `device`, not `unauthorized`.
3. **Install a VNC viewer** — recommended: **[AVNC](https://github.com/gujjwal00/avnc)**
   (FOSS, F-Droid/Play Store), or `adb install avnc.apk`.
4. **Start PaperCast, bridge the port, connect:**
   ```console
   $ papercast run --config boox-tab-x-c.toml
   $ adb reverse tcp:5900 tcp:5900          # tablet's 127.0.0.1:5900 → host; re-run after replug
   ```
   Then point AVNC at `127.0.0.1:5900` (no password — see [Security](#security)).

**Boox tips.** Give the viewer app a fast panel refresh mode via the Boox control center
(**A2/X** for typing latency, **Regal/GC** for reading quality); PaperCast's
`full-refresh-secs` periodically clears the ghosting that fast modes accumulate. Match the
pipeline to the panel with `target-size = [3200, 2400]` and `levels = 16`. The panel is
effectively grayscale at full resolution, so PaperCast deliberately doesn't do color.

## Native Android receiver (custom transport)

The VNC path above works with any viewer app. The `android/` app is the native receiver
for `--transport papercast` (Phase 2): a thin Kotlin shell over the Rust
`papercast-recv-core`, which owns the connection, protocol, decode, and pull flow control.
Kotlin is only an Activity, a `SurfaceView`, lifecycle glue, and the `RefreshBackend`
seam — no protocol logic. It's device-neutral: refresh intent (Auto / Fast / Quality) maps
to a panel waveform in a per-device backend; today only `generic` (draw, ignore the hint)
exists.

Prerequisites:

- **Android SDK** (platform 34, build-tools) and an **NDK**, with `ANDROID_HOME` and
  `ANDROID_NDK_HOME` set:
  `sdkmanager "platform-tools" "platforms;android-34" "ndk;<version>"`.
- **Rust Android targets + cargo-ndk:**
  `rustup target add aarch64-linux-android x86_64-linux-android && cargo install cargo-ndk`.
- **A JDK the Android Gradle Plugin supports (17–21).** If your default `java` is newer,
  point the build at a 21 JDK, e.g. `export JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64`.
- `android/local.properties` with `sdk.dir=<your Android SDK path>` (gitignored; create it
  once).

Build, install, and run:

```console
$ scripts/build-recv-core.sh                 # cross-compile the core → android/app/src/main/jniLibs/<abi>/
$ cd android && ./gradlew assembleDebug      # → app/build/outputs/apk/debug/app-debug.apk
$ adb install -r app/build/outputs/apk/debug/app-debug.apk
$ adb reverse tcp:5920 tcp:5920              # host :5920 reachable as 127.0.0.1:5920 on device
$ adb shell am start -n com.papercast/.MainActivity
```

The `.so`s are build artifacts (gitignored) — re-run `build-recv-core.sh` (arm64 for the
device, x86_64 for the emulator; pass `debug` for a debug build) whenever the receiver
core changes. On the host, serve the custom transport:

```console
$ papercast run --source test --transport papercast     # or --output <MONITOR>
```

The app connects to `127.0.0.1:5920` and reconnects on its own once the host is up (and
after replug / re-running `adb reverse`). Force a specific backend with an intent extra:
`adb shell am start -n com.papercast/.MainActivity --es backend generic`.

**Verified** in an emulator (pixel_tablet, Android 14, x86_64): the dithered test pattern
renders and updates live, delivered zero-copy through a direct `ByteBuffer` and drawn
letterboxed to the `SurfaceView`. Not yet exercised on real hardware.

## Configuration

Two example configs ship in the repo root: `papercast.toml` (desktop defaults, every knob
documented) and `boox-tab-x-c.toml` (tuned for the Boox Tab X C).

The `[eink]` section **hot-reloads** — edit and save while PaperCast runs and the next
frame uses the new settings, so you can tune contrast live with the viewer open.
(`target-size` is the exception; it fixes the framebuffer size at startup.)

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
| `full-refresh-secs` | `60` | Force a full-frame redraw every N seconds; `0` off |
| `full-refresh-updates` | `0` | ...or after N incremental updates; `0` off |
| `mode` | unset | Startup display mode (see below); unset = plain base config |

### Display modes

A **mode** is a named bundle of pipeline + refresh settings, trading update speed against
image quality and stability — in the spirit of the Modos Flow monitor. A mode is an
**overlay on your base config**: it changes only the fields below, so your own `[eink]`
settings (e.g. `invert`) are preserved.

| Mode | fps | levels | sharpen | tile | full refresh | Intent |
|---|---|---|---|---|---|---|
| `reading` | 5 | 16 | 1.0 | 64 | every update | Max quality; each page turn earns a clean full redraw |
| `browsing` | 15 | 16 | 1.0 | 64 | every 60 s | Balanced default (≈ no-mode behavior) |
| `writing` | 30 | 4 | 1.5 | 32 | every 300 s | Min latency; few levels = crisp, cheap text updates |
| `video` | 30 | 16 | 0.0 | 64 | never | Motion; no sharpen halos, no interrupting redraws |

Select one at startup with `--mode <name>` or `[mirror] mode = "<name>"`. All built-ins
use Bayer dithering.

**Switch at runtime** through the control socket — no restart, works with or without a
startup `--mode`:

```console
$ papercast ctl mode writing    # switch mode (forces a clean full redraw)
$ papercast ctl refresh         # force a full redraw now (clear e-ink ghosting)
$ papercast ctl status          # print the effective mode/fps/levels/dither/…
```

Wayland has no global-hotkey API, so bind these to compositor shortcuts (COSMIC: Settings
→ Keyboard → Custom Shortcuts, e.g. `Super+F2` → `papercast ctl mode writing`). The socket
is `$XDG_RUNTIME_DIR/papercast.sock` (user-only, loopback-equivalent).

Override a built-in or define your own with a `[modes.<name>]` table — same keys as
`[eink]` (except `target-size`) plus `fps`, `tile-size`, `full-refresh-secs`, and
`full-refresh-updates`; only the keys you set are applied:

```toml
[modes.reading]        # slow reading mode down, switch it to Atkinson dithering
fps = 3
dither = "atkinson"

[modes.proofing]       # a new mode, selectable as --mode proofing
levels = 2
sharpen = 2.0
fps = 4
```

## Security

The VNC session is **unauthenticated**, so PaperCast binds `127.0.0.1` by default and the
tablet reaches it over USB via `adb reverse` — nothing is exposed to any network. Passing
`--listen 0.0.0.0:5900` puts your unauthenticated screen on the LAN; only do that on a
network you trust.

## How it works

Cargo workspace, five crates plus one vendored dependency:

- **`papercast-core`** — the pixel pipeline, no I/O: BT.709 grayscale → tone LUT
  (black/white point, gamma, contrast, invert) → unsharp mask → scale → quantize with
  ordered Bayer dithering anchored to absolute framebuffer coordinates (so partial updates
  never seam) → 64-px dirty-tile diff against the previous frame.
- **`papercast-capture`** — frame sources behind one channel interface: a synthetic test
  pattern and Wayland capture via **ext-image-copy-capture-v1** on a dedicated thread.
  Capture is damage-driven, so an idle mirror costs ~zero CPU — a natural fit for e-ink.
- **`papercast-proto`** — the custom transport's wire format: length-prefixed framing and
  messages (hello / update / mode-changed / ready), per-rect Gray8 zstd-compressed. No I/O
  or async, so it cross-compiles to the Android NDK and the receiver links it directly.
  Flow control is **pull-based** — the client requests each frame — so a slow EPD never
  builds a latency queue.
- **`papercast-recv-core`** — the tablet-side receiver core: host-testable Rust, built as
  a `cdylib` for Android. One native thread owns the TCP connection, handshake, decode,
  pull pacing, and reconnect. It's **device-neutral** — refresh intent (Auto / Fast /
  Quality) passes straight through; mapping it to a concrete EPD waveform is the per-device
  backend's job in the Kotlin shell (see `android/`).
- **`papercast`** — the binary: CLI, hot-reloading TOML config, pipeline thread, and two
  output transports (VNC by default, the custom `--transport papercast` sender).
- **`vendor/rustvncserver`** — [rustvncserver](https://crates.io/crates/rustvncserver)
  2.2.1 (Apache-2.0) with three patches: a configurable bind address, a variable-length
  `SetEncodings`/`ClientCutText` parser fix, and a continuous-update pacing fix (~20 → ~31
  fps). See [`vendor/rustvncserver/VENDORED.md`](vendor/rustvncserver/VENDORED.md); all to
  be upstreamed.

**Viewers.** The server offers Raw, RRE, Hextile, Zlib, ZRLE, and Tight (RFB 3.8). Over
USB, Raw is fine; over Wi-Fi, let the viewer pick Tight or ZRLE. TigerVNC 1.14.0 and
scripted protocol-level clients are verified; AVNC (Android) is the recommended receiver,
untested pending hardware.

**Latency.** `--latency-test` stamps a millisecond counter on every frame; film both
screens and subtract. The pipeline runs ~23 ms/frame at 2560×1440 (release), well inside
the 15 fps budget. On real e-ink the panel dominates: ~100–150 ms in A2 mode, several
hundred in GC16 — physics, not transport; USB adds single-digit milliseconds.

## Roadmap

- **Phase 1 — e-ink display modes (done).** Reading / Browsing / Writing / Video modes
  over the VNC path, switchable at runtime via `papercast ctl`; Atkinson dithering
  available. All host-side, no tablet needed.
- **Phase 2 — custom protocol + native receiver (in progress).** The pull-based wire
  protocol, host sender (`--transport papercast`), Rust receiver core, and thin
  Android/Kotlin shell (`android/`) are landed and **emulator-verified** end to end. The
  receiver is **device-neutral**: refresh intent maps to a waveform in a small per-device
  `RefreshBackend` — only the `generic` one exists today. A vendor backend (Onyx/Boox,
  Daylight, …) and on-device tuning arrive with the chosen tablet. VNC stays the universal
  fallback.
- **Phase 3 — reach.** Portal/PipeWire capture (GNOME/KDE); true extended display (not a
  mirror); wgpu compute pipeline; upstreaming the rustvncserver patches.

## License

PaperCast is MIT (see [`LICENSE`](LICENSE)). The vendored `rustvncserver` remains
Apache-2.0 (compatible with MIT) with its `LICENSE`/`NOTICE` intact; see
[`vendor/rustvncserver/VENDORED.md`](vendor/rustvncserver/VENDORED.md) for what was
patched.
