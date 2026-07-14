# PaperCast

PaperCast mirrors a Linux Wayland output to an Android e-ink tablet over USB. It
captures the desktop, applies an e-paper pipeline (grayscale, tone, sharpening,
scaling, and dithering), then serves the result on loopback. `adb reverse`
connects the tablet without exposing the screen to a network.

```text
Wayland → e-ink pipeline → VNC :5900 → adb reverse → tablet viewer
                         ↘ native protocol :5920 → PaperCast Android app
```

VNC is the supported path today. The native receiver is emulator-tested but
experimental until it has been measured on real e-ink hardware.

## Requirements

- Stable Rust
- A Wayland compositor exposing both ext-image-copy-capture-v1 managers
  (check with `papercast probe`)
- `adb` for the tablet connection
- Any Android VNC viewer; [AVNC](https://github.com/gujjwal00/avnc) is a good fit

PaperCast currently implements only ext-image-copy-capture. `probe` may detect
COSMIC-specific or legacy wlroots screencopy globals, but they are not selectable
backends. Rotated or transformed Wayland outputs are not yet handled correctly.

## Quick start

```console
$ cargo build --release
$ ./target/release/papercast probe
$ ./target/release/papercast run --output DP-1
$ vncviewer 127.0.0.1:5900
```

Later examples use bare `papercast` assuming it is installed on `PATH`; after a
local build, use `./target/release/papercast` instead.

Use a viewer on a different output from the one being captured to avoid a mirror
tunnel. A compositor-free smoke test is also available:

```console
$ papercast run --source test
```

Run `papercast run --help` for every CLI option. Useful starting points include
`--raw`, `--invert`, `--latency-test`, `--mode writing`, and
`--config papercast.toml`.

## Tablet over VNC

1. Enable USB debugging, connect the tablet, and confirm `adb devices` reports
   it as `device`.
2. Install a VNC viewer on the tablet.
3. Start PaperCast and bridge its loopback listener:

   ```console
   $ papercast run --config boox-tab-x-c.toml
   $ adb reverse tcp:5900 tcp:5900
   ```

4. Connect the tablet viewer to `127.0.0.1:5900` with no password.

Re-run `adb reverse` after reconnecting the cable. On e-ink hardware, choose the
viewer app's fastest panel mode for writing and a quality mode for reading;
PaperCast's full-refresh policy periodically clears accumulated ghosting.

## Experimental native receiver

The app in `android/` is a thin Kotlin shell over `papercast-recv-core`. The Rust
core owns connection, decoding, reconnect, and pull-based flow control: the host
sends the newest frame only after the receiver is ready, so a slow panel cannot
build a latency queue. The current `generic` refresh backend draws frames but
does not select vendor waveforms.

Prerequisites: Android SDK platform 34, an Android NDK, JDK 17–21, `cargo-ndk`,
and Rust targets `aarch64-linux-android` and `x86_64-linux-android`. Set
`ANDROID_HOME` and `ANDROID_NDK_HOME`, then add an ignored
`android/local.properties` containing `sdk.dir=<SDK path>`.

```console
$ rustup target add aarch64-linux-android x86_64-linux-android
$ cargo install cargo-ndk
$ scripts/build-recv-core.sh
$ cd android && ./gradlew assembleDebug && cd ..
$ adb install -r android/app/build/outputs/apk/debug/app-debug.apk
$ adb reverse tcp:5920 tcp:5920
$ adb shell am start -n com.papercast/.MainActivity
$ papercast run --output DP-1 --transport papercast
```

The native listener defaults to `127.0.0.1:5920`; override it only with the CLI
`--listen`. It deliberately ignores the VNC-oriented `[mirror].listen` setting.
The native transport requires the e-ink pipeline and rejects `--raw`.

## Modes and configuration

Configuration precedence is built-in defaults, then TOML, then CLI flags. The
annotated [`papercast.toml`](papercast.toml) explains the general settings;
[`boox-tab-x-c.toml`](boox-tab-x-c.toml) is a hardware starting point. The
`[eink]` section hot-reloads while running, except `target-size`, which fixes the
framebuffer geometry at startup.

Modes overlay the base config without replacing unrelated choices such as
`invert`:

- `reading`: 5 fps, clean full redraws, maximum stability
- `browsing`: 15 fps, balanced default
- `writing`: 30 fps, four gray levels and smaller tiles for latency
- `video`: 30 fps, no sharpening or periodic full redraw

```console
$ papercast ctl mode writing
$ papercast ctl refresh
$ papercast ctl status
```

Custom mode overlays live under `[modes.<name>]` in the TOML. The control socket
is `$XDG_RUNTIME_DIR/papercast.sock` and is accessible only to the current user.

## Security

Both transports are unauthenticated. Their defaults bind only to `127.0.0.1`,
and `adb reverse` supplies the USB tunnel. A command such as
`--listen 0.0.0.0:5900` exposes the mirrored screen to the LAN; use it only on a
network you trust.

## Architecture

- `papercast-core`: pure pixel processing and processed-frame tile diffing
- `papercast-capture`: test source, Wayland capture, and capability probing
- `papercast-proto`: versioned native wire messages and zstd rect encoding
- `papercast-recv-core`: device-neutral receiver and optional JNI boundary
- `papercast`: CLI, config/control assembly, and concrete VNC/native transports
- `android`: Activity, renderer, lifecycle, and device refresh-backend seam

The VNC transport uses a vendored `rustvncserver` 2.2.1 with three local patches:
exact-address binding, correct fragmented variable-length message parsing, and
update pacing that permits writing mode to approach 30 fps. Provenance and patch
details live in
[`vendor/rustvncserver/VENDORED.md`](vendor/rustvncserver/VENDORED.md). The native
transport does not depend on this crate.

## Road to 1.0

- Validate both transports, latency, ghosting, modes, dithering, receiver cost,
  and color on real hardware. Make native the documented daily path only if it
  wins writing-mode median latency by at least 30 ms or visibly improves panel
  behavior; if it loses, re-test after adding any needed vendor refresh backend.
- Tune device TOMLs from measurements. Make Atkinson the reading default only if
  it is both crisper than Bayer and stable during partial updates.
- Measure receiver conversion/draw time and host pipeline cost. Move Gray8-to-ARGB
  into Rust only above about 10 ms/frame; consider GPU work only when the measured
  pipeline exceeds the active budget (33 ms in writing mode).
- Add first-class color only if it clearly wins for at least two of code,
  photo-heavy web content, and color figures/PDFs. Color requires an explicit
  protocol version; reading and writing remain monochrome unless hardware says
  otherwise.
- Add a vendor refresh backend only where hardware requires it. Keep `generic` as
  fallback, preserve emulator builds, and confine device APIs to the Android
  backend seam. A cached full-screen redraw must request Quality, never replay a
  possibly Fast hint.
- Submit the three VNC changes upstream and return to the released crate when
  possible. Keep portal capture, extended display, and GPU work demand-driven.
- Release 1.0 after hardware decisions are executed, VNC patches are submitted,
  versions and changelog are ready, and a tagged release ships a properly signed
  APK without committing signing material.

## License

PaperCast is MIT licensed; see [`LICENSE`](LICENSE). Vendored `rustvncserver`
remains Apache-2.0 with its license and notice intact.
