//! Grayscale resizing with aspect-ratio fit modes (desktop is ~16:9/16:10,
//! e-ink tablets are 4:3 — something has to give).

use fast_image_resize as fir;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FitMode {
    /// Scale to fit entirely, pad the rest with white (paper-like margins).
    #[default]
    Letterbox,
    /// Scale to fill, cropping source edges symmetrically.
    Crop,
    /// Ignore aspect ratio.
    Stretch,
}

#[derive(Debug, thiserror::Error)]
#[error("resize failed: {0}")]
pub struct ScaleError(String);

pub struct Scaler {
    resizer: fir::Resizer,
}

impl Default for Scaler {
    fn default() -> Self {
        Self { resizer: fir::Resizer::new() }
    }
}

impl Scaler {
    /// Resize gray `src` (sw×sh) into `dst` (dw×dh) honoring `fit`.
    /// `dst` is fully rewritten (letterbox bars are white).
    pub fn scale_gray(
        &mut self,
        src: &mut [u8],
        (sw, sh): (u32, u32),
        dst: &mut Vec<u8>,
        (dw, dh): (u32, u32),
        fit: FitMode,
    ) -> Result<(), ScaleError> {
        let err = |e: &dyn std::fmt::Display| ScaleError(e.to_string());
        dst.clear();
        dst.resize(dw as usize * dh as usize, 255);

        let src_img = fir::images::Image::from_slice_u8(sw, sh, src, fir::PixelType::U8)
            .map_err(|e| err(&e))?;

        match fit {
            FitMode::Stretch => {
                let mut dst_img =
                    fir::images::Image::from_slice_u8(dw, dh, dst, fir::PixelType::U8)
                        .map_err(|e| err(&e))?;
                self.resizer.resize(&src_img, &mut dst_img, None).map_err(|e| err(&e))?;
            }
            FitMode::Crop => {
                // Scale factor that fills the destination; crop the source
                // overhang symmetrically via the resizer's sub-pixel crop.
                let scale = f64::max(f64::from(dw) / f64::from(sw), f64::from(dh) / f64::from(sh));
                let crop_w = f64::from(dw) / scale;
                let crop_h = f64::from(dh) / scale;
                let left = (f64::from(sw) - crop_w) / 2.0;
                let top = (f64::from(sh) - crop_h) / 2.0;
                let mut dst_img =
                    fir::images::Image::from_slice_u8(dw, dh, dst, fir::PixelType::U8)
                        .map_err(|e| err(&e))?;
                let opts = fir::ResizeOptions::new().crop(left, top, crop_w, crop_h);
                self.resizer.resize(&src_img, &mut dst_img, &opts).map_err(|e| err(&e))?;
            }
            FitMode::Letterbox => {
                let (pw, ph, px, py) = letterbox_placement((sw, sh), (dw, dh));
                let mut region = fir::images::Image::new(pw, ph, fir::PixelType::U8);
                self.resizer.resize(&src_img, &mut region, None).map_err(|e| err(&e))?;
                // Blit the scaled region into the white destination.
                let region_buf = region.buffer();
                for row in 0..ph as usize {
                    let dst_off = (py as usize + row) * dw as usize + px as usize;
                    let src_off = row * pw as usize;
                    dst[dst_off..dst_off + pw as usize]
                        .copy_from_slice(&region_buf[src_off..src_off + pw as usize]);
                }
            }
        }
        Ok(())
    }
}

/// Scaled size and top-left placement that centers `src` in `dst`
/// preserving aspect ratio: (width, height, x, y).
pub fn letterbox_placement((sw, sh): (u32, u32), (dw, dh): (u32, u32)) -> (u32, u32, u32, u32) {
    let scale = f64::min(f64::from(dw) / f64::from(sw), f64::from(dh) / f64::from(sh));
    let pw = ((f64::from(sw) * scale).round() as u32).clamp(1, dw);
    let ph = ((f64::from(sh) * scale).round() as u32).clamp(1, dh);
    ((pw), (ph), (dw - pw) / 2, (dh - ph) / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterbox_16_9_into_4_3_pads_top_and_bottom() {
        // 1920x1080 into 800x600: width-limited → 800x450 at y=75.
        let (pw, ph, px, py) = letterbox_placement((1920, 1080), (800, 600));
        assert_eq!((pw, ph, px, py), (800, 450, 0, 75));
    }

    #[test]
    fn letterbox_bars_are_white_and_content_lands_centered() {
        let mut scaler = Scaler::default();
        let mut src = vec![0u8; 100 * 50]; // black 2:1 source
        let mut dst = Vec::new();
        scaler.scale_gray(&mut src, (100, 50), &mut dst, (100, 100), FitMode::Letterbox).unwrap();
        assert_eq!(dst[0], 255, "top bar should be white");
        assert_eq!(dst[50 * 100 + 50], 0, "center should be black content");
        assert_eq!(dst[99 * 100 + 50], 255, "bottom bar should be white");
    }

    #[test]
    fn stretch_fills_everything() {
        let mut scaler = Scaler::default();
        let mut src = vec![0u8; 10 * 10];
        let mut dst = Vec::new();
        scaler.scale_gray(&mut src, (10, 10), &mut dst, (20, 40), FitMode::Stretch).unwrap();
        assert!(dst.iter().all(|&v| v == 0));
    }

    #[test]
    fn crop_keeps_center() {
        let mut scaler = Scaler::default();
        // Left half black, right half white, into a tall destination:
        // crop takes the horizontal middle → output splits black/white.
        let mut src: Vec<u8> =
            (0..100 * 100).map(|i| if i % 100 < 50 { 0 } else { 255 }).collect();
        let mut dst = Vec::new();
        scaler.scale_gray(&mut src, (100, 100), &mut dst, (50, 100), FitMode::Crop).unwrap();
        assert!(dst[0] < 32, "left edge should be black-ish, got {}", dst[0]);
        assert!(dst[49] > 223, "right edge should be white-ish, got {}", dst[49]);
    }
}
