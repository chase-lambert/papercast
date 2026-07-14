//! VNC output: a push-based framebuffer server for standard viewer apps.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use papercast_core::tiles::TileDiff;
use papercast_core::{Frame, Rect};
// The crate re-exports `events::ServerEvent`, but `VncServer::new` returns
// `server::ServerEvent` on its receiver.
use rustvncserver::server::ServerEvent;
use rustvncserver::VncServer;
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::mode::{ModeSettings, ModeState};

pub struct Config {
    pub listen: String,
    pub framebuffer: (u16, u16),
    pub raw: bool,
    pub effective: ModeSettings,
    pub mode_state: Arc<Mutex<ModeState>>,
}

pub async fn serve(
    cfg: Config,
    mut frames: mpsc::Receiver<Frame>,
    mut settings_rx: watch::Receiver<ModeSettings>,
    mut refresh_rx: mpsc::Receiver<()>,
) -> anyhow::Result<()> {
    let (fb_w, fb_h) = cfg.framebuffer;
    let (server, mut events) = VncServer::new(
        fb_w,
        fb_h,
        "PaperCast".to_string(),
        None, // unauthenticated; loopback plus adb reverse is the safe default
    );
    let server = Arc::new(server);

    let mut listener = {
        let server = Arc::clone(&server);
        let addr = cfg.listen.clone();
        tokio::spawn(async move {
            info!("VNC server listening on {addr}");
            server.listen(addr.as_str()).await
        })
    };

    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                ServerEvent::ClientConnected { client_id } => {
                    info!("client #{client_id} connected");
                }
                ServerEvent::ClientDisconnected { client_id } => {
                    info!("client #{client_id} disconnected");
                }
                _ => {} // PaperCast is a view-only server.
            }
        }
    });

    let active_mode = cfg
        .mode_state
        .lock()
        .expect("mode state poisoned")
        .active()
        .map(|m| format!("mode: {m}"))
        .unwrap_or_else(|| "no mode".into());
    info!(
        "mirroring {fb_w}x{fb_h} @ {} fps ({active_mode}) — connect a VNC viewer to {}",
        cfg.effective.fps, cfg.listen,
    );

    let mut current = cfg.effective;
    let mut tiler = TileDiff::new(current.tile_size, 8);
    let mut rgba = Vec::new();
    let mut updates_since_full: u64 = 0;
    let mut last_full = Instant::now();

    loop {
        tokio::select! {
            Ok(()) = settings_rx.changed() => {
                let next = settings_rx.borrow_and_update().clone();
                if next.tile_size != current.tile_size {
                    tiler = TileDiff::new(next.tile_size, 8);
                } else if next != current {
                    server
                        .framebuffer()
                        .mark_dirty_region(0, 0, fb_w, fb_h)
                        .await;
                }
                last_full = Instant::now();
                updates_since_full = 0;
                current = next;
            }
            Some(()) = refresh_rx.recv() => {
                server.framebuffer().mark_dirty_region(0, 0, fb_w, fb_h).await;
                last_full = Instant::now();
                updates_since_full = 0;
                tracing::debug!("forced full refresh (ctl)");
            }
            maybe_frame = frames.recv() => {
                let Some(frame) = maybe_frame else {
                    warn!("frame source ended");
                    return Ok(());
                };

                if cfg.raw {
                    papercast_core::pixel::frame_to_rgba(&frame, &mut rgba);
                    if let Err(e) = server.framebuffer().update_from_slice(&rgba).await {
                        error!("framebuffer update failed: {e}");
                    }
                    continue;
                }

                let rects = tiler.diff(&frame.data, (frame.width, frame.height));
                for rect in &rects {
                    extract_rect_rgba(&frame.data, u32::from(fb_w), *rect, &mut rgba);
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

                let due_time = current.full_refresh_secs > 0
                    && last_full.elapsed().as_secs() >= current.full_refresh_secs;
                let due_count = current.full_refresh_updates > 0
                    && updates_since_full >= current.full_refresh_updates;
                if due_time || due_count {
                    server.framebuffer().mark_dirty_region(0, 0, fb_w, fb_h).await;
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
