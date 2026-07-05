//! The papercast custom transport: serves `papercast-proto` over TCP with
//! pull-based flow control, selected by `--transport papercast`.
//!
//! It shares all of `serve()`'s setup (source, mode state, control socket,
//! config watcher); only the *output* half differs from the VNC path. Where VNC
//! pushes rects into a framebuffer, this speaks framed protocol messages and,
//! crucially, only sends an [`Update`] when the client has a [`Ready`] pending —
//! keeping just the newest frame otherwise, so a slow EPD never builds a latency
//! queue.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Context;
use papercast_core::tiles::TileDiff;
use papercast_core::{Frame, Rect as CoreRect};
use papercast_proto::{
    encode, Message, RefreshHint, Rect, ServerHello, Update, UpdateRect, PROTO_VERSION,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::mode::{ModeSettings, ModeState};

/// What the transport needs beyond its I/O channels.
pub struct ProtoConfig {
    /// Framebuffer size (width, height), already validated to fit `u16`.
    pub framebuffer: (u16, u16),
    /// Shared mode state — read for the mode name (`ModeChanged`) and to derive
    /// the per-update refresh hint.
    pub mode_state: Arc<Mutex<ModeState>>,
}

/// Run the transport until the frame source ends or the listener fails. One
/// client at a time: a new connection replaces the previous one.
pub async fn serve_proto(
    listener: TcpListener,
    cfg: ProtoConfig,
    mut frames: mpsc::Receiver<Frame>,
    mut settings_rx: watch::Receiver<ModeSettings>,
    mut refresh_rx: mpsc::Receiver<()>,
) -> anyhow::Result<()> {
    let (fb_w, fb_h) = cfg.framebuffer;
    // `Ready` messages from whichever client is currently connected funnel back
    // here so the loop can gate its sends on them. Each carries the connection
    // generation it was read under, so a replaced client's in-flight Ready is
    // recognizable and ignored.
    let (ready_tx, mut ready_rx) = mpsc::channel::<u64>(8);

    let mut current = settings_rx.borrow_and_update().clone();
    let mut tiler = TileDiff::new(current.tile_size, 8);
    let mut updates_since_full: u64 = 0;
    let mut last_full = Instant::now();
    // Suppress duplicate ModeChanged: a config edit re-applies the active mode's
    // overlay without changing its name, so only announce actual name changes.
    let mut last_mode_sent = cfg
        .mode_state
        .lock()
        .ok()
        .and_then(|s| s.active().map(str::to_string));
    let mut sink = Sink::new(fb_w, fb_h);

    loop {
        tokio::select! {
            // A new receiver connected: hand its read half to a parser task,
            // keep its write half, greet it, and queue a full paint for its
            // first Ready.
            accepted = listener.accept() => {
                let (stream, peer) = accepted.context("papercast transport accept failed")?;
                debug!("papercast client connected from {peer}");
                let (read, write) = stream.into_split();
                sink.attach(write);
                tokio::spawn(client_reader(read, ready_tx.clone(), sink.generation));
                let levels = cfg
                    .mode_state
                    .lock()
                    .map(|s| s.effective().eink.levels)
                    .unwrap_or(16);
                sink.send(&Message::ServerHello(ServerHello {
                    proto_version: PROTO_VERSION,
                    width: fb_w,
                    height: fb_h,
                    levels,
                }))
                .await;
                sink.pending_full = true;
            }

            // Client is ready for the next update. Ignore a Ready from a stale
            // generation — a client being replaced can leave one last Ready in
            // flight that the new client never requested.
            Some(gen) = ready_rx.recv() => {
                if gen == sink.generation {
                    sink.ready = true;
                    sink.flush().await;
                }
            }

            // A processed frame arrived: diff it into changed rects, keep the
            // newest update, and apply the periodic full-refresh policy.
            maybe_frame = frames.recv() => {
                let Some(frame) = maybe_frame else {
                    info!("papercast transport: frame source ended");
                    return Ok(());
                };
                let data = frame.data;
                let rects = tiler.diff(&data, (frame.width, frame.height));
                if !rects.is_empty() {
                    let hint = hint_for(&cfg.mode_state);
                    let update = Update {
                        refresh_hint: hint,
                        rects: rects
                            .iter()
                            .map(|r| UpdateRect { rect: to_proto_rect(*r), gray8: extract_gray(&data, fb_w, *r) })
                            .collect(),
                    };
                    sink.latest = Some(update);
                    updates_since_full += 1;
                }
                let due_time = current.full_refresh_secs > 0
                    && last_full.elapsed().as_secs() >= current.full_refresh_secs;
                let due_count = current.full_refresh_updates > 0
                    && updates_since_full >= current.full_refresh_updates;
                if due_time || due_count {
                    sink.pending_full = true;
                    last_full = Instant::now();
                    updates_since_full = 0;
                }
                sink.last_frame = Some(data);
                sink.flush().await;
            }

            // Mode switch or config edit: rebuild the tiler on a tile-size
            // change, tell the client the new mode name, and force a full paint.
            Ok(()) = settings_rx.changed() => {
                let next = settings_rx.borrow_and_update().clone();
                if next.tile_size != current.tile_size {
                    tiler = TileDiff::new(next.tile_size, 8);
                }
                current = next;
                let name = cfg
                    .mode_state
                    .lock()
                    .ok()
                    .and_then(|s| s.active().map(str::to_string));
                if name != last_mode_sent {
                    if let Some(n) = name.clone() {
                        sink.send(&Message::ModeChanged(n)).await;
                    }
                    last_mode_sent = name;
                }
                sink.pending_full = true;
                last_full = Instant::now();
                updates_since_full = 0;
                sink.flush().await;
            }

            // `ctl refresh`: force one full-quality repaint now.
            Some(()) = refresh_rx.recv() => {
                sink.pending_full = true;
                last_full = Instant::now();
                updates_since_full = 0;
                sink.flush().await;
                debug!("papercast transport: forced full refresh (ctl)");
            }

            else => return Ok(()),
        }
    }
}

/// The one connected client plus everything gating what it's owed. Sends happen
/// only through [`Sink::flush`], which enforces the pull contract.
struct Sink {
    client: Option<OwnedWriteHalf>,
    /// A `Ready` is pending — the client will accept exactly one update.
    ready: bool,
    /// A full-quality repaint is owed (mode change, periodic ghost-clear, ctl
    /// refresh, or a fresh connection). Takes priority over `latest` and, once
    /// sent, supersedes it.
    pending_full: bool,
    /// Newest partial update, replaced on every frame (keep-newest).
    latest: Option<Update>,
    /// Last full processed frame, so a full repaint can be sent even when no new
    /// frame is arriving (idle screen).
    last_frame: Option<Vec<u8>>,
    /// Incremented on every reconnect; a `Ready` carrying an older generation
    /// belongs to a client we've since replaced and is ignored.
    generation: u64,
    fb_w: u16,
    fb_h: u16,
}

impl Sink {
    fn new(fb_w: u16, fb_h: u16) -> Self {
        Self {
            client: None,
            ready: false,
            pending_full: false,
            latest: None,
            last_frame: None,
            generation: 0,
            fb_w,
            fb_h,
        }
    }

    /// Replace the current client (a second connection supersedes the first),
    /// bumping the generation so the old client's stray `Ready`s are ignored.
    fn attach(&mut self, write: OwnedWriteHalf) {
        self.client = Some(write);
        self.ready = false;
        self.generation += 1;
    }

    /// Write one message; drop the client on I/O error. Returns whether the
    /// client is still connected afterward.
    async fn send(&mut self, msg: &Message) -> bool {
        let Some(w) = self.client.as_mut() else { return false };
        if w.write_all(&encode(msg)).await.is_err() {
            debug!("papercast client write failed; dropping");
            self.client = None;
            false
        } else {
            true
        }
    }

    /// Send at most one update, honoring the pull contract: nothing without a
    /// pending `Ready` and a live client. A full repaint wins over a partial.
    async fn flush(&mut self) {
        if !self.ready || self.client.is_none() {
            return;
        }
        if self.pending_full {
            let Some(frame) = self.last_frame.clone() else {
                return; // nothing painted yet; wait for the first frame
            };
            let update = Update {
                refresh_hint: RefreshHint::Quality,
                rects: vec![UpdateRect {
                    rect: Rect { x: 0, y: 0, width: self.fb_w, height: self.fb_h },
                    gray8: frame,
                }],
            };
            if self.send(&Message::Update(update)).await {
                self.pending_full = false;
                self.latest = None; // the full frame supersedes any pending partial
                self.ready = false;
            }
        } else if let Some(update) = self.latest.take() {
            if self.send(&Message::Update(update)).await {
                self.ready = false;
            }
            // On failure the update is dropped; the next client gets a full paint.
        }
    }
}

/// Parse messages from one client, forwarding each `Ready` to the serve loop.
/// Ends on EOF, malformed input, or when the loop is gone.
async fn client_reader(mut read: OwnedReadHalf, ready_tx: mpsc::Sender<u64>, generation: u64) {
    // Legitimate client messages are tiny (Ready is 5 bytes, ClientHello 6), so
    // a buffer that grows past this is a stuck or hostile peer. `adb reverse`
    // exposes this port to any app on the tablet, so cap it and drop.
    const MAX_CLIENT_BUF: usize = 1024;
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    let mut chunk = [0u8; 256];
    loop {
        match read.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if buf.len() > MAX_CLIENT_BUF {
            warn!("papercast client exceeded {MAX_CLIENT_BUF}-byte read buffer; dropping");
            return;
        }
        loop {
            match papercast_proto::decode(&buf) {
                Ok(Some((msg, consumed))) => {
                    buf.drain(..consumed);
                    // Ready is the only client message that drives us; ClientHello
                    // carries just a version and anything else is unexpected but
                    // harmless, so both are ignored.
                    if msg == Message::Ready && ready_tx.send(generation).await.is_err() {
                        return; // serve loop gone
                    }
                }
                Ok(None) => break, // need more bytes
                Err(e) => {
                    warn!("papercast client sent malformed data: {e}; dropping");
                    return;
                }
            }
        }
    }
}

/// Which EPD waveform intent to request for a partial update: writing/video
/// mode want the fast waveform; everything else lets the receiver decide.
/// (Forced full refreshes always request quality — see [`Sink::flush`].)
fn hint_for(mode_state: &Arc<Mutex<ModeState>>) -> RefreshHint {
    match mode_state.lock() {
        Ok(state) => match state.active() {
            Some("writing" | "video") => RefreshHint::Fast,
            _ => RefreshHint::Auto,
        },
        Err(_) => RefreshHint::Auto,
    }
}

fn to_proto_rect(r: CoreRect) -> Rect {
    Rect { x: r.x as u16, y: r.y as u16, width: r.width as u16, height: r.height as u16 }
}

/// Copy a sub-rectangle out of a full-frame Gray8 buffer into a tightly-packed,
/// row-major buffer (what the wire format carries).
fn extract_gray(frame: &[u8], stride: u16, r: CoreRect) -> Vec<u8> {
    let stride = stride as usize;
    let mut out = Vec::with_capacity(r.width as usize * r.height as usize);
    for y in r.y..r.y + r.height {
        let start = y as usize * stride + r.x as usize;
        out.extend_from_slice(&frame[start..start + r.width as usize]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use papercast_core::PixelFormat;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;

    fn gray_frame(w: u32, h: u32, fill: u8) -> Frame {
        Frame { width: w, height: h, format: PixelFormat::Gray8, data: vec![fill; (w * h) as usize], damage: None }
    }

    // Read exactly one decoded message from a stream (blocking on more bytes).
    async fn read_message(stream: &mut TcpStream, buf: &mut Vec<u8>) -> Message {
        let mut chunk = [0u8; 4096];
        loop {
            if let Ok(Some((msg, consumed))) = papercast_proto::decode(buf) {
                buf.drain(..consumed);
                return msg;
            }
            let n = stream.read(&mut chunk).await.expect("read");
            assert_ne!(n, 0, "server closed unexpectedly");
            buf.extend_from_slice(&chunk[..n]);
        }
    }

    #[tokio::test]
    async fn no_update_without_ready_then_flows_after() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (frames_tx, frames_rx) = mpsc::channel::<Frame>(8);
        let base = ModeSettings {
            eink: papercast_core::eink::EinkConfig { levels: 16, ..Default::default() },
            fps: 15,
            tile_size: 64,
            full_refresh_secs: 0,
            full_refresh_updates: 0,
        };
        let state = Arc::new(Mutex::new(ModeState::new(base.clone(), Default::default(), None).unwrap()));
        let (_settings_tx, settings_rx) = watch::channel(base);
        let (_refresh_tx, refresh_rx) = mpsc::channel::<()>(8);

        let cfg = ProtoConfig { framebuffer: (32, 24), mode_state: state };
        let server = tokio::spawn(serve_proto(listener, cfg, frames_rx, settings_rx, refresh_rx));

        let mut client = TcpStream::connect(addr).await.unwrap();
        let mut buf = Vec::new();

        // Handshake: client hello, then expect the server hello.
        client.write_all(&encode(&Message::ClientHello { proto_version: PROTO_VERSION })).await.unwrap();
        match read_message(&mut client, &mut buf).await {
            Message::ServerHello(h) => {
                assert_eq!((h.width, h.height, h.levels), (32, 24, 16));
            }
            other => panic!("expected ServerHello, got {other:?}"),
        }

        // Push several frames WITHOUT sending Ready. The pull contract means no
        // Update may arrive. Assert the socket stays silent.
        for i in 0..3 {
            frames_tx.send(gray_frame(32, 24, 10 * i)).await.unwrap();
        }
        let mut chunk = [0u8; 64];
        let silent = tokio::time::timeout(Duration::from_millis(200), client.read(&mut chunk)).await;
        assert!(silent.is_err(), "server pushed an update before any Ready was sent");

        // Now send Ready; an Update (the newest frame, as a full-quality paint
        // for the fresh connection) must arrive.
        client.write_all(&encode(&Message::Ready)).await.unwrap();
        match read_message(&mut client, &mut buf).await {
            Message::Update(u) => {
                assert_eq!(u.refresh_hint, RefreshHint::Quality, "first paint is a full refresh");
                assert_eq!(u.rects.len(), 1);
                assert_eq!((u.rects[0].rect.width, u.rects[0].rect.height), (32, 24));
                assert_eq!(u.rects[0].gray8.len(), 32 * 24);
            }
            other => panic!("expected Update after Ready, got {other:?}"),
        }

        server.abort();
    }
}
