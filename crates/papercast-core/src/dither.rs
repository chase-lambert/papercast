//! Quantization to N gray levels, with optional dithering.
//!
//! E-ink panels render a limited set of gray levels (16 on most EPDs), so we
//! quantize explicitly and use dithering to trade banding for high-frequency
//! noise the eye integrates away.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DitherMode {
    /// Plain quantization: cheapest, visible banding on gradients.
    None,
    /// Ordered/Bayer 8×8: parallel, deterministic per pixel position, stable
    /// across frames (no shimmer on video). The default.
    #[default]
    Bayer,
    /// Error diffusion: nicest stills, but serial and unstable across
    /// frames — small input changes ripple the whole dither pattern.
    FloydSteinberg,
}

/// Standard 8×8 Bayer threshold matrix, values 0..=63.
const BAYER8: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// Precomputed value→quantized-value table for a given level count.
pub struct Quantizer {
    levels: u8,
    lut: [u8; 256],
}

impl Quantizer {
    pub fn new(levels: u8) -> Self {
        let levels = levels.clamp(2, 255);
        let mut lut = [0u8; 256];
        let max = f32::from(levels - 1);
        for (v, out) in lut.iter_mut().enumerate() {
            let level = (v as f32 * max / 255.0).round();
            *out = (level * 255.0 / max).round() as u8;
        }
        Self { levels, lut }
    }

    /// Quantization step size in 0..=255 space.
    fn step(&self) -> f32 {
        255.0 / f32::from(self.levels - 1)
    }

    /// Quantize `buf` (a `width`-stride image region) in place.
    ///
    /// `origin` is the region's top-left in *final framebuffer* coordinates:
    /// the Bayer matrix is indexed by absolute position so that processing a
    /// sub-region (dirty tile) yields byte-identical results to processing
    /// the whole frame — otherwise tile seams crawl.
    pub fn quantize(&self, buf: &mut [u8], width: usize, origin: (u32, u32), mode: DitherMode) {
        match mode {
            DitherMode::None => {
                for v in buf.iter_mut() {
                    *v = self.lut[*v as usize];
                }
            }
            DitherMode::Bayer => {
                let step = self.step();
                let (ox, oy) = (origin.0 as usize, origin.1 as usize);
                for (row_idx, row) in buf.chunks_mut(width).enumerate() {
                    let brow = &BAYER8[(oy + row_idx) % 8];
                    for (col_idx, v) in row.iter_mut().enumerate() {
                        let threshold = f32::from(brow[(ox + col_idx) % 8]);
                        // Map 0..=63 to a ±step/2 offset around zero.
                        let offset = (threshold + 0.5) / 64.0 - 0.5;
                        let biased = (f32::from(*v) + offset * step).clamp(0.0, 255.0);
                        *v = self.lut[biased as usize];
                    }
                }
            }
            DitherMode::FloydSteinberg => self.floyd_steinberg(buf, width),
        }
    }

    fn floyd_steinberg(&self, buf: &mut [u8], width: usize) {
        let height = buf.len() / width;
        // Error carried to the current and next row, f32 per column.
        let mut err_cur = vec![0.0f32; width];
        let mut err_next = vec![0.0f32; width];
        for y in 0..height {
            for x in 0..width {
                let i = y * width + x;
                let v = (f32::from(buf[i]) + err_cur[x]).clamp(0.0, 255.0);
                let q = self.lut[v as usize];
                buf[i] = q;
                let e = v - f32::from(q);
                if x + 1 < width {
                    err_cur[x + 1] += e * (7.0 / 16.0);
                    err_next[x + 1] += e * (1.0 / 16.0);
                }
                if x > 0 {
                    err_next[x - 1] += e * (3.0 / 16.0);
                }
                err_next[x] += e * (5.0 / 16.0);
            }
            std::mem::swap(&mut err_cur, &mut err_next);
            err_next.fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_quantize_two_levels_thresholds_at_midpoint() {
        let q = Quantizer::new(2);
        let mut buf = vec![0u8, 100, 127, 128, 200, 255];
        q.quantize(&mut buf, 6, (0, 0), DitherMode::None);
        assert_eq!(buf, [0, 0, 0, 255, 255, 255]);
    }

    #[test]
    fn quantized_values_are_representable_levels() {
        let q = Quantizer::new(16);
        let mut buf: Vec<u8> = (0..=255).collect();
        q.quantize(&mut buf, 16, (0, 0), DitherMode::Bayer);
        let step = 255.0 / 15.0;
        for &v in &buf {
            let level = f32::from(v) / step;
            assert!((level - level.round()).abs() < 0.01, "{v} is not on a level");
        }
    }

    #[test]
    fn bayer_is_anchored_to_absolute_coords() {
        let q = Quantizer::new(4);
        // Full 16-wide row processed at origin 0…
        let mut full: Vec<u8> = (0..16u8).map(|i| i * 16).collect();
        q.quantize(&mut full, 16, (0, 0), DitherMode::Bayer);
        // …must equal the right half processed separately with origin x=8.
        let mut half: Vec<u8> = (8..16u8).map(|i| i * 16).collect();
        q.quantize(&mut half, 8, (8, 0), DitherMode::Bayer);
        assert_eq!(&full[8..], &half[..]);
    }

    #[test]
    fn bayer_dithers_flat_midgray_to_mixed_levels() {
        let q = Quantizer::new(2);
        let mut buf = vec![128u8; 64];
        q.quantize(&mut buf, 8, (0, 0), DitherMode::Bayer);
        let whites = buf.iter().filter(|&&v| v == 255).count();
        // ~50% white: mid-gray becomes a checkerish mix, not a flat field.
        assert!((24..=40).contains(&whites), "whites={whites}");
    }

    #[test]
    fn floyd_steinberg_preserves_average_brightness() {
        let q = Quantizer::new(2);
        let mut buf = vec![64u8; 32 * 32];
        q.quantize(&mut buf, 32, (0, 0), DitherMode::FloydSteinberg);
        let avg: f64 = buf.iter().map(|&v| f64::from(v)).sum::<f64>() / buf.len() as f64;
        assert!((avg - 64.0).abs() < 8.0, "avg={avg}");
    }
}
