//! The e-ink processing pipeline: everything between a captured RGB frame
//! and the grayscale image a tablet should display.
//!
//! Per frame: RGB → luminance → scale to target (fit mode) → contrast/gamma
//! LUT → unsharp sharpen → quantize/dither to N gray levels.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::dither::{DitherMode, Quantizer};
use crate::scale::{FitMode, ScaleError, Scaler};
use crate::sharpen::unsharp;
use crate::{Frame, PixelFormat};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct EinkConfig {
    /// Contrast multiplier around mid-gray. 1.0 = unchanged; e-ink likes
    /// punchier than LCD, so the default leans in.
    pub contrast: f32,
    /// Gamma exponent applied to normalized luminance. <1 brightens mids.
    pub gamma: f32,
    /// Input level mapped to pure black (crushes dark noise to ink).
    pub black_point: u8,
    /// Input level mapped to pure white (lets near-white become paper).
    pub white_point: u8,
    /// Invert luminance. Dark desktop themes are e-ink poison (a mostly
    /// black panel ghosts badly and reads poorly); inverting turns them
    /// into black-on-paper.
    pub invert: bool,
    /// Unsharp mask strength; 0 disables. Critical for small text.
    pub sharpen: f32,
    /// Unsharp blur radius in pixels.
    pub sharpen_radius: u32,
    /// Dithering algorithm.
    pub dither: DitherMode,
    /// Gray levels to quantize to (EPD hardware renders 16).
    pub levels: u8,
    /// How to fit the source aspect ratio into the target.
    pub fit: FitMode,
    /// Output size; `None` = keep source size.
    pub target_size: Option<(u32, u32)>,
}

impl Default for EinkConfig {
    fn default() -> Self {
        Self {
            contrast: 1.2,
            gamma: 1.0,
            black_point: 8,
            white_point: 248,
            invert: false,
            sharpen: 1.0,
            sharpen_radius: 1,
            dither: DitherMode::default(),
            levels: 16,
            fit: FitMode::default(),
            target_size: None,
        }
    }
}

/// 256-entry tone curve baked from black/white point, gamma, and contrast.
fn build_tone_lut(cfg: &EinkConfig) -> [u8; 256] {
    let black = f32::from(cfg.black_point);
    let white = f32::from(cfg.white_point.max(cfg.black_point + 1));
    let mut lut = [0u8; 256];
    for (i, out) in lut.iter_mut().enumerate() {
        let mut x = ((i as f32 - black) / (white - black)).clamp(0.0, 1.0);
        x = x.powf(cfg.gamma.max(0.01));
        x = ((x - 0.5) * cfg.contrast.max(0.0) + 0.5).clamp(0.0, 1.0);
        if cfg.invert {
            x = 1.0 - x;
        }
        *out = (x * 255.0).round() as u8;
    }
    lut
}

pub struct Pipeline {
    config: EinkConfig,
    tone_lut: [u8; 256],
    quantizer: Quantizer,
    scaler: Scaler,
    // Reused across frames to avoid per-frame allocation.
    gray: Vec<u8>,
    scaled: Vec<u8>,
    scratch_a: Vec<u8>,
    scratch_b: Vec<u8>,
}

impl Pipeline {
    pub fn new(config: EinkConfig) -> Self {
        Self {
            tone_lut: build_tone_lut(&config),
            quantizer: Quantizer::new(config.levels),
            config,
            scaler: Scaler::default(),
            gray: Vec::new(),
            scaled: Vec::new(),
            scratch_a: Vec::new(),
            scratch_b: Vec::new(),
        }
    }

    pub fn config(&self) -> &EinkConfig {
        &self.config
    }

    /// Swap in a new config (hot-reload path); derived tables are rebuilt.
    pub fn set_config(&mut self, config: EinkConfig) {
        self.tone_lut = build_tone_lut(&config);
        self.quantizer = Quantizer::new(config.levels);
        self.config = config;
    }

    /// Output dimensions for a given source size.
    pub fn output_size(&self, source: (u32, u32)) -> (u32, u32) {
        self.config.target_size.unwrap_or(source)
    }

