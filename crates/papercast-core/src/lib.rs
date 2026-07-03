//! Core types and the e-ink processing pipeline.
//!
//! This crate is deliberately free of Wayland/network dependencies so the
//! image math is unit-testable anywhere (CI, non-Linux, etc.).

pub mod frame;

pub use frame::{Frame, PixelFormat, Rect};
