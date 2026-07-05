//! PaperCast receiver core: everything below the screen on the tablet side.
//!
//! One native thread owns a single TCP connection to the host sender (reached
//! via `adb reverse tcp:5920 tcp:5920`), the `papercast-proto` handshake, the
//! decode loop, pull-based flow control, and reconnect-with-backoff. It hands
//! decoded frames to a [`FrameSink`] through a framebuffer buffer it owns and
//! reuses, so steady-state frame delivery allocates nothing.
//!
//! The crate is deliberately **device-neutral**: [`RefreshHint`] passes straight
//! through to the sink as intent (Auto / Fast / Quality). Mapping a hint to a
//! concrete e-ink waveform is the Kotlin shell's job (per the device strategy),
//! never this crate's. That keeps the same `.so` correct on a Boox, a Daylight
//! DC-1, or any other Android target.
//!
//! JNI bindings are gated behind the `android` cargo feature; with the feature
//! off (the default) this builds as pure Rust and is driven directly by the
//! host-side integration tests.

use std::io::{self, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use papercast_proto::{decode, encode, Message, ServerHello, Update, PROTO_VERSION};
use tracing::debug;

/// Refresh intent for a delivered frame — re-exported so a consumer needs only
/// this crate, not `papercast-proto`, to pattern-match on it.
pub use papercast_proto::RefreshHint;

/// How long a single `connect` attempt may block before we give up and back off.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Read timeout so a blocked read wakes periodically to observe the stop flag.
const READ_TIMEOUT: Duration = Duration::from_millis(250);
/// Reconnect backoff bounds; reset to the minimum after a successful handshake.
const BACKOFF_MIN: Duration = Duration::from_millis(200);
const BACKOFF_MAX: Duration = Duration::from_secs(5);

/// A frame ready to draw. `pixels` borrows the core's reused framebuffer for the
/// duration of the [`FrameSink::on_frame`] call — copy out what you need to keep.
pub struct FrameView<'a> {
    pub width: u16,
    pub height: u16,
    pub refresh_hint: RefreshHint,
    /// Tightly packed, row-major Gray8, `width * height` bytes.
    pub pixels: &'a [u8],
}

