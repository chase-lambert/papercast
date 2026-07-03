//! The producer/consumer seam between frame sources and the rest of the app.
//!
//! A source is anything that pushes [`Frame`]s into a channel: a tokio task
//! (test pattern) or a dedicated OS thread running a Wayland event loop
//! (screen capture). Consumers only ever see a [`SourceHandle`], so backends
//! are interchangeable without trait-object gymnastics around `async fn`.

use papercast_core::Frame;
use tokio::sync::mpsc;

/// A running frame source. Dropping the handle (and its receiver) signals the
/// producer to shut down: its next `send` fails and it exits.
pub struct SourceHandle {
    pub width: u32,
    pub height: u32,
    pub frames: mpsc::Receiver<Frame>,
}

/// Small bound: if the consumer stalls we'd rather the producer wait (or
/// drop frames at the capture side) than queue stale frames — latency beats
/// completeness for a mirror.
pub const FRAME_CHANNEL_DEPTH: usize = 2;
