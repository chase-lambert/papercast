#!/usr/bin/env python3
"""Drive a `papercast run` over RFB + `papercast ctl` and assert mode behavior.

This regression check:

  * starts `papercast run --source test` on an isolated port + control socket,
  * connects as a minimal RFB 3.8 client (security None, Raw encoding) and
    enables the ContinuousUpdates extension so the server *pushes* frames (this
    is how real viewers run; a one-request-at-a-time client would instead be
    capped by the server's ~16 ms request-check latency, not its frame rate),
  * measures the framebuffer-update rate the server pushes in each mode (the
    test source animates every frame, so updates arrive at ~the source fps),
  * switches modes with `papercast ctl` and asserts:
      - a near-full-frame update lands right after a switch (the mode-change
        redraw), and
      - the update cadence tracks the mode fps.

Note on the writing-mode target: the vendored rustvncserver quantizes its
continuous-update pushes to a check tick against a min send-interval. As shipped
upstream (16 ms tick / 33 ms interval) that capped delivery at ~20 fps; the local
pacing patch (8 ms tick / 30 ms interval, see vendor/.../VENDORED.md) lifts the
ceiling to ~31 fps, so writing's 30 fps source is now observable. Reading (5) and
browsing (15) sit well under the ceiling and are measured accurately. Writing is
also the regression test for the serve-loop jitter bug: with that bug writing was
throttled to ~15 fps (halved), so asserting writing reaches ~27 catches both a
jitter regression and a revert of the pacing patch. (The native protocol
has no such cap at all.)

Usage:
    tools/rfb_mode_check.py [--papercast target/debug/papercast] [--port 5911]

Exit code 0 = all assertions passed.
"""

import argparse
import os
import socket
import struct
import subprocess
import sys
import tempfile
import time


def recv_exactly(sock, n):
    buf = bytearray()
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("server closed mid-message")
        buf += chunk
    return bytes(buf)


def rfb_handshake(sock):
    """RFB 3.8, security type None. Returns (width, height)."""
    server_version = recv_exactly(sock, 12)
    if not server_version.startswith(b"RFB "):
        raise ValueError(f"not an RFB server: {server_version!r}")
    sock.sendall(b"RFB 003.008\n")

    n_types = recv_exactly(sock, 1)[0]
    if n_types == 0:
        reason_len = struct.unpack(">I", recv_exactly(sock, 4))[0]
        raise ValueError("server refused: " + recv_exactly(sock, reason_len).decode())
    types = recv_exactly(sock, n_types)
    if 1 not in types:  # 1 = None
        raise ValueError(f"server offers no None security (got {list(types)})")
    sock.sendall(bytes([1]))

    security_result = struct.unpack(">I", recv_exactly(sock, 4))[0]
    if security_result != 0:
        raise ValueError("security handshake failed")

    sock.sendall(bytes([1]))  # ClientInit, shared=1
    server_init = recv_exactly(sock, 24)
    width, height = struct.unpack(">HH", server_init[:4])
    name_len = struct.unpack(">I", server_init[20:24])[0]
    recv_exactly(sock, name_len)  # desktop name
    return width, height


def set_encodings(sock, encodings):
    msg = struct.pack(">BBH", 2, 0, len(encodings))
    for e in encodings:
        msg += struct.pack(">i", e)
    sock.sendall(msg)


def request_update(sock, width, height, incremental):
    sock.sendall(struct.pack(">BBHHHH", 3, incremental, 0, 0, width, height))


def enable_continuous_updates(sock, width, height):
    # ClientMsg 150: enable(u8)=1, x, y, w, h. Server then pushes updates.
    sock.sendall(struct.pack(">BBHHHH", 150, 1, 0, 0, width, height))


def read_framebuffer_update(sock, width, height, bpp_bytes=4):
    """Read one FramebufferUpdate (Raw rects only). Returns pixels covered."""
    msg_type = recv_exactly(sock, 1)[0]
    if msg_type != 0:
        # Skip server messages we don't model (bell, cut text, etc.).
        _skip_other_message(sock, msg_type)
        return 0
    recv_exactly(sock, 1)  # padding
    n_rects = struct.unpack(">H", recv_exactly(sock, 2))[0]
    covered = 0
    for _ in range(n_rects):
        x, y, w, h, enc = struct.unpack(">HHHHi", recv_exactly(sock, 12))
        if enc != 0:
            raise ValueError(f"expected Raw(0) encoding, got {enc}")
        recv_exactly(sock, w * h * bpp_bytes)
        covered += w * h
    return covered