    /// Run the full pipeline; returns a Gray8 frame at `output_size`.
    pub fn process(&mut self, frame: &Frame) -> Result<Frame, ScaleError> {
        to_gray(frame, &mut self.gray);

        let src_size = (frame.width, frame.height);
        let (out_w, out_h) = self.output_size(src_size);
        let needs_scale = (out_w, out_h) != src_size;

        if needs_scale {
            self.scaler.scale_gray(
                &mut self.gray,
                src_size,
                &mut self.scaled,
                (out_w, out_h),
                self.config.fit,
            )?;
        } else {
            std::mem::swap(&mut self.gray, &mut self.scaled);
        }
        let buf = &mut self.scaled;

        // Tone curve (LUT is one shared read-only table: rayon-safe).
        let lut = &self.tone_lut;
        buf.par_chunks_mut(1 << 14).for_each(|chunk| {
            for v in chunk.iter_mut() {
                *v = lut[*v as usize];
            }
        });

        unsharp(
            buf,
            out_w as usize,
            self.config.sharpen,
            self.config.sharpen_radius as usize,
            &mut self.scratch_a,
            &mut self.scratch_b,
        );

        self.quantizer.quantize(buf, out_w as usize, (0, 0), self.config.dither);

        Ok(Frame {
            width: out_w,
            height: out_h,
            format: PixelFormat::Gray8,
            data: buf.clone(),
        })
    }
}

/// RGB(-ish) → luminance, BT.709 integer weights (sum = 256).
fn to_gray(frame: &Frame, out: &mut Vec<u8>) {
    let n = frame.width as usize * frame.height as usize;
    out.clear();
    out.resize(n, 0);
    let luma = |r: u8, g: u8, b: u8| -> u8 {
        ((54 * u32::from(r) + 183 * u32::from(g) + 19 * u32::from(b) + 128) >> 8) as u8
    };
    match frame.format {
        PixelFormat::Gray8 => out.copy_from_slice(&frame.data),
        PixelFormat::Bgrx8888 => {
            out.par_chunks_mut(1 << 13)
                .zip(frame.data.par_chunks(4 << 13))
                .for_each(|(dst, src)| {
                    for (d, px) in dst.iter_mut().zip(src.chunks_exact(4)) {
                        *d = luma(px[2], px[1], px[0]);
                    }
                });
        }
        PixelFormat::Rgbx8888 => {
            out.par_chunks_mut(1 << 13)
                .zip(frame.data.par_chunks(4 << 13))
                .for_each(|(dst, src)| {
                    for (d, px) in dst.iter_mut().zip(src.chunks_exact(4)) {
                        *d = luma(px[0], px[1], px[2]);
                    }
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray_frame(w: u32, h: u32, data: Vec<u8>) -> Frame {
        Frame { width: w, height: h, format: PixelFormat::Gray8, data }
    }

    #[test]
    fn tone_lut_is_monotonic_and_hits_endpoints() {
        let lut = build_tone_lut(&EinkConfig::default());
        assert_eq!(lut[0], 0);
        assert_eq!(lut[255], 255);
        for w in lut.windows(2) {
            assert!(w[1] >= w[0], "LUT must be monotonic");
        }
    }

    #[test]
    fn black_white_points_crush_extremes() {
        let cfg = EinkConfig { black_point: 20, white_point: 235, ..Default::default() };
        let lut = build_tone_lut(&cfg);
        assert_eq!(lut[20], 0);
        assert_eq!(lut[235], 255);
        assert_eq!(lut[5], 0);
        assert_eq!(lut[250], 255);
    }

    #[test]
    fn luminance_weights_green_highest() {
        let px = |r, g, b| Frame {
            width: 1,
            height: 1,
            format: PixelFormat::Bgrx8888,
            data: vec![b, g, r, 0],
        };
        let mut out = Vec::new();
        to_gray(&px(255, 0, 0), &mut out);
        let red = out[0];
        to_gray(&px(0, 255, 0), &mut out);
        let green = out[0];
        to_gray(&px(0, 0, 255), &mut out);
        let blue = out[0];
        assert!(green > red && red > blue, "BT.709 ordering: g={green} r={red} b={blue}");
        to_gray(&px(255, 255, 255), &mut out);
        assert_eq!(out[0], 255);
    }

    #[test]
    fn process_end_to_end_scales_and_quantizes() {
        let mut p = Pipeline::new(EinkConfig {
            target_size: Some((64, 64)),
            fit: FitMode::Letterbox,
            levels: 2,
            sharpen: 0.0,
            ..Default::default()
        });
        // 128x64 mid-gray source → letterboxed into 64x64.
        let out = p.process(&gray_frame(128, 64, vec![128; 128 * 64])).unwrap();
        assert_eq!((out.width, out.height), (64, 64));
        assert_eq!(out.format, PixelFormat::Gray8);
        assert_eq!(out.data.len(), 64 * 64);
        // Levels=2 → only pure black/white pixels remain.
        assert!(out.data.iter().all(|&v| v == 0 || v == 255));
        // Letterbox bars (top rows) are white.
        assert!(out.data[..64 * 16].iter().all(|&v| v == 255));
    }
}
