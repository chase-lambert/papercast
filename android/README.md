# PaperCast Android receiver

The thin Kotlin shell for PaperCast's custom transport (Phase 2). It loads the
Rust receiver core (`papercast-recv-core`, built as a `.so`), which owns the TCP
connection, protocol, decode, and pull flow control; the Kotlin side is only an
Activity, a `SurfaceView`, lifecycle glue, and a `RefreshBackend` seam. No
protocol or decode logic lives in Kotlin.

It's **device-neutral**: a frame carries a refresh *intent* (Auto/Fast/Quality),
and a `RefreshBackend` maps that intent to whatever the panel needs. This
milestone ships only the `generic` backend (draw and ignore the hint); vendor
backends (e.g. Onyx `EpdController`) arrive later behind the same interface.

## Prerequisites

- **Android SDK** (platform 34, build-tools) and an **NDK**, with `ANDROID_HOME`
  and `ANDROID_NDK_HOME` set. Command-line install:
  `sdkmanager "platform-tools" "platforms;android-34" "ndk;<version>"`.
- **Rust Android targets + cargo-ndk**:
  `rustup target add aarch64-linux-android x86_64-linux-android` and
  `cargo install cargo-ndk`.
- **A JDK the Android Gradle Plugin supports (17–21).** If your default `java` is
  newer, point the build at a 21 JDK, e.g.
  `export JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64`.
- `local.properties` with `sdk.dir=<your Android SDK path>` (gitignored; create
  it once).

## Build

1. **Cross-compile the native core** into `app/src/main/jniLibs/<abi>/` (arm64 for
   the device, x86_64 for the emulator):
   ```console
   $ ../scripts/build-recv-core.sh          # release; pass `debug` for a debug build
   ```
   These `.so`s are build artifacts (gitignored); re-run this whenever the Rust
   receiver core changes.

2. **Assemble the APK:**
   ```console
   $ ./gradlew assembleDebug
   ```
   Output: `app/build/outputs/apk/debug/app-debug.apk`.

## Install and run

Mirror the host over USB (device) or a reverse bridge (emulator):

```console
$ adb install -r app/build/outputs/apk/debug/app-debug.apk
$ adb reverse tcp:5920 tcp:5920          # host :5920 reachable as 127.0.0.1:5920 on device
$ adb shell am start -n com.papercast/.MainActivity
```

On the host, serve the custom transport:

```console
$ papercast run --source test --transport papercast     # or --output <MONITOR>
```

The app connects to `127.0.0.1:5920`, so it reconnects on its own once the host
is up (and after replug / `adb reverse` is re-run).

### Backend override

The backend is chosen by `Build.MANUFACTURER`, with `generic` as the fallback.
Force one with an intent extra:

```console
$ adb shell am start -n com.papercast/.MainActivity --es backend generic
```

## Verified

Emulator (pixel_tablet, Android 14, x86_64) + `adb reverse tcp:5920 tcp:5920`
mirrors `papercast run --source test --transport papercast`: the dithered test
pattern renders and updates live, with the frame delivered zero-copy through a
direct `ByteBuffer` and drawn letterboxed to the `SurfaceView`.
