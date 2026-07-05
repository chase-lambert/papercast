use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Context;
use clap::{Args, ValueEnum};
use papercast_core::dither::DitherMode;
use papercast_core::scale::FitMode;
use papercast_core::tiles::TileDiff;
use papercast_core::Rect;

use crate::control;
use crate::mode::{ModeSettings, ModeState};
// Note: the crate re-exports `events::ServerEvent`, but `VncServer::new`'s
// receiver actually carries `server::ServerEvent` — a different enum.
use rustvncserver::server::ServerEvent;
use rustvncserver::VncServer;
use tracing::{error, info, warn};

use crate::config;

/// Tunable args are `Option<T>` so we can tell "user passed it" from "left
/// default": precedence is defaults < config file < explicit CLI flag.
#[derive(Args)]
pub struct RunArgs {
    /// Frame source to mirror.
    #[arg(long, value_enum, default_value_t = SourceKind::Wayland)]
    pub source: SourceKind,

    /// Wire transport [default: vnc]. `vnc` serves any VNC viewer; `papercast`
    /// serves the custom protocol (TCP 127.0.0.1:5920 by default, override with
    /// --listen) for the native receiver — requires the e-ink pipeline (not --raw).
    #[arg(long, value_enum, default_value_t = TransportArg::Vnc)]
    pub transport: TransportArg,

    /// TOML config file; its [eink] section hot-reloads on save.
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Address to serve VNC on [default: 127.0.0.1:5900]. Loopback because
    /// the session is unauthenticated; the tablet reaches loopback via
    /// `adb reverse`. Use 0.0.0.0:5900 only if you want LAN exposure.
    #[arg(long)]
    pub listen: Option<String>,

    /// Framebuffer size WIDTHxHEIGHT (test source only).
    #[arg(long, default_value = "1280x960", value_parser = parse_size)]
    pub size: (u32, u32),

    /// Frame rate cap [default: 15]. The wayland source is damage-driven:
    /// an idle screen produces no frames at all.
    #[arg(long)]
    pub fps: Option<u32>,

    /// Output (monitor) to capture, by name from `papercast probe`.
    #[arg(long)]
    pub output: Option<String>,

    /// Startup display mode: reading | browsing | writing | video (or a custom
    /// mode from config). Omit for plain base config. Switch at runtime with
    /// `papercast ctl mode <name>` — that works whether or not a startup mode
    /// was set.
    #[arg(long)]
    pub mode: Option<String>,

    /// Skip the e-ink pipeline and mirror raw color frames.
    #[arg(long)]
    pub raw: bool,

    /// Scale output to WIDTHxHEIGHT (e.g. the tablet's 3200x2400).
    #[arg(long, value_parser = parse_size)]
    pub scale_to: Option<(u32, u32)>,

    /// Aspect-ratio handling when --scale-to changes the shape
    /// [default: letterbox].
    #[arg(long, value_enum)]
    pub fit: Option<FitArg>,

    /// Contrast multiplier (1.0 = unchanged) [default: 1.2].
    #[arg(long)]
    pub contrast: Option<f32>,

    /// Gamma exponent (<1 brightens midtones) [default: 1.0].
    #[arg(long)]
    pub gamma: Option<f32>,

    /// Unsharp-mask strength (0 = off) [default: 1.0].
    #[arg(long)]
    pub sharpen: Option<f32>,

    /// Invert luminance (dark desktop themes → black-on-paper).
    #[arg(long)]
    pub invert: bool,

    /// Dithering algorithm [default: bayer].
    #[arg(long, value_enum)]
    pub dither: Option<DitherArg>,

    /// Gray levels to quantize to [default: 16].
    #[arg(long)]
    pub levels: Option<u8>,

    /// Dirty-diff tile size in pixels [default: 64].
    #[arg(long)]
    pub tile_size: Option<u32>,

    /// Force a full-frame refresh every N seconds (clears e-ink ghosting);
    /// 0 disables [default: 60].
    #[arg(long)]
    pub full_refresh_secs: Option<u64>,

    /// Force a full-frame refresh after N incremental updates; 0 disables
    /// [default: 0].
    #[arg(long)]
    pub full_refresh_updates: Option<u64>,

    /// Stamp a millisecond counter onto every frame: point a camera at both
    /// screens (or eyeball a side-by-side viewer) to measure latency.
    #[arg(long)]
    pub latency_test: bool,

