//! Runs the e-ink pipeline on its own OS thread, between the capture source
//! and the VNC server: frames in, processed frames out, same `SourceHandle`
//! shape on both sides so the server code doesn't know it's there.
//!
//! A dedicated thread (rather than processing inline in the async loop)
//! keeps CPU-heavy image work off tokio's reactor threads, and gives the
//! pipeline natural backpressure through the bounded channels on both sides.

use std::path::PathBuf;

use papercast_capture::source::{SourceHandle, FRAME_CHANNEL_DEPTH};
use papercast_core::{EinkConfig, Pipeline};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Which processed frame `--save-frame` writes (frame 1 can be a splash of
/// mid-connection garbage; by 10 the screen content has settled).
const SAVE_FRAME_INDEX: u64 = 10;

pub fn spawn(
    mut input: SourceHandle,
    config: EinkConfig,
    save_frame: Option<PathBuf>,
    latency_stamp: bool,
    mut config_rx: tokio::sync::watch::Receiver<EinkConfig>,
) -> SourceHandle {
    let pipeline = Pipeline::new(config);
    let (out_w, out_h) = pipeline.output_size((input.width, input.height));
    let (tx, rx) = mpsc::channel(FRAME_CHANNEL_DEPTH);

    std::thread::Builder::new()
        .name("eink-pipeline".into())
        .spawn(move || {
            let mut pipeline = pipeline;
            let mut save_frame = save_frame;
            let mut frame_no: u64 = 0;
            let mut busy_total = std::time::Duration::ZERO;
            let epoch = std::time::Instant::now();

            while let Some(frame) = input.frames.blocking_recv() {
                // Hot reload: apply a pending config between frames. The
                // output size is fixed at startup (the VNC framebuffer was
                // sized from it), so a target-size edit is refused.
                if config_rx.has_changed().unwrap_or(false) {
                    let mut cfg = config_rx.borrow_and_update().clone();
                    let fixed = pipeline.config().target_size;
                    if cfg.target_size != fixed {
                        warn!("target-size can't change at runtime; keeping {fixed:?}");
                        cfg.target_size = fixed;
                    }
                    pipeline.set_config(cfg);
                    info!("pipeline config reloaded");
                }

                let started = std::time::Instant::now();
                let mut processed = match pipeline.process(&frame) {
                    Ok(p) => p,
                    Err(e) => {
                        error!("pipeline failed: {e}");
                        break;
                    }
                };
                if latency_stamp {
                    let scale = (processed.width as usize / 160).clamp(2, 8);
                    papercast_core::overlay::draw_ms_counter(
                        &mut processed.data,
                        processed.width as usize,
                        epoch.elapsed().as_millis() as u64,
                        scale,
                    );
                }
                busy_total += started.elapsed();
                frame_no += 1;

                if frame_no == SAVE_FRAME_INDEX {
                    if let Some(path) = save_frame.take() {
                        save_png(&processed, &path);
                    }
                }
                if frame_no.is_multiple_of(300) {
                    info!(
                        "pipeline: {frame_no} frames, avg {:.1} ms/frame",
                        busy_total.as_secs_f64() * 1000.0 / frame_no as f64
                    );
                }
                if tx.blocking_send(processed).is_err() {
                    break; // server side gone
                }
            }
            // If we exit with --save-frame never written, say so rather than
            // leaving the user waiting for a file that won't appear.
            if let Some(path) = save_frame {
                warn!("exited before frame {SAVE_FRAME_INDEX}; {} not written", path.display());
            }
        })
        .expect("failed to spawn pipeline thread");

    SourceHandle { width: out_w, height: out_h, frames: rx }
}

fn save_png(frame: &papercast_core::Frame, path: &PathBuf) {
    let img = image::GrayImage::from_raw(frame.width, frame.height, frame.data.clone())
        .expect("frame buffer size matches dimensions");
    match img.save(path) {
        Ok(()) => info!("saved processed frame to {}", path.display()),
        Err(e) => error!("failed to save {}: {e}", path.display()),
    }
}
