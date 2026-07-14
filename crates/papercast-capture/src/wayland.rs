//! Live screen capture via ext-image-copy-capture-v1 (the standard Wayland
//! capture protocol; implemented by cosmic-comp and recent wlroots).
//!
//! Threading: everything Wayland lives on one dedicated OS thread — proxies
//! aren't meant to hop across `await` points, and `blocking_dispatch` wants a
//! thread to itself. Finished frames leave via `blocking_send` on the tokio
//! channel inside [`SourceHandle`].
//!
//! Protocol flow per session:
//!   create_source(output) → create_session → session events (buffer_size,
//!   shm_format…, done) → allocate one wl_shm buffer → per frame:
//!   create_frame + attach_buffer + capture → ready event → copy out.
//!
//! The compositor delays `ready` until the screen actually changed, so an
//! idle desktop produces no frames and no CPU load — ideal for e-ink. The
//! processed output is diffed into tiles by each transport.

use std::fs::File;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use memmap2::MmapMut;
use papercast_core::{Frame, PixelFormat};
use tokio::sync::mpsc;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason},
    ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

use crate::source::{SourceHandle, FRAME_CHANNEL_DEPTH};

pub struct WaylandConfig {
    /// Output (monitor) name as shown by `papercast probe`, e.g. "eDP-1".
    /// `None` = first output.
    pub output: Option<String>,
    /// Upper bound on capture rate; the real rate is change-driven and
    /// usually lower. Used as the fixed rate when `fps_rx` is `None`, and as
    /// the initial rate otherwise.
    pub max_fps: u32,
    /// Optional live fps override (mode switches). The capture thread reads it
    /// with a non-blocking `borrow()` between frames — no `await`, so it's
    /// safe on the Wayland thread.
    pub fps_rx: Option<tokio::sync::watch::Receiver<u32>>,
}

/// Start the capture thread and wait for the session to negotiate a size.
pub fn spawn(config: WaylandConfig) -> anyhow::Result<SourceHandle> {
    let (frame_tx, frame_rx) = mpsc::channel(FRAME_CHANNEL_DEPTH);
    let (init_tx, init_rx) = std::sync::mpsc::channel::<anyhow::Result<(u32, u32)>>();

    std::thread::Builder::new()
        .name("wayland-capture".into())
        .spawn(move || {
            let mut init_tx = Some(init_tx);
            match capture_thread(config, frame_tx, &mut init_tx) {
                Ok(()) => tracing::info!("capture session ended"),
                Err(e) => {
                    tracing::error!("wayland capture failed: {e:#}");
                    // If we died before announcing a size, unblock spawn().
                    if let Some(tx) = init_tx.take() {
                        let _ = tx.send(Err(e));
                    }
                }
            }
        })
        .context("failed to spawn capture thread")?;

    let (width, height) = init_rx
        .recv_timeout(Duration::from_secs(10))
        .context("capture session did not initialize within 10s")??;
    Ok(SourceHandle { width, height, frames: frame_rx })
}

/// Everything the Wayland event handlers write into. The main thread loop
/// reads these flags after each dispatch and drives the protocol forward —
/// handlers only record, they never act.
#[derive(Default)]
struct CaptureState {
    output_names: Vec<Option<String>>,

    // Session buffer constraints (re-sent by the compositor on changes).
    buffer_size: Option<(u32, u32)>,
    shm_formats: Vec<wl_shm::Format>,
    /// Bumped on each session `done`; the loop compares against the
    /// generation it last acted on.
    constraints_generation: u64,
    session_stopped: bool,

    // Current frame lifecycle.
    frame_ready: bool,
    frame_failed: Option<FailureReason>,
}