    /// Write processed frame #10 to this PNG for pipeline inspection.
    #[arg(long)]
    pub save_frame: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum SourceKind {
    /// Animated synthetic pattern (no compositor needed).
    Test,
    /// Live screen capture (ext-image-copy-capture-v1).
    Wayland,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TransportArg {
    /// Serve any VNC viewer (RFB over the --listen address).
    Vnc,
    /// Serve the native receiver over the papercast custom protocol.
    Papercast,
}

// CLI mirrors of the core enums: papercast-core stays clap-free.
#[derive(Clone, Copy, ValueEnum)]
pub enum FitArg {
    Letterbox,
    Crop,
    Stretch,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum DitherArg {
    None,
    Bayer,
    Fs,
    Atkinson,
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected WIDTHxHEIGHT, got '{s}'"))?;
    let parse =
        |v: &str| v.trim().parse::<u32>().map_err(|e| format!("bad dimension '{v}': {e}"));
    Ok((parse(w)?, parse(h)?))
}

/// Everything `serve` needs, after merging defaults, file, and CLI. The
/// mode-switchable settings (eink, fps, tile, refresh) live inside `mode` as
/// its *base*; `listen`/`output` are fixed for the process lifetime.
struct Settings {
    listen: String,
    output: Option<String>,
    mode: ModeState,
}

fn resolve(args: &RunArgs) -> anyhow::Result<Settings> {
    let file = match &args.config {
        Some(path) => config::load(path)?,
        None => config::FileConfig::default(),
    };

    let mut eink = file.eink;
    if let Some(v) = args.contrast {
        eink.contrast = v;
    }
    if let Some(v) = args.gamma {
        eink.gamma = v;
    }
    if let Some(v) = args.sharpen {
        eink.sharpen = v;
    }
    if args.invert {
        eink.invert = true;
    }
    if let Some(v) = args.levels {
        eink.levels = v;
    }
    if let Some(v) = args.scale_to {
        eink.target_size = Some(v);
    }
    if let Some(v) = args.dither {
        eink.dither = match v {
            DitherArg::None => DitherMode::None,
            DitherArg::Bayer => DitherMode::Bayer,
            DitherArg::Fs => DitherMode::FloydSteinberg,
            DitherArg::Atkinson => DitherMode::Atkinson,
        };
    }
    if let Some(v) = args.fit {
        eink.fit = match v {
            FitArg::Letterbox => FitMode::Letterbox,
            FitArg::Crop => FitMode::Crop,
            FitArg::Stretch => FitMode::Stretch,
        };
    }

    let m = file.mirror;
    // Base settings = defaults < config < CLI, before any mode overlay.
    let base = ModeSettings {
        eink,
        fps: args.fps.or(m.fps).unwrap_or(15),
        tile_size: args.tile_size.or(m.tile_size).unwrap_or(64),
        full_refresh_secs: args.full_refresh_secs.or(m.full_refresh_secs).unwrap_or(60),
        full_refresh_updates: args.full_refresh_updates.or(m.full_refresh_updates).unwrap_or(0),
    };
    let active = args.mode.clone().or(m.mode);
    let mode = ModeState::new(base, file.modes, active)?;

    Ok(Settings {
        listen: args
            .listen
            .clone()
            .or(m.listen)
            .unwrap_or_else(|| "127.0.0.1:5900".into()),
        output: args.output.clone().or(m.output),
        mode,
    })
}

pub fn run(args: RunArgs) -> anyhow::Result<()> {
    // The runtime is built here rather than with #[tokio::main] so that
    // commands like `probe` stay plain blocking code.
    let runtime = tokio::runtime::Runtime::new().context("failed to start tokio runtime")?;
    runtime.block_on(serve(args))
}

async fn serve(args: RunArgs) -> anyhow::Result<()> {
    let Settings { listen, output, mode } = resolve(&args)?;
    let effective = mode.effective();
    // One shared mode state, mutated by both the config watcher (base reload)
    // and the ctl server (set_mode) — never cloned, so the two can't diverge.
    let mode_state = Arc::new(Mutex::new(mode));

    // Two watch channels carry runtime settings:
    //  - `settings_tx`: the full effective ModeSettings — pipeline reads
    //    `.eink`, the serve loop reads tile/refresh.
    //  - `fps_tx`: fps only, decoupled so the capture crate needn't know the
    //    binary's ModeSettings type. The source paces itself off this, so a
    //    runtime mode switch re-paces at the source — no serve-loop dropping,
    //    no wasted pipeline work, no fixed-fps-at-startup constraint.
    let (settings_tx, mut settings_rx) = tokio::sync::watch::channel(effective.clone());
    let (fps_tx, fps_rx) = tokio::sync::watch::channel(effective.fps);
    // `ctl refresh` → force one full redraw from the serve loop.
    let (refresh_tx, mut refresh_rx) = tokio::sync::mpsc::channel::<()>(8);

    let captured = match args.source {
        SourceKind::Test => {
            let (w, h) = args.size;
            papercast_capture::test_pattern::spawn(w, h, effective.fps, Some(fps_rx))
        }
        SourceKind::Wayland => {
            papercast_capture::wayland::spawn(papercast_capture::wayland::WaylandConfig {
                output: output.clone(),
                max_fps: effective.fps,
                fps_rx: Some(fps_rx),
            })?
        }
    };

    let mut source = if args.raw {
        captured
    } else {
        crate::pipeline_thread::spawn(
            captured,
            effective.eink.clone(),
            args.save_frame.clone(),
            args.latency_test,
            settings_rx.clone(),
        )
    };
    if let (Some(path), false) = (&args.config, args.raw) {
        // The watcher mutates the shared mode state: a config edit updates the
        // *base* eink and re-applies the active mode's overlay, so editing the
        // file never drops the active mode.
        spawn_config_watcher(
            path.clone(),
            Arc::clone(&mode_state),
            settings_tx.clone(),
            fps_tx.clone(),
        );
    }

    anyhow::ensure!(
        source.width <= u16::MAX as u32 && source.height <= u16::MAX as u32,
        "framebuffer {}x{} exceeds RFB's u16 coordinate space",
        source.width,
        source.height
    );
    let (fb_w, fb_h) = (source.width, source.height);

    // Control socket: `papercast ctl` drives this running mirror regardless of
    // transport. Held for the process lifetime; the guard unlinks the socket on
    // clean exit. Set up before the transport branch so both paths get it.
    let _socket_guard = control::spawn_server(control::ServerCtx {
        state: Arc::clone(&mode_state),
        settings_tx: settings_tx.clone(),
        fps_tx: fps_tx.clone(),
        refresh_tx: refresh_tx.clone(),
        framebuffer: (fb_w, fb_h),
        output: output.clone(),
    })?;

    // Papercast custom transport: it shares all of the setup above and differs
    // only in the output half, so it's handled as an early return — the VNC path
    // below stays exactly as it was (it's the M9 baseline and the M11 fallback).
    if args.transport == TransportArg::Papercast {
        anyhow::ensure!(
            !args.raw,
            "--transport papercast serves the e-ink pipeline; --raw is not supported yet"
        );
        let addr = args.listen.clone().unwrap_or_else(|| "127.0.0.1:5920".into());
        let tcp = tokio::net::TcpListener::bind(&addr)
            .await
            .with_context(|| format!("binding papercast transport on {addr}"))?;
        info!(
            "papercast transport on {addr} ({fb_w}x{fb_h}, {} fps) — bridge with `adb reverse tcp:5920 tcp:5920`",
            effective.fps,
        );
        return tokio::select! {
            r = crate::transport::serve_proto(
                tcp,
                crate::transport::ProtoConfig {
                    framebuffer: (fb_w as u16, fb_h as u16),
                    mode_state: Arc::clone(&mode_state),
                },
                source.frames,
                settings_rx,
                refresh_rx,
            ) => r,
            _ = shutdown_signal() => {
                info!("shutting down");
                Ok(())
            }
        };
    }

    let (server, mut events) = VncServer::new(
        fb_w as u16,
        fb_h as u16,
        "PaperCast".to_string(),
        None, // no VNC auth: loopback-only by default, adb provides the tunnel
    );
    let server = Arc::new(server);

    // Listener task: accepts clients forever. If the bind itself fails
    // (port taken, bad address) we want the whole process to die loudly,
    // not limp along serving nobody. `mut`: select! polls it via &mut.
    let mut listener = {
        let server = Arc::clone(&server);
        let addr = listen.clone();
        tokio::spawn(async move {
            info!("VNC server listening on {addr}");
            server.listen(addr.as_str()).await
        })
    };

    // Event drain: Phase 0 is view-only, so pointer/key events are ignored;
    // connects/disconnects are logged so you can see the tablet arrive.
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                ServerEvent::ClientConnected { client_id } => {
                    info!("client #{client_id} connected");
                }
                ServerEvent::ClientDisconnected { client_id } => {
                    info!("client #{client_id} disconnected");
                }
                _ => {} // input/clipboard: ignored in Phase 0
            }
        }
    });

    let active_mode = mode_state
        .lock()
        .expect("mode state poisoned")
        .active()
        .map(|m| format!("mode: {m}"))
        .unwrap_or_else(|| "no mode".into());
    info!(
        "mirroring {fb_w}x{fb_h} @ {} fps ({active_mode}) — connect a VNC viewer to {listen}",
        effective.fps,
    );

    // Serve-loop view of the effective settings, refreshed from the watch
    // channel each iteration so a runtime mode switch changes tile/refresh
    // live. The source paces itself (via `fps_tx`); the serve loop no longer
    // drops frames.
    let mut current = effective.clone();
    let mut tiler = TileDiff::new(current.tile_size, 8);
    let mut rgba = Vec::new();
    let mut updates_since_full: u64 = 0;
    let mut last_full = Instant::now();

    loop {
        tokio::select! {
            // A mode switch or config edit changed the effective settings.
            // Handled in its own arm so it applies even on an idle screen (no
            // frames arriving). Rebuild the tiler on a tile-size change (its
            // full-frame first diff doubles as the switch redraw); otherwise
            // force one full refresh since levels/dither changed globally.
            //
            // Known limitation (Phase 1 backlog): on a *fully idle* screen this
            // redraw resends the framebuffer's current pixels, which were
            // processed under the *old* levels/dither — the new look only
            // appears once the next damage-driven frame flows through the
            // pipeline (any cursor movement triggers it). The fix is for the
            // pipeline to cache the last raw frame and reprocess it on a
            // settings change; deferred until it's worth the memory.
            Ok(()) = settings_rx.changed() => {
                let next = settings_rx.borrow_and_update().clone();
                if next.tile_size != current.tile_size {
                    tiler = TileDiff::new(next.tile_size, 8);
                } else if next != current {
                    server
                        .framebuffer()
                        .mark_dirty_region(0, 0, fb_w as u16, fb_h as u16)
                        .await;
                }
                last_full = Instant::now();
                updates_since_full = 0;
                current = next;
            }
            // `ctl refresh`: force one full redraw now.
            Some(()) = refresh_rx.recv() => {
                server
                    .framebuffer()
                    .mark_dirty_region(0, 0, fb_w as u16, fb_h as u16)
                    .await;
                last_full = Instant::now();
                updates_since_full = 0;
                tracing::debug!("forced full refresh (ctl)");
            }
            maybe_frame = source.frames.recv() => {
                let Some(frame) = maybe_frame else {
                    warn!("frame source ended");
                    return Ok(());
                };

                if args.raw {
                    // Raw color path: let the server's own full-frame diff
                    // find the changed bounding box.
                    papercast_core::pixel::frame_to_rgba(&frame, &mut rgba);
                    if let Err(e) = server.framebuffer().update_from_slice(&rgba).await {
                        error!("framebuffer update failed: {e}");
                    }
                    continue;
                }

                // Processed gray path: send exactly the tiles that changed.
                let rects = tiler.diff(&frame.data, (frame.width, frame.height));
                for rect in &rects {
                    extract_rect_rgba(&frame.data, fb_w, *rect, &mut rgba);
                    if let Err(e) = server
                        .framebuffer()
                        .update_cropped(
                            &rgba,
                            rect.x as u16,
                            rect.y as u16,
                            rect.width as u16,
                            rect.height as u16,
                        )
                        .await
                    {
                        error!("cropped update failed: {e}");
                    }
                }
                if !rects.is_empty() {
                    updates_since_full += 1;
                }

                // Ghost-clearing: periodically force clients to redraw the
                // whole frame even though pixel data didn't change. On the
                // tablet this gives the EPD a chance to do a clean settle.
                let due_time = current.full_refresh_secs > 0
                    && last_full.elapsed().as_secs() >= current.full_refresh_secs;
                let due_count = current.full_refresh_updates > 0
                    && updates_since_full >= current.full_refresh_updates;
                if due_time || due_count {
                    server
                        .framebuffer()
                        .mark_dirty_region(0, 0, fb_w as u16, fb_h as u16)
                        .await;
                    last_full = Instant::now();
                    updates_since_full = 0;
                    tracing::debug!("forced full refresh");
                }
            }
            listen_result = &mut listener => {
                match listen_result {
                    Ok(Err(e)) => anyhow::bail!("VNC listener failed: {e}"),
                    Ok(Ok(())) => anyhow::bail!("VNC listener exited unexpectedly"),
                    Err(e) => anyhow::bail!("VNC listener task panicked: {e}"),
                }
            }
            // Clean shutdown so the socket guard runs and unlinks the socket.
            _ = shutdown_signal() => {
                info!("shutting down");
                return Ok(());
            }
        }
    }
}