def _skip_other_message(sock, msg_type):
    if msg_type in (2, 150):  # Bell / EndOfContinuousUpdates: no payload
        return
    if msg_type == 3:  # ServerCutText
        recv_exactly(sock, 3)
        n = struct.unpack(">I", recv_exactly(sock, 4))[0]
        recv_exactly(sock, n)
        return
    raise ValueError(f"unexpected server message type {msg_type}")


def measure_fps(sock, width, height, seconds):
    """Count pushed framebuffer updates over `seconds`.

    Returns (fps, max_frac_covered). Relies on ContinuousUpdates being enabled
    so the server pushes without per-frame requests.
    """
    sock.settimeout(seconds + 1.0)
    deadline = time.monotonic() + seconds
    frames = 0
    max_frac = 0.0
    total = width * height
    while time.monotonic() < deadline:
        covered = read_framebuffer_update(sock, width, height)
        if covered:
            frames += 1
            max_frac = max(max_frac, covered / total)
    return frames / seconds, max_frac


def ctl(papercast, socket_path, *args):
    env = dict(os.environ, XDG_RUNTIME_DIR=os.path.dirname(socket_path))
    return subprocess.run(
        [papercast, "ctl", *args], env=env, capture_output=True, text=True
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--papercast", default="target/debug/papercast")
    ap.add_argument("--port", type=int, default=5911)
    ap.add_argument("--size", default="320x240")
    args = ap.parse_args()

    # Isolate the control socket in a short-path temp dir (Unix sockets have a
    # ~108-char limit).
    runtime = tempfile.mkdtemp(prefix="pct-", dir="/tmp")
    socket_path = os.path.join(runtime, "papercast.sock")
    env = dict(os.environ, XDG_RUNTIME_DIR=runtime, RUST_LOG="warn")

    server = subprocess.Popen(
        [args.papercast, "run", "--source", "test",
         "--size", args.size, "--listen", f"127.0.0.1:{args.port}"],
        env=env,
    )
    failures = []
    try:
        # Wait for the port to open.
        for _ in range(50):
            try:
                s = socket.create_connection(("127.0.0.1", args.port), timeout=0.5)
                break
            except OSError:
                time.sleep(0.1)
        else:
            raise SystemExit("server did not start")

        width, height = rfb_handshake(s)
        set_encodings(s, [0, -313])  # Raw + ContinuousUpdates pseudo-encoding
        enable_continuous_updates(s, width, height)
        print(f"connected: {width}x{height}")

        def check(mode, lo, hi, label):
            ctl(args.papercast, socket_path, "mode", mode)
            # Measure over a window; a switch forces a near-full-frame redraw,
            # so max coverage in the window should reach the whole framebuffer.
            fps, max_frac = measure_fps(s, width, height, 2.0)
            print(f"  {mode:8s}: {fps:5.1f} fps (expect {label}), "
                  f"max coverage {max_frac:.2f}")
            if not (lo <= fps <= hi):
                failures.append(f"{mode}: {fps:.1f} fps, expected {label}")
            if max_frac < 0.9:
                failures.append(f"{mode}: no full-frame redraw after switch "
                                f"(max coverage {max_frac:.2f})")

        # writing is the regression case: with the local pacing patch the server
        # ceiling is ~31 fps, so a 30 fps source is nearly fully delivered. The
        # jitter bug throttled it to ~15 (below browsing) and a pacing-patch
        # revert drops it to ~20, so requiring >= 27 catches either.
        check("writing", 27, 34, ">=27, server-capped ~31")
        check("reading", 3.5, 7, "~5")
        check("browsing", 12, 18, "~15")

        # Unknown mode must be rejected.
        r = ctl(args.papercast, socket_path, "mode", "nope")
        if r.returncode == 0:
            failures.append("unknown mode was accepted")

        s.close()
    finally:
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()

    if failures:
        print("\nFAILURES:")
        for f in failures:
            print("  -", f)
        sys.exit(1)
    print("\nall checks passed")


if __name__ == "__main__":
    main()