fn capture_thread(
    config: WaylandConfig,
    frame_tx: mpsc::Sender<Frame>,
    init_tx: &mut Option<std::sync::mpsc::Sender<anyhow::Result<(u32, u32)>>>,
) -> anyhow::Result<()> {
    let conn = Connection::connect_to_env().context("connecting to Wayland display")?;
    let (globals, mut queue) = registry_queue_init::<CaptureState>(&conn)?;
    let qh = queue.handle();
    let mut state = CaptureState::default();

    let source_mgr: ExtOutputImageCaptureSourceManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .context("compositor lacks ext_output_image_capture_source_manager_v1 — run `papercast probe`")?;
    let capture_mgr: ExtImageCopyCaptureManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .context("compositor lacks ext_image_copy_capture_manager_v1 — run `papercast probe`")?;
    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).context("compositor lacks wl_shm")?;

    // Bind every advertised output; their `name` events arrive on the
    // roundtrip below and land in state.output_names by index.
    let outputs: Vec<wl_output::WlOutput> = globals
        .contents()
        .clone_list()
        .into_iter()
        .filter(|g| g.interface == "wl_output")
        .map(|g| {
            let idx = state.output_names.len();
            state.output_names.push(None);
            globals
                .registry()
                .bind::<wl_output::WlOutput, usize, _>(g.name, g.version.min(4), &qh, idx)
        })
        .collect();
    anyhow::ensure!(!outputs.is_empty(), "compositor advertised no outputs");
    queue.roundtrip(&mut state)?;

    let output_idx = match &config.output {
        Some(wanted) => state
            .output_names
            .iter()
            .position(|n| n.as_deref() == Some(wanted))
            .with_context(|| {
                format!(
                    "output '{wanted}' not found; available: {:?}",
                    state.output_names.iter().flatten().collect::<Vec<_>>()
                )
            })?,
        None => 0,
    };
    tracing::info!(
        "capturing output {}",
        state.output_names[output_idx].as_deref().unwrap_or("<unnamed>")
    );

    let source = source_mgr.create_source(&outputs[output_idx], &qh, ());
    // PaintCursors: we get the cursor composited in; there's no client-side
    // cursor plane on the e-ink side to do better.
    let session = capture_mgr.create_session(&source, Options::PaintCursors, &qh, ());

    // Current min frame interval, re-read each frame so a mode switch re-paces
    // live. `borrow()` is a non-blocking read; nothing awaits on this thread.
    let frame_interval = || {
        let fps = config.fps_rx.as_ref().map(|r| *r.borrow()).unwrap_or(config.max_fps);
        Duration::from_secs_f64(1.0 / f64::from(fps.max(1)))
    };
    let mut announced_size: Option<(u32, u32)> = None;
    let mut handled_generation = 0u64;

    // Outer loop: one iteration per buffer-constraints generation. The
    // compositor re-negotiates (new `done`) on e.g. output resize.
    'negotiate: loop {
        while state.constraints_generation == handled_generation {
            if state.session_stopped {
                return Ok(()); // compositor ended the session (output gone)
            }
            queue.blocking_dispatch(&mut state)?;
        }
        handled_generation = state.constraints_generation;

        let (width, height) =
            state.buffer_size.context("session `done` without a buffer_size")?;
        let shm_format = pick_shm_format(&state.shm_formats)?;
        let frame_format = frame_format_for(shm_format);
        tracing::info!("session constraints: {width}x{height}, shm {shm_format:?}");

        match announced_size {
            None => {
                if let Some(tx) = init_tx.take() {
                    let _ = tx.send(Ok((width, height)));
                }
                announced_size = Some((width, height));
            }
            Some(prev) if prev != (width, height) => {
                // The output transport's framebuffer was sized from the first
                // negotiation. Live resize and client re-init are not supported.
                bail!(
                    "capture size changed {prev:?} → {width}x{height}; \
                     restart papercast (live resize not supported yet)"
                );
            }
            Some(_) => {} // same size, e.g. format-only renegotiation
        }

        // One shared-memory buffer: the compositor fills it between
        // capture() and `ready`; afterwards it's ours until the next
        // capture(), which is when we copy it out.
        let stride = width * 4;
        let byte_len = (stride * height) as usize;
        let memfd = rustix::fs::memfd_create("papercast-capture", rustix::fs::MemfdFlags::CLOEXEC)
            .context("memfd_create failed")?;
        let file = File::from(memfd);
        file.set_len(byte_len as u64)?;
        // SAFETY: the map lives as long as `file`; only this thread and the
        // compositor (via the protocol's ready/capture handoff) touch it.
        let mmap = unsafe { MmapMut::map_mut(&file)? }.make_read_only()?;
        let pool = shm.create_pool(file.as_fd(), byte_len as i32, &qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            shm_format,
            &qh,
            (),
        );

        let mut frame = start_capture(&session, &buffer, &qh);
        let mut last_send = Instant::now() - frame_interval();

        // Inner loop: one iteration per dispatched batch of events.
        loop {
            if state.session_stopped {
                frame.destroy();
                cleanup(&buffer, &pool);
                return Ok(());
            }
            if state.constraints_generation != handled_generation {
                frame.destroy();
                cleanup(&buffer, &pool);
                continue 'negotiate;
            }
            if let Some(reason) = state.frame_failed.take() {
                frame.destroy();
                match reason {
                    // New constraints are (or will be) queued; renegotiate.
                    FailureReason::BufferConstraints => {
                        cleanup(&buffer, &pool);
                        continue 'negotiate;
                    }
                    FailureReason::Stopped => {
                        cleanup(&buffer, &pool);
                        return Ok(());
                    }
                    other => {
                        tracing::warn!("frame failed ({other:?}); retrying");
                        frame = start_capture(&session, &buffer, &qh);
                    }
                }
            }
            if state.frame_ready {
                state.frame_ready = false;

                let out = Frame {
                    width,
                    height,
                    format: frame_format,
                    data: mmap.to_vec(),
                };
                if frame_tx.blocking_send(out).is_err() {
                    tracing::debug!("consumer dropped, ending capture");
                    frame.destroy();
                    cleanup(&buffer, &pool);
                    return Ok(());
                }

                // Cap the capture rate: the compositor would happily hand us
                // 120 fps of changes; e-ink has no use for it. The interval is
                // re-read here so a runtime mode switch takes effect at once.
                let elapsed = last_send.elapsed();
                let interval = frame_interval();
                if elapsed < interval {
                    std::thread::sleep(interval - elapsed);
                }
                last_send = Instant::now();

                frame.destroy();
                frame = start_capture(&session, &buffer, &qh);
            }
            queue.blocking_dispatch(&mut state)?;
        }
    }
}

