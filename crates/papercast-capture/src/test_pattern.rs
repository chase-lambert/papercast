//! Synthetic animated test source — lets the whole VNC path be exercised with
//! no compositor and no tablet. Deliberately built to stress what e-ink cares
//! about and what the M0b checklist demands:
//!
//! - static fine detail (1-px line gratings ≈ small text) to judge crispness
//! - a bouncing box → small sub-rect updates every frame
//! - a live millisecond seven-segment counter → localized churn + latency probe
//! - a full-frame inversion flash every 10 s → forces full-framebuffer updates

use std::time::{Duration, Instant};

use papercast_core::overlay::{draw_ms_counter, fill_rect};
use papercast_core::{Frame, PixelFormat};
use tokio::sync::{mpsc, watch};

use crate::source::{SourceHandle, FRAME_CHANNEL_DEPTH};

/// Spawn the test source onto the current tokio runtime. `fps_rx`, if given,
/// re-paces the source live (mode switches); otherwise `fps` is fixed.
pub fn spawn(
    width: u32,
    height: u32,
    fps: u32,
    fps_rx: Option<watch::Receiver<u32>>,
) -> SourceHandle {
    let (tx, rx) = mpsc::channel(FRAME_CHANNEL_DEPTH);
    tokio::spawn(produce(width, height, fps, fps_rx, tx));
    SourceHandle { width, height, frames: rx }
}

async fn produce(
    width: u32,
    height: u32,
    fps: u32,
    fps_rx: Option<watch::Receiver<u32>>,
    tx: mpsc::Sender<Frame>,
) {
    let background = render_background(width, height);
    let start = Instant::now();
    let mut last_send = Instant::now();

    loop {
        // Re-read the target fps each tick so a runtime mode switch re-paces
        // immediately (`borrow()` is a cheap non-blocking read), and sleep only
        // the *remainder* of the interval so render/send time doesn't stack on
        // top of the sleep — otherwise the effective fps undershoots.
        let target_fps = fps_rx.as_ref().map(|r| *r.borrow()).unwrap_or(fps).max(1);
        let interval = Duration::from_secs_f64(1.0 / f64::from(target_fps));
        let elapsed = last_send.elapsed();
        if elapsed < interval {
            tokio::time::sleep(interval - elapsed).await;
        }
        last_send = Instant::now();

        let frame = render(&background, width, height, start.elapsed());
        // send() fails only when the receiver is dropped == consumer gone.
        if tx.send(frame).await.is_err() {
            tracing::debug!("test source: consumer dropped, stopping");
            return;
        }
    }
}

/// Static parts, computed once: gradient, checkerboards, line gratings.
fn render_background(width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut px = vec![255u8; w * h];
    let band = h / 4;

    for y in 0..h {
        for x in 0..w {
            px[y * w + x] = match y / band.max(1) {
                // Band 0: horizontal gradient (LUT/contrast judgement).
                0 => ((x * 255) / w.max(1)) as u8,
                // Band 1: checkerboard, 8-px cells (dither judgement).
                1 => {
                    if ((x / 8) + (y / 8)) % 2 == 0 { 32 } else { 224 }
                }
                // Band 2: vertical line gratings of pitch 2/4/8 px
                // (crispness proxy for small text strokes).
                2 => {
                    let pitch = 2 << ((3 * x) / w.max(1)).min(2); // 2, 4, 8
                    if x % pitch == 0 { 0 } else { 255 }
                }
                // Band 3: flat mid-gray arena for the bouncing box.
                _ => 160,
            };
        }
    }
    px
}

fn render(background: &[u8], width: u32, height: u32, elapsed: Duration) -> Frame {
    let (w, h) = (width as usize, height as usize);
    let mut px = background.to_vec();
    let ms = elapsed.as_millis() as u64;

    // Bouncing box in the bottom band: ping-pong motion.
    let band = (h / 4) as i64;
    let box_size = (band / 2).clamp(16, 96) as usize;
    let track_w = (w - box_size).max(1) as u64;
    let track_h = (band as usize - box_size).max(1) as u64;
    let bx = ping_pong(ms / 4, track_w) as usize;
    let by = 3 * band as usize + ping_pong(ms / 7, track_h) as usize;
    fill_rect(&mut px, w, bx, by, box_size, box_size, 0);
    fill_rect(&mut px, w, bx + 4, by + 4, box_size - 8, box_size - 8, 255);

    // Millisecond counter, top-left: seven-segment, black on white plate.
    let scale = (w / 160).clamp(2, 8);
    draw_ms_counter(&mut px, w, ms, scale);

    // Full-frame inversion flash: 1 s out of every 10. Forces the server's
    // diff to cover the entire framebuffer — the "full update" stress case.
    if ms % 10_000 >= 9_000 {
        for p in &mut px {
            *p = 255 - *p;
        }
    }

    Frame {
        width,
        height,
        format: PixelFormat::Gray8,
        data: px,
        // None = "damage unknown, treat everything as dirty". The VNC
        // framebuffer diffs for us in M0; real damage arrives with M1 capture.
        damage: None,
    }
}

/// Bounce `t` back and forth over `0..=len`.
fn ping_pong(t: u64, len: u64) -> u64 {
    let period = 2 * len;
    let m = t % period;
    if m < len { m } else { period - m }
}

// (fill_rect and the seven-segment renderer live in papercast_core::overlay,
// shared with the --latency-test frame stamp.)
