//! Runs the e-ink pipeline on its own OS thread, between the capture source
//! and the VNC server: frames in, processed frames out, same `SourceHandle`
//! shape on both sides so the server code doesn't know it's there.
//!
//! A dedicated thread (rather than processing inline in the async loop)
//! keeps CPU-heavy image work off tokio's reactor threads, and gives the
//! pipeline natural backpressure through the bounded channels on both sides.

use std::path::PathBuf;

use papercast_capture::source::{SourceHandle, FRAME_CHANNEL_DEPTH};
use papercast_core::{EinkConfig, Frame, Pipeline};
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::mode::ModeSettings;

/// Which processed frame `--save-frame` writes (frame 1 can be a splash of
/// mid-connection garbage; by 10 the screen content has settled).
const SAVE_FRAME_INDEX: u64 = 10;

/// Sync the pipeline to the latest effective settings. Only `.eink` concerns
/// the pipeline; a target-size edit is refused (the VNC framebuffer was sized
/// from it at startup). `set_config` (which rebuilds derived tables) runs only
/// when the effective config actually differs, so an fps-only mode change is a
/// no-op here. Consumes the watch's "changed" flag.
fn reload_config(pipeline: &mut Pipeline, settings_rx: &mut watch::Receiver<ModeSettings>) {
    let mut cfg = settings_rx.borrow_and_update().eink.clone();
    let fixed = pipeline.config().target_size;
    if cfg.target_size != fixed {
        warn!("target-size can't change at runtime; keeping {fixed:?}");
        cfg.target_size = fixed;
    }
    if *pipeline.config() != cfg {
        pipeline.set_config(cfg);
        info!("pipeline config reloaded");
    }
}

pub fn spawn(
    mut input: SourceHandle,
    config: EinkConfig,
    save_frame: Option<PathBuf>,
    latency_stamp: bool,
    mut settings_rx: tokio::sync::watch::Receiver<ModeSettings>,
) -> SourceHandle {
    let pipeline = Pipeline::new(config);
    let (out_w, out_h) = pipeline.output_size((input.width, input.height));
    let (tx, rx) = mpsc::channel(FRAME_CHANNEL_DEPTH);

    std::thread::Builder::new()
        .name("eink-pipeline".into())
        .spawn(move || {
            // A private current-thread runtime lets this thread wait on *either*
            // a new raw frame or a settings change, instead of blocking on frames
            // alone. That's what makes a mode switch re-render immediately on an
            // idle screen: capture is damage-driven, so with no fresh frame we
            // reprocess the cached last raw frame under the new config rather than
            // leaving the old-settings pixels up until the next damage event. The
            // heavy pipeline work still runs on this dedicated thread (its own
            // runtime), off the main reactor.
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("failed to build pipeline runtime");
            rt.block_on(async move {
                let mut pipeline = pipeline;
                let mut save_frame = save_frame;
                let mut frame_no: u64 = 0;
                let mut busy_total = std::time::Duration::ZERO;
                let epoch = std::time::Instant::now();
                // Last raw (pre-pipeline) frame, kept so a settings change can be
                // reflected without waiting for the source to produce another.
                let mut last_raw: Option<Frame> = None;

                loop {
                    // Prefer draining frames (`biased`) so a steady capture stream
                    // isn't starved by settings notifications; an idle source lets
                    // the settings-change branch fire.
                    let frame = tokio::select! {
                        biased;
                        recv = input.frames.recv() => match recv {
                            Some(f) => {
                                // A change may have landed between frames.
                                if settings_rx.has_changed().unwrap_or(false) {
                                    reload_config(&mut pipeline, &mut settings_rx);
                                }
                                last_raw = Some(f);
                                last_raw.as_ref().unwrap()
                            }
                            None => break, // source gone
                        },
                        res = settings_rx.changed() => {
                            if res.is_err() {
                                break; // settings sender dropped (shutdown)
                            }
                            reload_config(&mut pipeline, &mut settings_rx);
                            // Re-render the cached frame under the new config; if
                            // nothing has been captured yet, there's nothing to show.
                            match last_raw.as_ref() {
                                Some(f) => f,
                                None => continue,
                            }
                        }
                    };

                    let started = std::time::Instant::now();
                    let mut processed = match pipeline.process(frame) {
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
                    if tx.send(processed).await.is_err() {
                        break; // server side gone
                    }
                }
                // If we exit with --save-frame never written, say so rather than
                // leaving the user waiting for a file that won't appear.
                if let Some(path) = save_frame {
                    warn!("exited before frame {SAVE_FRAME_INDEX}; {} not written", path.display());
                }
            });
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

#[cfg(test)]
mod tests {
    use super::*;
    use papercast_capture::source::SourceHandle;
    use papercast_core::PixelFormat;

    fn settings(eink: EinkConfig) -> ModeSettings {
        ModeSettings { eink, fps: 30, tile_size: 32, full_refresh_secs: 0, full_refresh_updates: 0 }
    }

    fn solid(w: u32, h: u32, value: u8) -> Frame {
        Frame {
            width: w,
            height: h,
            format: PixelFormat::Gray8,
            data: vec![value; (w * h) as usize],
            damage: None,
        }
    }

    // The idle-screen fix: after one captured frame the source goes silent, a
    // mode switch changes the eink config, and the pipeline must re-render the
    // *cached* raw frame under the new config without waiting for a new capture.
    #[test]
    fn settings_change_reprocesses_last_raw_frame_while_idle() {
        let (in_tx, in_rx) = mpsc::channel(FRAME_CHANNEL_DEPTH);
        let input = SourceHandle { width: 8, height: 8, frames: in_rx };

        // Base config quantizes to 16 levels; the switch drops to 2 levels, so
        // the same mid-gray raw frame must produce different output pixels.
        let base = EinkConfig { levels: 16, sharpen: 0.0, ..Default::default() };
        let (set_tx, set_rx) = watch::channel(settings(base.clone()));

        let mut out = spawn(input, base.clone(), None, false, set_rx);

        in_tx.blocking_send(solid(8, 8, 128)).unwrap();
        let first = out.frames.blocking_recv().expect("first processed frame");

        // Switch modes; deliberately send NO new capture frame (idle screen).
        let coarse = EinkConfig { levels: 2, ..base };
        set_tx.send(settings(coarse)).unwrap();

        let second = out.frames.blocking_recv().expect("reprocessed frame after switch");
        assert_ne!(
            first.data, second.data,
            "a mode switch on an idle screen should re-render the cached frame"
        );

        drop(in_tx);
        drop(set_tx);
    }

    // A settings change that leaves `.eink` untouched (e.g. an fps-only switch)
    // still re-emits the cached frame, but the pixels are unchanged.
    #[test]
    fn fps_only_change_reemits_identical_pixels() {
        let (in_tx, in_rx) = mpsc::channel(FRAME_CHANNEL_DEPTH);
        let input = SourceHandle { width: 8, height: 8, frames: in_rx };

        let base = EinkConfig { levels: 16, sharpen: 0.0, ..Default::default() };
        let (set_tx, set_rx) = watch::channel(settings(base.clone()));
        let mut out = spawn(input, base.clone(), None, false, set_rx);

        in_tx.blocking_send(solid(8, 8, 128)).unwrap();
        let first = out.frames.blocking_recv().expect("first processed frame");

        let mut fps_only = settings(base);
        fps_only.fps = 5;
        set_tx.send(fps_only).unwrap();

        let second = out.frames.blocking_recv().expect("reprocessed frame after fps switch");
        assert_eq!(first.data, second.data, "fps-only change must not alter pixels");

        drop(in_tx);
        drop(set_tx);
    }
}
