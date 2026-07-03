//! Unsharp-mask sharpening: `out = in + amount * (in - blur(in))`.
//!
//! Small text is the whole point of an e-ink monitor, and EPD panels plus
//! scaling soften strokes; boosting local contrast around edges keeps glyphs
//! legible. The blur is a separable box filter — two O(n) running-sum passes —
//! which at radius 1–2 is visually close enough to gaussian and very cheap.

use rayon::prelude::*;

/// Sharpen `buf` (grayscale, `width`-stride) in place. `amount` 0.0 = no-op,
/// typical range 0.5–1.5. `radius` is the box half-width in pixels.
/// `scratch` buffers are caller-owned so repeated frames don't reallocate.
pub fn unsharp(
    buf: &mut [u8],
    width: usize,
    amount: f32,
    radius: usize,
    scratch_h: &mut Vec<u8>,
    scratch_v: &mut Vec<u8>,
) {
    if amount <= 0.0 || radius == 0 || buf.is_empty() {
        return;
    }
    let height = buf.len() / width;
    scratch_h.resize(buf.len(), 0);
    scratch_v.resize(buf.len(), 0);

    blur_horizontal(buf, scratch_h, width, radius);
    blur_vertical(scratch_h, scratch_v, width, height, radius);

    // Fixed-point amount (×256) keeps the hot loop in integer math.
    let amount_fp = (amount * 256.0) as i32;
    buf.par_chunks_mut(width)
        .zip(scratch_v.par_chunks(width))
        .for_each(|(row, blur_row)| {
            for (v, &b) in row.iter_mut().zip(blur_row) {
                let orig = i32::from(*v);
                let diff = orig - i32::from(b);
                *v = (orig + ((amount_fp * diff) >> 8)).clamp(0, 255) as u8;
            }
        });
}

/// Box blur along rows: independent per row, so rayon splits by row.
fn blur_horizontal(src: &[u8], dst: &mut [u8], width: usize, radius: usize) {
    let window = (2 * radius + 1) as u32;
    src.par_chunks(width)
        .zip(dst.par_chunks_mut(width))
        .for_each(|(src_row, dst_row)| {
            // Running sum with clamped (edge-replicated) borders.
            let at = |x: isize| -> u32 {
                let x = x.clamp(0, width as isize - 1) as usize;
                u32::from(src_row[x])
            };
            let mut sum: u32 = (-(radius as isize)..=radius as isize).map(at).sum();
            for (x, out) in dst_row.iter_mut().enumerate() {
                *out = (sum / window) as u8;
                sum += at(x as isize + radius as isize + 1);
                sum -= at(x as isize - radius as isize);
            }
        });
}

/// Box blur along columns. Row-major memory makes per-column parallelism
/// cache-hostile, so instead we keep one running sum per column and sweep
/// rows top to bottom — a single serial pass, but each step is a whole-row
/// add/subtract that vectorizes well.
fn blur_vertical(src: &[u8], dst: &mut [u8], width: usize, height: usize, radius: usize) {
    let window = (2 * radius + 1) as u32;
    let row = |y: isize| -> &[u8] {
        let y = y.clamp(0, height as isize - 1) as usize;
        &src[y * width..(y + 1) * width]
    };
    let mut sums = vec![0u32; width];
    for y in -(radius as isize)..=(radius as isize) {
        for (s, &v) in sums.iter_mut().zip(row(y)) {
            *s += u32::from(v);
        }
    }
    for y in 0..height {
        let dst_row = &mut dst[y * width..(y + 1) * width];
        for (d, &s) in dst_row.iter_mut().zip(&sums) {
            *d = (s / window) as u8;
        }
        let add = row(y as isize + radius as isize + 1);
        let sub = row(y as isize - radius as isize);
        for ((s, &a), &b) in sums.iter_mut().zip(add).zip(sub) {
            *s += u32::from(a);
            *s -= u32::from(b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sharpen(buf: &mut [u8], width: usize, amount: f32, radius: usize) {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        unsharp(buf, width, amount, radius, &mut a, &mut b);
    }

    #[test]
    fn flat_region_unchanged() {
        let mut buf = vec![100u8; 16 * 16];
        sharpen(&mut buf, 16, 1.0, 1);
        assert!(buf.iter().all(|&v| v == 100));
    }

    #[test]
    fn step_edge_gains_overshoot() {
        // Vertical step edge: left dark, right bright.
        let width = 16;
        let mut buf: Vec<u8> = (0..16 * width)
            .map(|i| if i % width < 8 { 50 } else { 200 })
            .collect();
        sharpen(&mut buf, width, 1.0, 1);
        let row = &buf[4 * width..5 * width];
        // Dark side of the edge dips darker, bright side pushes brighter.
        assert!(row[7] < 50, "dark side {}", row[7]);
        assert!(row[8] > 200, "bright side {}", row[8]);
        // Far from the edge: untouched.
        assert_eq!(row[0], 50);
        assert_eq!(row[15], 200);
    }

    #[test]
    fn zero_amount_is_noop() {
        let mut buf: Vec<u8> = (0..64u8).collect();
        let orig = buf.clone();
        sharpen(&mut buf, 8, 0.0, 1);
        assert_eq!(buf, orig);
    }
}
