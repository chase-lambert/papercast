# PaperCast

Mirror a Linux desktop onto an e-ink tablet — a Linux answer to macOS "SuperMirror".

PaperCast captures a Wayland output, runs it through an e-ink-tuned image pipeline
(grayscale, contrast, sharpening, dithering), and serves the result as a VNC session on
localhost. An Android e-ink tablet connected over USB reaches it through `adb reverse`,
so nothing ever touches the network.

**Status: Phase 0 (working MVP).** Live mirroring over VNC, testable with any desktop VNC
viewer. Phases 1–2 (custom protocol with real e-ink refresh-mode control, GPU pipeline,
virtual extended display) are roadmap — see [Roadmap](#roadmap).

```
Wayland compositor ──ext-image-copy-capture──▶ e-ink pipeline ──▶ VNC :5900 (loopback)
                                                                        ▲
                                              tablet ──USB/adb reverse──┘
```

## Requirements

- A Wayland compositor implementing **ext-image-copy-capture-v1** (COSMIC does; recent
  wlroots compositors do; GNOME/KDE currently don't — portal support is on the roadmap).
  Check yours with `papercast probe`.
- Rust (stable) to build.
- For the tablet path: `adb` (package `android-tools` on Fedora, `adb` on Debian/Ubuntu/Pop!_OS).

## Quick start (desktop, no tablet needed)

```console
$ cargo build --release
$ ./target/release/papercast probe          # confirm capture support, list outputs
$ ./target/release/papercast run --output DP-1
```

Then connect any VNC viewer to `127.0.0.1:5900`:

```console
$ vncviewer 127.0.0.1:5900                  # TigerVNC
```

You should see a grayscale, dithered mirror of the chosen output. Don't point the viewer
at the same monitor you're capturing, or you get an infinite tunnel.

Useful variations:

```console
$ papercast run --source test               # synthetic pattern; no compositor needed
$ papercast run --raw                       # skip the e-ink pipeline (color passthrough)
$ papercast run --invert                    # dark desktop theme → black-on-paper
$ papercast run --save-frame out.png        # dump processed frame #10 for inspection
$ papercast run --latency-test              # stamp a ms counter on every frame
$ papercast run --config papercast.toml     # config file; [eink] hot-reloads on save
```

`papercast run --help` lists every knob. Precedence: built-in defaults < config file <
explicitly passed CLI flags.

## Tablet setup (Onyx Boox / Android)

Written for the Boox Tab X C (13.3″ Kaleido 3, 3200×2400 monochrome / 1600×1200 color,
Android 13); the flow is the same for any Android e-ink device.

1. **Enable USB debugging on the tablet.** Settings → About Device → tap *Build Number*
   seven times to unlock Developer Options, then Settings → Developer Options → enable
   *USB Debugging*. (Boox firmware moves these menus around; search Settings for
   "developer" if you don't find them.)
2. **Connect USB and authorize.** Plug into the Linux box, run `adb devices`, and accept
   the authorization prompt on the tablet. It should list as `device`, not `unauthorized`.
3. **Install a VNC viewer on the tablet.** Recommended: **[AVNC](https://github.com/gujjwal00/avnc)**
   (FOSS, on F-Droid and Play Store). Install via store, or from the host:
   `adb install avnc.apk`.
4. **Start PaperCast on the host:**
   ```console
   $ papercast run --config boox-tab-x-c.toml
   ```
5. **Bridge the port over USB:**
   ```console
   $ adb reverse tcp:5900 tcp:5900
   ```
   This makes `127.0.0.1:5900` *on the tablet* reach PaperCast on the host — the tablet
   connects to "itself" and the traffic rides the USB cable. Re-run after re-plugging.
6. **Connect AVNC to `127.0.0.1:5900`.** No password (see [Security](#security)).

Boox tips:

- Give the viewer app a fast refresh mode: open the app, pull up the Boox control center
  (or the floating ball) → app optimization → set refresh to **A2/X mode** for typing
  latency, **Regal/GC** for reading quality. PaperCast's `full-refresh-secs` periodically
  redraws the whole frame to clear the ghosting that fast modes accumulate.
- Match the pipeline to the panel: `target-size = [3200, 2400]` and `levels = 16` in
  `boox-tab-x-c.toml`. The panel is effectively grayscale at full resolution — PaperCast
  deliberately doesn't do color.

## Configuration

Two example configs ship in the repo root:

- `papercast.toml` — desktop-testing defaults, every knob documented.
- `boox-tab-x-c.toml` — tuned for the Boox Tab X C.

The `[eink]` section **hot-reloads**: edit and save while PaperCast runs and the next
frame uses the new settings — tune contrast live with the viewer open. (`target-size`
is the one exception; it fixes the VNC framebuffer size at startup.)

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
| `dither` | `"bayer"` | `"none"`, `"bayer"` (ordered, stable), `"floyd-steinberg"` |
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

## Security

The VNC session is **unauthenticated**, so PaperCast binds `127.0.0.1` by default and the
tablet reaches it over USB via `adb reverse` — nothing is exposed to any network. Passing
`--listen 0.0.0.0:5900` (or setting it in config) exposes your unauthenticated screen to
the LAN; only do that on a network you trust.

## Viewer compatibility

The server offers Raw, RRE, Hextile, Zlib, ZRLE, and Tight encodings (RFB 3.8). Over USB,
Raw is fine — the cable has bandwidth to spare. Over Wi-Fi, let the viewer pick Tight or
ZRLE.

| Viewer | Platform | Status | Notes |
|---|---|---|---|
| AVNC | Android | untested (no tablet yet) | Recommended receiver; FOSS |
| TigerVNC (`vncviewer`) | desktop | untested | primary desktop test target |
| Remmina | desktop | untested | GTK; uses gtk-vnc/FreeRDP backends |
| Protocol-level RFB client | — | **verified** | scripted RFB 3.8 checks: handshake, Raw rects, tile-granular incremental updates, forced full refresh |

(Desktop-viewer rows get filled in as they're tested; the protocol path underneath them
is exercised on every milestone.)

## How it works

Cargo workspace, three crates plus one vendored dependency:

- **`papercast-core`** — the pixel pipeline, no I/O: BT.709 grayscale → tone LUT
  (black/white point, gamma, contrast, invert) → unsharp mask → scale (letterbox/crop/
  stretch) → quantize with ordered Bayer dithering anchored to absolute framebuffer
  coordinates (so partial updates never show seams) → 64-px dirty-tile diff against the
  previous frame.
- **`papercast-capture`** — frame sources behind one channel-based interface: a synthetic
  test pattern, and Wayland capture via **ext-image-copy-capture-v1** on a dedicated
  thread. Capture is damage-driven: the compositor only delivers frames when something
  changed, so an idle mirror costs ~zero CPU — a natural fit for e-ink.
- **`papercast`** — the binary: CLI, TOML config with hot-reload, pipeline thread, and
  VNC serving. Dirty tiles become individual VNC rects; a timer forces periodic
  full-frame updates to clear e-ink ghosting.
- **`vendor/rustvncserver`** — [rustvncserver](https://crates.io/crates/rustvncserver)
  2.2.1 (Apache-2.0) with one patch: a configurable bind address instead of hardcoded
  `0.0.0.0`. See `vendor/rustvncserver/VENDORED.md`; to be upstreamed.

### Latency

`--latency-test` stamps a millisecond counter on every frame; film both screens (or
eyeball them side by side) and subtract. On the desktop-viewer path the pipeline runs
~23 ms/frame at 2560×1440 (release build), well inside the 15 fps budget. On real e-ink
the panel's own refresh dominates: expect ~100–150 ms in A2 mode and several hundred in
GC16 — that's physics, not transport; USB adds single-digit milliseconds.

## Roadmap

- **Phase 1 — real e-ink control.** Replace VNC with a small length-prefixed protocol and
  a minimal Kotlin receiver using the Onyx SDK, gaining per-region EPD refresh-mode
  control (A2 for typing, GC16 for full refreshes) instead of approximating it from the
  host side.
- **Phase 2 — extended display.** Headless virtual output so the tablet is a true second
  monitor rather than a mirror; wgpu compute pipeline for the image processing; clamshell
  mode. (COSMIC's `zcosmic_workspace_image_capture_source_manager_v1` may offer a
  workspace-capture shortcut worth exploring.)
- **Portal backend.** xdg-desktop-portal + PipeWire capture for GNOME/KDE and anything
  else without ext-image-copy-capture.
- Smaller items: live resize when the captured output changes mode, damage-region
  passthrough when scaling, rotated-output transforms, upstreaming the rustvncserver
  bind patch.

## License

Not yet chosen (Apache-2.0/MIT dual, the Rust convention, is the likely pick). The
vendored `rustvncserver` is Apache-2.0, which is compatible either way.
