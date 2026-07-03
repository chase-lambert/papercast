//! Core types and the e-ink processing pipeline.
//!
//! This crate is deliberately free of Wayland/network dependencies so the
//! image math is unit-testable anywhere (CI, non-Linux, etc.).

pub mod dither;
pub mod eink;
pub mod frame;
pub mod overlay;
pub mod pixel;
pub mod scale;
pub mod sharpen;
pub mod tiles;

pub use eink::{EinkConfig, Pipeline};
pub use frame::{Frame, PixelFormat, Rect};
