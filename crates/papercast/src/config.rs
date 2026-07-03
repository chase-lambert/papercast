//! TOML config file support.
//!
//! Precedence, lowest to highest: built-in defaults → config file (--config)
//! → CLI flags the user actually passed. That's why the tunable CLI args are
//! `Option<T>`: `None` means "not passed", so the file value survives.
//!
//! The `[eink]` section is hot-reloadable: edit the file while running and
//! the pipeline picks it up on the next frame — tune contrast/sharpen/dither
//! live while watching the mirror.

use std::path::Path;

use anyhow::Context;
use papercast_core::EinkConfig;
use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct FileConfig {
    pub eink: EinkConfig,
    pub mirror: MirrorConfig,
}

/// Non-pipeline settings. All optional: unset means "use default/CLI".
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct MirrorConfig {
    /// VNC bind address, e.g. "127.0.0.1:5900".
    pub listen: Option<String>,
    /// Frame rate cap.
    pub fps: Option<u32>,
    /// Output (monitor) name to capture.
    pub output: Option<String>,
    /// Dirty-diff tile size in pixels.
    pub tile_size: Option<u32>,
    /// Force a full-frame refresh every N seconds (ghost clearing). 0 = off.
    pub full_refresh_secs: Option<u64>,
    /// Force a full-frame refresh after N incremental updates. 0 = off.
    pub full_refresh_updates: Option<u64>,
}

pub fn load(path: &Path) -> anyhow::Result<FileConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))
}

/// Reload just the hot-reloadable part. Errors are returned, not fatal:
/// a half-saved file mid-edit shouldn't kill the mirror.
pub fn reload_eink(path: &Path) -> anyhow::Result<EinkConfig> {
    Ok(load(path)?.eink)
}
