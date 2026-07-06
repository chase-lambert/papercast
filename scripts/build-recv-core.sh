#!/usr/bin/env bash
# Cross-compile papercast-recv-core for Android and drop the .so's where the
# Kotlin shell (M11b) packages them: android/app/src/main/jniLibs/<abi>/.
#
# Prerequisites (see the "Native Android receiver" section in README.md):
#   - rustup targets aarch64-linux-android + x86_64-linux-android
#   - cargo-ndk (cargo install cargo-ndk)
#   - an Android NDK, with ANDROID_NDK_HOME pointing at it
#
# Usage: scripts/build-recv-core.sh [release|debug]   (default: release)
set -euo pipefail

profile="${1:-release}"
case "$profile" in
  release) build_flag="--release" ;;
  debug)   build_flag="" ;;
  *) echo "usage: $0 [release|debug]" >&2; exit 1 ;;
esac

repo="$(cd "$(dirname "$0")/.." && pwd)"
out="$repo/android/app/src/main/jniLibs"

# arm64-v8a is the device; x86_64 is the emulator. cargo-ndk creates the per-ABI
# subdirectories under -o and copies the built .so into each.
cargo ndk -t arm64-v8a -t x86_64 -o "$out" \
  build -p papercast-recv-core --features android ${build_flag}

echo "wrote libpapercast_recv_core.so (arm64-v8a, x86_64) to $out"
