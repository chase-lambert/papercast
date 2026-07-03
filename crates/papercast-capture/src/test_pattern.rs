//! Synthetic animated test source — lets the whole VNC path be exercised with
//! no compositor and no tablet. Deliberately built to stress what e-ink cares
//! about and what the M0b checklist demands:
//!
//! - static fine detail (1-px line gratings ≈ small text) to judge crispness
//! - a bouncing box → small sub-rect updates every frame
//! - a live millisecond seven-segment counter → localized churn + latency probe
//! - a full-frame inversion flash every 10 s → forces full-framebuffer updates

use std::time::{Duration, Instant};

use papercast_core::{Frame, PixelFormat};
use tokio::sync::mpsc;

use crate::source::{SourceHandle, FRAME_CHANNEL_DEPTH};

/// Spawn the test source onto the current tokio runtime.
pub fn spawn(width: u32, height: u32, fps: u32) -> SourceHandle {
    let (tx, rx) = mpsc::channel(FRAME_CHANNEL_DEPTH);
    tokio::spawn(produce(width, height, fps, tx));
    SourceHandle { width, height, frames: rx }
}

async fn produce(width: u32, height: u32, fps: u32, tx: mpsc::Sender<Frame>) {
    let background = render_background(width, height);
    let start = Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_secs_f64(1.0 / f64::from(fps.max(1))));
    // If a tick is missed (slow consumer), don't try to "catch up" with a
    // burst of stale frames — skip ahead. Latency over completeness.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        ticker.tick().await;
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
    draw_counter(&mut px, w, ms, scale);

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

fn fill_rect(px: &mut [u8], stride: usize, x: usize, y: usize, w: usize, h: usize, value: u8) {
    let rows = px.len() / stride;
    for yy in y..(y + h).min(rows) {
        let row = &mut px[yy * stride..yy * stride + stride];
        for p in &mut row[x.min(stride)..(x + w).min(stride)] {
            *p = value;
        }
    }
}

/// Segment layout: bit 6..0 = a,b,c,d,e,f,g (a=top, g=middle, clockwise).
const SEGMENTS: [u8; 10] = [
    0b1111110, // 0
    0b0110000, // 1
    0b1101101, // 2
    0b1111001, // 3
    0b0110011, // 4
    0b1011011, // 5
    0b1011111, // 6
    0b1110000, // 7
    0b1111111, // 8
    0b1111011, // 9
];

fn draw_counter(px: &mut [u8], stride: usize, ms: u64, scale: usize) {
    let digits: Vec<u8> = format!("{ms:08}").bytes().map(|b| b - b'0').collect();
    let cell_w = 6 * scale;
    let plate_w = digits.len() * cell_w + 2 * scale;
    fill_rect(px, stride, 0, 0, plate_w, 12 * scale, 255);
    for (i, &d) in digits.iter().enumerate() {
        draw_digit(px, stride, scale + i * cell_w, scale, scale, d);
    }
}

fn draw_digit(px: &mut [u8], stride: usize, x: usize, y: usize, s: usize, digit: u8) {
    let seg = SEGMENTS[digit as usize];
    let (len, th) = (4 * s, s); // segment length and thickness
    let mut horiz = |on: bool, dy: usize| {
        if on {
            fill_rect(px, stride, x + th, y + dy, len - 2 * th, th, 0);
        }
    };
    horiz(seg & 0b1000000 != 0, 0); // a: top
    horiz(seg & 0b0000001 != 0, len); // g: middle
    horiz(seg & 0b0001000 != 0, 2 * len); // d: bottom
    let mut vert = |on: bool, dx: usize, dy: usize| {
        if on {
            fill_rect(px, stride, x + dx, y + dy, th, len, 0);
        }
    };
    vert(seg & 0b0100000 != 0, len - th, 0); // b: top-right
    vert(seg & 0b0000100 != 0, len - th, len); // c: bottom-right
    vert(seg & 0b0000010 != 0, 0, 0); // f: top-left
    vert(seg & 0b0010000 != 0, 0, len); // e: bottom-left
}