/// Resolve when the process is asked to stop (Ctrl-C / SIGTERM).
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            warn!("cannot install SIGTERM handler: {e}");
            // Fall back to Ctrl-C only.
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

/// Copy a sub-rectangle of a gray frame into a contiguous RGBA buffer
/// (the layout `update_cropped` expects).
fn extract_rect_rgba(gray: &[u8], stride: u32, rect: Rect, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(rect.width as usize * rect.height as usize * 4);
    for y in rect.y..rect.y + rect.height {
        let row_start = (y * stride + rect.x) as usize;
        for &g in &gray[row_start..row_start + rect.width as usize] {
            out.extend_from_slice(&[g, g, g, 255]);
        }
    }
}

/// Watch the config file's directory and push `[eink]` changes through the
/// mode state to the effective-settings channels. Directory (not file)
/// watching survives editors that replace the file on save; parse errors are
/// logged and skipped.
///
/// The reloaded `[eink]` is the *base* config: it's fed through the mode
/// state so the active mode's overlay is re-applied before the pipeline sees
/// it. A config edit therefore never drops the active mode.
fn spawn_config_watcher(
    path: PathBuf,
    state: Arc<Mutex<ModeState>>,
    settings_tx: tokio::sync::watch::Sender<ModeSettings>,
    fps_tx: tokio::sync::watch::Sender<u32>,
) {
    use notify::{RecursiveMode, Watcher};

    std::thread::Builder::new()
        .name("config-watch".into())
        .spawn(move || {
            let dir = path.parent().map(PathBuf::from).unwrap_or_else(|| ".".into());
            let (raw_tx, raw_rx) =
                std::sync::mpsc::channel::<Result<notify::Event, notify::Error>>();
            let mut watcher = match notify::recommended_watcher(raw_tx) {
                Ok(w) => w,
                Err(e) => {
                    warn!("config watcher unavailable: {e}");
                    return;
                }
            };
            if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
                warn!("cannot watch {}: {e}", dir.display());
                return;
            }
            info!("hot-reload active: edit {} while running", path.display());

            for event in raw_rx {
                let Ok(event) = event else { continue };
                let ours = event.paths.iter().any(|p| p.file_name() == path.file_name());
                if !ours || !(event.kind.is_modify() || event.kind.is_create()) {
                    continue;
                }
                // Editors write in bursts (truncate+write, or tmp+rename);
                // give the file a moment to be complete.
                std::thread::sleep(std::time::Duration::from_millis(50));
                let base_eink = match config::reload_eink(&path) {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        warn!("config reload skipped: {e:#}");
                        continue;
                    }
                };
                // Recompute the effective settings and broadcast them — all
                // under the state lock, so a concurrent `ctl mode` switch can't
                // interleave with us and leave a channel carrying stale
                // settings. The reloaded `[eink]` is the *base*; `effective()`
                // re-applies the active mode's overlay, so the mode is never
                // dropped. Compare against what the channels currently carry
                // (not a private snapshot — a `ctl mode` switch would leave that
                // stale) so an overlay-masked or already-current change is a
                // no-op. `watch::Sender::send` is sync (no await), safe here.
                let mut st = state.lock().expect("mode state poisoned");
                if base_eink == *st.base_eink() {
                    continue; // base config unchanged
                }
                st.set_base_eink(base_eink);
                let effective = st.effective();
                if effective == *settings_tx.borrow() {
                    continue; // overlay masked the change, or already current
                }
                info!("config changed, applying [eink] update");
                if effective.fps != *fps_tx.borrow() && fps_tx.send(effective.fps).is_err() {
                    return; // capture gone
                }
                if settings_tx.send(effective).is_err() {
                    return; // pipeline/serve gone
                }
            }
        })
        .expect("failed to spawn config watcher");
}
