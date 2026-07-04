use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::{Args, ValueEnum};
use papercast_core::dither::DitherMode;
use papercast_core::scale::FitMode;
use papercast_core::tiles::TileDiff;
use papercast_core::{EinkConfig, Rect};

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

    /// Display mode: reading | browsing | writing | video (or a custom mode
    /// from config). Omit for plain base config (current behavior). Runtime
    /// switching (`papercast ctl`) is planned and will require a mode active
    /// at startup.
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
    let settings = resolve(&args)?;
    let effective = settings.mode.effective();
    let mode_active = settings.mode.active().is_some();

    // Capture rate (design rule): with a mode active, run the source at the
    // max fps any mode may switch to, and let the serve loop drop frames down
    // to the effective rate. With no mode active, keep the configured fps
    // exactly (preserves pre-modes behavior; no serve-loop dropping).
    let capture_fps =
        if mode_active { settings.mode.max_fps() } else { effective.fps };

    let captured = match args.source {
        SourceKind::Test => {
            let (w, h) = args.size;
            papercast_capture::test_pattern::spawn(w, h, capture_fps)
        }
        SourceKind::Wayland => {
            papercast_capture::wayland::spawn(papercast_capture::wayland::WaylandConfig {
                output: settings.output.clone(),
                max_fps: capture_fps,
            })?
        }
    };

    // Hot-reload channel: the notify watcher pushes new configs, the
    // pipeline thread applies them between frames.
    let (eink_tx, eink_rx) = tokio::sync::watch::channel(effective.eink.clone());
    let mut source = if args.raw {
        captured
    } else {
        crate::pipeline_thread::spawn(
            captured,
            effective.eink.clone(),
            args.save_frame.clone(),
            args.latency_test,
            eink_rx,
        )
    };
    if let (Some(path), false) = (&args.config, args.raw) {
        // The watcher owns a clone of the mode state so a config edit updates
        // the *base* eink and re-applies the active mode's overlay — editing
        // the file never drops the active mode.
        spawn_config_watcher(path.clone(), settings.mode.clone(), eink_tx);
    }

    anyhow::ensure!(
        source.width <= u16::MAX as u32 && source.height <= u16::MAX as u32,
        "framebuffer {}x{} exceeds RFB's u16 coordinate space",
        source.width,
        source.height
    );
    let (fb_w, fb_h) = (source.width, source.height);

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
        let addr = settings.listen.clone();
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

    info!(
        "mirroring {}x{} @ {} fps cap ({}) — connect a VNC viewer to {}",
        fb_w,
        fb_h,
        effective.fps,
        settings.mode.active().map(|m| format!("mode: {m}")).unwrap_or_else(|| "no mode".into()),
        settings.listen
    );

    let mut tiler = TileDiff::new(effective.tile_size, 8);
    let mut rgba = Vec::new();
    let mut updates_since_full: u64 = 0;
    let mut last_full = Instant::now();

    // Serve-loop pacing: only when a mode is active (see capture_fps above).
    // Drop a frame if it arrives sooner than the effective mode's interval.
    let frame_interval = Duration::from_secs_f64(1.0 / effective.fps.max(1) as f64);
    let mut last_sent = Instant::now() - frame_interval;

    loop {
        tokio::select! {
            maybe_frame = source.frames.recv() => {
                let Some(frame) = maybe_frame else {
                    warn!("frame source ended");
                    return Ok(());
                };

                if mode_active && last_sent.elapsed() < frame_interval {
                    continue; // pace down to the effective mode fps
                }
                last_sent = Instant::now();

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
                let due_time = effective.full_refresh_secs > 0
                    && last_full.elapsed().as_secs() >= effective.full_refresh_secs;
                let due_count = effective.full_refresh_updates > 0
                    && updates_since_full >= effective.full_refresh_updates;
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
        }
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

/// Watch the config file's directory and push `[eink]` changes to the
/// pipeline. Directory (not file) watching survives editors that replace
/// the file on save; parse errors are logged and skipped.
///
/// The reloaded `[eink]` is the *base* config: it's fed through the mode
/// state so the active mode's overlay is re-applied before the pipeline sees
/// it. A config edit therefore never drops the active mode.
fn spawn_config_watcher(
    path: PathBuf,
    mut state: ModeState,
    tx: tokio::sync::watch::Sender<EinkConfig>,
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

            let mut last_sent = state.effective().eink;
            for event in raw_rx {
                let Ok(event) = event else { continue };
                let ours = event.paths.iter().any(|p| p.file_name() == path.file_name());
                if !ours || !(event.kind.is_modify() || event.kind.is_create()) {
                    continue;
                }
                // Editors write in bursts (truncate+write, or tmp+rename);
                // give the file a moment to be complete.
                std::thread::sleep(std::time::Duration::from_millis(50));
                match config::reload_eink(&path) {
                    Ok(base_eink) if base_eink != *state.base_eink() => {
                        info!("config changed, applying [eink] update");
                        state.set_base_eink(base_eink);
                        let effective_eink = state.effective().eink;
                        if effective_eink != last_sent {
                            last_sent = effective_eink.clone();
                            if tx.send(effective_eink).is_err() {
                                return; // pipeline gone
                            }
                        }
                    }
                    Ok(_) => {} // no effective change
                    Err(e) => warn!("config reload skipped: {e:#}"),
                }
            }
        })
        .expect("failed to spawn config watcher");
}
