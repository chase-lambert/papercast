//! Screen capture: compositor probing and capture backends.
//!
//! Capture is probe-first: we never assume a protocol is present, we inspect
//! the registry globals at runtime (`probe::run`) and pick the best supported
//! backend. Priority: ext-image-copy-capture-v1 (standard) → COSMIC-specific
//! zcosmic_* → portal/PipeWire (contingency, separate backend).

pub mod probe;
pub mod source;
pub mod test_pattern;
pub mod wayland;
