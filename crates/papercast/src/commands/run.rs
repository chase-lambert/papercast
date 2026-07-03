use std::sync::Arc;

use anyhow::Context;
use clap::{Args, ValueEnum};
// Note: the crate re-exports `events::ServerEvent`, but `VncServer::new`'s
// receiver actually carries `server::ServerEvent` — a different enum.
use rustvncserver::server::ServerEvent;
use rustvncserver::VncServer;
use tracing::{error, info, warn};

#[derive(Args)]
pub struct RunArgs {
    /// Frame source to mirror.
    #[arg(long, value_enum, default_value_t = SourceKind::Test)]
    pub source: SourceKind,

    /// Address to serve VNC on. Loopback by default: the session is
    /// unauthenticated, and the tablet reaches loopback via `adb reverse`.
    /// Use 0.0.0.0:5900 only if you really want LAN exposure.
    #[arg(long, default_value = "127.0.0.1:5900")]
    pub listen: String,

    /// Framebuffer size WIDTHxHEIGHT (test source only; capture sources
    /// take their size from the screen).
    #[arg(long, default_value = "1280x960", value_parser = parse_size)]
    pub size: (u32, u32),

    /// Target frames per second (an upper bound: the wayland source is
    /// damage-driven and only produces frames when the screen changes).
    #[arg(long, default_value_t = 15)]
    pub fps: u32,

    /// Which output (monitor) to capture, by name from `papercast probe`,
    /// e.g. "eDP-1". Default: the first output.
    #[arg(long)]
    pub output: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum SourceKind {
    /// Animated synthetic pattern (no compositor needed).
    Test,
    /// Live screen capture (ext-image-copy-capture-v1).
    Wayland,
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected WIDTHxHEIGHT, got '{s}'"))?;
    let parse = |v: &str| v.trim().parse::<u32>().map_err(|e| format!("bad dimension '{v}': {e}"));
    Ok((parse(w)?, parse(h)?))
}

pub fn run(args: RunArgs) -> anyhow::Result<()> {
    // The runtime is built here rather than with #[tokio::main] so that
    // commands like `probe` stay plain blocking code.
    let runtime = tokio::runtime::Runtime::new().context("failed to start tokio runtime")?;
    runtime.block_on(serve(args))
}

async fn serve(args: RunArgs) -> anyhow::Result<()> {
    let (width, height) = args.size;
    let mut source = match args.source {
        SourceKind::Test => papercast_capture::test_pattern::spawn(width, height, args.fps),
        SourceKind::Wayland => papercast_capture::wayland::spawn(
            papercast_capture::wayland::WaylandConfig {
                output: args.output.clone(),
                max_fps: args.fps,
            },
        )?,
    };

    anyhow::ensure!(
        source.width <= u16::MAX as u32 && source.height <= u16::MAX as u32,
        "framebuffer {}x{} exceeds RFB's u16 coordinate space",
        source.width,
        source.height
    );

    let (server, mut events) = VncServer::new(
        source.width as u16,
        source.height as u16,
        "PaperCast".to_string(),
        None, // no VNC auth: loopback-only by default, adb provides the tunnel
    );
    let server = Arc::new(server);

    // Listener task: accepts clients forever. If the bind itself fails
    // (port taken, bad address) we want the whole process to die loudly,
    // not limp along serving nobody.
    // `mut` because `select!` polls it through `&mut` below.
    let mut listener = {
        let server = Arc::clone(&server);
        let addr = args.listen.clone();
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
        "mirroring {}x{} @ {} fps target — connect a VNC viewer to {}",
        source.width, source.height, args.fps, args.listen
    );

    let mut rgba = Vec::new();
    loop {
        tokio::select! {
            maybe_frame = source.frames.recv() => {
                let Some(frame) = maybe_frame else {
                    warn!("frame source ended");
                    return Ok(());
                };
                tracing::debug!(
                    "frame: {}x{} {:?}, damage rects: {:?}",
                    frame.width,
                    frame.height,
                    frame.format,
                    frame.damage.as_ref().map(Vec::len)
                );
                papercast_core::pixel::frame_to_rgba(&frame, &mut rgba);
                // update_from_slice diffs against the previous framebuffer
                // and marks only the changed bounding region dirty, so
                // connected clients get incremental updates automatically.
                if let Err(e) = server.framebuffer().update_from_slice(&rgba).await {
                    error!("framebuffer update failed: {e}");
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