/// The consumer of decoded frames. The host test implements this directly; the
/// Kotlin shell will implement it (via JNI) to blit into a `Bitmap` and drive
/// the device's refresh backend. All callbacks run on the receiver's own thread.
pub trait FrameSink: Send {
    /// A decoded frame is ready. Returning promptly is what paces the stream:
    /// the core sends the next `Ready` only after this returns, so a slow draw
    /// naturally throttles the sender (one update in flight at a time).
    fn on_frame(&mut self, frame: FrameView<'_>);

    /// A connection was established; carries the framebuffer geometry and the
    /// sender's quantization level count. Default: ignore.
    fn on_connect(&mut self, _width: u16, _height: u16, _levels: u8) {}

    /// The active display mode changed. Informational only — it never changes
    /// receiver behavior (the per-frame [`RefreshHint`] carries all the intent).
    /// Default: ignore.
    fn on_mode_changed(&mut self, _name: &str) {}

    /// An established connection was lost; the core will reconnect. Default:
    /// ignore.
    fn on_disconnect(&mut self) {}
}

/// A running receiver. Dropping it (or calling [`Receiver::stop`]) signals the
/// worker thread to finish the current read cycle and exit.
pub struct Receiver {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

/// Start a receiver: resolve `addr`, spawn the worker thread, and return a handle
/// that stops it on drop. The address is resolved eagerly so a typo is reported
/// here rather than swallowed on the thread.
pub fn start<S: FrameSink + 'static>(addr: &str, sink: S) -> io::Result<Receiver> {
    let resolved = addr
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "address resolved to nothing"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let handle = thread::Builder::new()
        .name("papercast-recv".into())
        .spawn(move || run(resolved, &stop_thread, sink))?;
    Ok(Receiver { stop, handle: Some(handle) })
}

impl Receiver {
    /// Signal the worker to stop and wait for it to exit.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The reconnect loop: connect, serve until the connection ends, back off, repeat
/// — until the stop flag is set.
fn run<S: FrameSink>(addr: SocketAddr, stop: &AtomicBool, mut sink: S) {
    let mut backoff = BACKOFF_MIN;
    while !stop.load(Ordering::Relaxed) {
        match connect_and_serve(addr, stop, &mut sink, &mut backoff) {
            // Only an interrupt (stop requested) short-circuits without backing
            // off; every other error is a normal disconnect → reconnect.
            Err(e) if e.kind() == ErrorKind::Interrupted => break,
            Err(e) => debug!("papercast-recv connection ended: {e}"),
            Ok(()) => {}
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        sleep_interruptible(stop, backoff);
        backoff = (backoff.saturating_mul(2)).min(BACKOFF_MAX);
    }
    debug!("papercast-recv thread exiting");
}

/// One connection's lifetime: connect, handshake, then pull frames until the
/// link drops or stop is requested. Resets `backoff` once the handshake proves
/// the endpoint is healthy, so only genuinely flapping servers keep backing off.
fn connect_and_serve<S: FrameSink>(
    addr: SocketAddr,
    stop: &AtomicBool,
    sink: &mut S,
    backoff: &mut Duration,
) -> io::Result<()> {
    let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;

    let mut buf = Vec::new();
    let mut fb = handshake(&stream, &mut buf, stop)?;
    *backoff = BACKOFF_MIN;
    sink.on_connect(fb.width, fb.height, fb.levels);

    let result = pull_loop(&stream, &mut buf, &mut fb, sink, stop);
    sink.on_disconnect();
    result
}

/// Send our `ClientHello` and read until the sender's `ServerHello`, which sizes
/// the framebuffer we'll accumulate rects into.
fn handshake(
    mut stream: &TcpStream,
    buf: &mut Vec<u8>,
    stop: &AtomicBool,
) -> io::Result<Framebuffer> {
    stream.write_all(&encode(&Message::ClientHello { proto_version: PROTO_VERSION }))?;
    loop {
        match read_message(stream, buf, stop)? {
            Message::ServerHello(h) => return Ok(Framebuffer::new(&h)),
            // Nothing else is expected before the hello; skip defensively.
            other => debug!("papercast-recv: ignoring {other:?} before ServerHello"),
        }
    }
}

/// The steady-state loop: request a frame with `Ready`, apply the `Update` the
/// sender returns, deliver it, and repeat — exactly one update in flight.
fn pull_loop<S: FrameSink>(
    mut stream: &TcpStream,
    buf: &mut Vec<u8>,
    fb: &mut Framebuffer,
    sink: &mut S,
    stop: &AtomicBool,
) -> io::Result<()> {
    // Prime the pull: ask for the first frame.
    stream.write_all(&encode(&Message::Ready))?;
    loop {
        match read_message(stream, buf, stop)? {
            Message::Update(u) => {
                fb.apply(&u)?;
                sink.on_frame(FrameView {
                    width: fb.width,
                    height: fb.height,
                    refresh_hint: u.refresh_hint,
                    pixels: &fb.pixels,
                });
                // Request the next frame only now — this is the back-pressure.
                stream.write_all(&encode(&Message::Ready))?;
            }
            Message::ModeChanged(name) => sink.on_mode_changed(&name),
            // A mid-stream ServerHello would mean a framebuffer resize; the host
            // never does this today, so treat it as a protocol reset.
            Message::ServerHello(h) => {
                *fb = Framebuffer::new(&h);
                sink.on_connect(fb.width, fb.height, fb.levels);
            }
            // Client→server messages arriving from the server are nonsense; a
            // conforming sender never emits them, so drop the connection.
            other => {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("unexpected server message: {other:?}"),
                ));
            }
        }
    }
}

/// Read one decoded message, draining consumed bytes from `buf`. Blocks (waking
/// every [`READ_TIMEOUT`] to check `stop`) until a full frame arrives; returns an
/// `Interrupted` error if stop is requested and `UnexpectedEof` if the peer
/// closes.
fn read_message(
    mut stream: &TcpStream,
    buf: &mut Vec<u8>,
    stop: &AtomicBool,
) -> io::Result<Message> {
    let mut chunk = [0u8; 8192];
    loop {
        match decode(buf) {
            Ok(Some((msg, consumed))) => {
                buf.drain(..consumed);
                return Ok(msg);
            }
            Ok(None) => {} // need more bytes
            Err(e) => return Err(io::Error::new(ErrorKind::InvalidData, e.to_string())),
        }
        if stop.load(Ordering::Relaxed) {
            return Err(io::Error::new(ErrorKind::Interrupted, "stop requested"));
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err(io::Error::new(ErrorKind::UnexpectedEof, "sender closed")),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            // Read timeout: loop back to re-check the stop flag.
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e),
        }
    }
}

/// Sleep up to `dur`, but wake every 50 ms to observe the stop flag so teardown
/// never waits a full backoff interval.
fn sleep_interruptible(stop: &AtomicBool, dur: Duration) {
    let step = Duration::from_millis(50);
    let mut remaining = dur;
    while !remaining.is_zero() && !stop.load(Ordering::Relaxed) {
        let s = remaining.min(step);
        thread::sleep(s);
        remaining = remaining.saturating_sub(s);
    }
}

/// The reused Gray8 framebuffer. Allocated once per connection at handshake and
/// mutated in place by each update, so steady-state delivery is allocation-free.
struct Framebuffer {
    width: u16,
    height: u16,
    levels: u8,
    pixels: Vec<u8>,
}

impl Framebuffer {
    fn new(h: &ServerHello) -> Self {
        let len = h.width as usize * h.height as usize;
        Self { width: h.width, height: h.height, levels: h.levels, pixels: vec![0u8; len] }
    }

    /// Blit each update rect into the persistent framebuffer. A rect that
    /// escapes bounds or disagrees with its pixel count is a malformed frame:
    /// reject it (the caller drops the connection) rather than panic.
    fn apply(&mut self, update: &Update) -> io::Result<()> {
        let stride = self.width as usize;
        for r in &update.rects {
            let (x, y, w, h) = (
                r.rect.x as usize,
                r.rect.y as usize,
                r.rect.width as usize,
                r.rect.height as usize,
            );
            if x + w > self.width as usize || y + h > self.height as usize {
                return Err(io::Error::new(ErrorKind::InvalidData, "update rect out of bounds"));
            }
            if r.gray8.len() != w * h {
                return Err(io::Error::new(ErrorKind::InvalidData, "update rect size mismatch"));
            }
            for row in 0..h {
                let src = row * w;
                let dst = (y + row) * stride + x;
                self.pixels[dst..dst + w].copy_from_slice(&r.gray8[src..src + w]);
            }
        }
        Ok(())
    }
}