fn start_capture(
    session: &ExtImageCopyCaptureSessionV1,
    buffer: &wl_buffer::WlBuffer,
    qh: &QueueHandle<CaptureState>,
) -> ExtImageCopyCaptureFrameV1 {
    let frame = session.create_frame(qh, ());
    frame.attach_buffer(buffer);
    frame.capture();
    frame
}

fn cleanup(buffer: &wl_buffer::WlBuffer, pool: &wl_shm_pool::WlShmPool) {
    buffer.destroy();
    pool.destroy();
}

/// Prefer formats we can convert cheaply; 4-byte little-endian layouts only.
fn pick_shm_format(offered: &[wl_shm::Format]) -> anyhow::Result<wl_shm::Format> {
    const PREFERRED: [wl_shm::Format; 4] = [
        wl_shm::Format::Xrgb8888,
        wl_shm::Format::Argb8888,
        wl_shm::Format::Xbgr8888,
        wl_shm::Format::Abgr8888,
    ];
    PREFERRED
        .into_iter()
        .find(|f| offered.contains(f))
        .with_context(|| format!("no supported shm format offered (got {offered:?})"))
}

fn frame_format_for(shm: wl_shm::Format) -> PixelFormat {
    match shm {
        // xrgb8888 on little-endian = B,G,R,X in memory.
        wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => PixelFormat::Bgrx8888,
        _ => PixelFormat::Rgbx8888,
    }
}

// --- event handlers: record into CaptureState, never act ---

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for CaptureState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Global add/remove after startup (e.g. output hotplug) is ignored;
        // the session's `stopped` event covers our output disappearing.
    }
}

impl Dispatch<wl_output::WlOutput, usize> for CaptureState {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        &idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            state.output_names[idx] = Some(name);
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        match event {
            Event::BufferSize { width, height } => {
                state.buffer_size = Some((width, height));
            }
            Event::ShmFormat { format: WEnum::Value(f) } => state.shm_formats.push(f),
            Event::Done => state.constraints_generation += 1,
            Event::Stopped => state.session_stopped = true,
            _ => {} // dmabuf constraints and unknown formats are unused
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        match event {
            Event::Ready => state.frame_ready = true,
            Event::Failed { reason } => {
                state.frame_failed = Some(match reason {
                    WEnum::Value(r) => r,
                    WEnum::Unknown(_) => FailureReason::Unknown,
                });
            }
            Event::Transform { transform: WEnum::Value(t) }
                if t != wl_output::Transform::Normal =>
            {
                tracing::warn!(
                    "output transform {t:?} is not handled; captured pixels may be oriented incorrectly"
                );
            }
            _ => {} // damage, presentation time, and normal transforms are unused
        }
    }
}

// Interfaces whose events we never care about.
wayland_client::delegate_noop!(CaptureState: ignore wl_shm::WlShm);
wayland_client::delegate_noop!(CaptureState: ignore wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(CaptureState: ignore wl_buffer::WlBuffer);
wayland_client::delegate_noop!(CaptureState: ignore ExtImageCaptureSourceV1);
wayland_client::delegate_noop!(CaptureState: ignore ExtOutputImageCaptureSourceManagerV1);
wayland_client::delegate_noop!(CaptureState: ignore ExtImageCopyCaptureManagerV1);
