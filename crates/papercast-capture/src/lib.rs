//! Screen capture: compositor probing and capture backends.
//!
//! PaperCast implements ext-image-copy-capture-v1 and checks for its required
//! globals at runtime with [`probe::run`]. The probe also reports detected
//! COSMIC-specific and legacy wlroots capture globals, but no backend selects
//! them. Portal/PipeWire capture remains demand-driven future work.

pub mod probe;
pub mod source;
pub mod test_pattern;
pub mod wayland;
